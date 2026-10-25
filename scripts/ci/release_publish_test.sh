#!/usr/bin/env bash
# G04.11 (bd-g00-root-epic-ewths.6.22): Verification suite for scripts/release_publish_crates.sh.
#
# Exercises:
#   (a) API 200 in --registry-only mode -> status=registry-present, no cargo publish call
#   (b) API 404 before + cargo publish + API 200 after -> status=published, retained archive saved
#   (c) Idempotent re-run of --publish after (b) when API returns 200 -> status=already-published, 0 cargo publish calls
#   (d) API 404 + cargo publish failure -> status=failed, loop halts on first failure and exits 1
#   (e) --dry-run mode -> cargo publish --dry-run invoked per crate in dependency order, status=dry-run-ok
#   (f) Version/tag mismatch (v9.9.9 vs 0.9.0) -> rejected before any registry or cargo publish call
#   (g) API 200 in --publish mode without matching candidate archive -> status=failed (rejects unverified registry bytes)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd -P)"
PUBLISH_SCRIPT="$PROJECT_ROOT/scripts/release_publish_crates.sh"
TEST_ROOT="${TEST_ROOT:-/data/tmp/release_publish_test_$(date +%Y%m%d_%H%M%S)_$$}"

mkdir -p "$TEST_ROOT"

python3 -B - "$PROJECT_ROOT" "$PUBLISH_SCRIPT" "$TEST_ROOT" <<'PY'
import hashlib
import io
import json
import os
import subprocess
import sys
import tarfile
from pathlib import Path

project_root = Path(sys.argv[1]).resolve()
publish_script = Path(sys.argv[2]).resolve()
test_root = Path(sys.argv[3]).resolve()


def make_crate_tarball(dest: Path, crate: str, version: str, commit: str) -> str:
    dest.parent.mkdir(parents=True, exist_ok=True)
    prefix = f"{crate}-{version}/"
    vcs_bytes = json.dumps(
        {"git": {"sha1": commit, "dirty": False}, "path_in_vcs": f"crates/{crate}"}
    ).encode("utf-8")
    manifest_bytes = (
        f'[package]\nname = "{crate}"\nversion = "{version}"\n'
    ).encode("utf-8")

    with tarfile.open(dest, "w:gz") as tar:
        for rel_name, payload in (
            (".cargo_vcs_info.json", vcs_bytes),
            ("Cargo.toml", manifest_bytes),
        ):
            info = tarfile.TarInfo(name=prefix + rel_name)
            info.size = len(payload)
            info.mtime = 0
            tar.addfile(info, io.BytesIO(payload))

    return hashlib.sha256(dest.read_bytes()).hexdigest()


def build_fixture(name: str) -> tuple[Path, Path, str, dict[str, str]]:
    base = test_root / name
    repo = base / "repo"
    bin_dir = base / "bin"
    target_dir = base / "target"
    repo.mkdir(parents=True)
    bin_dir.mkdir(parents=True)
    target_dir.mkdir(parents=True)

    subprocess.run(["git", "init", "-b", "main", str(repo)], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)

    (repo / "rust-toolchain.toml").write_text(
        '[toolchain]\nchannel = "nightly-2026-02-10"\n', encoding="utf-8"
    )
    (repo / "Cargo.lock").write_text("# lockfile\n", encoding="utf-8")
    (repo / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/ftui-core", "crates/ftui"]\n'
        '[workspace.package]\nversion = "0.9.0"\n',
        encoding="utf-8",
    )
    (repo / "crates/ftui-core").mkdir(parents=True)
    (repo / "crates/ftui-core/Cargo.toml").write_text(
        '[package]\nname = "ftui-core"\nversion.workspace = true\n',
        encoding="utf-8",
    )
    (repo / "crates/ftui").mkdir(parents=True)
    (repo / "crates/ftui/Cargo.toml").write_text(
        '[package]\nname = "ftui"\nversion.workspace = true\n'
        '[dependencies]\nftui-core = { version = "0.9.0", path = "../ftui-core" }\n',
        encoding="utf-8",
    )

    subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-m", "init"], check=True, capture_output=True)
    commit = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()

    # Pre-compute deterministic candidate archives per crate
    staging = base / "staging"
    checksums = {}
    for crate in ("ftui-core", "ftui"):
        archive_path = staging / f"{crate}-0.9.0.crate"
        checksums[crate] = make_crate_tarball(archive_path, crate, "0.9.0", commit)

    # Stub rustc
    rustc_stub = bin_dir / "rustc"
    rustc_stub.write_text('#!/usr/bin/env bash\necho "rustc 1.95.0-nightly"\n', encoding="utf-8")
    rustc_stub.chmod(0o755)

    # Stub cargo
    cargo_stub = bin_dir / "cargo"
    cargo_stub.write_text(
        f"""#!/usr/bin/env bash
set -euo pipefail
if [[ "${{1:-}}" == "--version" ]]; then
    echo "cargo 1.95.0-nightly"
    exit 0
fi
if [[ "${{1:-}}" == "metadata" ]]; then
    printf '{{"target_directory":"%s"}}\\n' "{target_dir}"
    exit 0
fi
echo "$*" >> "{base}/cargo_calls.log"
crate=""
dry_run=0
for ((i=1; i<=$#; i++)); do
    arg="${{!i}}"
    if [[ "$arg" == "-p" ]]; then
        next=$((i + 1))
        crate="${{!next}}"
    elif [[ "$arg" == "--dry-run" ]]; then
        dry_run=1
    fi
done
if [[ "${{STUB_CARGO_FAIL_CRATE:-}}" == "$crate" ]]; then
    echo "error: failed to verify package tarball for $crate" >&2
    exit 1
fi
if [[ "$dry_run" == "0" && -n "$crate" ]]; then
    mkdir -p "{target_dir}/package/tmp-crate"
    cp "{staging}/${{crate}}-0.9.0.crate" "{target_dir}/package/tmp-crate/${{crate}}-0.9.0.crate"
    touch "{base}/published_${{crate}}"
fi
exit 0
""",
        encoding="utf-8",
    )
    cargo_stub.chmod(0o755)

    # Stub curl
    curl_stub = bin_dir / "curl"
    curl_stub.write_text(
        f"""#!/usr/bin/env bash
set -euo pipefail
out=""
url=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -A|--retry|--connect-timeout|--max-time|-w) shift 2 ;;
        -sS|-L) shift ;;
        http*) url="$1"; shift ;;
        *) shift ;;
    esac
done
# URL ends with /crates/<crate>/<version>
version="${{url##*/}}"
rem="${{url%/*}}"
crate="${{rem##*/}}"
mode="${{STUB_REGISTRY_MODE:-200}}"
if [[ "$mode" == "404_then_200" ]]; then
    if [[ -f "{base}/published_${{crate}}" ]]; then
        mode="200"
    else
        mode="404"
    fi
fi
if [[ "$mode" == "200" ]]; then
    checksum=""
    case "$crate" in
        ftui-core) checksum="{checksums['ftui-core']}" ;;
        ftui) checksum="{checksums['ftui']}" ;;
    esac
    printf '{{"version":{{"crate":"%s","num":"%s","yanked":false,"checksum":"%s"}}}}\\n' "$crate" "$version" "$checksum" > "$out"
    printf '200'
else
    printf '{{"errors":[{{"detail":"Not Found"}}]}}\\n' > "$out"
    printf '404'
fi
""",
        encoding="utf-8",
    )
    curl_stub.chmod(0o755)

    return base, repo, commit, checksums


def run_publish(base: Path, repo: Path, out_dir: Path, version: str, mode: str, extra_env: dict[str, str]):
    env = dict(os.environ)
    env["PATH"] = f"{base / 'bin'}:{env.get('PATH', '')}"
    env["RELEASE_PUBLISH_ROOT"] = str(repo)
    env["RELEASE_PUBLISH_SLEEP_SECS"] = "0"
    env.update(extra_env)
    proc = subprocess.run(
        ["bash", str(publish_script), version, str(out_dir), mode],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    log_path = out_dir / "release_log.jsonl"
    rows = []
    if log_path.exists():
        rows = [json.loads(line) for line in log_path.read_text(encoding="utf-8").splitlines() if line.strip()]
    cargo_calls = []
    calls_file = base / "cargo_calls.log"
    if calls_file.exists():
        cargo_calls = calls_file.read_text(encoding="utf-8").splitlines()
    return proc, rows, cargo_calls


# Case (a): API 200 in --registry-only mode -> status=registry-present, 0 cargo publish calls
base_a, repo_a, _, checksums_a = build_fixture("case_a_registry_only")
proc_a, rows_a, calls_a = run_publish(
    base_a, repo_a, base_a / "out_a", "v0.9.0", "--registry-only", {"STUB_REGISTRY_MODE": "200"}
)
assert proc_a.returncode == 0, f"case (a) failed: {proc_a.stderr}"
assert [r["crate"] for r in rows_a] == ["ftui-core", "ftui"], f"unexpected order: {rows_a}"
assert all(r["status"] == "registry-present" for r in rows_a), f"unexpected status: {rows_a}"
assert calls_a == [], f"expected 0 cargo publish calls in --registry-only, got {calls_a}"
print("PASS: (a) API 200 --registry-only records registry-present with 0 cargo publish calls")

# Case (b) & (c): API 404 -> publish -> status=published, followed by idempotent re-run (API 200 -> already-published)
base_b, repo_b, _, checksums_b = build_fixture("case_b_publish_and_rerun")
proc_b, rows_b, calls_b = run_publish(
    base_b, repo_b, base_b / "out_b1", "v0.9.0", "--publish", {"STUB_REGISTRY_MODE": "404_then_200"}
)
assert proc_b.returncode == 0, f"case (b) publish failed: {proc_b.stderr}"
assert [r["status"] for r in rows_b] == ["published", "published"], f"expected published rows, got {rows_b}"
assert len(calls_b) == 2, f"expected 2 cargo publish calls, got {calls_b}"
for r in rows_b:
    assert r["checksum"] == checksums_b[r["crate"]]

# Clear cargo_calls.log and re-run --publish against API 200 using retained candidate archives
(base_b / "cargo_calls.log").write_text("", encoding="utf-8")
proc_c, rows_c, calls_c = run_publish(
    base_b, repo_b, base_b / "out_b2", "v0.9.0", "--publish", {"STUB_REGISTRY_MODE": "200"}
)
assert proc_c.returncode == 0, f"case (c) idempotent re-run failed: {proc_c.stderr}"
assert [r["status"] for r in rows_c] == ["already-published", "already-published"], f"expected already-published, got {rows_c}"
assert calls_c == [], f"expected 0 cargo publish calls on idempotent re-run, got {calls_c}"
print("PASS: (b) & (c) API 404 -> published + idempotent re-run -> already-published with verified retained archives")

# Case (d): API 404 + cargo publish failure -> status=failed, loop stops and exits 1
base_d, repo_d, _, _ = build_fixture("case_d_cargo_failure")
proc_d, rows_d, calls_d = run_publish(
    base_d,
    repo_d,
    base_d / "out_d",
    "v0.9.0",
    "--publish",
    {"STUB_REGISTRY_MODE": "404", "STUB_CARGO_FAIL_CRATE": "ftui-core"},
)
assert proc_d.returncode == 1, f"case (d) expected exit 1, got {proc_d.returncode}"
assert len(rows_d) == 1 and rows_d[0]["crate"] == "ftui-core" and rows_d[0]["status"] == "failed", f"unexpected rows: {rows_d}"
assert len(calls_d) == 1, f"loop should stop after first failure, got {calls_d}"
print("PASS: (d) cargo publish failure logs status=failed and halts loop with exit 1")

# Case (e): --dry-run mode -> cargo publish --dry-run invoked, status=dry-run-ok
base_e, repo_e, _, _ = build_fixture("case_e_dry_run")
proc_e, rows_e, calls_e = run_publish(
    base_e, repo_e, base_e / "out_e", "v0.9.0", "--dry-run", {"STUB_REGISTRY_MODE": "404"}
)
assert proc_e.returncode == 0, f"case (e) dry-run failed: {proc_e.stderr}"
assert [r["status"] for r in rows_e] == ["dry-run-ok", "dry-run-ok"], f"unexpected dry-run rows: {rows_e}"
assert len(calls_e) == 2 and all("--dry-run" in c for c in calls_e), f"expected --dry-run calls, got {calls_e}"
print("PASS: (e) --dry-run invokes cargo publish --dry-run and logs status=dry-run-ok")

# Case (f): Version mismatch (v9.9.9 vs 0.9.0) -> rejected before any cargo call
base_f, repo_f, _, _ = build_fixture("case_f_version_mismatch")
proc_f, rows_f, calls_f = run_publish(
    base_f, repo_f, base_f / "out_f", "v9.9.9", "--dry-run", {"STUB_REGISTRY_MODE": "404"}
)
assert proc_f.returncode != 0, "case (f) expected non-zero exit on version mismatch"
assert "workspace version mismatch" in proc_f.stderr, f"expected workspace version mismatch error, got: {proc_f.stderr}"
assert rows_f == [] and calls_f == []
print("PASS: (f) mismatched tag v9.9.9 rejected before registry/cargo invocation")

# Case (g): API 200 in --publish mode without retained candidate archive -> status=failed
base_g, repo_g, _, _ = build_fixture("case_g_unverified_200")
proc_g, rows_g, calls_g = run_publish(
    base_g, repo_g, base_g / "out_g", "v0.9.0", "--publish", {"STUB_REGISTRY_MODE": "200"}
)
assert proc_g.returncode == 1, f"case (g) expected exit 1 when archive is missing, got {proc_g.returncode}"
assert len(rows_g) == 1 and rows_g[0]["status"] == "failed", f"expected failed status, got {rows_g}"
print("PASS: (g) --publish rejects API 200 when no matching candidate archive exists")

# Case (h): Live workspace topological publish order check (all 17 publishable crates acyclic)
import tomllib

workspace = tomllib.loads((project_root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
ws_version = workspace["package"]["version"]
packages = {}
for member in workspace["members"]:
    manifest = tomllib.loads((project_root / member / "Cargo.toml").read_text(encoding="utf-8"))
    package = manifest["package"]
    if package.get("publish") is False:
        continue
    packages[package["name"]] = manifest

dependencies = {}
for name, manifest in packages.items():
    tables = [manifest] + list(manifest.get("target", {}).values())
    internal = set()
    for table in tables:
        for kind in ("dependencies", "build-dependencies", "dev-dependencies"):
            for alias, dep in table.get(kind, {}).items():
                if not isinstance(dep, dict):
                    continue
                if kind == "dev-dependencies" and "version" not in dep:
                    continue
                target = dep.get("package", alias)
                if target in packages:
                    assert dep.get("version") == ws_version, f"dependency version mismatch: {name} -> {target}"
                    internal.add(target)
    dependencies[name] = internal

order = []
while len(order) < len(packages):
    ready = sorted(name for name, deps in dependencies.items() if name not in order and deps <= set(order))
    assert ready, f"cycle in live workspace publishable dependencies among {sorted(set(packages) - set(order))}"
    order.extend(ready)
assert len(order) == 17, f"expected 17 publishable crates in workspace, got {len(order)}: {order}"
print(f"PASS: (h) live workspace topological publish order verified ({len(order)} crates acyclic)")
PY

echo "ALL release_publish_crates.sh test cases passed (artifacts retained in $TEST_ROOT)"
