# EXISTING_TUI_INSPECTOR_STRUCTURE.md

This document is the extracted behavior spec for `/dp/tui_inspector`.

## 1. Project Overview

`tui_inspector` is a shell/VHS toolkit for deterministic TUI capture. It targets an MCP server process and records interaction artifacts for local/CI regression checks.

Primary capabilities:

- generate video captures with reproducible scripted key sequences,
- optionally seed live demo data via MCP JSON-RPC during capture,
- extract a snapshot image at a chosen timestamp,
- batch run profile suites,
- render aggregate JSON/HTML reports,
- run doctor checks for dependencies and wiring.

## 2. Directory Structure

```
/dp/tui_inspector/
├── README.md
├── profiles/
│   ├── analytics-empty.env
│   ├── analytics-seeded.env
│   ├── messages-seeded.env
│   └── tour-seeded.env
├── scripts/
│   ├── capture_mcp_agent_mail_tui.sh
│   ├── capture_mcp_agent_mail_analytics.sh
│   ├── seed_mcp_agent_mail_demo.sh
│   ├── run_mcp_agent_mail_tui_suite.sh
│   ├── generate_tui_inspector_report.sh
│   └── doctor_tui_inspector.sh
└── tapes/
    └── mcp-agent-mail-analytics.tape
```

## 3. Data Types / Schemas

### 3.1 Capture Run Metadata (`run_meta.json`)

Fields written by runner:

- `status`: string (`running` initially, final `ok` or `failed`)
- `started_at`: ISO8601 UTC timestamp
- `finished_at`: ISO8601 UTC timestamp (final only)
- `duration_seconds`: integer (final only)
- `profile`: string
- `profile_description`: string
- `binary`: string path
- `project_dir`: string path
- `host`: string
- `port`: string
- `path`: normalized HTTP path, starts and ends with `/`
- `keys`: raw key token sequence string
- `seed_demo`: integer-like boolean (`0`/`1`)
- `seed_required`: integer-like boolean (`0`/`1`)
- `seed_exit_code`: integer or null
- `snapshot_required`: integer-like boolean (`0`/`1`)
- `snapshot_status`: string (`ok`, `failed`, `skipped`)
- `snapshot_exit_code`: integer or null
- `vhs_exit_code`: integer
- `video_exists`: JSON boolean
- `snapshot_exists`: JSON boolean
- `video_duration_seconds`: numeric from `ffprobe` or null
- `output`: string path to video
- `snapshot`: string path or empty string
- `run_dir`: string path

### 3.2 Run Summary (`run_summary.txt`)

Flat key-value text file, includes at least:

- profile fields and startup config,
- final status fields (`final_status`, `final_exit`, `vhs_exit`, etc.),
- existence booleans and computed duration.

### 3.3 Suite Manifest (`suite_manifest.json`)

Written by suite runner, contains:

- `suite_name`: string
- `suite_dir`: string
- `started_at`: string
- `finished_at`: string
- `success_count`: integer
- `failure_count`: integer
- `runs`: array of loaded `run_meta.json` objects

### 3.4 Report JSON (`report.json`)

Written by report script:

- `title`: string
- `suite_dir`: string
- `generated_at`: string
- `total_runs`: integer
- `ok_runs`: integer
- `failed_runs`: integer
- `runs`: array of run metadata objects

### 3.5 Profile Env Fields (`profiles/*.env`)

Known fields used by runner:

- `profile_description`
- `keys`
- `seed_demo`
- `seed_messages`
- `boot_sleep`
- `step_sleep`
- `tail_sleep`
- `snapshot_second`
- `theme`
- `font_size`
- `width`
- `height`
- `framerate`

## 4. CLI Commands and Options

## 4.1 `capture_mcp_agent_mail_tui.sh`

Primary command. Option surface:

- profile and discovery:
  - `--profile NAME` (default `analytics-empty`)
  - `--list-profiles` (print profile names, exit 0)
- target process:
  - `--binary PATH` (default `/data/tmp/cargo-target/debug/mcp-agent-mail`)
  - `--project-dir PATH` (default `/data/projects/mcp_agent_mail_rust`)
  - `--host HOST` (default `127.0.0.1`)
  - `--port PORT` (default `8879`)
  - `--path PATH` (default `/mcp/`)
  - `--auth-token TOKEN` (default `tui-inspector-token`)
- artifact layout:
  - `--run-root DIR` (default `/tmp/tui_inspector/runs`)
  - `--run-name NAME` (default `<timestamp>_<profile>`)
  - `--output PATH` (overrides default video naming)
  - `--video-ext EXT` (default `mp4`)
  - `--snapshot PATH`
  - `--snapshot-second SEC` (default `9`)
  - `--no-snapshot`
- interaction/pacing:
  - `--keys LIST`
  - `--jump-key KEY` (legacy alias; maps to `<KEY>,sleep:<capture-sleep>,q`)
  - `--boot-sleep DUR` (default `6`)
  - `--step-sleep DUR` (default `1`)
  - `--tail-sleep DUR` (default `1`)
  - `--capture-sleep DUR` (legacy, default `8`)
- display config:
  - `--theme NAME` (default `GruvboxDark`)
  - `--font-size N` (default `20`)
  - `--width N` (default `1600`)
  - `--height N` (default `900`)
  - `--framerate N` (default `30`)
- seed controls:
  - `--seed-demo` / `--no-seed-demo`
  - `--seed-timeout SEC` (default `30`)
  - `--seed-project KEY` (default `/tmp/tui_inspector_demo_project`)
  - `--seed-agent-a NAME` (default `InspectorRed`)
  - `--seed-agent-b NAME` (default `InspectorBlue`)
  - `--seed-messages N` (default `6`)
  - `--seed-delay DUR` (default `1`)
  - `--seed-required`
- strictness/execution:
  - `--snapshot-required`
  - `--dry-run`
  - `-h|--help`

Unknown options cause error + usage + exit 1.

## 4.2 `capture_mcp_agent_mail_analytics.sh`

Compatibility wrapper, always executes:

- `capture_mcp_agent_mail_tui.sh --profile analytics-empty "$@"`

## 4.3 `seed_mcp_agent_mail_demo.sh`

Options:

- `--host` (default `127.0.0.1`)
- `--port` (default `8879`)
- `--path` (default `/mcp/`)
- `--auth-token` (default empty)
- `--project-key` (default `/tmp/tui_inspector_demo_project`)
- `--agent-a` (default `InspectorRed`)
- `--agent-b` (default `InspectorBlue`)
- `--messages` (default `6`)
- `--timeout` (default `30`)
- `--log-file`
- `-h|--help`

Unknown options => exit 1.

## 4.4 `run_mcp_agent_mail_tui_suite.sh`

Options:

- `--profiles LIST` (default all profiles from runner)
- `--binary PATH`
- `--project-dir PATH`
- `--run-root DIR` (default `/tmp/tui_inspector/suites`)
- `--suite-name NAME` (default `suite_<timestamp>`)
- `--host HOST`
- `--port PORT`
- `--path PATH`
- `--auth-token TOKEN`
- `--fail-fast`
- `--skip-report`
- `--keep-going` (default behavior)
- `--` pass-through args to capture runner
- `-h|--help`

Unknown options => exit 1.

## 4.5 `generate_tui_inspector_report.sh`

Options:

- `--suite-dir DIR` (required)
- `--output-html PATH` (default `<suite-dir>/index.html`)
- `--output-json PATH` (default `<suite-dir>/report.json`)
- `--title TEXT` (default `TUI Inspector Report`)
- `-h|--help`

Unknown options => exit 1.

## 4.6 `doctor_tui_inspector.sh`

Options:

- `--binary PATH`
- `--project-dir PATH`
- `--full`
- `--run-root DIR`
- `-h|--help`

Unknown options => exit 1.

## 5. Behavior Rules and Defaults

## 5.1 HTTP Path Normalization

Both capture and seed scripts normalize path:

- prepend `/` if missing,
- append `/` if missing.

## 5.2 Duration Literal Rules

`duration_literal` behavior:

- if value has any alphabetic char, use literal as-is (`500ms`, `2s`),
- otherwise append `s` (e.g., `6` => `6s`).

## 5.3 Key Token Translation

`emit_token` rules:

- trim token whitespace,
- case-insensitive handling for recognized tokens:
  - `sleep:*` or `wait:*` => `Sleep <duration>`
  - `tab` => `Tab`
  - `enter` => `Enter`
  - `esc`/`escape` => `Escape`
  - `up/down/left/right`
  - `pageup/pagedown`
  - `ctrl+c` or `ctrl-c`
- prefix `text:` => `Type "<escaped>"`
- single character token => `Type "<char>"`
- otherwise default => `Type "<token>"`

After each non-sleep/non-wait token, append `Sleep <step_sleep>`.

## 5.4 Capture Tape Skeleton

Generated tape emits in order:

1. `Output "<output path>"`
2. `Require "ttyd"`
3. `Require "<binary>"`
4. Set shell/render options (`Shell`, `FontSize`, `Width`, `Height`, `Framerate`, `TypingSpeed`, `Theme`)
5. hidden setup:
   - `cd <project_dir>`
   - run server command with env:
     - `AM_INTERFACE_MODE` unset
     - `DATABASE_URL=sqlite:///<run_dir>/storage.sqlite3`
     - `STORAGE_ROOT=<run_dir>/storage_root`
     - `HTTP_BEARER_TOKEN=<auth_token>`
     - `<binary> serve --host ... --port ... --path ... --no-reuse-running`
6. show capture
7. boot sleep
8. key sequence + step sleeps
9. tail sleep
10. `Ctrl+C`
11. `Sleep 500ms`

## 5.5 Seeder RPC Behavior

RPC calls sent in this order (after health check passes):

1. `ensure_project`
2. `register_agent` for agent A
3. `register_agent` for agent B
4. loop `send_message` for `N=messages` alternating sender/recipient
5. `fetch_inbox`
6. `search_messages`
7. `file_reservation_paths` (allowed to fail without abort: `|| true`)

### Seeder validation rules

- `--messages` must match `^[0-9]+$` and be >= 1.
- `--timeout` must match `^[0-9]+$` and be >= 1.
- RPC errors detected for:
  - curl non-zero,
  - empty response,
  - response missing `"jsonrpc"`,
  - response containing `"error"`.

## 5.6 Capture Final Status Rules

Final status starts as `ok` then switches to failed based on gates:

- if `vhs_exit != 0`: failed, exit code = `vhs_exit`
- if `seed_required == 1` and `seed_exit != 0`: failed, exit code = `20`
- if `snapshot_required == 1` and snapshot not `ok`: failed, exit code = `21`

Snapshot behavior:

- if `--no-snapshot`: `snapshot_status=skipped`
- if snapshot enabled and `ffmpeg` exists:
  - success => `ok`
  - failure => `failed`
- if snapshot enabled and no `ffmpeg`: warning + `snapshot_exit=127`

Video duration behavior:

- if output file exists and `ffprobe` exists, query duration.

## 5.7 Suite Behavior Rules

- if no `--profiles`, ask runner for all profile names,
- each profile run output log: `<suite_dir>/<suite_name>_<profile>.runner.log`,
- on run failure:
  - increment failure count,
  - if fail-fast: stop loop,
- write `suite_summary.txt`, optional `suite_manifest.json`, optional report,
- exit 1 when `failure_count > 0`, else 0.

## 5.8 Report Generation Rules

- requires `jq` and at least one `run_meta.json` at depth 2,
- JSON report aggregates counts and includes raw runs,
- HTML report card per run with status class:
  - `ok` => green border,
  - other => red border,
- includes relative links to video/snapshot when files exist.

## 5.9 Doctor Rules

Command checks:

- hard requirements: `bash`, `jq`, `curl`, `vhs`, `ttyd`
- warning only: `ffmpeg`

Path checks:

- binary must be executable,
- project dir must exist.

Behavior checks:

- runner/suite/reporter `--help` must succeed,
- at least one profile listed,
- dry-run capture invoked with analytics-empty profile,
- optional `--full` triggers short real capture.

## 6. Dependencies

Legacy dependency classes:

- shell/runtime: `bash`, `find`, `sed`, `grep`, `date`, `mktemp`, etc.
- JSON parsing/aggregation: `jq`
- HTTP client: `curl`
- capture toolchain: `vhs`, `ttyd`
- media extraction: `ffmpeg`, `ffprobe` (optional behavior)

## 7. Storage and SQL

- No direct SQL queries are issued by `tui_inspector` scripts.
- A `DATABASE_URL` is passed to the target binary (`mcp-agent-mail`), which owns database schema/queries.

## 8. Error Handling Surface

Common error exits:

- unknown option => exit 1
- missing command dependency => exit 1
- missing profile/binary/project path => exit 1
- required seed/snapshot failure => fixed non-zero exits (20/21)
- suite exits 1 when any profile run fails

## 9. Porting Considerations

- preserve option names and defaults for agent compatibility,
- replace jq/curl shell plumbing with typed Rust modules,
- keep artifact file names and metadata keys stable,
- keep external `vhs`/`ttyd`/`ffmpeg` checks and warnings,
- preserve deterministic ordering and status semantics.
