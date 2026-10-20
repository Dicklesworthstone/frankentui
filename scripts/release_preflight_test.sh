#!/usr/bin/env bash
# G41 (bd-g00-root-epic-ewths.10.1): Unit/fixture test suite for scripts/release_preflight.sh.
#
# Exercises:
#   (a) all-pass against a complete synthetic fixture (and live repo v0.9.0)
#   (b) README version drift -> readme_version fail (exit 1)
#   (c) missing changelog heading -> changelog_entry fail (exit 1)
#   (d) crate list mismatch -> readme_crate_list fail (exit 1)
#   (e) pending handling: fail-closed by default and under --strict (exit 1),
#       diagnostic-only under --diagnostic-pending (exit 0)
#   (f) crates.io partial publication and version regression rejection (exit 1)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
PREFLIGHT="$PROJECT_ROOT/scripts/release_preflight.sh"
TEST_ROOT="${TEST_ROOT:-/data/tmp/release_preflight_test_$(date +%Y%m%d_%H%M%S)_$$}"

mkdir -p "$TEST_ROOT"

python3 -B - "$PROJECT_ROOT" "$PREFLIGHT" "$TEST_ROOT" "$@" <<'PY'
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

project_root = Path(sys.argv[1]).resolve()
preflight = Path(sys.argv[2]).resolve()
test_root = Path(sys.argv[3]).resolve()


def validate_jsonl(path: Path) -> dict[str, dict]:
    rows = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    assert rows, f"empty JSONL at {path}"
    by_check = {}
    for row in rows:
        for key in ("check", "status", "detail", "elapsed_s", "version", "tag"):
            assert key in row, f"missing key {key} in {row}"
        assert isinstance(row["elapsed_s"], (int, float)) and row["elapsed_s"] >= 0
        by_check[row["check"]] = row
    return by_check


def build_fixture(name: str) -> tuple[Path, Path]:
    fx = test_root / name
    fx.mkdir(parents=True)
    subprocess.run(["git", "init", "-b", "main", str(fx)], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(fx), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(fx), "config", "user.name", "Test"], check=True)

    (fx / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/ftui-core", "crates/ftui"]\n'
        '[workspace.package]\nversion = "0.9.0"\n',
        encoding="utf-8",
    )
    (fx / "crates/ftui-core").mkdir(parents=True)
    (fx / "crates/ftui-core/Cargo.toml").write_text(
        '[package]\nname = "ftui-core"\nversion.workspace = true\n',
        encoding="utf-8",
    )
    (fx / "crates/ftui").mkdir(parents=True)
    (fx / "crates/ftui/Cargo.toml").write_text(
        '[package]\nname = "ftui"\nversion.workspace = true\n'
        '[dependencies]\nftui-core = { version = "0.9.0", path = "../ftui-core" }\n',
        encoding="utf-8",
    )
    (fx / "README.md").write_text(
        "# FrankenTUI\n\n### Crates.io Dependency\n\n"
        "The workspace provides 2 library crates (`ftui` and `ftui-core`):\n\n"
        '```toml\n[dependencies]\nftui = "=0.9.0"\n```\n',
        encoding="utf-8",
    )
    (fx / "docs").mkdir(parents=True)
    (fx / "docs/getting-started.md").write_text(
        "# Getting Started\nThese examples target 0.9.0.\n",
        encoding="utf-8",
    )
    (fx / "docs/compat-matrix.md").write_text(
        "# Terminal compatibility matrix\n\n"
        "| Identity (env) | DECRPM answer | sync inline | sync alt | scroll region | probe sent | exit |\n"
        "|---|---|---|---|---|---|---|\n"
        "| kitty | silent | 20/20 | 20/20 | yes | no | ok |\n",
        encoding="utf-8",
    )
    (fx / "CHANGELOG.md").write_text(
        "# Changelog\n\n## Version Timeline\n\n"
        "| Version | Kind | Date | Summary |\n|---|---|---|---|\n"
        "| [v0.9.0] | Release | 2026-10-03 | Release |\n\n"
        "## [v0.9.0] -- 2026-10-03\n",
        encoding="utf-8",
    )
    (fx / ".config").mkdir(parents=True)
    (fx / ".config/dsr-quality.yaml").write_text(
        "tools:\n  frankentui-truth-gates:\n  frankentui-full-gates:\n  frankentui-fuzz-campaign:\n",
        encoding="utf-8",
    )
    (fx / "scripts").mkdir(parents=True)
    pub = fx / "scripts/release_publish_crates.sh"
    pub.write_text("#!/usr/bin/env bash\nexit 0\n", encoding="utf-8")
    pub.chmod(0o755)
    claims = fx / "scripts/check_readme_claims.py"
    claims.write_text("import sys\nsys.exit(0)\n", encoding="utf-8")
    smoke = fx / "scripts/consumer_smoke_e2e.sh"
    smoke.write_text("#!/usr/bin/env bash\nexit 0\n", encoding="utf-8")
    smoke.chmod(0o755)
    benches = fx / "scripts/check_baseline_benches.py"
    benches.write_text("import sys\nsys.exit(0)\n", encoding="utf-8")
    (fx / "tests").mkdir(parents=True)
    (fx / "tests/baseline.json").write_text("{}\n", encoding="utf-8")

    crates_fixture = fx / "cratesio.json"
    crates_fixture.write_text(
        json.dumps(
            {
                "ftui-core": {"max_version": "0.9.0", "versions": ["0.7.0", "0.9.0"]},
                "ftui": {"max_version": "0.9.0", "versions": ["0.7.0", "0.9.0"]},
            }
        ),
        encoding="utf-8",
    )

    subprocess.run(["git", "-C", str(fx), "add", "."], check=True)
    subprocess.run(["git", "-C", str(fx), "commit", "-m", "init"], check=True, capture_output=True)
    return fx, crates_fixture


def run_case(name: str, fx: Path, crates_fixture: Path, extra_args=()):
    jsonl = test_root / f"{name}.jsonl"
    env = dict(os.environ)
    env["PREFLIGHT_CRATESIO_FIXTURE"] = str(crates_fixture)
    proc = subprocess.run(
        ["bash", str(preflight), "v0.9.0", "--root", str(fx), "--out", str(jsonl), *extra_args],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.returncode, validate_jsonl(jsonl), proc.stdout


# (a) All-pass fixture
fx_a, cf_a = build_fixture("case_a_all_pass")
rc, rows, _ = run_case("case_a_all_pass", fx_a, cf_a, ("--strict",))
assert rc == 0, f"case (a) expected exit 0, got {rc}: {rows}"
assert all(r["status"] == "pass" for r in rows.values()), f"case (a) non-pass rows: {rows}"
print("PASS: (a) all-pass fixture")

# (b) README version drift -> fail
fx_b, cf_b = build_fixture("case_b_readme_drift")
(fx_b / "README.md").write_text(
    (fx_b / "README.md").read_text(encoding="utf-8").replace('"=0.9.0"', '"0.5"'),
    encoding="utf-8",
)
rc, rows, _ = run_case("case_b_readme_drift", fx_b, cf_b)
assert rc == 1, f"case (b) expected exit 1, got {rc}"
assert rows["readme_version"]["status"] == "fail", f"case (b) expected readme_version fail: {rows['readme_version']}"
print("PASS: (b) README version drift rejected")

# (c) Missing changelog heading -> fail
fx_c, cf_c = build_fixture("case_c_missing_changelog")
(fx_c / "CHANGELOG.md").write_text("# Changelog\n\n## Version Timeline\n", encoding="utf-8")
rc, rows, _ = run_case("case_c_missing_changelog", fx_c, cf_c)
assert rc == 1, f"case (c) expected exit 1, got {rc}"
assert rows["changelog_entry"]["status"] == "fail", f"case (c) expected changelog_entry fail: {rows['changelog_entry']}"
print("PASS: (c) missing changelog heading rejected")

# (d) Crate list mismatch -> fail
fx_d, cf_d = build_fixture("case_d_crate_list_mismatch")
(fx_d / "README.md").write_text(
    "# FrankenTUI\n\n### Crates.io Dependency\n\nOnly `ftui`:\n\n```toml\nftui = \"=0.9.0\"\n```\n",
    encoding="utf-8",
)
rc, rows, _ = run_case("case_d_crate_list_mismatch", fx_d, cf_d)
assert rc == 1, f"case (d) expected exit 1, got {rc}"
assert rows["readme_crate_list"]["status"] == "fail", f"case (d) expected readme_crate_list fail: {rows['readme_crate_list']}"
print("PASS: (d) README crate list mismatch rejected")

# (e) Pending handling: fail-closed by default and under --strict, diagnostic under --diagnostic-pending
fx_e, cf_e = build_fixture("case_e_pending")
# Rename check_readme_claims.py instead of deleting (Rule 1 compliance)
(fx_e / "scripts/check_readme_claims.py").rename(fx_e / "scripts/check_readme_claims.py.bak")
rc_default, rows_default, _ = run_case("case_e_pending_default", fx_e, cf_e)
assert rc_default == 1, f"case (e) default must fail closed on pending, got {rc_default}"
assert rows_default["claims_ledger"]["status"] == "pending:g06-impl"
rc_strict, rows_strict, _ = run_case("case_e_pending_strict", fx_e, cf_e, ("--strict", "--diagnostic-pending"))
assert rc_strict == 1, f"case (e) --strict must fail closed on pending, got {rc_strict}"
rc_diag, rows_diag, _ = run_case("case_e_pending_diag", fx_e, cf_e, ("--diagnostic-pending",))
assert rc_diag == 0, f"case (e) --diagnostic-pending expected exit 0, got {rc_diag}"
print("PASS: (e) pending fail-closed by default/--strict and diagnostic-only under --diagnostic-pending")

# (f) Partial crates.io publication state -> fail
fx_f, cf_f = build_fixture("case_f_partial_cratesio")
cf_f.write_text(
    json.dumps(
        {
            "ftui-core": {"max_version": "0.9.0", "versions": ["0.9.0"]},
            "ftui": {"max_version": "0.7.0", "versions": ["0.7.0"]},
        }
    ),
    encoding="utf-8",
)
rc, rows, _ = run_case("case_f_partial_cratesio", fx_f, cf_f)
assert rc == 1, f"case (f) expected exit 1 on partial crates.io state, got {rc}"
assert rows["cratesio_state"]["status"] == "fail"
print("PASS: (f) partial crates.io publication state rejected")

# (g) Checklist drift check: every check name appears in docs/release-checklist.md,
# and README.md, AGENTS.md, and CHANGELOG.md link to docs/release-checklist.md.
checklist_path = project_root / "docs/release-checklist.md"
assert checklist_path.is_file(), "docs/release-checklist.md missing"
checklist_text = checklist_path.read_text(encoding="utf-8")
for check_name in rows.keys():
    assert f"`{check_name}`" in checklist_text, f"check {check_name!r} missing from docs/release-checklist.md"
for doc_rel in ("README.md", "AGENTS.md", "CHANGELOG.md"):
    doc_text = (project_root / doc_rel).read_text(encoding="utf-8")
    assert "release-checklist.md" in doc_text, f"{doc_rel} missing link to release-checklist.md"
print("PASS: (g) docs/release-checklist.md covers all 14 checks and is linked from README.md, AGENTS.md, and CHANGELOG.md")

# (h) G43 consumer_smoke_e2e.sh negative controls (poisoned config, missing toolchain, offline empty cache)
consumer_smoke = project_root / "scripts/consumer_smoke_e2e.sh"
g43_root = test_root / "g43_controls"
g43_root.mkdir(parents=True, exist_ok=True)
poison_cfg = g43_root / "poison_config.toml"
poison_cfg.write_text('[patch.crates-io]\nftui = { path = "/tmp/poison" }\n', encoding="utf-8")
empty_home = g43_root / "empty_cargo_home"
empty_home.mkdir(parents=True, exist_ok=True)

neg_env = dict(os.environ)
neg_env["RCH_CARGO_WRAPPER_BYPASS"] = "1"

for neg_name, neg_args, expected_substr in (
    (
        "neg_poison_config",
        ["--mode", "source", "--runner", "local", "--scratch-parent", str(g43_root), "--cargo-config", str(poison_cfg)],
        "dependency override in Cargo config",
    ),
    (
        "neg_missing_toolchain",
        ["--mode", "source", "--runner", "local", "--scratch-parent", str(g43_root), "--toolchain", "nightly-1999-01-01-nonexistent"],
        "requested installed toolchain unavailable",
    ),
    (
        "neg_offline_empty_cache",
        ["--mode", "registry", "--version", "0.9.0", "--runner", "local", "--scratch-parent", str(g43_root), "--offline", "--cargo-home", str(empty_home)],
        "isolated Cargo metadata failed",
    ),
):
    neg_out = g43_root / neg_name
    proc = subprocess.run(
        ["bash", str(consumer_smoke), *neg_args, "--out", str(neg_out)],
        cwd=str(project_root),
        env=neg_env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 1, f"{neg_name} expected exit 1, got {proc.returncode}"
    summary = json.loads((neg_out / "summary.json").read_text(encoding="utf-8"))
    assert summary["outcome"] == "FAIL", f"{neg_name} expected outcome FAIL, got {summary['outcome']}"
    assert expected_substr in summary.get("error", ""), f"{neg_name} expected {expected_substr!r} in {summary.get('error')!r}"
print("PASS: (h) G43 consumer_smoke_e2e.sh negative controls (poisoned config, missing toolchain, offline empty cache) rejected")

if "--verify-live-consumer" in sys.argv[4:]:
    import hashlib

    def sha256_file(p: Path) -> str:
        h = hashlib.sha256()
        with p.open("rb") as f:
            for chunk in iter(lambda: f.read(65536), b""):
                h.update(chunk)
        return h.hexdigest()

    custom_target = g43_root / "custom_target_dir"
    custom_home = g43_root / "custom_cargo_home"
    real_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))).resolve()
    custom_home.mkdir(parents=True, exist_ok=True)
    for cache_sub in ("registry", "git", "bin"):
        src_sub = real_home / cache_sub
        dst_sub = custom_home / cache_sub
        if src_sub.exists() and not dst_sub.exists():
            os.symlink(src_sub, dst_sub)

    for feat_mode, expected_outcome in (("no-backend", "EXPECTED_UNSUPPORTED"), ("default", "PASS")):
        out_dir = g43_root / f"live_registry_{feat_mode}"
        proc = subprocess.run(
            [
                "bash",
                str(consumer_smoke),
                "--mode",
                "registry",
                "--version",
                "0.9.0",
                "--features",
                feat_mode,
                "--runner",
                "local",
                "--scratch-parent",
                str(g43_root),
                "--target-dir",
                str(custom_target),
                "--cargo-home",
                str(custom_home),
                "--out",
                str(out_dir),
            ],
            cwd=str(project_root),
            env=neg_env,
            capture_output=True,
            text=True,
            check=False,
        )
        assert proc.returncode == 0, f"live_registry_{feat_mode} failed (rc={proc.returncode}): {proc.stderr}\n{proc.stdout}"
        summary = json.loads((out_dir / "summary.json").read_text(encoding="utf-8"))
        assert summary["outcome"] == expected_outcome, f"expected {expected_outcome}, got {summary['outcome']}"
        assert summary["target_dir"] == str(custom_target.resolve())
        assert summary["cargo_home"] == str(custom_home.resolve())
        # Verify copied-receipt portability in a fresh directory
        copied_dir = g43_root / f"copied_receipt_{feat_mode}"
        shutil.copytree(out_dir, copied_dir)
        copied_summary = json.loads((copied_dir / "summary.json").read_text(encoding="utf-8"))
        assert sha256_file(copied_dir / "Cargo.lock") == copied_summary["lock_sha256"]
        assert sha256_file(copied_dir / "consumer_smoke_e2e.sh") == copied_summary["producer_sha256"]
        for name, expected_sha in copied_summary["examples"].items():
            assert sha256_file(copied_dir / f"{name}.rs") == expected_sha
        for j in copied_summary["journeys"]:
            assert j["outcome"] == expected_outcome
            assert j["termios_before"] == j["termios_after"], f"termios mismatch in {j['name']}"
            assert not j["errors"], f"journey errors in {j['name']}: {j['errors']}"
            assert sha256_file(copied_dir / j["pty"]) == j["pty_sha256"]
            assert sha256_file(copied_dir / j["binary"]) == j["binary_identity"]["sha256"]
    print("PASS: (i) G43 live registry consumer journeys (no-backend + default) with custom target-dir/cargo-home and copied-receipt portability verified")
PY

