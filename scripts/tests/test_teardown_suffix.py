#!/usr/bin/env python3
"""Unit tests for tests/e2e/lib/teardown_suffix.py."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

# Add tests/e2e/lib to sys.path
SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent.parent
sys.path.insert(0, str(REPO_ROOT / "tests" / "e2e" / "lib"))

from teardown_suffix import (
    EXPECTED_TEARDOWN_TOKENS,
    analyze_capture,
    coalesce_mouse_tokens,
    compare_captures,
    extract_teardown_suffix,
    tokenize_escape_sequences,
)

# Canonical mouse disable sequence from session_teardown.rs
MOUSE_DISABLE = (
    b"\x1b[?1000;1002;1006l\x1b[?1000l\x1b[?1002l\x1b[?1006l"
    b"\x1b[?1001l\x1b[?1003l\x1b[?1005l\x1b[?1015l\x1b[?1016l"
)

# Canonical teardown sequence body (after sync_end)
CANONICAL_TEARDOWN_BYTES = (
    b"\x1b7"  # DECSC
    b"\x1b[r"  # DECSTBM-reset
    b"\x1b8"  # DECRC
    b"\x1b[0m"  # SGR0
    b"\x1b[<u"  # kitty-pop
    b"\x1b[?1004l"  # focus-off
    b"\x1b[?2004l"  # paste-off
    + MOUSE_DISABLE  # mouse-off
    + b"\x1b[?25h"  # cursor-show
    + b"\x1b[?1049l"  # alt-leave
)


class TestTeardownSuffix(unittest.TestCase):
    def test_esc_2byte_tokens(self):
        data = b"\x1b7\x1b8\x1bc"
        tokens = tokenize_escape_sequences(data)
        self.assertEqual([t[0] for t in tokens], ["DECSC", "DECRC", "RIS"])

    def test_csi_tokenization(self):
        data = (
            b"\x1b[r"  # DECSTBM-reset
            b"\x1b[0m"  # SGR0
            b"\x1b[m"  # SGR0
            b"\x1b[<u"  # kitty-pop
            b"\x1b[>15u"  # kitty-push
            b"\x1b[?1004l"  # focus-off
            b"\x1b[?1004h"  # focus-on
            b"\x1b[?2004l"  # paste-off
            b"\x1b[?2004h"  # paste-on
            b"\x1b[?25h"  # cursor-show
            b"\x1b[?25l"  # cursor-hide
            b"\x1b[?1049l"  # alt-leave
            b"\x1b[?1049h"  # alt-enter
            b"\x1b[?2026l"  # sync-end
            b"\x1b[?2026h"  # sync-begin
        )
        tokens = tokenize_escape_sequences(data)
        names = [t[0] for t in tokens]
        self.assertEqual(
            names,
            [
                "DECSTBM-reset",
                "SGR0",
                "SGR0",
                "kitty-pop",
                "kitty-push",
                "focus-off",
                "focus-on",
                "paste-off",
                "paste-on",
                "cursor-show",
                "cursor-hide",
                "alt-leave",
                "alt-enter",
                "sync-end",
                "sync-begin",
            ],
        )

    def test_mouse_disable_classification(self):
        data = b"\x1b[?1000;1002;1006l\x1b[?1000l\x1b[?1016l"
        tokens = tokenize_escape_sequences(data)
        self.assertEqual([t[0] for t in tokens], ["mouse-off", "mouse-off", "mouse-off"])

        coalesced = coalesce_mouse_tokens(tokens)
        self.assertEqual(len(coalesced), 1)
        self.assertEqual(coalesced[0][0], "mouse-off")
        self.assertEqual(coalesced[0][1], data)

    def test_osc_and_dcs_tokenization(self):
        data = (
            b"\x1b]0;Title\x07"  # OSC with BEL
            b"\x1b]52;c;c29tZXRoaW5n\x1b\\"  # OSC with ST
            b"\x1bP$q\"p\x1b\\"  # DCS with ST
        )
        tokens = tokenize_escape_sequences(data)
        self.assertEqual([t[0] for t in tokens], ["OSC", "OSC", "DCS"])

    def test_canonical_teardown_tokens(self):
        tokens = tokenize_escape_sequences(CANONICAL_TEARDOWN_BYTES)
        coalesced = coalesce_mouse_tokens(tokens)
        names = [t[0] for t in coalesced if t[0] != "TEXT"]
        self.assertEqual(names, EXPECTED_TEARDOWN_TOKENS)
        self.assertEqual(len(CANONICAL_TEARDOWN_BYTES), 127)

    def test_extract_from_full_capture(self):
        # Simulate full terminal session capture
        frame1 = b"\x1b[?2026h\x1b[1;1HFrame 1\x1b[?2026l"
        frame2 = b"\x1b[?2026h\x1b[1;1HFrame 2\x1b[?2026l"
        teardown = b"\x1b[?2026l" + CANONICAL_TEARDOWN_BYTES
        full_capture = frame1 + frame2 + teardown

        suffix, tokens = extract_teardown_suffix(full_capture)
        self.assertEqual(suffix, CANONICAL_TEARDOWN_BYTES)
        self.assertEqual(tokens, EXPECTED_TEARDOWN_TOKENS)

    def test_extract_with_trailing_panic_message(self):
        frame = b"\x1b[?2026h\x1b[1;1HFrame\x1b[?2026l"
        panic_msg = b"\nthread 'main' panicked at 'deliberate demo panic'\nstack backtrace:\n"
        full_capture = frame + b"\x1b[?2026l" + CANONICAL_TEARDOWN_BYTES + panic_msg

        suffix, tokens = extract_teardown_suffix(full_capture)
        self.assertEqual(suffix, CANONICAL_TEARDOWN_BYTES)
        self.assertEqual(tokens, EXPECTED_TEARDOWN_TOKENS)

    def test_compare_captures(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            p1 = Path(tmpdir) / "native.raw"
            p2 = Path(tmpdir) / "crossterm.raw"

            frame = b"\x1b[?2026h\x1b[1;1HHello\x1b[?2026l"
            p1.write_bytes(frame + b"\x1b[?2026l" + CANONICAL_TEARDOWN_BYTES)
            p2.write_bytes(frame + b"\x1b[?2026l" + CANONICAL_TEARDOWN_BYTES)

            result = compare_captures(p1, p2)
            self.assertTrue(result["teardown_bytes_identical"])
            self.assertTrue(result["tokens_identical"])
            self.assertTrue(result["tokens_match_expected"])
            self.assertEqual(result["native"]["kitty_pop_count"], 1)
            self.assertEqual(result["crossterm"]["kitty_pop_count"], 1)


if __name__ == "__main__":
    unittest.main()
