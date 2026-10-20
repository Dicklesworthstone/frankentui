#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${1:-${ROOT_DIR}/target/doctor_frankentui_coverage}"
THRESHOLDS_TOML="${ROOT_DIR}/crates/doctor_frankentui/coverage/thresholds.toml"
SUMMARY_JSON="${OUT_DIR}/coverage_summary.json"
GATE_JSON="${OUT_DIR}/coverage_gate_report.json"
GATE_TXT="${OUT_DIR}/coverage_gate_report.txt"
OBJECTS_JSON="${OUT_DIR}/complete_executed_objects.json"
COV_TARGET_DIR="${DOCTOR_COV_TARGET_DIR:-${ROOT_DIR}/target/doctor_frankentui_cov_target}"
COV_WORK_DIR="${DOCTOR_COV_WORK_DIR:-${ROOT_DIR}/target/doctor_frankentui_ci/coverage}"
RUN_TAG="$(date +%Y%m%d_%H%M%S)_$$"
PROFRAW_DIR="${COV_WORK_DIR}/profraw_${RUN_TAG}"
PROFDATA_FILE="${COV_WORK_DIR}/doctor_frankentui.profdata"
export TMPDIR="${DOCTOR_COV_TMPDIR:-${COV_WORK_DIR}/tmp}"
export CARGO_INCREMENTAL=0

mkdir -p "${OUT_DIR}" "${COV_TARGET_DIR}" "${COV_WORK_DIR}" "${PROFRAW_DIR}" "${TMPDIR}"
touch "${COV_TARGET_DIR}/.sbh-protect" "${COV_WORK_DIR}/.sbh-protect"

require_command() {
  local command="$1"
  local hint="$2"
  if ! command -v "${command}" >/dev/null 2>&1; then
    echo "[coverage] missing required command: ${command} (${hint})" >&2
    exit 2
  fi
}

require_command "python3" "install Python 3"
require_command "cargo" "install Rust/Cargo toolchain"
require_command "rustc" "install Rust/Cargo toolchain"

RUST_SYSROOT="$(rustc --print sysroot)"
RUST_HOST="$(rustc -vV | sed -n 's/^host: //p')"
LLVM_BIN_DIR="${RUST_SYSROOT}/lib/rustlib/${RUST_HOST}/bin"

LLVM_PROFDATA=""
if [[ -x "${LLVM_BIN_DIR}/llvm-profdata" ]]; then
  LLVM_PROFDATA="${LLVM_BIN_DIR}/llvm-profdata"
elif command -v llvm-profdata >/dev/null 2>&1; then
  LLVM_PROFDATA="$(command -v llvm-profdata)"
fi

LLVM_COV=""
if [[ -x "${LLVM_BIN_DIR}/llvm-cov" ]]; then
  LLVM_COV="${LLVM_BIN_DIR}/llvm-cov"
elif command -v llvm-cov >/dev/null 2>&1; then
  LLVM_COV="$(command -v llvm-cov)"
fi

if [[ -z "${LLVM_PROFDATA}" || -z "${LLVM_COV}" ]]; then
  echo "[coverage] llvm-profdata and llvm-cov are required (install rustup component llvm-tools-preview)" >&2
  exit 2
fi

python3 - <<'PY'
import sys

if sys.version_info >= (3, 11):
    raise SystemExit(0)

try:
    import tomli  # noqa: F401
except ModuleNotFoundError:
    raise SystemExit(
        "[coverage] Python < 3.11 detected and module 'tomli' is missing. "
        "Install tomli (python3 -m pip install tomli) or use Python 3.11+."
    )
PY

COV_RUSTFLAGS="${RUSTFLAGS:-} -C instrument-coverage -Z coverage-options=branch -C debuginfo=0"

echo "[coverage] discovering complete executed object set for doctor_frankentui (cold or warm cache)"
python3 - "${ROOT_DIR}" "${COV_TARGET_DIR}" "${COV_RUSTFLAGS}" "${OBJECTS_JSON}" <<'PY'
import json
import os
import subprocess
import sys
from pathlib import Path

root_dir = Path(sys.argv[1])
target_dir = sys.argv[2]
cov_rustflags = sys.argv[3]
objects_json_path = Path(sys.argv[4])

env = os.environ.copy()
env["RCH_CARGO_WRAPPER_BYPASS"] = "1"
env["CARGO_TARGET_DIR"] = target_dir
env["RUSTFLAGS"] = cov_rustflags
env["DOCTOR_REAL_TOOL_INTEGRATION"] = "1"

proc = subprocess.run(
    ["cargo", "test", "-p", "doctor_frankentui", "--all-targets", "--no-run", "--message-format=json"],
    cwd=root_dir,
    env=env,
    stdout=subprocess.PIPE,
    text=True,
    check=True,
)

executables: list[str] = []
seen: set[str] = set()
for line in proc.stdout.splitlines():
    line = line.strip()
    if not line.startswith("{"):
        continue
    msg = json.loads(line)
    if msg.get("reason") != "compiler-artifact":
        continue
    pkg_id = msg.get("package_id", "")
    if "doctor_frankentui" not in pkg_id:
        continue
    exe = msg.get("executable")
    if exe and exe not in seen and Path(exe).is_file():
        seen.add(exe)
        executables.append(exe)

if not executables:
    raise SystemExit("[coverage] failed to discover any doctor_frankentui test/bin executables")

payload = {
    "package": "doctor_frankentui",
    "target_dir": target_dir,
    "rustflags": cov_rustflags.strip(),
    "executable_count": len(executables),
    "executables": executables,
}
objects_json_path.write_text(json.dumps(payload, indent=2) + "\n")
print(f"[coverage] discovered {len(executables)} instrumented executables -> {objects_json_path}")
PY

echo "[coverage] executing doctor_frankentui unit + integration test suite with DOCTOR_REAL_TOOL_INTEGRATION=1"
RCH_CARGO_WRAPPER_BYPASS=1 \
  CARGO_TARGET_DIR="${COV_TARGET_DIR}" \
  RUSTFLAGS="${COV_RUSTFLAGS}" \
  DOCTOR_REAL_TOOL_INTEGRATION=1 \
  LLVM_PROFILE_FILE="${PROFRAW_DIR}/doctor-%p-%m.profraw" \
  cargo test -p doctor_frankentui --all-targets

echo "[coverage] merging profraw files and exporting branch+line+function coverage summary"
python3 - "${LLVM_PROFDATA}" "${LLVM_COV}" "${PROFRAW_DIR}" "${PROFDATA_FILE}" "${OBJECTS_JSON}" "${ROOT_DIR}/crates/doctor_frankentui/src" "${SUMMARY_JSON}" <<'PY'
import json
import subprocess
import sys
from pathlib import Path

llvm_profdata = sys.argv[1]
llvm_cov = sys.argv[2]
profraw_dir = Path(sys.argv[3])
profdata_file = Path(sys.argv[4])
objects_json = Path(sys.argv[5])
source_scope = sys.argv[6]
summary_json = Path(sys.argv[7])

profraw_files = sorted(str(p) for p in profraw_dir.glob("*.profraw"))
if not profraw_files:
    raise SystemExit(f"[coverage] no .profraw files produced in {profraw_dir}")

subprocess.run(
    [llvm_profdata, "merge", "-sparse", *profraw_files, "-o", str(profdata_file)],
    check=True,
)

objects_payload = json.loads(objects_json.read_text())
executables = objects_payload["executables"]

cov_cmd = [
    llvm_cov,
    "export",
    "--format=text",
    "--summary-only",
    f"--instr-profile={profdata_file}",
]
for idx, exe in enumerate(executables):
    if idx == 0:
        cov_cmd.append(exe)
    else:
        cov_cmd.extend(["-object", exe])
cov_cmd.append(source_scope)

res = subprocess.run(cov_cmd, stdout=subprocess.PIPE, text=True, check=True)
summary_json.write_text(res.stdout)
PY

python3 - "${THRESHOLDS_TOML}" "${SUMMARY_JSON}" "${GATE_JSON}" "${GATE_TXT}" "${OBJECTS_JSON}" <<'PY'
import json
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    try:
        import tomli as tomllib  # type: ignore
    except ModuleNotFoundError as exc:  # pragma: no cover
        raise SystemExit(
            "[coverage] Python TOML parser unavailable. Install tomli or use Python 3.11+."
        ) from exc

thresholds_path = Path(sys.argv[1])
summary_path = Path(sys.argv[2])
gate_json_path = Path(sys.argv[3])
gate_txt_path = Path(sys.argv[4])
objects_json_path = Path(sys.argv[5])

thresholds = tomllib.loads(thresholds_path.read_text())
summary = json.loads(summary_path.read_text())
objects_payload = json.loads(objects_json_path.read_text())
root = summary["data"][0]
files = root["files"]


def percent(summary_obj: dict, metric: str) -> float:
    return float(summary_obj[metric]["percent"])


def normalize_path(path: str) -> str:
    return path.replace("\\", "/")


def path_matches(pattern: str, candidate_path: str) -> bool:
    normalized_pattern = normalize_path(pattern.strip()).lstrip("./")
    normalized_candidate = normalize_path(candidate_path)

    if "/" in normalized_pattern:
        return normalized_candidate.endswith(f"/{normalized_pattern}") or normalized_candidate == normalized_pattern

    return Path(normalized_candidate).name == normalized_pattern


def aggregate_group(group_files: list[str], metric: str) -> float:
    selected_paths: set[str] = set()

    for raw_pattern in group_files:
        pattern = raw_pattern.strip()
        if not pattern:
            continue

        matches = sorted(
            {
                normalize_path(file_entry["filename"])
                for file_entry in files
                if path_matches(pattern, file_entry["filename"])
            }
        )

        if not matches:
            raise RuntimeError(f"group pattern did not match any files: {pattern}")

        if len(matches) > 1:
            raise RuntimeError(
                f"ambiguous group file pattern '{pattern}' matched multiple files: {matches}. "
                "Use a more specific repo-relative path like "
                "'crates/doctor_frankentui/src/<file>.rs'."
            )

        selected_paths.update(matches)

    covered = 0
    count = 0

    for file_entry in files:
        filename = normalize_path(file_entry["filename"])
        if filename not in selected_paths:
            continue
        metric_summary = file_entry["summary"][metric]
        covered += int(metric_summary["covered"])
        count += int(metric_summary["count"])

    if count == 0:
        if metric == "branches":
            # Files with no branches should not fail per-module coverage floors.
            return 100.0
        raise RuntimeError(
            f"group has zero tracked elements for metric={metric} files={group_files}"
        )

    return covered * 100.0 / count


def evaluate_scope(scope_name: str, observed: dict[str, float], required: dict[str, float]):
    rows = []
    for metric, min_value in required.items():
        got = observed[metric]
        ok = got >= float(min_value)
        rows.append(
            {
                "metric": metric,
                "required": float(min_value),
                "observed": got,
                "pass": ok,
            }
        )
    return {"scope": scope_name, "checks": rows}

results = []
failed = False

raw_total = thresholds["total"]
required_total = {
    metric: float(raw_total[metric])
    for metric in ("lines", "branches", "functions")
    if metric in raw_total
}
observed_total = {
    "lines": percent(root["totals"], "lines"),
    "branches": percent(root["totals"], "branches"),
    "functions": percent(root["totals"], "functions"),
}
results.append(evaluate_scope("total", observed_total, required_total))

for group_name, group_cfg in thresholds["group"].items():
    observed_group = {
        "lines": aggregate_group(group_cfg["files"], "lines"),
        "branches": aggregate_group(group_cfg["files"], "branches"),
        "functions": aggregate_group(group_cfg["files"], "functions"),
    }
    required_group = {
        "lines": float(group_cfg["lines"]),
        "branches": float(group_cfg["branches"]),
        "functions": float(group_cfg["functions"]),
    }
    results.append(evaluate_scope(f"group:{group_name}", observed_group, required_group))

for scope in results:
    for row in scope["checks"]:
        if not row["pass"]:
            failed = True

report = {
    "status": "failed" if failed else "passed",
    "summary_path": str(summary_path),
    "thresholds_path": str(thresholds_path),
    "complete_executed_objects_path": str(objects_json_path),
    "executable_count": objects_payload.get("executable_count", 0),
    "scopes": results,
}

gate_json_path.write_text(json.dumps(report, indent=2) + "\n")

lines = []
lines.append(f"status={report['status']}")
lines.append(f"summary_path={summary_path}")
lines.append(f"complete_executed_objects_path={objects_json_path}")
for scope in results:
    lines.append(f"[{scope['scope']}]")
    for row in scope["checks"]:
        verdict = "PASS" if row["pass"] else "FAIL"
        lines.append(
            f"{row['metric']}: {verdict} observed={row['observed']:.3f}% required={row['required']:.3f}%"
        )

gate_txt_path.write_text("\n".join(lines) + "\n")
print("\n".join(lines))

if failed:
    sys.exit(1)
PY

echo "[coverage] complete executed objects: ${OBJECTS_JSON}"
echo "[coverage] coverage summary: ${SUMMARY_JSON}"
echo "[coverage] gate report (json): ${GATE_JSON}"
echo "[coverage] gate report (txt): ${GATE_TXT}"
