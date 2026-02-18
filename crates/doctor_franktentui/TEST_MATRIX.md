# doctor_franktentui Test Matrix and Acceptance Contract

This document is the canonical verification contract for `crates/doctor_franktentui`.

## Scope

- Code scope: every production module in `crates/doctor_franktentui/src`.
- Verification levels:
  - `unit`: pure logic and deterministic transforms.
  - `integration`: real subprocess/network/filesystem interactions on local host.
  - `e2e`: full command workflows across multiple commands with artifact inspection.

## No-Mock Realism Contract

- Allowed:
  - `tempfile` or explicit temp dirs for isolated filesystem state.
  - Real local subprocess execution (`std::process::Command`) of the compiled binary.
  - Real local ephemeral HTTP server for RPC tests (loopback only).
  - Real JSON encode/decode and disk I/O.
- Disallowed for acceptance:
  - Mocking command execution.
  - Mocking business-logic responses for capture/suite/report/doctor/seed flows.
  - Fake shortcut assertions that do not validate produced artifacts, exit codes, and logs.

## Required Observability Artifacts

- Unit/integration:
  - explicit expected error strings and exit codes (when applicable).
  - deterministic serialized outputs (`run_meta.json`, JSON summaries) validated as content.
- E2E:
  - per-step stdout/stderr logs.
  - run artifact manifest containing checksums, file sizes, and timestamps.
  - machine-readable results summary (`json`) and human summary (`txt/md`).
  - retention of failure diagnostics without rerun.

## Status Legend

- `planned`: row defined, test not yet implemented.
- `implemented`: test exists but not yet tied to explicit matrix verification review.
- `verified`: test implemented and reviewed against this matrix.

## Matrix

| behavior_id | module | level | behavior_description | deterministic_input | expected_output_or_side_effect | failure_signature | log_artifacts_required | status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| DF-CLI-001 | `src/cli.rs` | unit | dispatches each subcommand to the correct runner | synthetic `Cli { command: ... }` | returns downstream `Result` unchanged | wrong command routed or swallowed error | n/a | implemented |
| DF-CLI-002 | `src/main.rs` + `src/error.rs` | integration | process exit code equals `DoctorError::exit_code()` | invoke binary with invalid args/profile | non-zero process exit matches mapped error code | mismatched exit code mapping | stderr + exit code | planned |
| DF-ERR-001 | `src/error.rs` | unit | `DoctorError::exit` preserves code/message | explicit constructor input | `exit_code()` equals provided code | code collapsed to default `1` | n/a | implemented |
| DF-ERR-002 | `src/error.rs` | unit | external command failure maps to exact code | construct `ExternalCommandFailed` variant | `exit_code()` returns `exit_code` field | returns incorrect fallback code | n/a | implemented |
| DF-UTIL-001 | `src/util.rs` | unit | duration parser supports `ms`, `s`, and bare seconds | `500ms`, `5s`, `5` | returns expected `Duration` | invalid parsing accepted/rejected incorrectly | n/a | implemented |
| DF-UTIL-002 | `src/util.rs` | unit | duration parser rejects empty and malformed values | `""`, `"abc"` | returns `DoctorError::InvalidArgument` | silent fallback to zero duration | n/a | implemented |
| DF-UTIL-003 | `src/util.rs` | unit | `normalize_http_path` enforces leading/trailing slash | `mcp`, `/mcp`, `mcp/` | all normalize to `/mcp/` | malformed endpoint path | n/a | implemented |
| DF-UTIL-004 | `src/util.rs` | unit | `shell_single_quote` escapes embedded single quotes safely | values containing `'` | shell-safe quoted string | broken runtime command quoting | n/a | implemented |
| DF-UTIL-005 | `src/util.rs` | unit | `output_for` suppresses human output in JSON mode | synthetic `OutputIntegration` modes | `CliOutput` disabled when `sqlmodel_mode=json` | mixed human + json output contract break | stdout contract assertions | implemented |
| DF-UTIL-006 | `src/util.rs` | integration | ensure path helpers (`ensure_dir`, `ensure_exists`, `write_string`, `append_line`) handle parent creation correctly | temp dirs and nested files | files written and appended deterministically | missing parent dir creation; truncated append | created files and contents | planned |
| DF-PROF-001 | `src/profile.rs` | unit | lists all built-in profile names | built-in table | deterministic list contains 4 expected profiles | missing profile in list | n/a | implemented |
| DF-PROF-002 | `src/profile.rs` | unit | profile parsing handles comments and quoted values | env fragment with comments/quotes | expected key/value map | quotes/comments parsed incorrectly | n/a | implemented |
| DF-PROF-003 | `src/profile.rs` | unit | typed getters (`get_bool/u16/u32/u64`) accept valid values and reject invalid | representative strings | `Some(parsed)` or `None` per type | type coercion bugs | n/a | implemented |
| DF-PROF-004 | `src/profile.rs` | unit | loading unknown profile returns `ProfileNotFound` | non-existent name | deterministic error variant | fallback to wrong profile | n/a | implemented |
| DF-KEY-001 | `src/keyseq.rs` | unit | special keys map to VHS tokens correctly | `tab`, `enter`, `ctrl-c`, arrows | exact mapped token lines | wrong key emission | n/a | implemented |
| DF-KEY-002 | `src/keyseq.rs` | unit | sleep/wait tokens produce `Sleep` with duration literal | `sleep:2`, `wait:500ms` | `Sleep 2s`, `Sleep 500ms` | duration token mangling | n/a | implemented |
| DF-KEY-003 | `src/keyseq.rs` | unit | text and single-char tokens are escaped safely | quotes/backslashes | `Type "..."` escaped by `tape_escape` | VHS injection/escape regression | n/a | implemented |
| DF-TAPE-001 | `src/tape.rs` | unit | base tape contains required setup, command, and teardown directives | deterministic `TapeSpec` | expected lines and ordering | missing `Ctrl+C`/sleep/setup lines | full tape text snapshot | implemented |
| DF-TAPE-002 | `src/tape.rs` | unit | `required_binary` inserts `Require` stanza correctly | spec with and without binary | conditional `Require` block present/absent | legacy binary safety check missing | full tape text snapshot | implemented |
| DF-TAPE-003 | `src/tape.rs` | unit | step sleep inserted after non-sleep tokens only | mixed key sequence | no duplicate step sleeps after `Sleep` tokens | timing inflation or skipped pacing | full tape text snapshot | implemented |
| DF-RMETA-001 | `src/runmeta.rs` | unit | `RunMeta` write/read round-trip is lossless | populated struct with optional fields | deserialized struct equals source | metadata drift or missing fields | serialized JSON comparison | implemented |
| DF-RMETA-002 | `src/runmeta.rs` | unit | default + sparse JSON deserialize works | minimal JSON with subset fields | absent fields resolve to defaults | backward parse failures | serialized output + parsed struct | implemented |
| DF-RMETA-003 | `src/runmeta.rs` | unit | decision records append valid JSONL lines | multiple append operations | one well-formed JSON object per line | malformed jsonl or truncation | ledger file content | implemented |
| DF-DOCTOR-001 | `src/doctor.rs` | integration | environment checks fail fast on missing required commands | run with manipulated PATH | `MissingCommand` and non-zero exit | silent success without requirements | stdout/stderr + exit code | implemented |
| DF-DOCTOR-002 | `src/doctor.rs` | integration | help checks run silently and validate subcommands | invoke `doctor` with current binary | no `--help` text pollution; success when all pass | noisy stdout contamination or false positives | doctor stdout + stderr logs | implemented |
| DF-DOCTOR-003 | `src/doctor.rs` | integration | dry-run smoke produces capture artifacts | run `doctor` against temp run root | generated tape + run dir from nested capture | dry-run path not exercised | run root artifact tree | implemented |
| DF-DOCTOR-004 | `src/doctor.rs` | integration | `--full` triggers full capture smoke branch | run with `--full` under controlled env | full smoke command path executed and validated | `--full` ignored | run root logs and meta | implemented |
| DF-DOCTOR-005 | `src/doctor.rs` | integration | JSON mode emits single JSON summary contract | run with `SQLMODEL_JSON=1` | machine-readable summary line without human status noise | mixed-format stdout in json mode | raw stdout capture | implemented |
| DF-CAP-001 | `src/capture.rs` | unit | profile + arg merge precedence resolves deterministically | fixed profile + arg combinations | expected resolved config fields | precedence regression | resolved config assertions | implemented |
| DF-CAP-002 | `src/capture.rs` | unit | legacy runtime auto-selected when legacy flags used without `--app-command` | args with `--binary/--host/...` | `app_command=None`, legacy runtime path | legacy flags ignored | resolved config assertions | implemented |
| DF-CAP-003 | `src/capture.rs` | unit | conflicting seed flags rejected early | `--seed-demo` with `--no-seed-demo` | invalid argument error | ambiguous seed state | error text assertion | implemented |
| DF-CAP-004 | `src/capture.rs` | unit | `--seed-required` without active seed fails early with clear error | seed-required + seed-demo false | invalid argument error before VHS | late ambiguous failure | error text assertion | implemented |
| DF-CAP-005 | `src/capture.rs` | integration | dry-run creates tape/meta/summary but skips VHS execution | `capture --dry-run` | artifacts written; status `dry_run_ok` | attempts expensive runtime during dry-run | artifact files + stdout contract | implemented |
| DF-CAP-006 | `src/capture.rs` | integration | timeout path returns exit `124` and fallback reason | minimal VHS workload with forced timeout | `final_exit=124`, fallback reason in meta | hung process or wrong fallback metadata | run_meta + run_summary + vhs.log | implemented |
| DF-CAP-007 | `src/capture.rs` | integration | snapshot policy (`required` vs `optional`) maps to exit semantics | with/without `ffmpeg`, required toggles | required snapshot failures map to exit `21` | incorrect pass/fail on snapshot requirement | run_meta + summary + stderr | implemented |
| DF-CAP-008 | `src/capture.rs` | integration | JSON mode output is machine clean | `SQLMODEL_JSON=1 capture ...` | JSON summary payload only (no human status lines) | mixed stdout protocol | raw stdout capture | implemented |
| DF-CAP-009 | `src/capture.rs` | integration | evidence ledger writes both decision points when enabled | run with default evidence ledger | ledger has config + finalize entries | missing or malformed evidence trail | `evidence_ledger.jsonl` | implemented |
| DF-SUITE-001 | `src/suite.rs` | unit | `keep_going` overrides `fail_fast` semantics | `fail_fast=true`, `keep_going=true` | fail-fast disabled | premature stop despite keep_going | summary and branch assertions | implemented |
| DF-SUITE-002 | `src/suite.rs` | unit | runtime command selection honors legacy mode | args with legacy flags, no app-command | capture invocation omits `--app-command` | legacy mode accidentally shadowed | command assembly assertions | implemented |
| DF-SUITE-003 | `src/suite.rs` | integration | per-profile run logs and summary counts are accurate | deterministic profile list | expected success/failure counters | count drift or missing logs | suite summary + `*.runner.log` | implemented |
| DF-SUITE-004 | `src/suite.rs` | integration | report failures propagate to failed suite status/exit | induce report failure | suite returns non-zero with explicit report-failed indicator | silent pass on broken report | stdout/stderr + suite_report.log + exit code | implemented |
| DF-SUITE-005 | `src/suite.rs` | integration | JSON mode summary matches human status decisions | run suite with JSON mode in success/failure | `status` and `report_failed` fields consistent | inconsistent machine status | raw stdout + suite artifacts | implemented |
| DF-REPORT-001 | `src/report.rs` | unit | report generation builds JSON + HTML from run_meta files | temp suite dir with run_meta fixtures | `report.json` + `index.html` created | missing artifacts from valid input | output files | implemented |
| DF-REPORT-002 | `src/report.rs` | unit | missing suite dir and missing run_meta fail clearly | invalid dirs / empty suite | deterministic invalid/missing-path errors | silent empty reports | error text assertion | planned |
| DF-REPORT-003 | `src/report.rs` | unit | HTML escaping and relative path rendering are safe | run_meta with special chars and nested paths | escaped HTML, stable links | broken HTML or unsafe injection | generated html snapshot | planned |
| DF-REPORT-004 | `src/report.rs` | integration | JSON mode output contract emits machine summary only | `SQLMODEL_JSON=1 report ...` | clean JSON summary output | mixed human and machine output | raw stdout capture | implemented |
| DF-SEED-001 | `src/seed.rs` | integration | wait loop succeeds when health endpoint eventually returns result | local scripted server: fail then success | success within timeout window | premature timeout | request transcript + logs | implemented |
| DF-SEED-002 | `src/seed.rs` | integration | retry policy handles empty/non-JSON/JSON-RPC error responses | scripted response sequence | retries with bounded attempts and final failure/success | no retry or infinite retry | transcript + retry lines in log file | implemented |
| DF-SEED-003 | `src/seed.rs` | integration | bearer auth is forwarded when token provided | local server asserting `Authorization` | header present for all requests | missing auth header | captured request headers | implemented |
| DF-SEED-004 | `src/seed.rs` | integration | endpoint path normalization is respected | path variants (`mcp`, `/mcp`, `/mcp/`) | requests hit normalized endpoint | wrong URL formation | captured request path | implemented |
| DF-SEED-005 | `src/seed.rs` | integration | optional `file_reservation_paths` failure is warning-only | server returns error for reservation call | command succeeds with warning | hard-fail on optional operation | stdout/stderr + log file | implemented |
| DF-SEED-006 | `src/seed.rs` | integration | JSON mode output summary is machine readable | `SQLMODEL_JSON=1 seed-demo ...` | final JSON summary includes endpoint/messages | mixed output in json mode | raw stdout capture | planned |
| DF-E2E-001 | command workflow | e2e | happy-path command chain across doctor/capture/suite/report | scripted shell workflow + deterministic profile set | all commands pass and produce complete artifact tree | hidden failures despite zero exit | per-step stdout/stderr + artifact manifest + summary | implemented |
| DF-E2E-002 | command workflow | e2e | failure-path matrix validates expected non-zero exits and messages | scripted negative cases | each case asserts expected code and signature | ambiguous or silent failure semantics | case logs + case_results.json | implemented |
| DF-E2E-003 | command workflow | e2e | JSON mode contract preserved end-to-end for all commands | run key commands with `SQLMODEL_JSON=1` | output parseable as JSON per command | human status leakage into JSON streams | raw stdout archives | implemented |

## Traceability to Current Beads

- `bd-1p2xv.2` targets: `DF-UTIL-*`, `DF-PROF-*`, `DF-KEY-*`, `DF-TAPE-*`, `DF-RMETA-*`.
- `bd-1p2xv.3` targets: `DF-CLI-*`, `DF-ERR-*`, `DF-CAP-001..004`, `DF-SUITE-001..002`, `DF-DOCTOR-001..005`, `DF-SEED-006`.
- `bd-1p2xv.4` targets: `DF-SEED-001..005`.
- `bd-1p2xv.5` targets: `DF-DOCTOR-*`, `DF-CAP-005..009`, `DF-SUITE-003..005`, `DF-REPORT-004`.
- `bd-1p2xv.7` targets: `DF-E2E-001`.
- `bd-1p2xv.6` targets: `DF-E2E-002..003`.
- `bd-1p2xv.8` applies coverage policy across all rows.
- `bd-1p2xv.9` wires CI and docs for all rows.
