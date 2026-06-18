#!/usr/bin/env bash
# Pane profiling runner (bd-1y0ph)
#
# Captures repeatable benchmark output for the pane core, terminal adapter,
# and web pointer-capture paths into one artifact directory.
#
# Usage:
#   ./scripts/pane_profile.sh
#   ./scripts/pane_profile.sh --test
#   ./scripts/pane_profile.sh --perf-stat
#   ./scripts/pane_profile.sh --out-dir target/pane-profiling/custom

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
OUT_DIR="${PROJECT_ROOT}/target/pane-profiling/bd-1y0ph"
TEST_MODE=false
RESOURCE_STATS=false
PERF_STAT=false
STACK_REPORTS=false
declare -A BENCH_BINARY_PATHS=()
declare -A BENCH_EXECUTED_PATHS=()
declare -A BENCH_WORKERS=()
declare -A BENCH_WORKER_ENDPOINTS=()
declare -A BENCH_EXACT_BINARY_ARTIFACTS=()
declare -A BENCH_FETCH_ERRORS=()
declare -A BENCH_BINARY_SOURCES=()
EXACT_BINARY_DIR=""

if command -v rch >/dev/null 2>&1; then
    CARGO_RUNNER=(rch exec -- cargo)
else
    CARGO_RUNNER=(cargo)
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        --test)
            TEST_MODE=true
            shift
            ;;
        --out-dir)
            OUT_DIR="$2"
            shift 2
            ;;
        --time)
            RESOURCE_STATS=true
            shift
            ;;
        --perf-stat)
            PERF_STAT=true
            shift
            ;;
        --stack-reports)
            STACK_REPORTS=true
            shift
            ;;
        -h|--help)
            cat <<EOF
Usage: $0 [--test] [--time] [--perf-stat] [--stack-reports] [--out-dir PATH]

  --test          Run Criterion targets in fast test mode.
  --time          Capture /usr/bin/time -v resource stats per bench.
  --perf-stat     Capture perf stat counters for representative pane benches.
  --stack-reports Capture representative perf record/report artifacts plus
                  post-symbolized user-space stack summaries.
  --out-dir PATH  Write captured outputs under PATH.
EOF
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

mkdir -p "$OUT_DIR"
EXACT_BINARY_DIR="${OUT_DIR}/executed-binaries"
mkdir -p "$EXACT_BINARY_DIR"

bench_args=()
if [[ "$TEST_MODE" == "true" ]]; then
    bench_args+=(--test)
fi

strip_ansi_file() {
    local input_file="$1"
    perl -pe 's/\e\[[0-9;]*[[:alpha:]]//g' "$input_file"
}

parse_bench_binary_from_output() {
    local output_file="$1"
    strip_ansi_file "$output_file" \
        | sed -nE 's#^[[:space:]]*Running benches/[^ ]+[[:space:]]+\(([^)]+)\)$#\1#p' \
        | tail -n1
}

parse_worker_from_output() {
    local output_file="$1"
    strip_ansi_file "$output_file" \
        | sed -nE 's#.*Selected worker: ([^ ]+) at [^ ]+([[:space:]].*)?$#\1#p' \
        | tail -n1
}

parse_worker_endpoint_from_output() {
    local output_file="$1"
    strip_ansi_file "$output_file" \
        | sed -nE 's#.*Selected worker: [^ ]+ at ([^ ]+)([[:space:]].*)?$#\1#p' \
        | tail -n1
}

fetch_exact_bench_binary() {
    local label="$1"
    local executed_path="$2"
    local worker="$3"
    local artifact_path

    artifact_path="${EXACT_BINARY_DIR}/${label}-$(basename "$executed_path")"

    if [[ -z "$worker" ]]; then
        BENCH_FETCH_ERRORS["$label"]="worker_unknown"
        return 1
    fi

    if scp -O -q "$worker:$executed_path" "$artifact_path"; then
        chmod u+x "$artifact_path" || true
        BENCH_EXACT_BINARY_ARTIFACTS["$label"]="$artifact_path"
        BENCH_BINARY_PATHS["$label"]="$artifact_path"
        BENCH_BINARY_SOURCES["$label"]="executed_remote_fetched"
        return 0
    fi

    BENCH_FETCH_ERRORS["$label"]="scp_failed"
    return 1
}

preserve_local_bench_binary() {
    local label="$1"
    local bench_binary="$2"
    local artifact_path

    artifact_path="${EXACT_BINARY_DIR}/${label}-$(basename "$bench_binary")"

    cp "$bench_binary" "$artifact_path"
    chmod u+x "$artifact_path" || true
    BENCH_EXACT_BINARY_ARTIFACTS["$label"]="$artifact_path"
    BENCH_BINARY_PATHS["$label"]="$artifact_path"
    BENCH_BINARY_SOURCES["$label"]="executed_local_preserved"
}

record_executed_binary_metadata() {
    local label="$1"
    local output_file="$2"
    local bench_binary
    local worker
    local worker_endpoint

    bench_binary="$(parse_bench_binary_from_output "$output_file")"
    worker="$(parse_worker_from_output "$output_file")"
    worker_endpoint="$(parse_worker_endpoint_from_output "$output_file")"

    if [[ -n "$worker" ]]; then
        BENCH_WORKERS["$label"]="$worker"
    fi
    if [[ -n "$worker_endpoint" ]]; then
        BENCH_WORKER_ENDPOINTS["$label"]="$worker_endpoint"
    fi

    if [[ -z "$bench_binary" ]]; then
        BENCH_FETCH_ERRORS["$label"]="bench_binary_not_reported"
        return 0
    fi

    if [[ "$bench_binary" != /* ]]; then
        bench_binary="${PROJECT_ROOT}/${bench_binary}"
    fi

    BENCH_EXECUTED_PATHS["$label"]="$bench_binary"

    if [[ -x "$bench_binary" ]]; then
        preserve_local_bench_binary "$label" "$bench_binary"
        return 0
    fi

    fetch_exact_bench_binary "$label" "$bench_binary" "$worker" || true
}

run_bench() {
    local label="$1"
    shift
    local output_file="${OUT_DIR}/${label}.txt"
    local time_file="${OUT_DIR}/${label}.time.txt"
    echo "==> ${label}"
    if [[ "$RESOURCE_STATS" == "true" ]]; then
        if [[ ! -x /usr/bin/time ]]; then
            echo "ERROR: /usr/bin/time is required for --time mode" >&2
            exit 1
        fi
        /usr/bin/time -v -o "$time_file" "${CARGO_RUNNER[@]}" "$@" 2>&1 | tee "$output_file"
    else
        "${CARGO_RUNNER[@]}" "$@" 2>&1 | tee "$output_file"
    fi

    record_executed_binary_metadata "$label" "$output_file"
}

find_latest_local_bench_binary() {
    local prefix="$1"
    local binary
    local deps_dir="${PROJECT_ROOT}/target/release/deps"

    if [[ ! -d "$deps_dir" ]]; then
        printf '\n'
        return 0
    fi

    binary="$(
        find "$deps_dir" \
            -maxdepth 1 \
            -type f \
            -name "${prefix}-*" \
            ! -name '*.d' \
            -perm -u+x \
            -printf '%T@ %p\n' \
            | sort -n \
            | tail -n1 \
            | cut -d' ' -f2-
    )"

    printf '%s\n' "$binary"
}

find_bench_binary() {
    local prefix="$1"
    local binary="${BENCH_BINARY_PATHS[$prefix]:-}"

    if [[ -n "$binary" && -x "$binary" ]]; then
        printf '%s\n' "$binary"
        return 0
    fi

    binary="$(find_latest_local_bench_binary "$prefix")"
    if [[ -n "$binary" ]]; then
        printf '%s\n' "$binary"
        return 0
    fi

    echo "ERROR: no bench binary found for ${prefix}" >&2
    echo "Hint: run the matching cargo bench target first." >&2
    exit 1
}

run_perf_stat() {
    local label="$1"
    local binary_prefix="$2"
    local benchmark_name="$3"
    local output_file="${OUT_DIR}/${label}.perfstat.txt"
    local binary

    if ! command -v perf >/dev/null 2>&1; then
        echo "ERROR: perf is required for --perf-stat mode" >&2
        exit 1
    fi

    binary="$(find_bench_binary "$binary_prefix")"
    echo "==> ${label} (perf stat)"
    perf stat -d -r 3 -o "$output_file" -- \
        "$binary" \
        "$benchmark_name" \
        --exact \
        --profile-time 2 \
        --noplot
}

symbolize_perf_user_frames() {
    local perf_data="$1"
    local binary="$2"
    local output_file="$3"

    python3 - "$perf_data" "$binary" "$output_file" <<'PY'
import collections
import pathlib
import re
import subprocess
import sys

perf_data = pathlib.Path(sys.argv[1])
binary = pathlib.Path(sys.argv[2]).resolve()
output_file = pathlib.Path(sys.argv[3])

script = subprocess.check_output(
    ["perf", "script", "--show-mmap-events", "-i", str(perf_data)],
    text=True,
    errors="replace",
)

mmap_re = re.compile(
    r"PERF_RECORD_MMAP2 .*: \[(0x[0-9a-f]+)\(0x[0-9a-f]+\) @ (0x[0-9a-f]+) <[^>]+>\]: [^ ]+ (.+)$"
)
symbolized_frame_re = re.compile(r"^\s*([0-9a-f]+)\s+(.+)\s+\((.+)\)$")

mmaps = []
weighted_symbols = collections.Counter()
for line in script.splitlines():
    match = mmap_re.search(line)
    if not match:
        continue
    path = pathlib.Path(match.group(3)).resolve()
    if path != binary:
        continue
    mmaps.append((int(match.group(1), 16), int(match.group(2), 16)))

counter = collections.Counter()
for line in script.splitlines():
    symbol_match = symbolized_frame_re.match(line)
    if not symbol_match:
        continue
    path = pathlib.Path(symbol_match.group(3)).resolve()
    if path != binary:
        continue
    symbol = symbol_match.group(2).strip()
    if symbol != "[unknown]":
        weighted_symbols[symbol] += 1
        continue

    absolute = int(symbol_match.group(1), 16)
    candidates = [(start, pgoff) for start, pgoff in mmaps if absolute >= start]
    if not candidates:
        continue
    start, pgoff = max(candidates, key=lambda pair: pair[0])
    relative = absolute - start + pgoff
    if relative >= 0:
        counter[relative] += 1

with output_file.open("w", encoding="utf-8") as fh:
    fh.write(f"Post-symbolized user-space frames for {binary.name}\n")
    fh.write(f"perf_data={perf_data}\n")
    if mmaps:
        fh.write("mmap_ranges=\n")
        for start, pgoff in mmaps:
            fh.write(f"- start={hex(start)} pgoff={hex(pgoff)}\n")
    else:
        fh.write("mmap_ranges=missing\n")
    user_space_weight = sum(weighted_symbols.values()) + sum(counter.values())
    fh.write(f"user_space_frames={user_space_weight}\n\n")

    if not weighted_symbols and not counter:
        fh.write("No user-space frames from the target binary were recovered.\n")
        sys.exit(0)

    fh.write("Top frames:\n")
    combined = collections.Counter(weighted_symbols)
    for relative, count in counter.items():
        symbol = subprocess.check_output(
            ["addr2line", "-Cfipe", str(binary), hex(relative)],
            text=True,
            errors="replace",
        ).strip()
        combined[f"{symbol} @ {hex(relative)}"] += count

    total = sum(combined.values())
    for symbol, count in combined.most_common(12):
        percent = (count / total) * 100.0 if total else 0.0
        fh.write(f"- {percent:5.1f}% ({count} frame hits)\n")
        for line in symbol.splitlines():
            fh.write(f"  {line}\n")
PY
}

run_stack_report() {
    local stem="$1"
    local binary_prefix="$2"
    local benchmark_name="$3"
    local data_file="${OUT_DIR}/${stem}.perf.data"
    local report_file="${OUT_DIR}/${stem}.perf.txt"
    local symbols_file="${OUT_DIR}/${stem}.symbols.txt"
    local binary

    if ! command -v perf >/dev/null 2>&1; then
        echo "ERROR: perf is required for --stack-reports mode" >&2
        exit 1
    fi
    if ! command -v addr2line >/dev/null 2>&1; then
        echo "ERROR: addr2line is required for --stack-reports mode" >&2
        exit 1
    fi

    binary="$(find_bench_binary "$binary_prefix")"
    echo "==> ${stem} (perf record/report)"
    perf record -q -F 999 -g --call-graph dwarf,16384 -o "$data_file" -- \
        "$binary" \
        "$benchmark_name" \
        --exact \
        --profile-time 2 \
        --noplot
    perf report --stdio -g none -i "$data_file" > "$report_file"
    symbolize_perf_user_frames "$data_file" "$binary" "$symbols_file"
}

validate_stack_reports() {
    local stems=(
        pane_core_timeline_apply_and_replay_32_ops
        pane_terminal_down_drag_120_up
        pane_pointer_down_ack_move_120_up
    )
    local stem

    for stem in "${stems[@]}"; do
        local symbols_file="${OUT_DIR}/${stem}.symbols.txt"
        if [[ ! -s "$symbols_file" ]]; then
            echo "ERROR: missing or empty stack summary ${symbols_file}" >&2
            exit 1
        fi
        if ! grep -q '^user_space_frames=' "$symbols_file"; then
            echo "ERROR: malformed stack summary ${symbols_file}" >&2
            exit 1
        fi
    done
}

# Emit machine-parseable symbolization fields for one bench binary so the
# replay-artifact index (bd-1pvzq.1) can decide whether a profile of this exact
# binary can be symbolized back to source without re-inspecting the ELF.
emit_binary_symbolization_fields() {
    local binary="$1"
    local build_id="unknown"
    local debug_info="unknown"
    local has_debug_line="false"
    local stripped="unknown"
    local addr2line_ready="false"
    local binary_sha256="unknown"

    if command -v readelf >/dev/null 2>&1; then
        local sections
        sections="$(readelf -S "$binary" 2>/dev/null || true)"
        if grep -q '\.debug_info' <<<"$sections"; then
            debug_info="present"
        else
            debug_info="missing"
        fi
        if grep -q '\.debug_line' <<<"$sections"; then
            has_debug_line="true"
        fi
        if grep -q '\.symtab' <<<"$sections"; then
            stripped="false"
        else
            stripped="true"
        fi
        build_id="$(readelf -n "$binary" 2>/dev/null \
            | sed -nE 's/.*Build ID: ([0-9a-fA-F]+).*/\1/p' | head -n1)"
        [[ -n "$build_id" ]] || build_id="missing"
    fi

    # addr2line maps sample addresses to source via .debug_line/.debug_info; a
    # binary is symbolizable iff both are present and it was not stripped.
    if [[ "$debug_info" == "present" && "$has_debug_line" == "true" && "$stripped" == "false" ]]; then
        addr2line_ready="true"
    fi

    if command -v sha256sum >/dev/null 2>&1; then
        binary_sha256="$(sha256sum "$binary" 2>/dev/null | cut -d' ' -f1)"
        [[ -n "$binary_sha256" ]] || binary_sha256="unknown"
    fi

    echo "build_id=${build_id}"
    echo "debug_info=${debug_info}"
    echo "debug_line=${has_debug_line}"
    echo "stripped=${stripped}"
    echo "addr2line_ready=${addr2line_ready}"
    echo "binary_sha256=${binary_sha256}"
    # symbolization_ready needs both a content-addressable build-id (to match a
    # profile to this binary) and addr2line readiness (to resolve the symbols).
    if [[ "$addr2line_ready" == "true" && "$build_id" != "missing" && "$build_id" != "unknown" ]]; then
        echo "symbolization_ready=true"
    else
        echo "symbolization_ready=false"
    fi
}

record_symbol_metadata() {
    local output_file="${OUT_DIR}/symbol_metadata.txt"
    local prefixes=(
        pane_profile_harness
        layout_bench
        pane_terminal_bench
        pane_pointer_bench
    )

    : > "$output_file"
    {
        echo "Pane bench symbol metadata"
        echo "Generated by: ${0##*/}"
        echo
    } >> "$output_file"

    for prefix in "${prefixes[@]}"; do
        local binary="${BENCH_EXACT_BINARY_ARTIFACTS[$prefix]:-}"
        local executed_path="${BENCH_EXECUTED_PATHS[$prefix]:-}"
        local worker="${BENCH_WORKERS[$prefix]:-local}"
        local worker_endpoint="${BENCH_WORKER_ENDPOINTS[$prefix]:-local}"
        local fetch_error="${BENCH_FETCH_ERRORS[$prefix]:-}"
        local local_candidate=""
        local source="${BENCH_BINARY_SOURCES[$prefix]:-executed_local}"
        local exact_status="available"

        if [[ -z "$binary" ]]; then
            source="executed_remote_missing"
            exact_status="missing"
            local_candidate="$(find_latest_local_bench_binary "$prefix")"
        fi

        {
            echo "== ${prefix} =="
            echo "executed_path=${executed_path:-unknown}"
            echo "worker=${worker}"
            echo "worker_endpoint=${worker_endpoint}"
            echo "binary_source=${source}"
            echo "exact_binary_status=${exact_status}"
            if [[ -n "$binary" ]]; then
                echo "exact_binary_local=${binary}"
            else
                echo "exact_binary_local=missing"
            fi
            if [[ -n "$fetch_error" ]]; then
                echo "fetch_error=${fetch_error}"
            else
                echo "fetch_error=none"
            fi
            if [[ -n "$local_candidate" ]]; then
                echo "local_candidate=${local_candidate}"
            else
                echo "local_candidate=missing"
            fi
            if [[ -e "$binary" ]]; then
                file "$binary"
                emit_binary_symbolization_fields "$binary"
            else
                echo "local_binary_status=missing"
                echo "build_id=unknown"
                echo "debug_info=unknown (exact executed binary not present locally)"
                echo "stripped=unknown"
                echo "addr2line_ready=false"
                echo "binary_sha256=unknown"
                echo "symbolization_ready=false"
            fi
            echo
        } >> "$output_file"
    done
}

validate_symbol_metadata() {
    local output_file="${OUT_DIR}/symbol_metadata.txt"
    local prefixes=(
        pane_profile_harness
        layout_bench
        pane_terminal_bench
        pane_pointer_bench
    )

    local field
    for prefix in "${prefixes[@]}"; do
        for field in executed_path binary_source exact_binary_status \
            build_id debug_info stripped addr2line_ready symbolization_ready; do
            if ! grep -A20 -F "== ${prefix} ==" "$output_file" | grep -q "^${field}="; then
                echo "ERROR: symbol metadata missing ${field} for ${prefix}" >&2
                exit 1
            fi
        done
    done
}

run_core_harness() {
    local output_file="${OUT_DIR}/pane_core_profile_harness.txt"
    local harness_dir="${OUT_DIR}/pane_core_profile_harness"
    local harness_args=(
        bench -p ftui-layout --bench pane_profile_harness --
        --out-dir "${harness_dir}"
    )

    if [[ "$TEST_MODE" == "true" ]]; then
        harness_args+=(--iterations 64 --warmup-iterations 8)
    else
        harness_args+=(--iterations 2000 --warmup-iterations 200)
    fi

    echo "==> pane_core_profile_harness"
    "${CARGO_RUNNER[@]}" "${harness_args[@]}" 2>&1 | tee "$output_file"

    record_executed_binary_metadata "pane_profile_harness" "$output_file"

    mkdir -p "$harness_dir"
    python3 - "$output_file" "$harness_dir" <<'PY'
import json
import pathlib
import sys

output_path = pathlib.Path(sys.argv[1])
harness_dir = pathlib.Path(sys.argv[2])
prefix_map = {
    "HARNESS_MANIFEST_JSON=": "manifest.json",
    "HARNESS_BASELINE_SNAPSHOT_JSON=": "baseline_snapshot.json",
    "HARNESS_FINAL_SNAPSHOT_JSON=": "final_snapshot.json",
    "HARNESS_RUN_LOG_JSON=": "run.log",
}

lines = output_path.read_text().splitlines()
payloads = {}
for line in lines:
    for prefix, filename in prefix_map.items():
        if line.startswith(prefix):
            payloads[filename] = json.loads(line[len(prefix):])
            break

missing = [filename for filename in prefix_map.values() if filename not in payloads]
if missing:
    raise SystemExit(f"missing harness payloads: {', '.join(missing)}")

for filename, payload in payloads.items():
    path = harness_dir / filename
    if filename == "run.log":
        path.write_text("".join(f"{entry}\n" for entry in payload))
    else:
        path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
PY
}

run_core_harness

run_bench \
    layout_bench \
    bench -p ftui-layout --bench layout_bench -- pane/core/ "${bench_args[@]}"

run_bench \
    pane_terminal_bench \
    bench -p ftui-runtime --bench pane_terminal_bench -- pane/terminal/ "${bench_args[@]}"

run_bench \
    pane_pointer_bench \
    bench -p ftui-web --bench pane_pointer_bench -- pane/web_pointer/ "${bench_args[@]}"

if [[ "$PERF_STAT" == "true" ]]; then
    run_perf_stat \
        layout_bench \
        layout_bench \
        "pane/core/timeline/apply_and_replay_32_ops"

    run_perf_stat \
        pane_terminal_bench \
        pane_terminal_bench \
        "pane/terminal/lifecycle/down_drag_120_up"

    run_perf_stat \
        pane_pointer_bench \
        pane_pointer_bench \
        "pane/web_pointer/lifecycle/down_ack_move_120_up"
fi

if [[ "$STACK_REPORTS" == "true" ]]; then
    run_stack_report \
        pane_core_timeline_apply_and_replay_32_ops \
        layout_bench \
        "pane/core/timeline/apply_and_replay_32_ops"

    run_stack_report \
        pane_terminal_down_drag_120_up \
        pane_terminal_bench \
        "pane/terminal/lifecycle/down_drag_120_up"

    run_stack_report \
        pane_pointer_down_ack_move_120_up \
        pane_pointer_bench \
        "pane/web_pointer/lifecycle/down_ack_move_120_up"

    validate_stack_reports
fi

record_symbol_metadata
validate_symbol_metadata

# Symbolized replay artifact index (bd-1pvzq.1): tie the harness replay manifest
# (deterministic state hashes + diagnostics) to the per-binary symbolization
# provenance under one checksummed, schema-versioned index, then self-validate
# the contract. The self-check is lenient (structure + replay + checksums) so
# rch local runs that could not fetch the exact remote binary still pass; CI runs
# the validator separately with --require-symbolization for the strict gate.
emit_and_validate_replay_index() {
    local emit_args=(emit --out-dir "$OUT_DIR")
    if [[ "$TEST_MODE" == "true" ]]; then emit_args+=(--test-mode); fi
    if [[ "$PERF_STAT" == "true" ]]; then emit_args+=(--perf-stat); fi
    if [[ "$STACK_REPORTS" == "true" ]]; then emit_args+=(--stack-reports); fi
    if command -v rch >/dev/null 2>&1; then
        emit_args+=(--runner rch)
    else
        emit_args+=(--runner local)
    fi

    python3 "${SCRIPT_DIR}/pane_replay_artifacts.py" "${emit_args[@]}"
    python3 "${SCRIPT_DIR}/pane_replay_artifacts.py" validate \
        --index "${OUT_DIR}/replay_artifact_index.json"

    # Golden replay-oracle gate (bd-1pvzq.3): compare the run's deterministic
    # replay state-hashes against the committed golden oracle so a behavior
    # regression (semantic drift) cannot pass silently as just a timing number.
    # The cross-strategy differential proof (the pane_determinism_matrix test)
    # is supplied separately by CI; locally the matrix result is "unknown", so
    # the golden-oracle half still gates while the matrix half is recorded.
    local golden="${SCRIPT_DIR}/pane_replay_golden.json"
    if [[ -f "$golden" ]]; then
        python3 "${SCRIPT_DIR}/pane_replay_artifacts.py" certify \
            --index "${OUT_DIR}/replay_artifact_index.json" \
            --golden "$golden" \
            --require-match
    fi
}

emit_and_validate_replay_index

cat > "${OUT_DIR}/README.txt" <<EOF
Pane profiling artifacts for bd-1y0ph (replay-artifact contract: bd-1pvzq.1).

Files:
- replay_artifact_index.json checksummed index tying replay evidence to
                            symbolization provenance (validate with
                            scripts/pane_replay_artifacts.py)
- differential_certification.json golden replay-oracle + differential proof
                            verdict (bd-1pvzq.3): certified | semantic_drift |
                            differential_matrix_failed | scenario_not_in_golden
- pane_core_profile_harness.txt  long-lived pane-core harness output
- pane_core_profile_harness/     manifest, snapshots, and verbose log (replay evidence)
- layout_bench.txt          pane/core/* Criterion output
- pane_terminal_bench.txt   pane/terminal/* Criterion output
- pane_pointer_bench.txt    pane/web_pointer/* Criterion output
- *.time.txt                optional /usr/bin/time -v resource summaries
- *.perfstat.txt            optional perf stat counter summaries
- *.perf.data / *.perf.txt  optional perf record/report stack artifacts
- *.symbols.txt             post-symbolized user-space top-frame summaries
- executed-binaries/        fetched exact remote bench binaries when materialized locally
- symbol_metadata.txt       executed-path provenance + build-id/debug-info/addr2line readiness

Validate the contract:
- python3 scripts/pane_replay_artifacts.py validate \\
      --index ${OUT_DIR}/replay_artifact_index.json [--require-symbolization]

Runner:
- ${0##*/}

Mode:
- TEST_MODE=${TEST_MODE}
- RESOURCE_STATS=${RESOURCE_STATS}
- PERF_STAT=${PERF_STAT}
- STACK_REPORTS=${STACK_REPORTS}
EOF
