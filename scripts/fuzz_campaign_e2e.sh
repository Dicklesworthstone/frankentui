#!/usr/bin/env bash
set -euo pipefail

# bd-3fc.10: E2E test — Fuzz campaign validation (no crashes in N hours).
#
# Build through cargo-fuzz on the owner-coordinated DSR native host, then run
# the retained binaries directly. `cargo fuzz run` automatically formats crash
# inputs using a deleting temporary-file wrapper; direct invocation avoids it.
# A pass requires every selected target/job to finish its requested duration,
# report positive executions and coverage, and leave no crash artifacts.
# No automatic tool installation, artifact replay/minimization or cleanup.
# The DSR owner must inspect tooling and apply the retention guard before use.
#
# Environment variables:
#   FUZZ_DURATION_SECS  — per-job fuzz duration (default: 30, soak: 300+)
#   FUZZ_MAX_LEN        — max input length (default: 4096)
#   FUZZ_JOBS            — libFuzzer jobs per target (default: 1)
#   LOG_DIR              — NEW retained directory for builds, logs and JSONL
#   Project/log/artifact paths must use only ASCII letters, digits, _ . / : + -.
#   libFuzzer -jobs concatenates argv into shell commands without quoting; other
#   resolved paths are rejected before building or launching any fuzz process.
#   The nightly toolchain is read from rust-toolchain.toml.
#   RUN_ID               — deterministic run id override
#   FUZZ_TARGET_FILTER   — exact target for explicit focused scope; excluded
#                          configured targets remain unverified, never passed
#   FUZZ_ARTIFACT_ROOT   — crash artifact root (default: fuzz/artifacts)
#   FUZZ_INJECT_CRASH_TARGET — target name for deliberate artifact failure injection
#
# Usage:
#   ./scripts/fuzz_campaign_e2e.sh
#   FUZZ_DURATION_SECS=3600 ./scripts/fuzz_campaign_e2e.sh  # 1 hour per job

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
FUZZ_DIR="$PROJECT_ROOT/fuzz"

FUZZ_DURATION_SECS="${FUZZ_DURATION_SECS:-30}"
FUZZ_MAX_LEN="${FUZZ_MAX_LEN:-4096}"
FUZZ_JOBS="${FUZZ_JOBS:-1}"
PYTHON_BIN="${PYTHON_BIN:-python3}"
NIGHTLY_TOOLCHAIN=""
FUZZ_TARGET_FILTER="${FUZZ_TARGET_FILTER:-}"
LOG_DIR="${LOG_DIR:-/tmp/fuzz_campaign_e2e_$(date +%Y%m%d_%H%M%S)_$$}"
RUN_ID="${RUN_ID:-fuzz-campaign-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
RUN_ID="${RUN_ID//[^A-Za-z0-9_.:-]/_}"
FUZZ_ARTIFACT_ROOT="${FUZZ_ARTIFACT_ROOT:-$FUZZ_DIR/artifacts}"
FUZZ_INJECT_CRASH_TARGET="${FUZZ_INJECT_CRASH_TARGET:-}"
CORRELATION_SEQ=0
CONFIGURED_TARGETS=()
FUZZ_TARGETS=()
EXCLUDED_TARGETS=()
FINISHED_TARGETS=()
TOTAL_CRASHES=0
TOTAL_RUNS=0
PASSED_TARGETS=0
FAILED_TARGETS=0
INCOMPLETE_TARGETS=0
SKIPPED_TARGETS=0
CAMPAIGN_START=$(date +%s)

# An existing output directory may contain previous evidence or fuzz-N.log
# files. Refuse reuse rather than truncate, append another run, or clean it.
mkdir -p -- "$(dirname "$LOG_DIR")"
if ! mkdir -- "$LOG_DIR"; then
    echo "ERROR: LOG_DIR must be a new retained directory: $LOG_DIR" >&2
    exit 2
fi
LOG_DIR="$(cd "$LOG_DIR" && pwd -P)"
LOG_JSONL="$LOG_DIR/fuzz_campaign_e2e.jsonl"
if ! command -v "$PYTHON_BIN"; then
    echo "ERROR: Python 3.11+ is required; nothing was fuzzed" >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# JSONL logging
# ---------------------------------------------------------------------------

emit_event() {
    local event="$1"
    shift
    CORRELATION_SEQ=$((CORRELATION_SEQ + 1))
    "$PYTHON_BIN" - "$LOG_JSONL" "$RUN_ID" "$event" "$CORRELATION_SEQ" "$@" <<'PY'
import json
import sys
import time

path, run_id, event, sequence, *fields = sys.argv[1:]
if len(fields) % 2:
    raise ValueError("event fields must be key/value pairs")
numbers = set("""target_count configured_target_count excluded_target_count
duration_per_target_secs max_len jobs exit_code build_exit_code runs edges
peak_rss_mb crashes duration_secs fuzz_crashes_found_total total_runs
targets_passed targets_failed targets_incomplete targets_skipped
existing_crash_artifacts campaign_duration_secs""".split())
data = dict(event=event, run_id=run_id,
            correlation_id=f"{run_id}-corr-{int(sequence):04d}",
            ts=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()))
for key, value in zip(fields[::2], fields[1::2]):
    if key.endswith("_json"):
        data[key[:-5]] = json.loads(value)
    elif key in numbers:
        data[key] = None if value == "null" else int(value)
    else:
        data[key] = value
with open(path, "a", encoding="utf-8") as output:
    output.write(json.dumps(data, sort_keys=True) + "\n")
PY
}

incomplete_campaign() {
    local reason="$1"
    local index target
    for ((index=${#FINISHED_TARGETS[@]}; index<${#FUZZ_TARGETS[@]}; index++)); do
        target="${FUZZ_TARGETS[$index]}"
        emit_event target_end target "$target" status incomplete reason "$reason" \
            exit_code null build_exit_code null runs 0 crashes 0
        INCOMPLETE_TARGETS=$((INCOMPLETE_TARGETS + 1))
    done
    emit_event run_end status incomplete reason "$reason" \
        target_count "${#FUZZ_TARGETS[@]}" configured_target_count "${#CONFIGURED_TARGETS[@]}" \
        excluded_target_count "${#EXCLUDED_TARGETS[@]}" \
        targets_passed "$PASSED_TARGETS" targets_failed "$FAILED_TARGETS" \
        targets_incomplete "$INCOMPLETE_TARGETS" targets_skipped 0 \
        total_runs "$TOTAL_RUNS" fuzz_crashes_found_total "$TOTAL_CRASHES"
    echo "INCOMPLETE: $reason; selected campaign did not pass. JSONL: $LOG_JSONL" >&2
    exit 2
}

# ---------------------------------------------------------------------------
# Discover fuzz targets
# ---------------------------------------------------------------------------

if ! TARGET_NAMES="$("$PYTHON_BIN" - "$FUZZ_DIR/Cargo.toml" <<'PY'
import pathlib
import re
import sys
import tomllib

try:
    path = pathlib.Path(sys.argv[1])
    with path.open("rb") as source:
        document = tomllib.load(source)
    bins = document.get("bin")
    if not isinstance(bins, list) or not bins:
        raise ValueError("manifest must declare a nonempty [[bin]] target list")
    names = []
    for target in bins:
        name = target.get("name")
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", name):
            raise ValueError("every declared bin needs a safe, nonempty name")
        if name in names:
            raise ValueError(f"duplicate declared target: {name}")
        names.append(name)
    print("\n".join(names))
except (OSError, ValueError, AttributeError) as error:
    print(f"invalid fuzz target manifest: {error}", file=sys.stderr)
    sys.exit(1)
PY
)"; then
    incomplete_campaign invalid_target_manifest
fi
mapfile -t CONFIGURED_TARGETS <<< "$TARGET_NAMES"

for target in "${CONFIGURED_TARGETS[@]}"; do
    if [[ -z "$FUZZ_TARGET_FILTER" || "$target" == "$FUZZ_TARGET_FILTER" ]]; then
        FUZZ_TARGETS+=("$target")
    else
        EXCLUDED_TARGETS+=("$target")
    fi
done

if [[ ${#FUZZ_TARGETS[@]} -eq 0 ]]; then
    incomplete_campaign target_filter_did_not_match
fi

for value in "$FUZZ_DURATION_SECS" "$FUZZ_MAX_LEN" "$FUZZ_JOBS"; do
    if [[ ! "$value" =~ ^[1-9][0-9]{0,9}$ ]] || (( value > 2147483647 )); then
        incomplete_campaign invalid_positive_integer_configuration
    fi
done
if [[ "$RUN_ID" == "." || "$RUN_ID" == ".." ]]; then
    incomplete_campaign invalid_run_id
fi
if [[ -n "$FUZZ_INJECT_CRASH_TARGET" ]] && \
    [[ ! " $(printf '%s ' "${FUZZ_TARGETS[@]}")" == *" $FUZZ_INJECT_CRASH_TARGET "* ]]; then
    incomplete_campaign injection_target_not_selected
fi
if ! NIGHTLY_TOOLCHAIN="$(bash "$SCRIPT_DIR/ci/parse_toolchain_pin.sh" \
    "$PROJECT_ROOT/rust-toolchain.toml" 2> "$LOG_DIR/toolchain-pin.stderr")"; then
    cat "$LOG_DIR/toolchain-pin.stderr" >&2
    incomplete_campaign invalid_toolchain_pin
fi
export RUSTUP_TOOLCHAIN="$NIGHTLY_TOOLCHAIN"
FUZZ_ARTIFACT_ROOT="$("$PYTHON_BIN" -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$FUZZ_ARTIFACT_ROOT")"
for value in "$PROJECT_ROOT" "$LOG_DIR" "$FUZZ_ARTIFACT_ROOT"; do
    if [[ ! "$value" =~ ^[A-Za-z0-9_./:+-]+$ ]]; then
        echo "ERROR: libFuzzer -jobs cannot safely quote this resolved path: $value" >&2
        incomplete_campaign unsafe_libfuzzer_job_path
    fi
done
# cargo-fuzz 0.13.1 forwards build -Z flags to its child Cargo command and
# inherits this setting; both are required for retained-source freshness.
export CARGO_BUILD_FINGERPRINT=content
if ! GIT_COMMIT="$(git -C "$PROJECT_ROOT" rev-parse HEAD 2> "$LOG_DIR/git-revision.stderr")"; then
    cat "$LOG_DIR/git-revision.stderr" >&2
    GIT_COMMIT="unknown"
fi

echo "=== Fuzz Campaign E2E ==="
echo "Targets: ${#FUZZ_TARGETS[@]}"
echo "Duration per target: ${FUZZ_DURATION_SECS}s"
echo "Max input length: ${FUZZ_MAX_LEN}"
echo "Run ID: $RUN_ID"
echo "Log directory: $LOG_DIR"
echo ""

# ---------------------------------------------------------------------------
# Emit env record
# ---------------------------------------------------------------------------

emit_event env target_count "${#FUZZ_TARGETS[@]}" \
    configured_target_count "${#CONFIGURED_TARGETS[@]}" \
    excluded_target_count "${#EXCLUDED_TARGETS[@]}" \
    duration_per_target_secs "$FUZZ_DURATION_SECS" max_len "$FUZZ_MAX_LEN" jobs "$FUZZ_JOBS" \
    nightly_toolchain "$NIGHTLY_TOOLCHAIN" target_filter "$FUZZ_TARGET_FILTER" \
    git_commit "$GIT_COMMIT" platform "$(uname -s)" artifact_root "$FUZZ_ARTIFACT_ROOT" \
    failure_injection_target "$FUZZ_INJECT_CRASH_TARGET" \
    targets_json "$("$PYTHON_BIN" -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "${FUZZ_TARGETS[@]}")" \
    configured_targets_json "$("$PYTHON_BIN" -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "${CONFIGURED_TARGETS[@]}")"
for target in "${EXCLUDED_TARGETS[@]}"; do
    emit_event target_excluded target "$target" status unverified reason explicit_target_filter
done

# ---------------------------------------------------------------------------
# Check nightly toolchain
# ---------------------------------------------------------------------------

if ! rustup run "$NIGHTLY_TOOLCHAIN" rustc -vV > "$LOG_DIR/rustc-version.log" 2>&1; then
    cat "$LOG_DIR/rustc-version.log" >&2
    incomplete_campaign pinned_nightly_unavailable
fi
cat "$LOG_DIR/rustc-version.log"
if ! HOST_TRIPLE="$("$PYTHON_BIN" - "$LOG_DIR/rustc-version.log" <<'PY'
import pathlib
import re
import sys

hosts = re.findall(r"^host: ([A-Za-z0-9_-]+)$", pathlib.Path(sys.argv[1]).read_text(), re.M)
if len(hosts) != 1:
    raise ValueError("pinned rustc must report exactly one host triple")
print(hosts[0])
PY
)"; then
    incomplete_campaign invalid_rustc_host_report
fi

# Probe only: installation requires a separate owner-coordinated action.
if ! cargo +"$NIGHTLY_TOOLCHAIN" fuzz --version > "$LOG_DIR/cargo-fuzz-version.log" 2>&1; then
    cat "$LOG_DIR/cargo-fuzz-version.log" >&2
    incomplete_campaign cargo_fuzz_unavailable
fi
cat "$LOG_DIR/cargo-fuzz-version.log"

# Count artifacts without suppressing traversal errors or following symlinks.
# Retained artifacts are evidence, not inputs to an automatic cleanup/replay.
count_artifacts() {
    "$PYTHON_BIN" - "$1" <<'PY'
import os
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
def count(path):
    total = 0
    with os.scandir(path) as entries:
        for entry in entries:
            if entry.is_symlink():
                raise ValueError(f"artifact traversal refuses symlink: {entry.path}")
            if entry.is_dir(follow_symlinks=False):
                total += count(entry.path)
            elif entry.is_file(follow_symlinks=False) and entry.name.startswith(("crash-", "oom-", "timeout-", "leak-")):
                total += 1
    return total
try:
    root.lstat()
except FileNotFoundError:
    print(0)
else:
    if root.is_symlink():
        raise ValueError(f"artifact traversal refuses symlink: {root}")
    print(count(root))
PY
}

# Parse each retained worker log, not the last statistics in merged output.
# The manager must identify every requested job exactly once with exit code 0.
# Coverage is the maximum reported job edge count, not a union across jobs.
parse_target_stats() {
    "$PYTHON_BIN" - "$1" "$2" "$FUZZ_JOBS" "$FUZZ_DURATION_SECS" <<'PY'
import pathlib
import re
import sys

manager_path, worker_path, jobs, duration = sys.argv[1:]
jobs, duration = int(jobs), int(duration)
limit = 2**53 - 1

def integer(value, label, positive=True):
    if not re.fullmatch(r"[0-9]+", value):
        raise ValueError(f"invalid {label}: {value!r}")
    result = int(value)
    if not (1 if positive else 0) <= result <= limit:
        raise ValueError(f"out-of-range {label}: {value!r}")
    return result

def one(lines, prefix, pattern, label):
    candidates = [line for line in lines if re.match(prefix, line)]
    if len(candidates) != 1:
        raise ValueError(f"expected exactly one {label}, got {len(candidates)}")
    match = re.fullmatch(pattern, candidates[0])
    if match is None:
        raise ValueError(f"malformed {label}: {candidates[0]!r}")
    return match.groups()

try:
    manager = pathlib.Path(manager_path).read_text(encoding="utf-8").splitlines()
    markers = [line for line in manager if line.startswith("================== Job ")]
    seen = set()
    for line in markers:
        match = re.fullmatch(r"================== Job ([0-9]+) exited with exit code ([0-9]+) ============", line)
        if match is None:
            raise ValueError(f"malformed worker exit marker: {line!r}")
        index, status = map(int, match.groups())
        if index in seen or not 0 <= index < jobs or status != 0:
            raise ValueError(f"duplicate, unexpected or failed worker: {line!r}")
        seen.add(index)
    if len(seen) != jobs:
        raise ValueError(f"only {len(seen)} of {jobs} worker exits reported")
    worker_root = pathlib.Path(worker_path)
    if {path.name for path in worker_root.glob("fuzz-*.log")} != {f"fuzz-{i}.log" for i in range(jobs)}:
        raise ValueError("retained worker log identities do not match requested jobs")
    total_runs = peak_rss = edges = 0
    for index in range(jobs):
        path = worker_root / f"fuzz-{index}.log"
        if path.is_symlink():
            raise ValueError(f"worker log must not be a symlink: {path}")
        lines = path.read_text(encoding="utf-8").splitlines()
        units, = one(lines, r"^stat::number_of_executed_", r"stat::number_of_executed_(?:inputs|units):\s*([0-9]+)", "execution count")
        rss, = one(lines, r"^stat::peak_rss_mb", r"stat::peak_rss_mb:\s*([0-9]+)", "peak RSS")
        done_runs, seconds = one(lines, r"^Done ", r"Done ([0-9]+) runs in ([0-9]+) second\(s\)", "completed duration")
        done_count, done_edges = one(lines, r"^#[0-9]+\s+DONE\b", r"#([0-9]+)\s+DONE\s+cov:\s*([0-9]+)(?:\s+.*)?", "final coverage")
        runs = integer(units, "execution count")
        if runs != integer(done_runs, "completed run count") or runs != integer(done_count, "coverage run count"):
            raise ValueError("execution, completion and final coverage counts disagree")
        if integer(seconds, "completed seconds", positive=False) < duration:
            raise ValueError("worker ended before its requested campaign duration")
        total_runs += runs
        if total_runs > limit:
            raise ValueError("aggregate execution count exceeds exact reporting range")
        peak_rss = max(peak_rss, integer(rss, "peak RSS", positive=False))
        edges = max(edges, integer(done_edges, "coverage edges"))
    print(total_runs, edges, peak_rss)
except (OSError, UnicodeError, ValueError) as error:
    print(f"incomplete fuzz execution evidence: {error}", file=sys.stderr)
    sys.exit(1)
PY
}

# ---------------------------------------------------------------------------
# Run fuzz campaign
# ---------------------------------------------------------------------------

BUILD_ROOT="$LOG_DIR/build"

for target in "${FUZZ_TARGETS[@]}"; do
    echo "--- Fuzzing: $target (${FUZZ_DURATION_SECS}s) ---"
    TARGET_LOG="$LOG_DIR/${target}.log"
    BUILD_LOG="$LOG_DIR/${target}.build.log"
    WORKER_DIR="$LOG_DIR/$target"
    TARGET_START=$(date +%s)
    TARGET_STATUS="incomplete"
    TARGET_REASON="build_failed"
    TARGET_CRASHES=0
    TARGET_RUNS=0
    TARGET_EDGES=0
    TARGET_RSS=0
    BUILD_EXIT="null"
    FUZZ_EXIT="null"
    TARGET_ARTIFACT_DIR="$FUZZ_ARTIFACT_ROOT/$target/$RUN_ID"
    TARGET_BINARY="$BUILD_ROOT/$HOST_TRIPLE/release/$target"
    CORPUS_DIR="$FUZZ_DIR/corpus/$target"
    printf -v TARGET_REPLAY_CMD '%q ' "$TARGET_BINARY"
    if ! mkdir -- "$WORKER_DIR" || ! mkdir -p -- "$FUZZ_ARTIFACT_ROOT/$target" "$CORPUS_DIR"; then
        incomplete_campaign target_directory_setup_failed
    fi
    if ! mkdir -- "$TARGET_ARTIFACT_DIR"; then
        incomplete_campaign artifact_run_directory_already_exists
    fi

    emit_event target_start target "$target" duration_secs "$FUZZ_DURATION_SECS" \
        target_log "$TARGET_LOG" build_log "$BUILD_LOG" worker_dir "$WORKER_DIR" \
        artifact_dir "$TARGET_ARTIFACT_DIR" replay_command "$TARGET_REPLAY_CMD"

    # The build directory is fresh for this campaign, so an old executable
    # cannot turn a failed build into a run. All compiler stderr is retained.
    if (cd "$PROJECT_ROOT" && cargo +"$NIGHTLY_TOOLCHAIN" fuzz build "$target" \
        --fuzz-dir "$FUZZ_DIR" --target "$HOST_TRIPLE" --target-dir "$BUILD_ROOT" \
        -Z checksum-freshness) \
        > "$BUILD_LOG" 2>&1; then
        BUILD_EXIT=0
        if [[ -f "$TARGET_BINARY" && -x "$TARGET_BINARY" && ! -L "$TARGET_BINARY" ]]; then
            # Normal max_total_time completion exits 0. libFuzzer's error 77,
            # per-input timeout 70, signals and all other nonzero exits fail.
            if (cd "$WORKER_DIR" && \
                ASAN_OPTIONS="${ASAN_OPTIONS:+$ASAN_OPTIONS:}detect_odr_violation=0" \
                "$TARGET_BINARY" "$CORPUS_DIR" \
                -max_len="$FUZZ_MAX_LEN" -max_total_time="$FUZZ_DURATION_SECS" \
                -jobs="$FUZZ_JOBS" -print_final_stats=1 -verbosity=1 \
                -artifact_prefix="$TARGET_ARTIFACT_DIR/") > "$TARGET_LOG" 2>&1; then
                FUZZ_EXIT=0
                if STATS="$(parse_target_stats "$TARGET_LOG" "$WORKER_DIR" \
                    2> "$LOG_DIR/${target}.stats.stderr")"; then
                    read -r TARGET_RUNS TARGET_EDGES TARGET_RSS <<< "$STATS"
                    TARGET_STATUS="pass"
                    TARGET_REASON="complete_execution"
                else
                    cat "$LOG_DIR/${target}.stats.stderr" >&2
                    TARGET_REASON="incomplete_execution_evidence"
                fi
            else
                FUZZ_EXIT=$?
                TARGET_STATUS="fail"
                TARGET_REASON="fuzz_process_failed"
                cat "$TARGET_LOG" >&2
            fi
        else
            TARGET_REASON="built_binary_missing"
            echo "ERROR: build did not produce executable $TARGET_BINARY" >&2
        fi
    else
        BUILD_EXIT=$?
        cat "$BUILD_LOG" >&2
    fi

    TARGET_END=$(date +%s)
    TARGET_ELAPSED=$((TARGET_END - TARGET_START))

    if [[ "$FUZZ_INJECT_CRASH_TARGET" == "$target" ]]; then
        INJECTED_ARTIFACT="$TARGET_ARTIFACT_DIR/crash-injected-$RUN_ID"
        (set -o noclobber; printf 'deliberate failure injection for %s\n' "$target" > "$INJECTED_ARTIFACT")
        emit_event failure_injection target "$target" artifact "$INJECTED_ARTIFACT" \
            diagnostic "synthetic artifact tests gate rejection, not an observed fuzz crash"
    fi

    if TARGET_CRASHES="$(count_artifacts "$TARGET_ARTIFACT_DIR")"; then
        if (( TARGET_CRASHES > 0 )); then
            TARGET_STATUS="fail"
            TARGET_REASON="crash_artifacts_present"
        fi
    else
        TARGET_CRASHES=0
        if [[ "$TARGET_STATUS" == "fail" ]]; then
            TARGET_REASON="fuzz_process_failed_and_artifact_inspection_failed"
        else
            TARGET_STATUS="incomplete"
            TARGET_REASON="artifact_inspection_failed"
        fi
    fi
    case "$TARGET_STATUS" in
        pass) PASSED_TARGETS=$((PASSED_TARGETS + 1)) ;;
        fail) FAILED_TARGETS=$((FAILED_TARGETS + 1)) ;;
        incomplete) INCOMPLETE_TARGETS=$((INCOMPLETE_TARGETS + 1)) ;;
        *) echo "ERROR: invalid internal target status" >&2; exit 2 ;;
    esac

    TOTAL_CRASHES=$((TOTAL_CRASHES + TARGET_CRASHES))
    TOTAL_RUNS=$((TOTAL_RUNS + TARGET_RUNS))

    echo "  Status: $TARGET_STATUS | Runs: $TARGET_RUNS | Edges: $TARGET_EDGES | Crashes: $TARGET_CRASHES | Time: ${TARGET_ELAPSED}s"

    emit_event target_end target "$target" status "$TARGET_STATUS" reason "$TARGET_REASON" \
        exit_code "$FUZZ_EXIT" build_exit_code "$BUILD_EXIT" runs "$TARGET_RUNS" \
        edges "$TARGET_EDGES" peak_rss_mb "$TARGET_RSS" crashes "$TARGET_CRASHES" \
        target_log "$TARGET_LOG" build_log "$BUILD_LOG" worker_dir "$WORKER_DIR" \
        artifact_dir "$TARGET_ARTIFACT_DIR" replay_command "$TARGET_REPLAY_CMD" \
        duration_secs "$TARGET_ELAPSED"
    FINISHED_TARGETS+=("$target")
done

# ---------------------------------------------------------------------------
# Check existing crash artifacts (from previous campaigns)
# ---------------------------------------------------------------------------

EXISTING_CRASHES=0
ARTIFACT_CHECK_FAILED=0
if ! EXISTING_CRASHES="$(count_artifacts "$FUZZ_ARTIFACT_ROOT")"; then
    ARTIFACT_CHECK_FAILED=1
    EXISTING_CRASHES=0
fi

emit_event artifact_check existing_crash_artifacts "$EXISTING_CRASHES" \
    inspection_failed "$ARTIFACT_CHECK_FAILED" artifacts_dir "$FUZZ_ARTIFACT_ROOT"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

CAMPAIGN_END=$(date +%s)
CAMPAIGN_ELAPSED=$((CAMPAIGN_END - CAMPAIGN_START))

if (( TOTAL_CRASHES > 0 || EXISTING_CRASHES > 0 || FAILED_TARGETS > 0 )); then
    OVERALL_STATUS="fail"
    EXIT_CODE=1
elif (( INCOMPLETE_TARGETS > 0 || ARTIFACT_CHECK_FAILED > 0 || \
    PASSED_TARGETS != ${#FUZZ_TARGETS[@]} || TOTAL_RUNS <= 0 )); then
    OVERALL_STATUS="incomplete"
    EXIT_CODE=2
else
    OVERALL_STATUS="pass"
    EXIT_CODE=0
fi

emit_event run_end status "$OVERALL_STATUS" \
    target_count "${#FUZZ_TARGETS[@]}" configured_target_count "${#CONFIGURED_TARGETS[@]}" \
    excluded_target_count "${#EXCLUDED_TARGETS[@]}" \
    fuzz_crashes_found_total "$TOTAL_CRASHES" total_runs "$TOTAL_RUNS" \
    targets_passed "$PASSED_TARGETS" targets_failed "$FAILED_TARGETS" \
    targets_incomplete "$INCOMPLETE_TARGETS" targets_skipped "$SKIPPED_TARGETS" \
    existing_crash_artifacts "$EXISTING_CRASHES" campaign_duration_secs "$CAMPAIGN_ELAPSED"

echo ""
echo "=== Fuzz Campaign Summary ==="
echo "Status:          $OVERALL_STATUS"
echo "Targets:         ${#FUZZ_TARGETS[@]} (pass=$PASSED_TARGETS, fail=$FAILED_TARGETS, incomplete=$INCOMPLETE_TARGETS)"
echo "Configured:      ${#CONFIGURED_TARGETS[@]} (excluded/unverified=${#EXCLUDED_TARGETS[@]})"
echo "Total runs:      $TOTAL_RUNS"
echo "Total crashes:   $TOTAL_CRASHES"
echo "Duration:        ${CAMPAIGN_ELAPSED}s"
echo "Run ID:          $RUN_ID"
echo "JSONL log:       $LOG_JSONL"
echo "Target logs:     $LOG_DIR/*.log"
echo ""

if [[ $EXIT_CODE -eq 0 ]]; then
    echo "PASS: Every selected target and job completed with positive execution evidence."
    if (( ${#EXCLUDED_TARGETS[@]} > 0 )); then
        echo "Focused scope only; excluded configured targets remain unverified."
    fi
else
    echo "${OVERALL_STATUS^^}: selected campaign evidence is not complete and clean." >&2
fi

exit $EXIT_CODE
