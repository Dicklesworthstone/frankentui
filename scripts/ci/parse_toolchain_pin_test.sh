#!/usr/bin/env bash
# Exercise the parser used by CI, including TOML decoys and invalid dates.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
parser="$script_dir/parse_toolchain_pin.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/ftui-toolchain-pin.XXXXXX")"
passed=0

check_case() {
    local name="$1" content="$2" expected_status="$3" expected="$4"
    local status=0
    printf '%s' "$content" > "$scratch/$name.toml"
    bash "$parser" "$scratch/$name.toml" > "$scratch/$name.stdout" 2> "$scratch/$name.stderr" || status=$?
    if [[ "$status" != "$expected_status" ]]; then
        echo "FAIL $name: expected exit $expected_status, got $status" >&2
        cat "$scratch/$name.stdout" "$scratch/$name.stderr" >&2
        exit 1
    fi
    if [[ "$expected_status" == 0 ]]; then
        [[ "$(cat "$scratch/$name.stdout")" == "$expected" && ! -s "$scratch/$name.stderr" ]] || {
            echo "FAIL $name: wrong parsed channel or unexpected diagnostic" >&2
            cat "$scratch/$name.stdout" "$scratch/$name.stderr" >&2
            exit 1
        }
    else
        if [[ -s "$scratch/$name.stdout" ]] || ! grep -Fq "$expected" "$scratch/$name.stderr"; then
            echo "FAIL $name: rejected pin emitted a value or the wrong diagnostic" >&2
            cat "$scratch/$name.stdout" "$scratch/$name.stderr" >&2
            exit 1
        fi
        cat "$scratch/$name.stderr"
    fi
    passed=$((passed + 1))
    echo "PASS $name"
}

check_case dated $'[toolchain]\nchannel = "nightly-2026-08-31"\n' 0 nightly-2026-08-31
check_case changed_pin $'[toolchain]\nchannel = "nightly-2026-08-25"\n' 0 nightly-2026-08-25
check_case crlf $'[toolchain]\r\n channel = "nightly-2026-08-31" # pin\r\n' 0 nightly-2026-08-31
check_case literal_string $'[toolchain]\nchannel = \'nightly-2026-08-31\'\n' 0 nightly-2026-08-31
check_case floating $'[toolchain]\nchannel = "nightly"\n' 1 'floating channel not allowed'
check_case stable $'[toolchain]\nchannel = "1.89.0"\n' 1 'must be a dated nightly'
check_case absent $'[toolchain]\nprofile = "minimal"\n' 1 'must be a dated nightly'
check_case empty '' 1 'must be a dated nightly'
check_case wrong_section $'[other]\nchannel = "nightly-2026-08-31"\n' 1 'must be a dated nightly'
check_case duplicate $'[toolchain]\nchannel = "nightly-2026-08-31"\nchannel = "nightly"\n' 1 'Cannot overwrite a value'
check_case invalid_date $'[toolchain]\nchannel = "nightly-2026-02-30"\n' 1 'invalid nightly calendar date'
check_case invalid_width $'[toolchain]\nchannel = "nightly-2026-8-31"\n' 1 'must be a dated nightly'
check_case suffix $'[toolchain]\nchannel = "nightly-2026-08-31-x86_64-unknown-linux-gnu"\n' 1 'must be a dated nightly'
check_case wrong_type $'[toolchain]\nchannel = 2026\n' 1 'must be a dated nightly'
check_case table_type $'toolchain = "nightly-2026-08-31"\n' 1 'toolchain must be a TOML table'
check_case syntax $'[toolchain]\nchannel = "nightly-2026-08-31\n' 1 'Illegal character'
check_case multiline_decoy $'description = """\n[toolchain]\nchannel = "nightly-2026-08-31"\n"""\n' 1 'must be a dated nightly'

missing_status=0
bash "$parser" "$scratch/missing.toml" > "$scratch/missing.stdout" 2> "$scratch/missing.stderr" || missing_status=$?
[[ "$missing_status" == 1 && ! -s "$scratch/missing.stdout" ]] && grep -Fq 'No such file' "$scratch/missing.stderr"
cat "$scratch/missing.stderr"
passed=$((passed + 1))
echo "PASS missing_file"
echo "$passed parser cases passed; diagnostics retained in $scratch"
