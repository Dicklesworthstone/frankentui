#!/usr/bin/env python3
"""Teardown suffix extractor and escape sequence tokenizer for FrankenTUI.

Extracts the terminal teardown suffix from raw PTY captures and tokenizes
escape sequences to verify byte-identity and canonical teardown order across
native and crossterm backends.
"""

import argparse
import json
import re
import sys
from pathlib import Path
from typing import List, Optional, Tuple

# Canonical expected token sequence for full alt-screen teardown
EXPECTED_TEARDOWN_TOKENS = [
    "DECSC",
    "DECSTBM-reset",
    "DECRC",
    "SGR0",
    "kitty-pop",
    "focus-off",
    "paste-off",
    "mouse-off",
    "cursor-show",
    "alt-leave",
]

# Regex patterns for terminal escape sequences
CSI_RE = re.compile(rb"^\x1b\[([\x30-\x3f]*)([\x20-\x2f]*)([\x40-\x7e])")
OSC_RE = re.compile(rb"^\x1b\]([^\x07\x1b]*(?:\x07|\x1b\\))")
DCS_RE = re.compile(rb"^\x1bP([^\x1b]*(?:\x1b\\))")
ESC_2BYTE_RE = re.compile(rb"^\x1b([\x20-\x7e])")

# Known mouse disable parameters (DECSET/DECRST mode numbers)
MOUSE_MODES = {1000, 1001, 1002, 1003, 1005, 1006, 1015, 1016}


def is_mouse_disable_csi(params: bytes, final: bytes) -> bool:
    """Return True if this CSI sequence disables mouse tracking modes."""
    if final != b"l" or not params.startswith(b"?"):
        return False
    param_str = params[1:].decode("ascii", errors="ignore")
    try:
        modes = [int(p) for p in param_str.split(";") if p]
        return bool(modes) and all(m in MOUSE_MODES for m in modes)
    except ValueError:
        return False


def classify_csi(params: bytes, intermediates: bytes, final: bytes) -> str:
    """Classify a CSI sequence into a semantic token name."""
    if final == b"r" and not params and not intermediates:
        return "DECSTBM-reset"
    if final == b"m" and (not params or params == b"0"):
        return "SGR0"
    if final == b"u" and params == b"<":
        return "kitty-pop"
    if final == b"u" and params.startswith(b">"):
        return "kitty-push"
    if final == b"l":
        if params == b"?1004":
            return "focus-off"
        if params == b"?2004":
            return "paste-off"
        if params == b"?25":
            return "cursor-hide"
        if params == b"?1049":
            return "alt-leave"
        if params == b"?2026":
            return "sync-end"
        if is_mouse_disable_csi(params, final):
            return "mouse-off"
    if final == b"h":
        if params == b"?1004":
            return "focus-on"
        if params == b"?2004":
            return "paste-on"
        if params == b"?25":
            return "cursor-show"
        if params == b"?1049":
            return "alt-enter"
        if params == b"?2026":
            return "sync-begin"
    return f"CSI:{params.decode('latin1', errors='replace')}{intermediates.decode('latin1', errors='replace')}{chr(final[0])}"


def tokenize_escape_sequences(data: bytes) -> List[Tuple[str, bytes]]:
    """Tokenize a raw byte stream into a list of (token_name, raw_bytes) tuples."""
    tokens = []
    i = 0
    n = len(data)

    while i < n:
        if data[i] == 0x1B:
            # Check CSI
            m = CSI_RE.match(data[i:])
            if m:
                raw = m.group(0)
                params, interm, final = m.group(1), m.group(2), m.group(3)
                token_name = classify_csi(params, interm, final)
                tokens.append((token_name, raw))
                i += len(raw)
                continue

            # Check OSC
            m = OSC_RE.match(data[i:])
            if m:
                raw = m.group(0)
                tokens.append(("OSC", raw))
                i += len(raw)
                continue

            # Check DCS
            m = DCS_RE.match(data[i:])
            if m:
                raw = m.group(0)
                tokens.append(("DCS", raw))
                i += len(raw)
                continue

            # Check 2-byte escape
            if i + 1 < n:
                b2 = data[i + 1 : i + 2]
                if b2 == b"7":
                    tokens.append(("DECSC", b"\x1b7"))
                    i += 2
                    continue
                if b2 == b"8":
                    tokens.append(("DECRC", b"\x1b8"))
                    i += 2
                    continue
                if b2 == b"c":
                    tokens.append(("RIS", b"\x1bc"))
                    i += 2
                    continue
                m = ESC_2BYTE_RE.match(data[i:])
                if m:
                    raw = m.group(0)
                    tokens.append((f"ESC:{raw[1:].decode('latin1', errors='replace')}", raw))
                    i += len(raw)
                    continue

            # Lone ESC
            tokens.append(("ESC", b"\x1b"))
            i += 1
        else:
            # Collect non-ESC text run
            start = i
            while i < n and data[i] != 0x1B:
                i += 1
            tokens.append(("TEXT", data[start:i]))

    return tokens


def coalesce_mouse_tokens(tokens: List[Tuple[str, bytes]]) -> List[Tuple[str, bytes]]:
    """Coalesce contiguous mouse-off sequences into a single mouse-off token."""
    coalesced = []
    i = 0
    while i < len(tokens):
        token_name, raw = tokens[i]
        if token_name == "mouse-off":
            combined_raw = bytearray(raw)
            j = i + 1
            while j < len(tokens) and tokens[j][0] == "mouse-off":
                combined_raw.extend(tokens[j][1])
                j += 1
            coalesced.append(("mouse-off", bytes(combined_raw)))
            i = j
        else:
            coalesced.append((token_name, raw))
            i += 1
    return coalesced


def extract_teardown_suffix(data: bytes) -> Tuple[bytes, List[str]]:
    """Extract teardown suffix from raw capture bytes.

    Locates the teardown sequence after the last sync-end (`ESC [ ? 2026 l`)
    or last frame boundary, up through terminal restoration (`ESC [ ? 1049 l`
    or `ESC [ ? 25 h`).

    Returns:
        (suffix_bytes, token_names)
    """
    # 1. First look for the last sync-end (ESC [ ? 2026 l)
    sync_end = b"\x1b[?2026l"
    last_sync = data.rfind(sync_end)

    search_region = data[last_sync + len(sync_end) :] if last_sync != -1 else data

    # 2. Look for the start of the canonical teardown plan: DECSC (ESC 7)
    decsc = b"\x1b7"
    decsc_idx = search_region.find(decsc)
    if decsc_idx == -1:
        # Fallback: search globally for the last DECSC in case sync_end was missing
        global_decsc = data.rfind(decsc)
        if global_decsc != -1:
            teardown_start = global_decsc
        else:
            # No DECSC found
            return b"", []
    else:
        if last_sync != -1:
            teardown_start = last_sync + len(sync_end) + decsc_idx
        else:
            teardown_start = decsc_idx

    teardown_candidate = data[teardown_start:]

    # 3. Find the end of teardown:
    # Alt-screen mode teardown ends with ALT_SCREEN_LEAVE (\x1b[?1049l).
    # Inline mode teardown ends with CURSOR_SHOW (\x1b[?25h).
    alt_leave = b"\x1b[?1049l"
    alt_idx = teardown_candidate.find(alt_leave)
    if alt_idx != -1:
        suffix_bytes = teardown_candidate[: alt_idx + len(alt_leave)]
    else:
        cursor_show = b"\x1b[?25h"
        cursor_idx = teardown_candidate.find(cursor_show)
        if cursor_idx != -1:
            suffix_bytes = teardown_candidate[: cursor_idx + len(cursor_show)]
        else:
            suffix_bytes = teardown_candidate

    tokens = tokenize_escape_sequences(suffix_bytes)
    coalesced = coalesce_mouse_tokens(tokens)
    token_names = [t[0] for t in coalesced if t[0] != "TEXT"]

    return suffix_bytes, token_names


def compute_escape_tallies(data: bytes) -> dict:
    """Compute escape sequence tallies from raw capture data."""
    return {
        "2026h": data.count(b"\x1b[?2026h"),
        "2026l": data.count(b"\x1b[?2026l"),
        "1049h": data.count(b"\x1b[?1049h"),
        "1049l": data.count(b"\x1b[?1049l"),
        "25h": data.count(b"\x1b[?25h"),
        "25l": data.count(b"\x1b[?25l"),
        "2004h": data.count(b"\x1b[?2004h"),
        "2004l": data.count(b"\x1b[?2004l"),
        "kitty_pop": data.count(b"\x1b[<u"),
        "mouse_1000l": data.count(b"\x1b[?1000l"),
        "mouse_1002l": data.count(b"\x1b[?1002l"),
        "mouse_1006l": data.count(b"\x1b[?1006l"),
    }


def analyze_capture(file_path: Path) -> dict:
    """Analyze a raw capture file and return structured teardown details."""
    data = file_path.read_bytes()
    suffix_bytes, tokens = extract_teardown_suffix(data)
    kitty_pop_count = tokens.count("kitty-pop")

    return {
        "file": str(file_path),
        "total_bytes": len(data),
        "suffix_len": len(suffix_bytes),
        "hex": suffix_bytes.hex(),
        "tokens": tokens,
        "tallies": compute_escape_tallies(data),
        "kitty_pop_count": kitty_pop_count,
        "kitty_pop_once": kitty_pop_count == 1,
        "tokens_match_expected": tokens == EXPECTED_TEARDOWN_TOKENS,
    }


def compare_captures(native_path: Path, crossterm_path: Path) -> dict:
    """Compare teardown suffixes between native and crossterm captures."""
    native_analysis = analyze_capture(native_path)
    crossterm_analysis = analyze_capture(crossterm_path)

    byte_identical = (
        native_analysis["hex"] == crossterm_analysis["hex"]
        and native_analysis["suffix_len"] > 0
    )
    tokens_identical = native_analysis["tokens"] == crossterm_analysis["tokens"]
    both_match_expected = (
        native_analysis["tokens_match_expected"]
        and crossterm_analysis["tokens_match_expected"]
    )

    return {
        "teardown_bytes_identical": byte_identical,
        "tokens_identical": tokens_identical,
        "tokens_match_expected": both_match_expected,
        "native": native_analysis,
        "crossterm": crossterm_analysis,
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Extract and tokenize teardown suffix from PTY captures."
    )
    parser.add_argument("capture", nargs="?", type=Path, help="Path to raw capture file")
    parser.add_argument(
        "--compare",
        nargs=2,
        metavar=("NATIVE", "CROSSTERM"),
        type=Path,
        help="Compare two captures for byte-identity",
    )
    parser.add_argument(
        "--json", action="store_true", help="Output results as JSON"
    )
    parser.add_argument(
        "--expected-tokens",
        action="store_true",
        help="Print expected teardown token list as JSON",
    )

    args = parser.parse_args()

    if args.expected_tokens:
        print(json.dumps(EXPECTED_TEARDOWN_TOKENS, indent=2))
        return 0

    if args.compare:
        native_path, crossterm_path = args.compare
        if not native_path.exists():
            sys.stderr.write(f"Error: file not found: {native_path}\n")
            return 2
        if not crossterm_path.exists():
            sys.stderr.write(f"Error: file not found: {crossterm_path}\n")
            return 2

        result = compare_captures(native_path, crossterm_path)
        if args.json:
            print(json.dumps(result, indent=2))
        else:
            print(f"Teardown bytes identical: {result['teardown_bytes_identical']}")
            print(f"Native tokens: {result['native']['tokens']}")
            print(f"Crossterm tokens: {result['crossterm']['tokens']}")
            print(f"Tokens match expected: {result['tokens_match_expected']}")
            print(f"Native hex:    {result['native']['hex']}")
            print(f"Crossterm hex: {result['crossterm']['hex']}")

        if (
            result["teardown_bytes_identical"]
            and result["tokens_match_expected"]
            and result["native"]["kitty_pop_once"]
            and result["crossterm"]["kitty_pop_once"]
        ):
            return 0
        return 1

    if args.capture:
        if not args.capture.exists():
            sys.stderr.write(f"Error: file not found: {args.capture}\n")
            return 2

        result = analyze_capture(args.capture)
        if args.json:
            print(json.dumps(result, indent=2))
        else:
            print(f"Tokens: {result['tokens']}")
            print(f"Hex: {result['hex']}")
            print(f"Suffix length: {result['suffix_len']}")
            print(f"Kitty pop count: {result['kitty_pop_count']}")
            print(f"Matches expected: {result['tokens_match_expected']}")
        return 0 if result["tokens_match_expected"] else 1

    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
