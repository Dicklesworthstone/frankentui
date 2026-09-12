#!/usr/bin/env python3
"""Check README environment attribution; read-only, stdlib-only, JSONL output.

Recognizes literal reads through the workspace's env helpers, not arbitrary Rust
data flow. Inverse coverage is deliberately scoped to harness main/showcase CLI.
Run --self-test for retained fixtures; no temporary files or cleanup are used.
"""

import argparse
import json
from pathlib import Path
import re
import shlex
import sys


BINARIES = ("ftui-harness", "ftui-demo-showcase")
READERS = {"var", "var_os", "get_env", "env_flag", "env_u64", "env_string",
           "env_flag_with"}
TOKEN = re.compile(r'//[^\n]*|/\*|r(?P<hashes>\#*)".*?"(?P=hashes)|'
                   r'"(?:\\.|[^"\\])*"|\'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\'\\])\'|'
                   r'[A-Za-z_][A-Za-z_0-9]*|[^\s]', re.S)
VARIABLE = re.compile(r"FTUI_[A-Z0-9_]+\Z")


def rust_tokens(source):
    """Keep strings atomic and discard comments, including nested blocks."""
    tokens = []
    position = 0
    while match := TOKEN.search(source, position):
        value = match.group()
        position = match.end()
        if value.startswith("//"):
            continue
        if value == "/*":
            depth = 1
            while depth:
                marker = re.search(r"/\*|\*/", source[position:])
                if marker is None:
                    raise ValueError("unterminated Rust block comment")
                depth += 1 if marker.group() == "/*" else -1
                position += marker.end()
            continue
        tokens.append(value)
    production = []
    i = 0
    while i < len(tokens):
        if tokens[i:i + 8] == ["#", "[", "cfg", "(", "test", ")", "]", "mod"]:
            i += 9  # Skip the attribute, mod keyword and module name.
            if tokens[i:i + 1] == [";"]:
                i += 1
                continue
            if tokens[i:i + 1] != ["{"]:
                raise ValueError("unrecognized cfg(test) module")
            depth = 1
            i += 1
            while i < len(tokens) and depth:
                depth += (tokens[i] == "{") - (tokens[i] == "}")
                i += 1
            if depth:
                raise ValueError("unterminated cfg(test) module")
            continue
        production.append(tokens[i])
        i += 1
    return production


def literal_variable(token):
    match = re.fullmatch(r'"(FTUI_[A-Z0-9_]+)"|r(?P<h>\#*)"(FTUI_[A-Z0-9_]+)"(?P=h)', token)
    return (match[1] or match[3]) if match else None


def reads(source):
    tokens = rust_tokens(source)
    found = set()
    for i, token in enumerate(tokens[:-2]):
        if tokens[i + 1] != "(":
            continue
        if token in READERS:
            literal = literal_variable(tokens[i + 2])
            if literal:
                found.add(literal)
        if token == "seed_from_env" and tokens[i + 2:i + 4] == ["&", "["]:
            for literal in tokens[i + 4:]:
                if literal == "]":
                    break
                if variable := literal_variable(literal):
                    found.add(variable)
    return found


def references(markdown):
    """Tagged blocks plus inline cargo-run assignments anywhere in README."""
    pairs = []
    pending = None
    owner = None
    fenced = False
    for number, line in enumerate(markdown.splitlines(), 1):
        tag = re.fullmatch(r"\s*<!-- env:([^ ]+) -->\s*", line)
        if tag:
            pending = tag[1]
            if pending not in BINARIES:
                raise ValueError(f"README:{number}: unknown env owner {pending}")
        if line.startswith("```"):
            fenced = not fenced
            owner = pending if fenced else None
            pending = None
            continue
        if owner:
            pairs.extend((owner, var, number) for var in
                         re.findall(r"\b(FTUI_[A-Z0-9_]+)\b", line))
        if "cargo run" in line and "FTUI_" in line:
            words = shlex.split(line, comments=True)
            if "-p" in words:
                index = words.index("-p") + 1
                if index < len(words) and words[index] in BINARIES:
                    binary = words[index]
                    if any(word == "--example" or word.startswith("--example=") for word in words):
                        raise ValueError(f"README:{number}: example environment attribution needs its own source scope")
                    pairs.extend((binary, word.split("=", 1)[0], number)
                                 for word in words[:words.index("cargo")]
                                 if "=" in word and VARIABLE.fullmatch(word.split("=", 1)[0]))
    return sorted(set(pairs))


def check(markdown, sources, inverse, allowlist):
    records = []
    pairs = references(markdown)
    for binary, var, line in pairs:
        locations = sorted(path for path, variables in sources[binary].items() if var in variables)
        records.append(dict(kind="attribution", binary=binary, var=var,
                            readme_line=line, found_in=locations,
                            status="passed" if locations else "failed"))
    for binary in BINARIES:
        if not any(pair[0] == binary for pair in pairs):
            records.append(dict(kind="missing_coverage", binary=binary, status="failed"))
        documented = {var for owner, var, _ in pairs if owner == binary}
        for var in sorted(inverse[binary] - documented):
            reason = allowlist.get((binary, var))
            records.append(dict(kind="inventory", binary=binary, var=var,
                                reason=reason, status="internal" if reason else "failed"))
    for (binary, var), reason in sorted(allowlist.items()):
        if var not in inverse[binary]:
            records.append(dict(kind="stale_allowlist", binary=binary, var=var,
                                reason=reason, status="failed"))
    return records


def load_allowlist(path):
    entries = {}
    for number, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split(None, 2)
        if len(parts) != 3 or parts[0] not in BINARIES or not VARIABLE.fullmatch(parts[1]):
            raise ValueError(f"{path}:{number}: expected binary VAR reason")
        key = tuple(parts[:2])
        if key in entries:
            raise ValueError(f"{path}:{number}: duplicate allowlist entry")
        entries[key] = parts[2]
    return entries


def self_test(root):
    cases = json.loads((root / "tests/fixtures/env_docs/cases.json").read_text())
    for case in cases:
        actual = sorted(reads(case["source"])) if "source" in case else [list(p) for p in references(case["markdown"])]
        if actual != case["expected"]:
            raise ValueError(f"fixture {case['name']}: {actual!r} != {case['expected']!r}")
    sources = {binary: {"fixture.rs": {"FTUI_HARNESS_VIEW"} if binary == "ftui-harness"
                                      else {"FTUI_DEMO_SCREEN"}} for binary in BINARIES}
    inverse = {binary: set().union(*paths.values()) for binary, paths in sources.items()}
    good = "<!-- env:ftui-harness -->\n```bash\nexport FTUI_HARNESS_VIEW=default\n```\n<!-- env:ftui-demo-showcase -->\n```bash\nexport FTUI_DEMO_SCREEN=2\n```"
    def require(condition, name):
        if not condition:
            raise ValueError(f"contract fixture failed: {name}")

    require(all(r["status"] == "passed" for r in check(good, sources, inverse, {})), "positive")
    bad = good + "\nFTUI_HARNESS_VIEW=dashboard cargo run -p ftui-demo-showcase"
    require(any(r["kind"] == "attribution" and r["status"] == "failed"
                for r in check(bad, sources, inverse, {})), "wrong binary")
    inverse["ftui-harness"].add("FTUI_HARNESS_NEW")
    require(any(r["kind"] == "inventory" and r["status"] == "failed"
                for r in check(good, sources, inverse, {})), "new variable")
    require(any(r["kind"] == "stale_allowlist" for r in
                check(good, sources, inverse, {("ftui-harness", "FTUI_HARNESS_GONE"): "obsolete"})), "stale exception")
    require(any(r["kind"] == "missing_coverage" for r in check("", sources, inverse, {})), "empty README")
    print(json.dumps(dict(kind="self_test", fixtures=len(cases), contract_cases=5, status="passed")))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--readme", type=Path, help="Read a retained alternate README for negative testing")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    try:
        if args.self_test:
            self_test(args.root)
            return 0
        sources = {binary: {str(path.relative_to(args.root)): reads(path.read_text())
                            for path in (args.root / "crates" / binary / "src").rglob("*.rs")}
                   for binary in BINARIES}
        inverse = {binary: sources[binary][f"crates/{binary}/src/{entry}"]
                   for binary, entry in zip(BINARIES, ("main.rs", "cli.rs"))}
        inverse = {binary: {var for var in variables if var.startswith(
                   "FTUI_HARNESS_" if binary == "ftui-harness" else "FTUI_DEMO_")}
                   for binary, variables in inverse.items()}
        records = check((args.readme or args.root / "README.md").read_text(), sources, inverse,
                        load_allowlist(args.root / "docs/env-internal-allowlist.txt"))
        for record in records:
            print(json.dumps(record))
        failed = sum(record["status"] == "failed" for record in records)
        print(json.dumps(dict(kind="summary", checks=len(records), failures=failed,
                              status="failed" if failed else "passed")))
        return int(bool(failed))
    except (OSError, ValueError, KeyError) as error:
        print(json.dumps(dict(kind="error", status="failed", detail=str(error))))
        return 1


if __name__ == "__main__":
    sys.exit(main())
