#!/bin/bash
set -euo pipefail

LOG_LEVEL="${LOG_LEVEL:-INFO}"
E2E_LOG_DIR="${E2E_LOG_DIR:-/tmp/ftui_e2e_logs}"
E2E_RESULTS_DIR="${E2E_RESULTS_DIR:-/tmp/ftui_e2e_results}"
LOG_FILE="${LOG_FILE:-$E2E_LOG_DIR/e2e.log}"
E2E_JSONL_FILE="${E2E_JSONL_FILE:-$E2E_LOG_DIR/e2e.jsonl}"
E2E_JSONL_DISABLE="${E2E_JSONL_DISABLE:-0}"
E2E_JSONL_SCHEMA_VERSION="${E2E_JSONL_SCHEMA_VERSION:-e2e-jsonl-v1}"
E2E_JSONL_VALIDATE="${E2E_JSONL_VALIDATE:-}"
E2E_JSONL_VALIDATE_MODE="${E2E_JSONL_VALIDATE_MODE:-}"
E2E_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_JSONL_SCHEMA_FILE="${E2E_JSONL_SCHEMA_FILE:-$E2E_LIB_DIR/e2e_jsonl_schema.json}"
E2E_JSONL_VALIDATOR="${E2E_JSONL_VALIDATOR:-$E2E_LIB_DIR/validate_jsonl.py}"
E2E_JSONL_REGISTRY_FILE="${E2E_JSONL_REGISTRY_FILE:-$E2E_LIB_DIR/e2e_hash_registry.json}"
E2E_DETERMINISTIC="${E2E_DETERMINISTIC:-1}"
E2E_SEED="${E2E_SEED:-0}"
E2E_TIME_STEP_MS="${E2E_TIME_STEP_MS:-100}"

e2e_is_deterministic() {
    [[ "${E2E_DETERMINISTIC:-1}" == "1" ]]
}

e2e_state_dir() {
    local dir="${E2E_STATE_DIR:-$E2E_LOG_DIR}"
    if [[ -z "$dir" ]]; then
        dir="/tmp/ftui_e2e_state"
    fi
    mkdir -p "$dir"
    printf '%s' "$dir"
}

e2e_counter_file() {
    local name="$1"
    local dir
    dir="$(e2e_state_dir)"
    printf '%s/.e2e_%s_counter' "$dir" "$name"
}

e2e_counter_read() {
    local name="$1"
    local env_var="${2:-}"
    local default="${3:-0}"
    local file value
    file="$(e2e_counter_file "$name")"
    value=""
    if [[ -f "$file" ]]; then
        value="$(cat "$file" 2>/dev/null || true)"
    fi
    if [[ -z "$value" && -n "$env_var" && -n "${!env_var:-}" ]]; then
        value="${!env_var}"
    fi
    if [[ -z "$value" || ! "$value" =~ ^[0-9]+$ ]]; then
        value="$default"
    fi
    printf '%s' "$value"
}

e2e_counter_set() {
    local name="$1"
    local value="$2"
    local env_var="${3:-}"
    local file
    file="$(e2e_counter_file "$name")"
    printf '%s' "$value" > "$file"
    if [[ -n "$env_var" ]]; then
        export "$env_var"="$value"
    fi
}

e2e_counter_next() {
    local name="$1"
    local step="${2:-1}"
    local env_var="${3:-}"
    local default="${4:-0}"
    local value
    value="$(e2e_counter_read "$name" "$env_var" "$default")"
    if [[ -z "$step" || ! "$step" =~ ^[0-9]+$ ]]; then
        step=1
    fi
    value=$((value + step))
    e2e_counter_set "$name" "$value" "$env_var"
    printf '%s' "$value"
}

e2e_timestamp() {
    if e2e_is_deterministic; then
        local seq
        seq="$(e2e_counter_next "ts" 1 "E2E_TS_COUNTER" 0)"
        printf 'T%06d' "$seq"
        return 0
    fi
    date -Iseconds
}

e2e_run_id() {
    if [[ -n "${E2E_RUN_ID:-}" ]]; then
        printf '%s' "$E2E_RUN_ID"
        return 0
    fi
    if e2e_is_deterministic; then
        local seed="${E2E_SEED:-0}"
        local seq
        seq="$(e2e_counter_next "run_seq" 1 "E2E_RUN_SEQ" 0)"
        printf 'det_%s_%s' "$seed" "$seq"
        return 0
    fi
    printf 'run_%s_%s' "$(date +%Y%m%d_%H%M%S)" "$$"
}

e2e_determinism_self_test() {
    local had_det="${E2E_DETERMINISTIC+x}"
    local had_seed="${E2E_SEED+x}"
    local had_run_id="${E2E_RUN_ID+x}"
    local had_run_seq="${E2E_RUN_SEQ+x}"
    local had_ts="${E2E_TS_COUNTER+x}"
    local had_ms="${E2E_MS_COUNTER+x}"
    local prev_det="${E2E_DETERMINISTIC:-}"
    local prev_seed="${E2E_SEED:-}"
    local prev_run_id="${E2E_RUN_ID:-}"
    local prev_run_seq="${E2E_RUN_SEQ:-}"
    local prev_ts="${E2E_TS_COUNTER:-}"
    local prev_ms="${E2E_MS_COUNTER:-}"
    local prev_run_seq_file prev_ts_file prev_ms_file
    prev_run_seq_file="$(e2e_counter_read "run_seq" "E2E_RUN_SEQ" 0)"
    prev_ts_file="$(e2e_counter_read "ts" "E2E_TS_COUNTER" 0)"
    prev_ms_file="$(e2e_counter_read "ms" "E2E_MS_COUNTER" 0)"

    export E2E_DETERMINISTIC="1"
    export E2E_SEED="${E2E_SEED:-0}"
    unset E2E_RUN_ID
    export E2E_RUN_SEQ="0"
    export E2E_TS_COUNTER="0"
    export E2E_MS_COUNTER="0"
    e2e_counter_set "run_seq" 0 "E2E_RUN_SEQ"
    e2e_counter_set "ts" 0 "E2E_TS_COUNTER"
    e2e_counter_set "ms" 0 "E2E_MS_COUNTER"

    local run1 run2 ts1 ts2 ms1 ms2 step
    run1="$(e2e_run_id)"
    run2="$(e2e_run_id)"
    ts1="$(e2e_timestamp)"
    ts2="$(e2e_timestamp)"
    ms1="$(e2e_now_ms)"
    ms2="$(e2e_now_ms)"

    step="${E2E_TIME_STEP_MS:-100}"
    local status=0
    if [[ "$run1" == "$run2" ]]; then
        echo "E2E determinism self-test failed: run_id did not advance ($run1)" >&2
        status=1
    fi
    if [[ "$ts1" != "T000001" || "$ts2" != "T000002" ]]; then
        echo "E2E determinism self-test failed: timestamp did not advance ($ts1/$ts2)" >&2
        status=1
    fi
    if [[ "$ms1" != "$step" || "$ms2" != "$((step * 2))" ]]; then
        echo "E2E determinism self-test failed: ms counter not step-aligned ($ms1/$ms2, step=$step)" >&2
        status=1
    fi

    if [[ -n "$had_det" ]]; then export E2E_DETERMINISTIC="$prev_det"; else unset E2E_DETERMINISTIC; fi
    if [[ -n "$had_seed" ]]; then export E2E_SEED="$prev_seed"; else unset E2E_SEED; fi
    if [[ -n "$had_run_id" ]]; then export E2E_RUN_ID="$prev_run_id"; else unset E2E_RUN_ID; fi
    if [[ -n "$had_run_seq" ]]; then
        export E2E_RUN_SEQ="$prev_run_seq"
        e2e_counter_set "run_seq" "$prev_run_seq_file" "E2E_RUN_SEQ"
    else
        unset E2E_RUN_SEQ
        e2e_counter_set "run_seq" "$prev_run_seq_file"
    fi
    if [[ -n "$had_ts" ]]; then
        export E2E_TS_COUNTER="$prev_ts"
        e2e_counter_set "ts" "$prev_ts_file" "E2E_TS_COUNTER"
    else
        unset E2E_TS_COUNTER
        e2e_counter_set "ts" "$prev_ts_file"
    fi
    if [[ -n "$had_ms" ]]; then
        export E2E_MS_COUNTER="$prev_ms"
        e2e_counter_set "ms" "$prev_ms_file" "E2E_MS_COUNTER"
    else
        unset E2E_MS_COUNTER
        e2e_counter_set "ms" "$prev_ms_file"
    fi

    return $status
}

e2e_run_start_ms() {
    if e2e_is_deterministic; then
        printf '0'
        return 0
    fi
    date +%s%3N
}

e2e_now_ms() {
    if e2e_is_deterministic; then
        local step="${E2E_TIME_STEP_MS:-100}"
        local seq
        seq="$(e2e_counter_next "ms" "$step" "E2E_MS_COUNTER" 0)"
        printf '%s' "$seq"
        return 0
    fi
    date +%s%3N
}

e2e_log_stamp() {
    if e2e_is_deterministic; then
        local seed="${E2E_SEED:-0}"
        printf 'det_%s' "$seed"
        return 0
    fi
    date +%Y%m%d_%H%M%S
}

e2e_hash_key() {
    local mode="$1"
    local cols="$2"
    local rows="$3"
    local seed="${4:-${E2E_SEED:-0}}"
    printf '%s-%sx%s-seed%s' "$mode" "$cols" "$rows" "$seed"
}

json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

jsonl_emit() {
    local json="$1"
    if [[ "$E2E_JSONL_DISABLE" == "1" ]]; then
        return 0
    fi
    mkdir -p "$(dirname "$E2E_JSONL_FILE")"
    echo "$json" >> "$E2E_JSONL_FILE"
}

jsonl_next_event_seq() {
    e2e_counter_next "event_seq" 1 "E2E_EVENT_SEQ" 0 >/dev/null
    printf '%s' "${E2E_EVENT_SEQ:-0}"
}

jsonl_trace_id() {
    local prefix="${1:-trace}"
    local normalized="${prefix//[^a-zA-Z0-9._-]/-}"
    local seq
    seq="$(jsonl_next_event_seq)"
    printf '%s-%s-%06d' "${E2E_RUN_ID:-run}" "$normalized" "$seq"
}

jsonl_pane_trace() {
    local trace_id="$1"
    local host="$2"
    local pane_tree_hash="$3"
    local focus_state_hash="$4"
    local splitter_state_hash="$5"
    local duration_ms="$6"
    local status="$7"
    local details="${8:-}"
    local failure_code="${9:-}"
    jsonl_init

    local ts
    ts="$(e2e_timestamp)"
    local seq
    seq="$(jsonl_next_event_seq)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then
        seed_json="${E2E_SEED}"
    fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "pane_trace" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg trace_id "$trace_id" \
            --arg host "$host" \
            --arg pane_tree_hash "$pane_tree_hash" \
            --arg focus_state_hash "$focus_state_hash" \
            --arg splitter_state_hash "$splitter_state_hash" \
            --arg status "$status" \
            --arg details "$details" \
            --arg failure_code "$failure_code" \
            --argjson duration_ms "$duration_ms" \
            --argjson event_seq "$seq" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,event_seq:$event_seq,trace_id:$trace_id,host:$host,pane_tree_hash:$pane_tree_hash,focus_state_hash:$focus_state_hash,splitter_state_hash:$splitter_state_hash,duration_ms:$duration_ms,status:$status,details:$details}
             + (if $failure_code != "" then {failure_code:$failure_code} else {} end)')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"pane_trace\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"event_seq\":${seq},\"trace_id\":\"$(json_escape "$trace_id")\",\"host\":\"$(json_escape "$host")\",\"pane_tree_hash\":\"$(json_escape "$pane_tree_hash")\",\"focus_state_hash\":\"$(json_escape "$focus_state_hash")\",\"splitter_state_hash\":\"$(json_escape "$splitter_state_hash")\",\"duration_ms\":${duration_ms},\"status\":\"$(json_escape "$status")\",\"details\":\"$(json_escape "$details")\"}"
    fi
}

jsonl_pane_write_manifest() {
    local manifest_path="$1"
    local trace_id="$2"
    local bundle_id="$3"
    local status="$4"
    local notes="${5:-}"
    local replay_trace_path="$6"
    local jsonl_path="$7"
    local snapshots_dir="$8"
    local event_seq="$9"

    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then
        seed_json="${E2E_SEED}"
    fi

    mkdir -p "$(dirname "$manifest_path")"

    if command -v jq >/dev/null 2>&1; then
        jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg generated_at "$(e2e_timestamp)" \
            --arg run_id "$E2E_RUN_ID" \
            --arg trace_id "$trace_id" \
            --arg bundle_id "$bundle_id" \
            --arg status "$status" \
            --arg notes "$notes" \
            --arg replay_trace "$replay_trace_path" \
            --arg jsonl "$jsonl_path" \
            --arg snapshots_dir "$snapshots_dir" \
            --argjson event_seq "$event_seq" \
            --argjson seed "$seed_json" \
            '{
                schema_version:$schema_version,
                generated_at:$generated_at,
                run_id:$run_id,
                seed:$seed,
                event_seq:$event_seq,
                trace_id:$trace_id,
                bundle_id:$bundle_id,
                status:$status,
                notes:$notes,
                artifacts:{
                    replay_trace:$replay_trace,
                    jsonl:$jsonl,
                    snapshots_dir:$snapshots_dir
                }
            }' > "$manifest_path"
    else
        cat > "$manifest_path" <<EOF_MANIFEST
{"schema_version":"${E2E_JSONL_SCHEMA_VERSION}","generated_at":"$(e2e_timestamp)","run_id":"$(json_escape "$E2E_RUN_ID")","seed":${seed_json},"event_seq":${event_seq},"trace_id":"$(json_escape "$trace_id")","bundle_id":"$(json_escape "$bundle_id")","status":"$(json_escape "$status")","notes":"$(json_escape "$notes")","artifacts":{"replay_trace":"$(json_escape "$replay_trace_path")","jsonl":"$(json_escape "$jsonl_path")","snapshots_dir":"$(json_escape "$snapshots_dir")"}}
EOF_MANIFEST
    fi
}

jsonl_pane_artifact_bundle() {
    local trace_id="$1"
    local bundle_id="$2"
    local manifest_path="$3"
    local jsonl_path="$4"
    local replay_trace_path="$5"
    local snapshots_dir="$6"
    local status="$7"
    local notes="${8:-}"
    jsonl_init

    local ts
    ts="$(e2e_timestamp)"
    local seq
    seq="$(jsonl_next_event_seq)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then
        seed_json="${E2E_SEED}"
    fi

    jsonl_pane_write_manifest \
        "$manifest_path" \
        "$trace_id" \
        "$bundle_id" \
        "$status" \
        "$notes" \
        "$replay_trace_path" \
        "$jsonl_path" \
        "$snapshots_dir" \
        "$seq"

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "pane_artifact_bundle" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg trace_id "$trace_id" \
            --arg bundle_id "$bundle_id" \
            --arg manifest_path "$manifest_path" \
            --arg jsonl_path "$jsonl_path" \
            --arg replay_trace_path "$replay_trace_path" \
            --arg snapshots_dir "$snapshots_dir" \
            --arg status "$status" \
            --arg notes "$notes" \
            --argjson event_seq "$seq" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,event_seq:$event_seq,trace_id:$trace_id,bundle_id:$bundle_id,manifest_path:$manifest_path,jsonl_path:$jsonl_path,replay_trace_path:$replay_trace_path,snapshots_dir:$snapshots_dir,status:$status,notes:$notes}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"pane_artifact_bundle\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"event_seq\":${seq},\"trace_id\":\"$(json_escape "$trace_id")\",\"bundle_id\":\"$(json_escape "$bundle_id")\",\"manifest_path\":\"$(json_escape "$manifest_path")\",\"jsonl_path\":\"$(json_escape "$jsonl_path")\",\"replay_trace_path\":\"$(json_escape "$replay_trace_path")\",\"snapshots_dir\":\"$(json_escape "$snapshots_dir")\",\"status\":\"$(json_escape "$status")\",\"notes\":\"$(json_escape "$notes")\"}"
    fi
}

jsonl_should_validate() {
    if [[ "${E2E_JSONL_VALIDATE:-}" == "1" ]]; then
        return 0
    fi
    if [[ "${E2E_JSONL_VALIDATE_MODE:-}" == "strict" || "${E2E_JSONL_VALIDATE_MODE:-}" == "warn" ]]; then
        return 0
    fi
    if [[ -n "${CI:-}" ]]; then
        return 0
    fi
    return 1
}

jsonl_validate_line() {
    local line="$1"
    local type
    type="$(jq -r '.type // .event // empty' <<<"$line" 2>/dev/null || true)"
    if [[ -z "$type" ]]; then
        return 1
    fi
    local ts
    ts="$(jq -r '.timestamp // .ts // empty' <<<"$line" 2>/dev/null || true)"
    if [[ -z "$ts" ]]; then
        return 1
    fi
    local run_id
    run_id="$(jq -r '.run_id // empty' <<<"$line" 2>/dev/null || true)"
    if [[ -z "$run_id" ]]; then
        return 1
    fi

    if ! jq -e 'has("schema_version")' >/dev/null <<<"$line"; then
        return 1
    fi

    case "$type" in
        env)
            jq -e 'has("seed") and has("deterministic") and has("term") and has("colorterm") and has("no_color")' >/dev/null <<<"$line"
            ;;
        browser_env)
            jq -e 'has("seed") and has("browser") and has("user_agent") and has("dpr")' >/dev/null <<<"$line"
            ;;
        gpu_adapter)
            jq -e 'has("seed") and has("api") and has("adapter_name")' >/dev/null <<<"$line"
            ;;
        ws_metrics)
            jq -e 'has("seed") and has("label") and has("ws_url") and has("bytes_tx") and has("bytes_rx") and has("messages_tx") and has("messages_rx")' >/dev/null <<<"$line"
            ;;
        run_start)
            jq -e 'has("seed") and has("command") and has("log_dir") and has("results_dir")' >/dev/null <<<"$line"
            ;;
        run_end)
            jq -e 'has("seed") and has("status") and has("duration_ms") and has("failed_count")' >/dev/null <<<"$line"
            ;;
        step_start)
            jq -e 'has("step") and has("mode") and has("cols") and has("rows") and has("seed")' >/dev/null <<<"$line"
            ;;
        step_end)
            jq -e 'has("step") and has("status") and has("duration_ms") and has("mode") and has("cols") and has("rows") and has("seed")' >/dev/null <<<"$line"
            ;;
        pane_trace)
            jq -e 'has("event_seq") and has("trace_id") and has("host") and has("pane_tree_hash") and has("focus_state_hash") and has("splitter_state_hash") and has("duration_ms") and has("status")' >/dev/null <<<"$line"
            ;;
        pane_artifact_bundle)
            jq -e 'has("event_seq") and has("trace_id") and has("bundle_id") and has("manifest_path") and has("jsonl_path") and has("replay_trace_path") and has("snapshots_dir") and has("status")' >/dev/null <<<"$line"
            ;;
        input)
            jq -e 'has("seed") and has("input_type") and has("encoding") and has("input_hash")' >/dev/null <<<"$line"
            ;;
        frame)
            jq -e 'has("seed") and has("frame_idx") and has("hash_algo") and has("frame_hash")' >/dev/null <<<"$line"
            ;;
        pty_capture)
            jq -e 'has("seed") and has("output_sha256") and has("output_bytes") and has("cols") and has("rows") and has("exit_code")' >/dev/null <<<"$line"
            ;;
        assert)
            jq -e 'has("seed") and has("assertion") and has("status")' >/dev/null <<<"$line"
            ;;
        error)
            jq -e 'has("seed") and has("message")' >/dev/null <<<"$line"
            ;;
        *)
            return 0
            ;;
    esac
}

jsonl_validate_file() {
    local jsonl_file="$1"
    local mode="${2:-}"
    if [[ ! -f "$jsonl_file" ]]; then
        return 0
    fi
    if ! command -v jq >/dev/null 2>&1; then
        if [[ "$mode" == "strict" ]] || jsonl_should_validate; then
            echo "WARN: jq not available; skipping JSONL validation for $jsonl_file" >&2
        fi
        return 0
    fi
    local line_no=0
    while IFS= read -r line || [[ -n "$line" ]]; do
        line_no=$((line_no + 1))
        if [[ -z "$line" ]]; then
            continue
        fi
        if ! jsonl_validate_line "$line"; then
            echo "JSONL schema violation at line $line_no: $line" >&2
            if [[ "$mode" == "strict" ]]; then
                return 1
            fi
            if [[ -z "$mode" ]] && jsonl_should_validate; then
                return 1
            fi
        fi
    done < "$jsonl_file"
    return 0
}

jsonl_validate_current() {
    if [[ "$E2E_JSONL_DISABLE" == "1" ]]; then
        return 0
    fi
    if [[ ! -f "$E2E_JSONL_FILE" ]]; then
        return 0
    fi

    local mode="${E2E_JSONL_VALIDATE_MODE:-}"
    if [[ -z "$mode" ]]; then
        if [[ -n "${CI:-}" || "${E2E_JSONL_VALIDATE:-}" == "1" ]]; then
            mode="strict"
        else
            mode="warn"
        fi
    fi

    if [[ -n "${E2E_PYTHON:-}" && -f "$E2E_JSONL_VALIDATOR" && -f "$E2E_JSONL_SCHEMA_FILE" ]]; then
        local flag="--warn"
        if [[ "$mode" == "strict" ]]; then
            flag="--strict"
        fi
        local registry_args=()
        if [[ -n "${E2E_JSONL_REGISTRY_FILE:-}" && -f "$E2E_JSONL_REGISTRY_FILE" ]]; then
            registry_args=(--registry "$E2E_JSONL_REGISTRY_FILE")
        fi
        if ! "$E2E_PYTHON" "$E2E_JSONL_VALIDATOR" "$E2E_JSONL_FILE" --schema "$E2E_JSONL_SCHEMA_FILE" "${registry_args[@]}" "$flag"; then
            log_error "JSONL schema validation failed for $E2E_JSONL_FILE"
            return 1
        fi
        return 0
    fi

    if [[ "$mode" == "strict" || "$mode" == "warn" ]]; then
        jsonl_validate_file "$E2E_JSONL_FILE" "$mode"
        return $?
    fi

    if jsonl_should_validate; then
        jsonl_validate_file "$E2E_JSONL_FILE"
    fi
}

jsonl_init() {
    if [[ "${E2E_JSONL_INIT:-}" == "1" ]]; then
        return 0
    fi
    export E2E_JSONL_INIT=1
    e2e_seed >/dev/null
    export E2E_RUN_ID="${E2E_RUN_ID:-$(e2e_run_id)}"
    export E2E_RUN_START_MS="${E2E_RUN_START_MS:-$(e2e_run_start_ms)}"
    jsonl_env
    jsonl_run_start "${E2E_RUN_CMD:-}"
    jsonl_assert "artifact_log_dir" "pass" "log_dir=$E2E_LOG_DIR"
}

jsonl_env() {
    local ts host rustc cargo git_commit git_dirty
    ts="$(e2e_timestamp)"
    host="$(hostname 2>/dev/null || echo unknown)"
    rustc="$(rustc --version 2>/dev/null || echo unknown)"
    cargo="$(cargo --version 2>/dev/null || echo unknown)"
    git_commit="$(git rev-parse HEAD 2>/dev/null || echo "")"
    if git diff --quiet --ignore-submodules -- 2>/dev/null; then
        git_dirty="false"
    else
        git_dirty="true"
    fi

    local seed_json="null"
    local deterministic_json="false"
    if e2e_is_deterministic; then deterministic_json="true"; fi
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "env" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg host "$host" \
            --arg rustc "$rustc" \
            --arg cargo "$cargo" \
            --arg git_commit "$git_commit" \
            --argjson git_dirty "$git_dirty" \
            --argjson seed "$seed_json" \
            --argjson deterministic "$deterministic_json" \
            --arg term "${TERM:-}" \
            --arg colorterm "${COLORTERM:-}" \
            --arg no_color "${NO_COLOR:-}" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,host:$host,rustc:$rustc,cargo:$cargo,git_commit:$git_commit,git_dirty:$git_dirty,seed:$seed,deterministic:$deterministic,term:$term,colorterm:$colorterm,no_color:$no_color}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"env\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"host\":\"$(json_escape "$host")\",\"rustc\":\"$(json_escape "$rustc")\",\"cargo\":\"$(json_escape "$cargo")\",\"git_commit\":\"$(json_escape "$git_commit")\",\"git_dirty\":${git_dirty},\"seed\":${seed_json},\"deterministic\":${deterministic_json},\"term\":\"$(json_escape "${TERM:-}")\",\"colorterm\":\"$(json_escape "${COLORTERM:-}")\",\"no_color\":\"$(json_escape "${NO_COLOR:-}")\"}"
    fi
}

jsonl_run_start() {
    local cmd="$1"
    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "run_start" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg command "$cmd" \
            --arg log_dir "$E2E_LOG_DIR" \
            --arg results_dir "$E2E_RESULTS_DIR" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,command:$command,log_dir:$log_dir,results_dir:$results_dir}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"run_start\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"command\":\"$(json_escape "$cmd")\",\"log_dir\":\"$(json_escape "$E2E_LOG_DIR")\",\"results_dir\":\"$(json_escape "$E2E_RESULTS_DIR")\"}"
    fi
}

jsonl_run_end() {
    local status="$1"
    local duration_ms="$2"
    local failed_count="$3"
    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "run_end" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg status "$status" \
            --argjson seed "$seed_json" \
            --argjson duration_ms "$duration_ms" \
            --argjson failed_count "$failed_count" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,status:$status,duration_ms:$duration_ms,failed_count:$failed_count}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"run_end\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"status\":\"$(json_escape "$status")\",\"duration_ms\":${duration_ms},\"failed_count\":${failed_count}}"
    fi
    jsonl_validate_current
}

jsonl_set_context() {
    export E2E_CONTEXT_MODE="${1:-${E2E_CONTEXT_MODE:-}}"
    export E2E_CONTEXT_COLS="${2:-${E2E_CONTEXT_COLS:-}}"
    export E2E_CONTEXT_ROWS="${3:-${E2E_CONTEXT_ROWS:-}}"
    export E2E_CONTEXT_SEED="${4:-${E2E_CONTEXT_SEED:-}}"
}

e2e_seed() {
    local seed="${E2E_SEED:-0}"
    export E2E_SEED="$seed"
    if e2e_is_deterministic; then
        if [[ -z "${FTUI_TEST_DETERMINISTIC:-}" ]]; then
            export FTUI_TEST_DETERMINISTIC="1"
        fi
        if [[ -z "${FTUI_SEED:-}" ]]; then
            export FTUI_SEED="$seed"
        fi
        if [[ -z "${FTUI_HARNESS_SEED:-}" ]]; then
            export FTUI_HARNESS_SEED="$seed"
        fi
        if [[ -z "${FTUI_DEMO_SEED:-}" ]]; then
            export FTUI_DEMO_SEED="$seed"
        fi
        if [[ -z "${FTUI_TEST_SEED:-}" ]]; then
            export FTUI_TEST_SEED="$seed"
        fi
        if [[ -z "${FTUI_DEMO_DETERMINISTIC:-}" ]]; then
            export FTUI_DEMO_DETERMINISTIC="1"
        fi
        if [[ -n "${E2E_TIME_STEP_MS:-}" && -z "${FTUI_TEST_TIME_STEP_MS:-}" ]]; then
            export FTUI_TEST_TIME_STEP_MS="$E2E_TIME_STEP_MS"
        fi
    fi
    if [[ -z "${E2E_CONTEXT_SEED:-}" ]]; then
        export E2E_CONTEXT_SEED="$seed"
    fi
    if [[ -z "${STORM_SEED:-}" ]]; then
        export STORM_SEED="$seed"
    fi
    printf '%s' "$seed"
}

if [[ "${E2E_AUTO_SEED:-1}" == "1" ]]; then
    e2e_seed >/dev/null 2>&1 || true
fi

jsonl_step_start() {
    local step="$1"
    local ts
    ts="$(e2e_timestamp)"
    local mode="${E2E_CONTEXT_MODE:-}"
    local cols="${E2E_CONTEXT_COLS:-}"
    local rows="${E2E_CONTEXT_ROWS:-}"
    local seed="${E2E_CONTEXT_SEED:-}"
    local hash_key=""
    if [[ -n "$mode" && -n "$cols" && -n "$rows" ]]; then
        hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed")"
    fi
    local cols_json="null"
    local rows_json="null"
    local seed_json="null"
    if [[ -n "$cols" ]]; then cols_json="$cols"; fi
    if [[ -n "$rows" ]]; then rows_json="$rows"; fi
    if [[ -n "$seed" ]]; then seed_json="$seed"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "step_start" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg step "$step" \
            --arg mode "$mode" \
            --arg hash_key "$hash_key" \
            --argjson cols "$cols_json" \
            --argjson rows "$rows_json" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,step:$step,mode:$mode,hash_key:$hash_key,cols:$cols,rows:$rows,seed:$seed}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"step_start\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"step\":\"$(json_escape "$step")\",\"mode\":\"$(json_escape "$mode")\",\"hash_key\":\"$(json_escape "$hash_key")\",\"cols\":${cols_json},\"rows\":${rows_json},\"seed\":${seed_json}}"
    fi
}

jsonl_step_end() {
    local step="$1"
    local status="$2"
    local duration_ms="$3"
    local ts
    ts="$(e2e_timestamp)"
    local mode="${E2E_CONTEXT_MODE:-}"
    local cols="${E2E_CONTEXT_COLS:-}"
    local rows="${E2E_CONTEXT_ROWS:-}"
    local seed="${E2E_CONTEXT_SEED:-}"
    local hash_key=""
    if [[ -n "$mode" && -n "$cols" && -n "$rows" ]]; then
        hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed")"
    fi
    local cols_json="null"
    local rows_json="null"
    local seed_json="null"
    if [[ -n "$cols" ]]; then cols_json="$cols"; fi
    if [[ -n "$rows" ]]; then rows_json="$rows"; fi
    if [[ -n "$seed" ]]; then seed_json="$seed"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "step_end" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg step "$step" \
            --arg status "$status" \
            --argjson duration_ms "$duration_ms" \
            --arg mode "$mode" \
            --arg hash_key "$hash_key" \
            --argjson cols "$cols_json" \
            --argjson rows "$rows_json" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,step:$step,status:$status,duration_ms:$duration_ms,mode:$mode,hash_key:$hash_key,cols:$cols,rows:$rows,seed:$seed}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"step_end\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"step\":\"$(json_escape "$step")\",\"status\":\"$(json_escape "$status")\",\"duration_ms\":${duration_ms},\"mode\":\"$(json_escape "$mode")\",\"hash_key\":\"$(json_escape "$hash_key")\",\"cols\":${cols_json},\"rows\":${rows_json},\"seed\":${seed_json}}"
    fi
}

jsonl_case_step_start() {
    local case_name="$1"
    local step="$2"
    local action="$3"
    local details="${4:-}"
    local ts
    ts="$(e2e_timestamp)"
    local mode="${E2E_CONTEXT_MODE:-}"
    local cols="${E2E_CONTEXT_COLS:-}"
    local rows="${E2E_CONTEXT_ROWS:-}"
    local seed="${E2E_CONTEXT_SEED:-}"
    local hash_key=""
    if [[ -n "$mode" && -n "$cols" && -n "$rows" ]]; then
        hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed")"
    fi
    local cols_json="null"
    local rows_json="null"
    local seed_json="null"
    if [[ -n "$cols" ]]; then cols_json="$cols"; fi
    if [[ -n "$rows" ]]; then rows_json="$rows"; fi
    if [[ -n "$seed" ]]; then seed_json="$seed"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "case_step_start" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg case "$case_name" \
            --arg step "$step" \
            --arg action "$action" \
            --arg details "$details" \
            --arg mode "$mode" \
            --arg hash_key "$hash_key" \
            --argjson cols "$cols_json" \
            --argjson rows "$rows_json" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,case:$case,step:$step,action:$action,details:$details,mode:$mode,hash_key:$hash_key,cols:$cols,rows:$rows,seed:$seed}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"case_step_start\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"case\":\"$(json_escape "$case_name")\",\"step\":\"$(json_escape "$step")\",\"action\":\"$(json_escape "$action")\",\"details\":\"$(json_escape "$details")\",\"mode\":\"$(json_escape "$mode")\",\"hash_key\":\"$(json_escape "$hash_key")\",\"cols\":${cols_json},\"rows\":${rows_json},\"seed\":${seed_json}}"
    fi
}

jsonl_case_step_end() {
    local case_name="$1"
    local step="$2"
    local status="$3"
    local duration_ms="$4"
    local action="${5:-}"
    local details="${6:-}"
    local ts
    ts="$(e2e_timestamp)"
    local mode="${E2E_CONTEXT_MODE:-}"
    local cols="${E2E_CONTEXT_COLS:-}"
    local rows="${E2E_CONTEXT_ROWS:-}"
    local seed="${E2E_CONTEXT_SEED:-}"
    local hash_key=""
    if [[ -n "$mode" && -n "$cols" && -n "$rows" ]]; then
        hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed")"
    fi
    local cols_json="null"
    local rows_json="null"
    local seed_json="null"
    if [[ -n "$cols" ]]; then cols_json="$cols"; fi
    if [[ -n "$rows" ]]; then rows_json="$rows"; fi
    if [[ -n "$seed" ]]; then seed_json="$seed"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "case_step_end" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg case "$case_name" \
            --arg step "$step" \
            --arg status "$status" \
            --argjson duration_ms "$duration_ms" \
            --arg action "$action" \
            --arg details "$details" \
            --arg mode "$mode" \
            --arg hash_key "$hash_key" \
            --argjson cols "$cols_json" \
            --argjson rows "$rows_json" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,case:$case,step:$step,status:$status,duration_ms:$duration_ms,action:$action,details:$details,mode:$mode,hash_key:$hash_key,cols:$cols,rows:$rows,seed:$seed}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"case_step_end\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"case\":\"$(json_escape "$case_name")\",\"step\":\"$(json_escape "$step")\",\"status\":\"$(json_escape "$status")\",\"duration_ms\":${duration_ms},\"action\":\"$(json_escape "$action")\",\"details\":\"$(json_escape "$details")\",\"mode\":\"$(json_escape "$mode")\",\"hash_key\":\"$(json_escape "$hash_key")\",\"cols\":${cols_json},\"rows\":${rows_json},\"seed\":${seed_json}}"
    fi
}

jsonl_pty_capture() {
    local output_file="$1"
    local cols="$2"
    local rows="$3"
    local exit_code="$4"
    local canonical_file="${5:-}"
    jsonl_init
    local ts output_sha output_bytes canonical_sha canonical_bytes
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
    output_sha="$(sha256_file "$output_file")"
    output_bytes=$(wc -c < "$output_file" 2>/dev/null | tr -d ' ')
    canonical_sha=""
    canonical_bytes=0
    if [[ -n "$canonical_file" && -f "$canonical_file" ]]; then
        canonical_sha="$(sha256_file "$canonical_file")"
        canonical_bytes=$(wc -c < "$canonical_file" 2>/dev/null | tr -d ' ')
    fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "pty_capture" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg output_file "$output_file" \
            --arg canonical_file "$canonical_file" \
            --arg output_sha256 "$output_sha" \
            --arg canonical_sha256 "$canonical_sha" \
            --argjson output_bytes "${output_bytes:-0}" \
            --argjson canonical_bytes "${canonical_bytes:-0}" \
            --argjson cols "$cols" \
            --argjson rows "$rows" \
            --argjson exit_code "$exit_code" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,output_file:$output_file,canonical_file:$canonical_file,output_sha256:$output_sha256,canonical_sha256:$canonical_sha256,output_bytes:$output_bytes,canonical_bytes:$canonical_bytes,cols:$cols,rows:$rows,exit_code:$exit_code}')"
    else
        local seed_json="null"
        if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"pty_capture\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"output_file\":\"$(json_escape "$output_file")\",\"canonical_file\":\"$(json_escape "$canonical_file")\",\"output_sha256\":\"$(json_escape "$output_sha")\",\"canonical_sha256\":\"$(json_escape "$canonical_sha")\",\"output_bytes\":${output_bytes:-0},\"canonical_bytes\":${canonical_bytes:-0},\"cols\":${cols},\"rows\":${rows},\"exit_code\":${exit_code}}"
    fi
}

artifact_strict_mode() {
    local mode="${E2E_JSONL_VALIDATE_MODE:-}"
    if [[ -z "$mode" ]]; then
        if [[ -n "${CI:-}" || "${E2E_JSONL_VALIDATE:-}" == "1" ]]; then
            mode="strict"
        else
            mode="warn"
        fi
    fi
    [[ "$mode" == "strict" ]]
}

artifact_path_from_details() {
    local details="$1"
    if [[ -z "$details" ]]; then
        printf ''
        return 0
    fi
    if [[ "$details" == *"="* ]]; then
        local value="${details#*=}"
        printf '%s' "${value%% *}"
        return 0
    fi
    printf '%s' "${details%% *}"
}

jsonl_artifact() {
    local artifact_type="$1"
    local path="$2"
    local status="${3:-present}"
    local ts sha bytes
    ts="$(e2e_timestamp)"
    sha=""
    bytes=0
    if [[ -n "$path" && -e "$path" ]]; then
        if [[ -f "$path" ]]; then
            sha="$(sha256_file "$path" 2>/dev/null || true)"
            bytes=$(wc -c < "$path" 2>/dev/null | tr -d ' ')
        fi
    else
        status="missing"
    fi
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "artifact" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg artifact_type "$artifact_type" \
            --arg path "$path" \
            --arg status "$status" \
            --arg sha256 "$sha" \
            --argjson bytes "${bytes:-0}" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,artifact_type:$artifact_type,path:$path,status:$status,sha256:$sha256,bytes:$bytes}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"artifact\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"artifact_type\":\"$(json_escape "$artifact_type")\",\"path\":\"$(json_escape "$path")\",\"status\":\"$(json_escape "$status")\",\"sha256\":\"$(json_escape "$sha")\",\"bytes\":${bytes:-0}}"
    fi
}

jsonl_assert() {
    local name="$1"
    local status="$2"
    local details="${3:-}"
    local assert_status="$status"
    if [[ "$name" == artifact_* ]]; then
        local artifact_type="${name#artifact_}"
        local path
        path="$(artifact_path_from_details "$details")"
        if [[ -z "$path" || ! -e "$path" ]]; then
            assert_status="failed"
            jsonl_artifact "$artifact_type" "$path" "missing"
            if artifact_strict_mode; then
                log_error "Missing required artifact: ${artifact_type} (${path:-no path})"
                return 1
            fi
        else
            jsonl_artifact "$artifact_type" "$path" "present"
        fi
    fi
    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "assert" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg assertion "$name" \
            --arg status "$assert_status" \
            --arg details "$details" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,assertion:$assertion,status:$status,details:$details}')"
    else
        local seed_json="null"
        if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"assert\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"assertion\":\"$(json_escape "$name")\",\"status\":\"$(json_escape "$assert_status")\",\"details\":\"$(json_escape "$details")\"}"
    fi
}

jsonl_input() {
    local input_type="$1"
    local encoding="$2"
    local input_hash="$3"
    local bytes_b64="${4:-}"
    local details="${5:-}"
    jsonl_init

    local ts
    ts="$(e2e_timestamp)"
    local mode="${E2E_CONTEXT_MODE:-}"
    local cols="${E2E_CONTEXT_COLS:-}"
    local rows="${E2E_CONTEXT_ROWS:-}"
    local seed="${E2E_CONTEXT_SEED:-}"
    local hash_key=""
    if [[ -n "$mode" && -n "$cols" && -n "$rows" ]]; then
        hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed")"
    fi

    local cols_json="null"
    local rows_json="null"
    local seed_json="null"
    if [[ -n "$cols" ]]; then cols_json="$cols"; fi
    if [[ -n "$rows" ]]; then rows_json="$rows"; fi
    if [[ -n "$seed" ]]; then seed_json="$seed"; fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "input" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg input_type "$input_type" \
            --arg encoding "$encoding" \
            --arg bytes_b64 "$bytes_b64" \
            --arg input_hash "$input_hash" \
            --arg details "$details" \
            --arg mode "$mode" \
            --arg hash_key "$hash_key" \
            --argjson cols "$cols_json" \
            --argjson rows "$rows_json" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,input_type:$input_type,encoding:$encoding,bytes_b64:$bytes_b64,input_hash:$input_hash,details:$details,mode:$mode,hash_key:$hash_key,cols:$cols,rows:$rows}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"input\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"input_type\":\"$(json_escape "$input_type")\",\"encoding\":\"$(json_escape "$encoding")\",\"bytes_b64\":\"$(json_escape "$bytes_b64")\",\"input_hash\":\"$(json_escape "$input_hash")\",\"details\":\"$(json_escape "$details")\",\"mode\":\"$(json_escape "$mode")\",\"hash_key\":\"$(json_escape "$hash_key")\",\"cols\":${cols_json},\"rows\":${rows_json}}"
    fi
}

jsonl_frame() {
    local frame_idx="$1"
    local hash_algo="$2"
    local frame_hash="$3"
    local patch_hash="${4:-}"
    local patch_bytes="${5:-}"
    local patch_cells="${6:-}"
    local patch_runs="${7:-}"
    local render_ms="${8:-}"
    local present_ms="${9:-}"
    local present_bytes="${10:-}"
    local checksum_chain="${11:-}"
    local ts_ms="${12:-}"
    jsonl_init

    local ts
    ts="$(e2e_timestamp)"
    local mode="${E2E_CONTEXT_MODE:-}"
    local cols="${E2E_CONTEXT_COLS:-}"
    local rows="${E2E_CONTEXT_ROWS:-}"
    local seed="${E2E_CONTEXT_SEED:-}"
    local hash_key=""
    if [[ -n "$mode" && -n "$cols" && -n "$rows" ]]; then
        hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed")"
    fi

    local cols_json="null"
    local rows_json="null"
    local seed_json="null"
    if [[ -n "$cols" ]]; then cols_json="$cols"; fi
    if [[ -n "$rows" ]]; then rows_json="$rows"; fi
    if [[ -n "$seed" ]]; then seed_json="$seed"; fi

    local patch_bytes_json="null"
    local patch_cells_json="null"
    local patch_runs_json="null"
    local render_ms_json="null"
    local present_ms_json="null"
    local present_bytes_json="null"
    local ts_ms_json="null"
    if [[ -n "$patch_bytes" ]]; then patch_bytes_json="$patch_bytes"; fi
    if [[ -n "$patch_cells" ]]; then patch_cells_json="$patch_cells"; fi
    if [[ -n "$patch_runs" ]]; then patch_runs_json="$patch_runs"; fi
    if [[ -n "$render_ms" ]]; then render_ms_json="$render_ms"; fi
    if [[ -n "$present_ms" ]]; then present_ms_json="$present_ms"; fi
    if [[ -n "$present_bytes" ]]; then present_bytes_json="$present_bytes"; fi
    if [[ -n "$ts_ms" ]]; then ts_ms_json="$ts_ms"; fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "frame" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg hash_algo "$hash_algo" \
            --arg frame_hash "$frame_hash" \
            --arg patch_hash "$patch_hash" \
            --arg checksum_chain "$checksum_chain" \
            --arg mode "$mode" \
            --arg hash_key "$hash_key" \
            --argjson frame_idx "$frame_idx" \
            --argjson ts_ms "$ts_ms_json" \
            --argjson cols "$cols_json" \
            --argjson rows "$rows_json" \
            --argjson patch_bytes "$patch_bytes_json" \
            --argjson patch_cells "$patch_cells_json" \
            --argjson patch_runs "$patch_runs_json" \
            --argjson render_ms "$render_ms_json" \
            --argjson present_ms "$present_ms_json" \
            --argjson present_bytes "$present_bytes_json" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,frame_idx:$frame_idx,hash_algo:$hash_algo,frame_hash:$frame_hash,mode:$mode,hash_key:$hash_key,cols:$cols,rows:$rows} 
             + (if $ts_ms != null then {ts_ms:$ts_ms} else {} end)
             + (if $patch_hash != "" then {patch_hash:$patch_hash} else {} end)
             + (if $patch_bytes != null then {patch_bytes:$patch_bytes} else {} end)
             + (if $patch_cells != null then {patch_cells:$patch_cells} else {} end)
             + (if $patch_runs != null then {patch_runs:$patch_runs} else {} end)
             + (if $render_ms != null then {render_ms:$render_ms} else {} end)
             + (if $present_ms != null then {present_ms:$present_ms} else {} end)
             + (if $present_bytes != null then {present_bytes:$present_bytes} else {} end)
             + (if $checksum_chain != "" then {checksum_chain:$checksum_chain} else {} end)')"
    else
        # Fallback: omit optional numeric fields if jq is unavailable.
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"frame\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"frame_idx\":${frame_idx},\"hash_algo\":\"$(json_escape "$hash_algo")\",\"frame_hash\":\"$(json_escape "$frame_hash")\",\"mode\":\"$(json_escape "$mode")\",\"hash_key\":\"$(json_escape "$hash_key")\",\"cols\":${cols_json},\"rows\":${rows_json}}"
    fi
}

jsonl_error() {
    local message="$1"
    local exit_code="${2:-}"
    local stack="${3:-}"
    local details="${4:-}"
    local case_name="${5:-}"
    local step="${6:-}"
    jsonl_init

    local ts
    ts="$(e2e_timestamp)"
    local mode="${E2E_CONTEXT_MODE:-}"
    local cols="${E2E_CONTEXT_COLS:-}"
    local rows="${E2E_CONTEXT_ROWS:-}"
    local seed="${E2E_CONTEXT_SEED:-}"
    local hash_key=""
    if [[ -n "$mode" && -n "$cols" && -n "$rows" ]]; then
        hash_key="$(e2e_hash_key "$mode" "$cols" "$rows" "$seed")"
    fi

    local cols_json="null"
    local rows_json="null"
    local seed_json="null"
    local exit_code_json="null"
    if [[ -n "$cols" ]]; then cols_json="$cols"; fi
    if [[ -n "$rows" ]]; then rows_json="$rows"; fi
    if [[ -n "$seed" ]]; then seed_json="$seed"; fi
    if [[ -n "$exit_code" ]]; then exit_code_json="$exit_code"; fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "error" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg message "$message" \
            --arg stack "$stack" \
            --arg details "$details" \
            --arg case "$case_name" \
            --arg step "$step" \
            --arg mode "$mode" \
            --arg hash_key "$hash_key" \
            --argjson cols "$cols_json" \
            --argjson rows "$rows_json" \
            --argjson exit_code "$exit_code_json" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,message:$message,exit_code:$exit_code,stack:$stack,details:$details,case:$case,step:$step,mode:$mode,hash_key:$hash_key,cols:$cols,rows:$rows}')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"error\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"message\":\"$(json_escape "$message")\",\"exit_code\":${exit_code_json},\"stack\":\"$(json_escape "$stack")\",\"details\":\"$(json_escape "$details")\",\"case\":\"$(json_escape "$case_name")\",\"step\":\"$(json_escape "$step")\",\"mode\":\"$(json_escape "$mode")\",\"hash_key\":\"$(json_escape "$hash_key")\",\"cols\":${cols_json},\"rows\":${rows_json}}"
    fi
}

jsonl_browser_env() {
    local browser="${1:-${E2E_BROWSER:-}}"
    local user_agent="${2:-${E2E_BROWSER_USER_AGENT:-}}"
    local dpr="${3:-${E2E_BROWSER_DPR:-}}"

    if [[ -z "$browser" || -z "$user_agent" || -z "$dpr" ]]; then
        echo "jsonl_browser_env: missing required fields (browser/user_agent/dpr)" >&2
        return 1
    fi

    local browser_version="${E2E_BROWSER_VERSION:-}"
    local platform="${E2E_BROWSER_PLATFORM:-}"
    local locale="${E2E_BROWSER_LOCALE:-}"
    local timezone="${E2E_BROWSER_TIMEZONE:-}"
    local headless="${E2E_BROWSER_HEADLESS:-}"
    local zoom="${E2E_BROWSER_ZOOM:-}"
    local viewport_css_px_json="${E2E_VIEWPORT_CSS_PX_JSON:-}"
    local viewport_px_json="${E2E_VIEWPORT_PX_JSON:-}"
    local canvas_css_px_json="${E2E_CANVAS_CSS_PX_JSON:-}"
    local canvas_px_json="${E2E_CANVAS_PX_JSON:-}"

    jsonl_init
    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi

    local dpr_json="$dpr"
    local headless_json="null"
    local zoom_json="null"
    local viewport_css_px_arg="null"
    local viewport_px_arg="null"
    local canvas_css_px_arg="null"
    local canvas_px_arg="null"
    if [[ -n "$headless" ]]; then headless_json="$headless"; fi
    if [[ -n "$zoom" ]]; then zoom_json="$zoom"; fi
    if [[ -n "$viewport_css_px_json" ]]; then viewport_css_px_arg="$viewport_css_px_json"; fi
    if [[ -n "$viewport_px_json" ]]; then viewport_px_arg="$viewport_px_json"; fi
    if [[ -n "$canvas_css_px_json" ]]; then canvas_css_px_arg="$canvas_css_px_json"; fi
    if [[ -n "$canvas_px_json" ]]; then canvas_px_arg="$canvas_px_json"; fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "browser_env" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg browser "$browser" \
            --arg browser_version "$browser_version" \
            --arg user_agent "$user_agent" \
            --arg platform "$platform" \
            --arg locale "$locale" \
            --arg timezone "$timezone" \
            --argjson dpr "$dpr_json" \
            --argjson headless "$headless_json" \
            --argjson zoom "$zoom_json" \
            --argjson viewport_css_px "$viewport_css_px_arg" \
            --argjson viewport_px "$viewport_px_arg" \
            --argjson canvas_css_px "$canvas_css_px_arg" \
            --argjson canvas_px "$canvas_px_arg" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,browser:$browser,user_agent:$user_agent,dpr:$dpr}
             + (if $browser_version != \"\" then {browser_version:$browser_version} else {} end)
             + (if $platform != \"\" then {platform:$platform} else {} end)
             + (if $locale != \"\" then {locale:$locale} else {} end)
             + (if $timezone != \"\" then {timezone:$timezone} else {} end)
             + (if $headless != null then {headless:$headless} else {} end)
             + (if $zoom != null then {zoom:$zoom} else {} end)
             + (if $viewport_css_px != null then {viewport_css_px:$viewport_css_px} else {} end)
             + (if $viewport_px != null then {viewport_px:$viewport_px} else {} end)
             + (if $canvas_css_px != null then {canvas_css_px:$canvas_css_px} else {} end)
             + (if $canvas_px != null then {canvas_px:$canvas_px} else {} end)')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"browser_env\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"browser\":\"$(json_escape "$browser")\",\"user_agent\":\"$(json_escape "$user_agent")\",\"dpr\":${dpr_json}}"
    fi
}

jsonl_gpu_adapter() {
    local api="${1:-${E2E_GPU_API:-}}"
    local adapter_name="${2:-${E2E_GPU_ADAPTER_NAME:-}}"

    if [[ -z "$api" || -z "$adapter_name" ]]; then
        echo "jsonl_gpu_adapter: missing required fields (api/adapter_name)" >&2
        return 1
    fi

    local backend="${E2E_GPU_BACKEND:-}"
    local vendor="${E2E_GPU_VENDOR:-}"
    local architecture="${E2E_GPU_ARCHITECTURE:-}"
    local device="${E2E_GPU_DEVICE:-}"
    local description="${E2E_GPU_DESCRIPTION:-}"
    local limits_json="${E2E_GPU_LIMITS_JSON:-}"
    local features_json="${E2E_GPU_FEATURES_JSON:-}"
    local is_fallback="${E2E_GPU_IS_FALLBACK_ADAPTER:-}"

    jsonl_init
    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi

    local limits_arg="null"
    local features_arg="null"
    local is_fallback_json="null"
    if [[ -n "$limits_json" ]]; then limits_arg="$limits_json"; fi
    if [[ -n "$features_json" ]]; then features_arg="$features_json"; fi
    if [[ -n "$is_fallback" ]]; then is_fallback_json="$is_fallback"; fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "gpu_adapter" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg api "$api" \
            --arg adapter_name "$adapter_name" \
            --arg backend "$backend" \
            --arg vendor "$vendor" \
            --arg architecture "$architecture" \
            --arg device "$device" \
            --arg description "$description" \
            --argjson limits "$limits_arg" \
            --argjson features "$features_arg" \
            --argjson is_fallback_adapter "$is_fallback_json" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,api:$api,adapter_name:$adapter_name}
             + (if $backend != \"\" then {backend:$backend} else {} end)
             + (if $vendor != \"\" then {vendor:$vendor} else {} end)
             + (if $architecture != \"\" then {architecture:$architecture} else {} end)
             + (if $device != \"\" then {device:$device} else {} end)
             + (if $description != \"\" then {description:$description} else {} end)
             + (if $limits != null then {limits:$limits} else {} end)
             + (if $features != null then {features:$features} else {} end)
             + (if $is_fallback_adapter != null then {is_fallback_adapter:$is_fallback_adapter} else {} end)')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"gpu_adapter\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"api\":\"$(json_escape "$api")\",\"adapter_name\":\"$(json_escape "$adapter_name")\"}"
    fi
}

jsonl_ws_metrics() {
    local label="${1:-${E2E_WS_LABEL:-}}"
    local ws_url="${2:-${E2E_WS_URL:-}}"
    local bytes_tx="${3:-${E2E_WS_BYTES_TX:-}}"
    local bytes_rx="${4:-${E2E_WS_BYTES_RX:-}}"
    local messages_tx="${5:-${E2E_WS_MESSAGES_TX:-}}"
    local messages_rx="${6:-${E2E_WS_MESSAGES_RX:-}}"

    if [[ -z "$label" || -z "$ws_url" || -z "$bytes_tx" || -z "$bytes_rx" || -z "$messages_tx" || -z "$messages_rx" ]]; then
        echo "jsonl_ws_metrics: missing required fields (label/ws_url/bytes_tx/bytes_rx/messages_tx/messages_rx)" >&2
        return 1
    fi

    local connect_ms="${E2E_WS_CONNECT_MS:-}"
    local reconnects="${E2E_WS_RECONNECTS:-}"
    local close_code="${E2E_WS_CLOSE_CODE:-}"
    local close_reason="${E2E_WS_CLOSE_REASON:-}"
    local dropped_messages="${E2E_WS_DROPPED_MESSAGES:-}"
    local rtt_hist_json="${E2E_WS_RTT_HISTOGRAM_MS_JSON:-}"
    local latency_hist_json="${E2E_WS_LATENCY_HISTOGRAM_MS_JSON:-}"

    jsonl_init
    local ts
    ts="$(e2e_timestamp)"
    local seed_json="null"
    if [[ -n "${E2E_SEED:-}" ]]; then seed_json="${E2E_SEED}"; fi

    local connect_ms_json="null"
    local reconnects_json="null"
    local close_code_json="null"
    local dropped_messages_json="null"
    local rtt_hist_arg="null"
    local latency_hist_arg="null"
    if [[ -n "$connect_ms" ]]; then connect_ms_json="$connect_ms"; fi
    if [[ -n "$reconnects" ]]; then reconnects_json="$reconnects"; fi
    if [[ -n "$close_code" ]]; then close_code_json="$close_code"; fi
    if [[ -n "$dropped_messages" ]]; then dropped_messages_json="$dropped_messages"; fi
    if [[ -n "$rtt_hist_json" ]]; then rtt_hist_arg="$rtt_hist_json"; fi
    if [[ -n "$latency_hist_json" ]]; then latency_hist_arg="$latency_hist_json"; fi

    if command -v jq >/dev/null 2>&1; then
        jsonl_emit "$(jq -nc \
            --arg schema_version "$E2E_JSONL_SCHEMA_VERSION" \
            --arg type "ws_metrics" \
            --arg timestamp "$ts" \
            --arg run_id "$E2E_RUN_ID" \
            --arg label "$label" \
            --arg ws_url "$ws_url" \
            --arg close_reason "$close_reason" \
            --argjson bytes_tx "$bytes_tx" \
            --argjson bytes_rx "$bytes_rx" \
            --argjson messages_tx "$messages_tx" \
            --argjson messages_rx "$messages_rx" \
            --argjson connect_ms "$connect_ms_json" \
            --argjson reconnects "$reconnects_json" \
            --argjson close_code "$close_code_json" \
            --argjson dropped_messages "$dropped_messages_json" \
            --argjson rtt_histogram_ms "$rtt_hist_arg" \
            --argjson latency_histogram_ms "$latency_hist_arg" \
            --argjson seed "$seed_json" \
            '{schema_version:$schema_version,type:$type,timestamp:$timestamp,run_id:$run_id,seed:$seed,label:$label,ws_url:$ws_url,bytes_tx:$bytes_tx,bytes_rx:$bytes_rx,messages_tx:$messages_tx,messages_rx:$messages_rx}
             + (if $connect_ms != null then {connect_ms:$connect_ms} else {} end)
             + (if $reconnects != null then {reconnects:$reconnects} else {} end)
             + (if $close_code != null then {close_code:$close_code} else {} end)
             + (if $close_reason != \"\" then {close_reason:$close_reason} else {} end)
             + (if $dropped_messages != null then {dropped_messages:$dropped_messages} else {} end)
             + (if $rtt_histogram_ms != null then {rtt_histogram_ms:$rtt_histogram_ms} else {} end)
             + (if $latency_histogram_ms != null then {latency_histogram_ms:$latency_histogram_ms} else {} end)')"
    else
        jsonl_emit "{\"schema_version\":\"${E2E_JSONL_SCHEMA_VERSION}\",\"type\":\"ws_metrics\",\"timestamp\":\"$(json_escape "$ts")\",\"run_id\":\"$(json_escape "$E2E_RUN_ID")\",\"seed\":${seed_json},\"label\":\"$(json_escape "$label")\",\"ws_url\":\"$(json_escape "$ws_url")\",\"bytes_tx\":${bytes_tx},\"bytes_rx\":${bytes_rx},\"messages_tx\":${messages_tx},\"messages_rx\":${messages_rx}}"
    fi
}

sha256_file() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1 && [[ -f "$file" ]]; then
        sha256sum "$file" | awk '{print $1}'
        return 0
    fi
    return 1
}

verify_sha256() {
    local file="$1"
    local expected="$2"
    local label="${3:-sha256_match}"
    local actual=""
    actual="$(sha256_file "$file" || true)"
    if [[ -z "$actual" ]]; then
        jsonl_assert "$label" "skipped" "sha256sum unavailable or file missing"
        return 2
    fi
    if [[ "$actual" == "$expected" ]]; then
        jsonl_assert "$label" "passed" "sha256 match"
        return 0
    fi
    jsonl_assert "$label" "failed" "expected ${expected}, got ${actual}"
    return 1
}

log() {
    local level="$1"
    shift
    local ts
    ts="$(date +"%Y-%m-%d %H:%M:%S.%3N")"
    echo "[$ts] [$level] $*" | tee -a "$LOG_FILE"
}

log_debug() {
    if [[ "$LOG_LEVEL" == "DEBUG" ]]; then
        log "DEBUG" "$@"
    fi
}

log_info() {
    log "INFO" "$@"
}

log_warn() {
    log "WARN" "$@"
}

log_error() {
    log "ERROR" "$@"
}

log_test_start() {
    local name="$1"
    jsonl_init
    jsonl_step_start "$name"
    log_info "========================================"
    log_info "STARTING TEST: $name"
    log_info "========================================"
}

log_test_pass() {
    local name="$1"
    log_info "PASS: $name"
}

log_test_fail() {
    local name="$1"
    local reason="$2"
    log_error "FAIL: $name"
    log_error "  Reason: $reason"
    log_error "  Log file: $LOG_FILE"
}

log_test_skip() {
    local name="$1"
    local reason="$2"
    log_warn "SKIP: $name"
    log_warn "  Reason: $reason"
}

record_result() {
    local name="$1"
    local status="$2"
    local duration_ms="$3"
    local log_file="$4"
    local error_msg="${5:-}"
    jsonl_init

    mkdir -p "$E2E_RESULTS_DIR"

    local result_file
    result_file="$E2E_RESULTS_DIR/${name}_$(date +%s%N)_$$.json"

    if command -v jq >/dev/null 2>&1; then
        if [[ -n "$error_msg" ]]; then
            jq -n \
                --arg name "$name" \
                --arg status "$status" \
                --argjson duration_ms "$duration_ms" \
                --arg log_file "$log_file" \
                --arg error "$error_msg" \
                '{name:$name,status:$status,duration_ms:$duration_ms,log_file:$log_file,error:$error}' \
                > "$result_file"
        else
            jq -n \
                --arg name "$name" \
                --arg status "$status" \
                --argjson duration_ms "$duration_ms" \
                --arg log_file "$log_file" \
                '{name:$name,status:$status,duration_ms:$duration_ms,log_file:$log_file}' \
                > "$result_file"
        fi
    else
        local safe_error
        safe_error="$(printf '%s' "$error_msg" | sed 's/"/\\"/g')"
        if [[ -n "$safe_error" ]]; then
            printf '{"name":"%s","status":"%s","duration_ms":%s,"log_file":"%s","error":"%s"}\n' \
                "$name" "$status" "$duration_ms" "$log_file" "$safe_error" \
                > "$result_file"
        else
            printf '{"name":"%s","status":"%s","duration_ms":%s,"log_file":"%s"}\n' \
                "$name" "$status" "$duration_ms" "$log_file" \
                > "$result_file"
        fi
    fi
    jsonl_assert "artifact_case_log" "pass" "case_log=$log_file"
    jsonl_step_end "$name" "$status" "$duration_ms"
}

finalize_summary() {
    local summary_file="$1"
    local end_ms
    end_ms="$(e2e_now_ms)"
    local start_ms="${E2E_RUN_START_MS:-$end_ms}"
    local duration_ms=$((end_ms - start_ms))

    if command -v jq >/dev/null 2>&1; then
        jq -s \
            --arg timestamp "$(e2e_timestamp)" \
            --argjson duration_ms "$duration_ms" \
            '{
                timestamp: $timestamp,
                total: length,
                passed: (map(select(.status=="passed")) | length),
                failed: (map(select(.status=="failed")) | length),
                skipped: (map(select(.status=="skipped")) | length),
                duration_ms: $duration_ms,
                tests: .
            }' \
            "$E2E_RESULTS_DIR"/*.json > "$summary_file"
    else
        local total
        total=$(ls -1 "$E2E_RESULTS_DIR"/*.json 2>/dev/null | wc -l | tr -d ' ')
        cat > "$summary_file" <<EOF_SUM
{"timestamp":"$(e2e_timestamp)","total":${total},"passed":0,"failed":0,"skipped":0,"duration_ms":${duration_ms},"tests":[]}
EOF_SUM
    fi
    local failed_count=0
    if command -v jq >/dev/null 2>&1; then
        failed_count=$(jq '.failed // 0' "$summary_file" 2>/dev/null || echo 0)
    fi
    if [[ "$failed_count" -gt 0 ]]; then
        jsonl_run_end "failed" "$duration_ms" "$failed_count"
    else
        jsonl_run_end "complete" "$duration_ms" "$failed_count"
    fi
    jsonl_assert "artifact_summary_json" "pass" "summary_json=$summary_file"
    jsonl_assert "artifact_e2e_jsonl" "pass" "e2e_jsonl=$E2E_JSONL_FILE"
}
