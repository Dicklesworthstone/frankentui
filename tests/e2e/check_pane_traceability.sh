#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

MATRIX_FILE="$PROJECT_ROOT/tests/e2e/pane_traceability_matrix.json"
OUTPUT_FILE="${E2E_RESULTS_DIR:-/tmp/ftui_e2e_results}/pane_traceability_status.json"
ROOT_BEAD_OVERRIDE=""
WARN_ONLY=0

usage() {
    cat <<USAGE
Usage: $0 [options]

Options:
  --matrix <path>     Traceability matrix JSON (default: tests/e2e/pane_traceability_matrix.json)
  --output <path>     Output status JSON path (default: \$E2E_RESULTS_DIR/pane_traceability_status.json)
  --root <bead-id>    Override root bead id (default: matrix.root_bead)
  --warn-only         Emit status JSON but do not fail on violations
  --help, -h          Show this help
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --matrix)
            MATRIX_FILE="${2:-}"
            shift 2
            ;;
        --output|--out)
            OUTPUT_FILE="${2:-}"
            shift 2
            ;;
        --root)
            ROOT_BEAD_OVERRIDE="${2:-}"
            shift 2
            ;;
        --warn-only)
            WARN_ONLY=1
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage
            exit 2
            ;;
    esac
done

if [[ -z "$MATRIX_FILE" || ! -f "$MATRIX_FILE" ]]; then
    echo "Matrix file not found: $MATRIX_FILE" >&2
    exit 2
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required" >&2
    exit 2
fi
if ! command -v br >/dev/null 2>&1; then
    echo "br is required" >&2
    exit 2
fi

ROOT_BEAD="$ROOT_BEAD_OVERRIDE"
if [[ -z "$ROOT_BEAD" ]]; then
    ROOT_BEAD="$(jq -r '.root_bead // empty' "$MATRIX_FILE")"
fi
if [[ -z "$ROOT_BEAD" ]]; then
    echo "root_bead is missing" >&2
    exit 2
fi

array_to_json() {
    if [[ $# -eq 0 ]]; then
        printf '[]'
        return 0
    fi
    printf '%s\n' "$@" | jq -Rsc 'split("\n") | map(select(length > 0))'
}

objects_to_json() {
    if [[ $# -eq 0 ]]; then
        printf '[]'
        return 0
    fi
    printf '%s\n' "$@" | jq -s '.'
}

collect_descendants_json() {
    local root="$1"
    local tmp_seen
    local tmp_out
    tmp_seen="$(mktemp)"
    tmp_out="$(mktemp)"
    : > "$tmp_seen"
    : > "$tmp_out"

    local queue=("$root")
    while [[ ${#queue[@]} -gt 0 ]]; do
        local id="${queue[0]}"
        queue=("${queue[@]:1}")

        if rg -qx "$id" "$tmp_seen" >/dev/null 2>&1; then
            continue
        fi
        printf '%s\n' "$id" >> "$tmp_seen"

        local info
        info="$(br show "$id" --json)"
        echo "$info" | jq -c '.[0] | {id, title, status, priority, issue_type}' >> "$tmp_out"

        local children
        children="$(echo "$info" | jq -r '.[0].dependents[]? | select(.dependency_type == "parent-child") | .id')"
        if [[ -n "$children" ]]; then
            while IFS= read -r child; do
                queue+=("$child")
            done <<< "$children"
        fi
    done

    jq -s 'sort_by(.id)' "$tmp_out"
    rm -f "$tmp_seen" "$tmp_out"
}

descendants_json="$(collect_descendants_json "$ROOT_BEAD")"
desc_file="$(mktemp)"
printf '%s\n' "$descendants_json" > "$desc_file"

declare -A descendant_seen
while IFS= read -r id; do
    descendant_seen["$id"]=1
done < <(jq -r '.[].id' "$desc_file")

declare -A profile_exists
while IFS= read -r profile; do
    profile_exists["$profile"]=1
done < <(jq -r '.profiles | keys[]' "$MATRIX_FILE")

declare -A allowed_status
while IFS= read -r status; do
    allowed_status["$status"]=1
done < <(jq -r '.status_values[]' "$MATRIX_FILE")

declare -A row_by_id
declare -A row_count
while IFS= read -r row; do
    bead_id="$(jq -r '.bead_id // empty' <<< "$row")"
    if [[ -z "$bead_id" ]]; then
        continue
    fi
    row_count["$bead_id"]=$(( ${row_count["$bead_id"]:-0} + 1 ))
    row_by_id["$bead_id"]="$row"
done < <(jq -c '.rows[]' "$MATRIX_FILE")

missing_profile_scripts=()
while IFS= read -r script_path; do
    if [[ -z "$script_path" ]]; then
        continue
    fi
    if [[ ! -e "$PROJECT_ROOT/$script_path" ]]; then
        missing_profile_scripts+=("$script_path")
    fi
done < <(jq -r '.profiles[]?.scripts[]?' "$MATRIX_FILE" | sort -u)

invalid_profile_artifacts=()
while IFS= read -r artifact_name; do
    if [[ -z "$artifact_name" ]]; then
        continue
    fi
    if [[ "$artifact_name" != artifact_* ]]; then
        invalid_profile_artifacts+=("$artifact_name")
    fi
done < <(jq -r '.profiles[]?.artifact_assertions[]?' "$MATRIX_FILE" | sort -u)

missing_rows=()
unknown_profiles=()
invalid_statuses=()
stale_rows=()

expanded_rows_file="$(mktemp)"
: > "$expanded_rows_file"

while IFS= read -r issue; do
    bead_id="$(jq -r '.id' <<< "$issue")"
    issue_title="$(jq -r '.title' <<< "$issue")"
    issue_status="$(jq -r '.status' <<< "$issue")"
    issue_type="$(jq -r '.issue_type' <<< "$issue")"

    row="${row_by_id[$bead_id]-}"
    if [[ -z "$row" ]]; then
        missing_rows+=("$bead_id")
        continue
    fi

    profile="$(jq -r '.profile // empty' <<< "$row")"
    if [[ -z "${profile_exists[$profile]+x}" ]]; then
        unknown_profiles+=("$bead_id:$profile")
    fi

    base_status_json="$(jq -c --arg status "$issue_status" '.default_evidence_status_by_bead_status[$status] // .default_evidence_status_by_bead_status.open' "$MATRIX_FILE")"
    override_status_json="$(jq -c --arg id "$bead_id" '.evidence_overrides[$id] // {}' "$MATRIX_FILE")"
    evidence_status_json="$(jq -cn --argjson base "$base_status_json" --argjson override "$override_status_json" '$base + $override')"

    for key in unit e2e logging; do
        value="$(jq -r --arg key "$key" '.[$key] // empty' <<< "$evidence_status_json")"
        if [[ -z "${allowed_status[$value]+x}" ]]; then
            invalid_statuses+=("$(jq -cn --arg bead_id "$bead_id" --arg key "$key" --arg value "$value" '{bead_id:$bead_id,evidence:$key,status:$value}')")
        fi
    done

    required_evidence_json="$(jq -c --arg profile "$profile" '.profiles[$profile].required_evidence // []' "$MATRIX_FILE")"
    scripts_json="$(jq -c --arg profile "$profile" '.profiles[$profile].scripts // []' "$MATRIX_FILE")"
    artifacts_json="$(jq -c --arg profile "$profile" '.profiles[$profile].artifact_assertions // []' "$MATRIX_FILE")"

    if [[ "$issue_status" != "open" ]]; then
        while IFS= read -r evidence_key; do
            evidence_value="$(jq -r --arg key "$evidence_key" '.[$key] // empty' <<< "$evidence_status_json")"
            if [[ "$issue_status" == "closed" && "$evidence_value" != "passing" ]]; then
                stale_rows+=("$(jq -cn --arg bead_id "$bead_id" --arg key "$evidence_key" --arg value "$evidence_value" --arg issue_status "$issue_status" '{bead_id:$bead_id,issue_status:$issue_status,evidence:$key,status:$value,reason:"closed bead requires passing evidence"}')")
            fi
            if [[ "$issue_status" == "in_progress" && ( "$evidence_value" == "planned" || "$evidence_value" == "blocked" ) ]]; then
                stale_rows+=("$(jq -cn --arg bead_id "$bead_id" --arg key "$evidence_key" --arg value "$evidence_value" --arg issue_status "$issue_status" '{bead_id:$bead_id,issue_status:$issue_status,evidence:$key,status:$value,reason:"in-progress bead has non-actionable required evidence"}')")
            fi
        done < <(jq -r '.[]' <<< "$required_evidence_json")
    fi

    jq -cn \
        --arg bead_id "$bead_id" \
        --arg issue_title "$issue_title" \
        --arg issue_status "$issue_status" \
        --arg issue_type "$issue_type" \
        --arg profile "$profile" \
        --argjson required_evidence "$required_evidence_json" \
        --argjson scripts "$scripts_json" \
        --argjson artifact_assertions "$artifacts_json" \
        --argjson evidence_status "$evidence_status_json" \
        '{
            bead_id: $bead_id,
            issue_title: $issue_title,
            issue_status: $issue_status,
            issue_type: $issue_type,
            profile: $profile,
            required_evidence: $required_evidence,
            scripts: $scripts,
            artifact_assertions: $artifact_assertions,
            evidence_status: $evidence_status
        }' >> "$expanded_rows_file"
done < <(jq -c '.[]' "$desc_file")

orphan_rows=()
for bead_id in "${!row_by_id[@]}"; do
    if [[ -z "${descendant_seen[$bead_id]+x}" ]]; then
        orphan_rows+=("$bead_id")
    fi
done

duplicate_rows=()
for bead_id in "${!row_count[@]}"; do
    if (( row_count["$bead_id"] > 1 )); then
        duplicate_rows+=("$bead_id")
    fi
done

missing_rows_json="$(array_to_json "${missing_rows[@]}")"
orphan_rows_json="$(array_to_json "${orphan_rows[@]}")"
duplicate_rows_json="$(array_to_json "${duplicate_rows[@]}")"
unknown_profiles_json="$(array_to_json "${unknown_profiles[@]}")"
missing_profile_scripts_json="$(array_to_json "${missing_profile_scripts[@]}")"
invalid_profile_artifacts_json="$(array_to_json "${invalid_profile_artifacts[@]}")"
invalid_statuses_json="$(objects_to_json "${invalid_statuses[@]}")"
stale_rows_json="$(objects_to_json "${stale_rows[@]}")"
expanded_rows_json="$(jq -s '.' "$expanded_rows_file")"

descendant_count="$(jq 'length' "$desc_file")"
matrix_row_count="$(jq '.rows | length' "$MATRIX_FILE")"

status_json="$(jq -n \
    --arg schema_version "pane-traceability-status-v1" \
    --arg generated_at "$(date -Iseconds)" \
    --arg root_bead "$ROOT_BEAD" \
    --arg matrix_file "$MATRIX_FILE" \
    --argjson descendant_count "$descendant_count" \
    --argjson matrix_row_count "$matrix_row_count" \
    --argjson missing_rows "$missing_rows_json" \
    --argjson orphan_rows "$orphan_rows_json" \
    --argjson duplicate_rows "$duplicate_rows_json" \
    --argjson unknown_profiles "$unknown_profiles_json" \
    --argjson missing_profile_scripts "$missing_profile_scripts_json" \
    --argjson invalid_profile_artifacts "$invalid_profile_artifacts_json" \
    --argjson invalid_statuses "$invalid_statuses_json" \
    --argjson stale_rows "$stale_rows_json" \
    --argjson rows "$expanded_rows_json" \
    '{
        schema_version: $schema_version,
        generated_at: $generated_at,
        root_bead: $root_bead,
        matrix_file: $matrix_file,
        descendant_count: $descendant_count,
        matrix_row_count: $matrix_row_count,
        checks: {
            missing_rows: $missing_rows,
            orphan_rows: $orphan_rows,
            duplicate_rows: $duplicate_rows,
            unknown_profiles: $unknown_profiles,
            missing_profile_scripts: $missing_profile_scripts,
            invalid_profile_artifacts: $invalid_profile_artifacts,
            invalid_statuses: $invalid_statuses,
            stale_rows: $stale_rows
        },
        rows: $rows,
        counts: {
            by_issue_status: (
                $rows
                | sort_by(.issue_status)
                | group_by(.issue_status)
                | map({key: .[0].issue_status, value: length})
                | from_entries
            ),
            by_profile: (
                $rows
                | sort_by(.profile)
                | group_by(.profile)
                | map({key: .[0].profile, value: length})
                | from_entries
            ),
            by_evidence_status: {
                unit: (
                    $rows
                    | sort_by(.evidence_status.unit)
                    | group_by(.evidence_status.unit)
                    | map({key: .[0].evidence_status.unit, value: length})
                    | from_entries
                ),
                e2e: (
                    $rows
                    | sort_by(.evidence_status.e2e)
                    | group_by(.evidence_status.e2e)
                    | map({key: .[0].evidence_status.e2e, value: length})
                    | from_entries
                ),
                logging: (
                    $rows
                    | sort_by(.evidence_status.logging)
                    | group_by(.evidence_status.logging)
                    | map({key: .[0].evidence_status.logging, value: length})
                    | from_entries
                )
            }
        },
        passed: (
            (($missing_rows | length) == 0)
            and (($orphan_rows | length) == 0)
            and (($duplicate_rows | length) == 0)
            and (($unknown_profiles | length) == 0)
            and (($missing_profile_scripts | length) == 0)
            and (($invalid_profile_artifacts | length) == 0)
            and (($invalid_statuses | length) == 0)
            and (($stale_rows | length) == 0)
        )
    }')"

mkdir -p "$(dirname "$OUTPUT_FILE")"
printf '%s\n' "$status_json" > "$OUTPUT_FILE"

jq '{passed, descendant_count, matrix_row_count, checks}' "$OUTPUT_FILE"

rm -f "$desc_file" "$expanded_rows_file"

if [[ "$WARN_ONLY" == "1" ]]; then
    exit 0
fi

if jq -e '.passed == true' "$OUTPUT_FILE" >/dev/null 2>&1; then
    exit 0
fi

exit 1
