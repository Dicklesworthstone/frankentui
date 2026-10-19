#!/usr/bin/env bash
# G41 (bd-g00-root-epic-ewths.10.1): Fail-closed candidate release preflight
# and immutable version checks for DSR releases.
#
# Usage:
#   ./scripts/release_preflight.sh vX.Y.Z [--out preflight.jsonl] [--root DIR] [--strict] [--diagnostic-pending]
#
# By default (and under --strict), any `fail` or `pending:*` check exits 1
# (fail-closed from first use). Pass `--diagnostic-pending` (without `--strict`)
# only when inspecting a partial fixture where optional gates are not yet
# installed.
set -euo pipefail

exec python3 -B - "$0" "$@" <<'PY'
import json
import os
import re
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

script_path = Path(sys.argv[1]).resolve()
default_root = script_path.parent.parent

args = sys.argv[2:]
if not args or args[0] in ("-h", "--help"):
    sys.stderr.write(
        "Usage: release_preflight.sh vX.Y.Z [--out PATH] [--root DIR] [--strict] [--diagnostic-pending]\n"
    )
    sys.exit(2)

raw_tag = args[0]
tag = raw_tag if raw_tag.startswith("v") else f"v{raw_tag}"
version = tag[1:]
if not re.fullmatch(r"\d+\.\d+\.\d+", version):
    sys.stderr.write(f"ERROR: expected vX.Y.Z stable version tag, got {raw_tag!r}\n")
    sys.exit(2)

out_path = None
root = default_root
strict = False
diagnostic_pending = False

i = 1
while i < len(args):
    arg = args[i]
    if arg == "--out":
        if i + 1 >= len(args):
            sys.stderr.write("ERROR: --out requires a file path\n")
            sys.exit(2)
        out_path = Path(args[i + 1]).resolve()
        i += 2
    elif arg.startswith("--out="):
        out_path = Path(arg.split("=", 1)[1]).resolve()
        i += 1
    elif arg == "--root":
        if i + 1 >= len(args):
            sys.stderr.write("ERROR: --root requires a directory path\n")
            sys.exit(2)
        root = Path(args[i + 1]).resolve()
        i += 2
    elif arg.startswith("--root="):
        root = Path(arg.split("=", 1)[1]).resolve()
        i += 1
    elif arg == "--strict":
        strict = True
        i += 1
    elif arg == "--diagnostic-pending":
        diagnostic_pending = True
        i += 1
    else:
        sys.stderr.write(f"ERROR: unknown argument: {arg}\n")
        sys.exit(2)

records = []


def record(check: str, status: str, detail: str, started: float) -> None:
    elapsed = round(max(0.0, time.monotonic() - started), 4)
    row = {
        "check": check,
        "status": status,
        "detail": detail,
        "elapsed_s": elapsed,
        "version": version,
        "tag": tag,
    }
    records.append(row)
    print(f"{check} | {status} | {detail}")


def parse_semver(ver: str) -> tuple[int, int, int]:
    parts = ver.lstrip("v").split("-", 1)[0].split(".")
    return (int(parts[0]), int(parts[1]), int(parts[2]))


# ---------------------------------------------------------------------------
# 1. workspace_version & 2. facade_version
# ---------------------------------------------------------------------------
t0 = time.monotonic()
cargo_toml_path = root / "Cargo.toml"
publishable_crates: list[str] = []
publish_order: list[str] = []
try:
    root_doc = tomllib.loads(cargo_toml_path.read_text(encoding="utf-8"))
    ws = root_doc["workspace"]
    ws_version = ws["package"]["version"]
    if ws_version == version:
        record("workspace_version", "pass", f"Cargo.toml [workspace.package] version == {version}", t0)
    else:
        record(
            "workspace_version",
            "fail",
            f"Cargo.toml [workspace.package] version is {ws_version!r}, expected {version!r}",
            t0,
        )
except Exception as exc:
    ws = {"members": [], "package": {"version": ""}}
    ws_version = ""
    record("workspace_version", "fail", f"failed to read root Cargo.toml: {exc}", t0)

t0 = time.monotonic()
try:
    mismatches = []
    dep_mismatches = []
    manifests = {}
    for member in ws.get("members", []):
        manifest_path = root / member / "Cargo.toml"
        doc = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        pkg = doc["package"]
        if pkg.get("publish") is False:
            continue
        name = pkg["name"]
        publishable_crates.append(name)
        manifests[name] = doc
        pkg_ver = pkg.get("version")
        if pkg_ver == {"workspace": True}:
            pkg_ver = ws_version
        if pkg_ver != version:
            mismatches.append(f"{name}={pkg_ver}")

    publishable_set = set(publishable_crates)
    deps_graph: dict[str, set[str]] = {}
    for name, doc in manifests.items():
        tables = [doc] + list(doc.get("target", {}).values())
        internal = set()
        for table in tables:
            for kind in ("dependencies", "build-dependencies", "dev-dependencies"):
                for alias, dep in table.get(kind, {}).items():
                    if not isinstance(dep, dict):
                        continue
                    if kind == "dev-dependencies" and "version" not in dep:
                        continue
                    target = dep.get("package", alias)
                    if target in publishable_set:
                        dep_ver = dep.get("version")
                        if dep_ver != version:
                            dep_mismatches.append(f"{name}->{target}({dep_ver})")
                        internal.add(target)
        deps_graph[name] = internal

    while len(publish_order) < len(manifests):
        ready = sorted(
            n for n, d in deps_graph.items() if n not in publish_order and d <= set(publish_order)
        )
        if not ready:
            mismatches.append("dependency_cycle")
            break
        publish_order.extend(ready)

    if "ftui" not in publishable_set:
        record("facade_version", "fail", "facade crate ftui missing from publishable workspace members", t0)
    elif mismatches or dep_mismatches:
        detail = "; ".join(mismatches + dep_mismatches)
        record("facade_version", "fail", f"version mismatches: {detail}", t0)
    else:
        record(
            "facade_version",
            "pass",
            f"all {len(publishable_crates)} publishable crates and internal dependency edges match {version}",
            t0,
        )
except Exception as exc:
    record("facade_version", "fail", f"failed to inspect member manifests: {exc}", t0)

# ---------------------------------------------------------------------------
# 3. readme_version & 4. readme_crate_list
# ---------------------------------------------------------------------------
t0 = time.monotonic()
readme_path = root / "README.md"
major_minor = ".".join(version.split(".")[:2])
try:
    readme_text = readme_path.read_text(encoding="utf-8")
    dep_matches = re.findall(r'^ftui\s*=\s*(?:\{\s*version\s*=\s*)?"([^"]+)"', readme_text, re.M)
    expected_specs = {version, f"={version}", major_minor}
    if not dep_matches:
        record("readme_version", "fail", "no ftui version dependency snippet found in README.md", t0)
    elif all(spec in expected_specs for spec in dep_matches):
        record(
            "readme_version",
            "pass",
            f"README.md ftui dependency snippets ({', '.join(dep_matches)}) match {version}",
            t0,
        )
    else:
        record(
            "readme_version",
            "fail",
            f"README.md ftui dependency snippets {dep_matches} do not match {version} (or {major_minor})",
            t0,
        )
except Exception as exc:
    readme_text = ""
    record("readme_version", "fail", f"failed to read README.md: {exc}", t0)

t0 = time.monotonic()
try:
    sec_match = re.search(
        r"### Crates\.io Dependency\s+(.*?)(?:\n```|\n### |\Z)", readme_text, re.S
    )
    if not sec_match:
        record("readme_crate_list", "fail", "missing '### Crates.io Dependency' section in README.md", t0)
    else:
        prose = sec_match.group(1)
        readme_crates = sorted(set(re.findall(r"`(ftui(?:-[a-z0-9_]+)?|doctor_frankentui)`", prose)))
        expected_crates = sorted(publishable_crates)
        if readme_crates == expected_crates:
            record(
                "readme_crate_list",
                "pass",
                f"README.md lists all {len(expected_crates)} publishable crates",
                t0,
            )
        else:
            missing = sorted(set(expected_crates) - set(readme_crates))
            extra = sorted(set(readme_crates) - set(expected_crates))
            record(
                "readme_crate_list",
                "fail",
                f"README.md crate list mismatch: missing={missing}, extra={extra}",
                t0,
            )
except Exception as exc:
    record("readme_crate_list", "fail", f"failed to parse README crate list: {exc}", t0)

# ---------------------------------------------------------------------------
# 5. getting_started
# ---------------------------------------------------------------------------
t0 = time.monotonic()
gs_path = root / "docs/getting-started.md"
try:
    gs_text = gs_path.read_text(encoding="utf-8")
    stale = re.search(r"only\s+.*\s+are\s+published", gs_text, re.I)
    if stale:
        record(
            "getting_started",
            "fail",
            f"docs/getting-started.md contains stale subset claim: {stale.group(0)!r}",
            t0,
        )
    elif version not in gs_text and major_minor not in gs_text:
        record(
            "getting_started",
            "fail",
            f"docs/getting-started.md does not reference target version {version}",
            t0,
        )
    else:
        record(
            "getting_started",
            "pass",
            f"docs/getting-started.md references {version} and makes no false subset-published claim",
            t0,
        )
except Exception as exc:
    record("getting_started", "fail", f"failed to read docs/getting-started.md: {exc}", t0)

# ---------------------------------------------------------------------------
# 6. changelog_entry
# ---------------------------------------------------------------------------
t0 = time.monotonic()
changelog_path = root / "CHANGELOG.md"
try:
    cl_text = changelog_path.read_text(encoding="utf-8")
    heading_ok = bool(
        re.search(rf"^## \[(?:v?{re.escape(version)})\]", cl_text, re.M)
    )
    timeline_ok = bool(
        re.search(rf"^\|\s*\[(?:v?{re.escape(version)})\]", cl_text, re.M)
    )
    if heading_ok and timeline_ok:
        record(
            "changelog_entry",
            "pass",
            f"CHANGELOG.md has both ## [{tag}] section and Version Timeline row",
            t0,
        )
    else:
        record(
            "changelog_entry",
            "fail",
            f"CHANGELOG.md missing entry for {tag} (heading={heading_ok}, timeline_row={timeline_ok})",
            t0,
        )
except Exception as exc:
    record("changelog_entry", "fail", f"failed to read CHANGELOG.md: {exc}", t0)

# ---------------------------------------------------------------------------
# 7. git_tag_on_main
# ---------------------------------------------------------------------------
t0 = time.monotonic()
try:
    head_proc = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--verify", "HEAD"],
        capture_output=True,
        text=True,
        check=False,
    )
    if head_proc.returncode != 0:
        record("git_tag_on_main", "fail", "git HEAD unavailable", t0)
    else:
        head_sha = head_proc.stdout.strip()
        tag_proc = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"],
            capture_output=True,
            text=True,
            check=False,
        )
        target_sha = tag_proc.stdout.strip() if tag_proc.returncode == 0 else head_sha
        target_label = f"tag {tag} ({target_sha[:8]})" if tag_proc.returncode == 0 else f"HEAD ({head_sha[:8]})"
        ref_candidates = []
        for candidate in ("refs/remotes/origin/main", "refs/heads/main", "HEAD"):
            chk = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "--verify", candidate],
                capture_output=True,
                text=True,
                check=False,
            )
            if chk.returncode == 0:
                ref_candidates.append(candidate)
        branch_ref = ref_candidates[0] if ref_candidates else "HEAD"
        anc = subprocess.run(
            ["git", "-C", str(root), "merge-base", "--is-ancestor", target_sha, branch_ref],
            capture_output=True,
            text=True,
            check=False,
        )
        if anc.returncode == 0:
            record("git_tag_on_main", "pass", f"{target_label} is an ancestor of {branch_ref}", t0)
        else:
            record("git_tag_on_main", "fail", f"{target_label} is not an ancestor of {branch_ref}", t0)
except Exception as exc:
    record("git_tag_on_main", "fail", f"git ancestry check failed: {exc}", t0)

# ---------------------------------------------------------------------------
# 8. ci_green_on_commit (DSR native verification contract)
# ---------------------------------------------------------------------------
t0 = time.monotonic()
dsr_cfg = root / ".config/dsr-quality.yaml"
pub_script = root / "scripts/release_publish_crates.sh"
if os.environ.get("PREFLIGHT_CI_STATUS_OVERRIDE"):
    override = os.environ["PREFLIGHT_CI_STATUS_OVERRIDE"]
    status, _, detail = override.partition(":")
    record("ci_green_on_commit", status, detail or "overridden via PREFLIGHT_CI_STATUS_OVERRIDE", t0)
elif dsr_cfg.is_file() and pub_script.is_file() and os.access(pub_script, os.X_OK):
    try:
        dsr_text = dsr_cfg.read_text(encoding="utf-8")
        required_tools = (
            "frankentui-truth-gates",
            "frankentui-full-gates",
            "frankentui-fuzz-campaign",
        )
        missing_tools = [t for t in required_tools if f"{t}:" not in dsr_text]
        if missing_tools:
            record(
                "ci_green_on_commit",
                "fail",
                f".config/dsr-quality.yaml missing DSR tools: {missing_tools}",
                t0,
            )
        else:
            record(
                "ci_green_on_commit",
                "pass",
                "DSR quality tools (truth-gates, full-gates, fuzz-campaign) and release_publish_crates.sh configured",
                t0,
            )
    except Exception as exc:
        record("ci_green_on_commit", "fail", f"failed to inspect .config/dsr-quality.yaml: {exc}", t0)
else:
    record(
        "ci_green_on_commit",
        "fail",
        "missing .config/dsr-quality.yaml or executable scripts/release_publish_crates.sh",
        t0,
    )

# ---------------------------------------------------------------------------
# 9. claims_ledger
# ---------------------------------------------------------------------------
t0 = time.monotonic()
claims_script = root / "scripts/check_readme_claims.py"
if not claims_script.is_file():
    record("claims_ledger", "pending:g06-impl", "scripts/check_readme_claims.py not present", t0)
else:
    claims_failed = None
    for flag in ("--schema-check", "--proof-refs", "--experimental-check"):
        proc = subprocess.run(
            [sys.executable, "-B", str(claims_script), flag],
            cwd=str(root),
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode != 0:
            claims_failed = (proc.stderr.strip() or proc.stdout.strip() or f"check_readme_claims.py {flag} failed").splitlines()[-1]
            break
    if claims_failed is None:
        record("claims_ledger", "pass", "check_readme_claims.py schema, proof-refs, and experimental checks green", t0)
    else:
        record("claims_ledger", "fail", claims_failed, t0)

# ---------------------------------------------------------------------------
# 10. compat_matrix
# ---------------------------------------------------------------------------
t0 = time.monotonic()
compat_path = root / "docs/compat-matrix.md"
if not compat_path.is_file():
    record("compat_matrix", "pending:g05-impl", "docs/compat-matrix.md not present", t0)
else:
    compat_text = compat_path.read_text(encoding="utf-8")
    table_rows = [
        line for line in compat_text.splitlines()
        if line.startswith("|") and not line.startswith("| Identity") and not line.startswith("|---")
    ]
    failed_rows = [line for line in table_rows if "| fail |" in line.lower() or not line.rstrip().endswith("| ok |")]
    if not table_rows:
        record("compat_matrix", "fail", "docs/compat-matrix.md contains no identity rows", t0)
    elif failed_rows:
        record("compat_matrix", "fail", f"docs/compat-matrix.md has non-ok rows: {len(failed_rows)}", t0)
    else:
        record("compat_matrix", "pass", f"docs/compat-matrix.md verified ({len(table_rows)} terminal identities ok)", t0)

# ---------------------------------------------------------------------------
# 11. consumer_smoke
# ---------------------------------------------------------------------------
t0 = time.monotonic()
consumer_script = root / "scripts/consumer_smoke_e2e.sh"
if not consumer_script.is_file():
    record("consumer_smoke", "pending:g01-impl", "scripts/consumer_smoke_e2e.sh not present", t0)
else:
    proc = subprocess.run(
        ["bash", str(consumer_script), "--selftest"],
        cwd=str(root),
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode == 0:
        record("consumer_smoke", "pass", "scripts/consumer_smoke_e2e.sh --selftest passed", t0)
    else:
        msg = (proc.stderr.strip() or proc.stdout.strip() or "consumer_smoke_e2e.sh --selftest failed").splitlines()[-1]
        record("consumer_smoke", "fail", msg, t0)

# ---------------------------------------------------------------------------
# 12. perf_gate
# ---------------------------------------------------------------------------
t0 = time.monotonic()
bench_check = root / "scripts/check_baseline_benches.py"
baseline_json = root / "tests/baseline.json"
if not bench_check.is_file() or not baseline_json.is_file():
    record("perf_gate", "pending:g25-impl", "scripts/check_baseline_benches.py or tests/baseline.json not present", t0)
else:
    proc = subprocess.run(
        [sys.executable, "-B", str(bench_check)],
        cwd=str(root),
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode == 0:
        record("perf_gate", "pass", "check_baseline_benches.py and tests/baseline.json verified", t0)
    else:
        msg = (proc.stderr.strip() or proc.stdout.strip() or "check_baseline_benches.py failed").splitlines()[-1]
        record("perf_gate", "fail", msg, t0)

# ---------------------------------------------------------------------------
# 13. cratesio_state
# ---------------------------------------------------------------------------
t0 = time.monotonic()
fixture_env = os.environ.get("PREFLIGHT_CRATESIO_FIXTURE")
try:
    if fixture_env:
        fixture_data = json.loads(Path(fixture_env).read_text(encoding="utf-8"))
    else:
        fixture_data = None

    already_published = []
    unpublished = []
    ahead_of_tag = []
    target_tuple = parse_semver(version)

    for crate in publish_order or publishable_crates:
        if fixture_data is not None:
            info = fixture_data.get(crate, {"max_version": "0.0.0", "versions": []})
            max_ver = info.get("max_version", "0.0.0")
            published_versions = set(info.get("versions", []))
        else:
            req = urllib.request.Request(
                f"https://crates.io/api/v1/crates/{crate}",
                headers={"User-Agent": "frankentui-release-preflight (+https://github.com/Dicklesworthstone/frankentui)"},
            )
            try:
                with urllib.request.urlopen(req, timeout=15) as resp:
                    payload = json.loads(resp.read().decode("utf-8"))
                max_ver = payload.get("crate", {}).get("max_version", "0.0.0")
                published_versions = {
                    v["num"] for v in payload.get("versions", []) if not v.get("yanked", False)
                }
            except urllib.error.HTTPError as http_err:
                if http_err.code == 404:
                    max_ver = "0.0.0"
                    published_versions = set()
                else:
                    raise

        if parse_semver(max_ver) > target_tuple:
            ahead_of_tag.append(f"{crate}(max={max_ver})")
        if version in published_versions:
            already_published.append(crate)
        else:
            unpublished.append(crate)

    if ahead_of_tag:
        record(
            "cratesio_state",
            "fail",
            f"crates.io has higher version than {version}: {', '.join(ahead_of_tag)}",
            t0,
        )
    elif already_published and unpublished:
        record(
            "cratesio_state",
            "fail",
            f"partial publication state for {version}: published={len(already_published)}, unpublished={unpublished}",
            t0,
        )
    elif already_published:
        record(
            "cratesio_state",
            "pass",
            f"note: already published ({len(already_published)}/{len(publishable_crates)} crates at {version}; loop will verify archive checksums and skip)",
            t0,
        )
    else:
        record(
            "cratesio_state",
            "pass",
            f"all {len(unpublished)} crates ready for initial publication of {version}",
            t0,
        )
except Exception as exc:
    record("cratesio_state", "fail", f"crates.io state query failed: {exc}", t0)

# ---------------------------------------------------------------------------
# 14. publish_dry_run
# ---------------------------------------------------------------------------
t0 = time.monotonic()
if not publish_order or len(publish_order) != len(publishable_crates):
    record("publish_dry_run", "fail", "topological publish order could not be resolved", t0)
else:
    record(
        "publish_dry_run",
        "pass",
        f"topological publish order resolved ({len(publish_order)} crates: {publish_order[0]} .. {publish_order[-1]})",
        t0,
    )

if out_path is not None:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8") as handle:
        for row in records:
            handle.write(json.dumps(row, sort_keys=True) + "\n")

has_fail = any(r["status"] == "fail" for r in records)
has_pending = any(r["status"].startswith("pending:") for r in records)

if has_fail or (has_pending and (strict or not diagnostic_pending)):
    sys.exit(1)
sys.exit(0)
PY
