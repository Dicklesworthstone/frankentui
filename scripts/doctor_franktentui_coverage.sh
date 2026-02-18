#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${1:-${ROOT_DIR}/target/doctor_franktentui_coverage}"
THRESHOLDS_TOML="${ROOT_DIR}/crates/doctor_franktentui/coverage/thresholds.toml"
SUMMARY_JSON="${OUT_DIR}/coverage_summary.json"
GATE_JSON="${OUT_DIR}/coverage_gate_report.json"
GATE_TXT="${OUT_DIR}/coverage_gate_report.txt"

mkdir -p "${OUT_DIR}"

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

if ! command -v cargo-llvm-cov >/dev/null 2>&1 && ! cargo llvm-cov --version >/dev/null 2>&1; then
  echo "[coverage] cargo-llvm-cov is required (cargo install cargo-llvm-cov)" >&2
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

echo "[coverage] generating branch+line+function coverage summary for doctor_franktentui"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT_DIR}/target/doctor_franktentui_cov}" \
  cargo llvm-cov -p doctor_franktentui --all-targets --branch --summary-only --json --output-path "${SUMMARY_JSON}"

python3 - "${THRESHOLDS_TOML}" "${SUMMARY_JSON}" "${GATE_JSON}" "${GATE_TXT}" <<'PY'
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

thresholds = tomllib.loads(thresholds_path.read_text())
summary = json.loads(summary_path.read_text())
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
                "'crates/doctor_franktentui/src/<file>.rs'."
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
        raise RuntimeError(f"group has zero tracked elements for metric={metric} files={group_files}")

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

required_total = thresholds["total"]
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
    "scopes": results,
}

gate_json_path.write_text(json.dumps(report, indent=2) + "\n")

lines = []
lines.append(f"status={report['status']}")
lines.append(f"summary_path={summary_path}")
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

echo "[coverage] coverage summary: ${SUMMARY_JSON}"
echo "[coverage] gate report (json): ${GATE_JSON}"
echo "[coverage] gate report (txt): ${GATE_TXT}"
