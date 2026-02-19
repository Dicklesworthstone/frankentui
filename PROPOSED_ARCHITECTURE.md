# PROPOSED_ARCHITECTURE.md

## Target Crate

- Name: `doctor_frankentui`
- Location: `crates/doctor_frankentui`
- Rust edition: 2024
- Unsafe policy: `#![forbid(unsafe_code)]`

## Design Goals

- full functional parity with `/dp/tui_inspector` behavior,
- typed, testable Rust implementation (no shell parsing dependencies),
- deterministic artifact generation and status gating,
- agent-friendly CLI subcommands with stable output files.

## Module Layout

```
crates/doctor_frankentui/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── main.rs
│   ├── cli.rs
│   ├── error.rs
│   ├── profile.rs
│   ├── keyseq.rs
│   ├── tape.rs
│   ├── runmeta.rs
│   ├── capture.rs
│   ├── seed.rs
│   ├── suite.rs
│   ├── report.rs
│   ├── doctor.rs
│   └── util.rs
└── profiles/
    ├── analytics-empty.env
    ├── analytics-seeded.env
    ├── messages-seeded.env
    └── tour-seeded.env
```

## Subcommand Architecture

Single binary with explicit subcommands:

- `capture`: parity with `capture_mcp_agent_mail_tui.sh`
- `seed-demo`: parity with `seed_mcp_agent_mail_demo.sh`
- `suite`: parity with `run_mcp_agent_mail_tui_suite.sh`
- `report`: parity with `generate_tui_inspector_report.sh`
- `doctor`: parity with `doctor_tui_inspector.sh`
- `list-profiles`: profile discovery utility

## Core Data Structures

- `CaptureOptions`: clap args + defaults for capture execution
- `ProfileConfig`: parsed profile env fragment
- `CaptureConfig`: resolved runtime config after profile + CLI merge
- `RunMeta`: serialized schema equivalent to legacy `run_meta.json`
- `RunSummary`: key-value text writer helper
- `SuiteOptions`: suite execution arguments
- `SuiteManifest`: aggregated run metadata
- `Report`: JSON report payload
- `RpcClient`: JSON-RPC helper for seed/demo operations
- `DoctorResult`: command/path check outcomes

## Dependency Plan

Rust crates (explicit versions in `Cargo.toml`):

- CLI: `clap`
- errors: `thiserror`
- serialization: `serde`, `serde_json`
- HTTP + JSON-RPC: `reqwest`
- date/time: `chrono`
- process execution: `std::process::Command`
- HTML escaping for report safety: `v_htmlescape`

No legacy shell dependencies (`jq`, `curl`) in core logic.
External runtime tool dependencies remain:

- `vhs`, `ttyd` required for capture
- `ffmpeg`/`ffprobe` optional for snapshot/duration

## Behavior Compatibility Strategy

- preserve option names/default values,
- preserve output filenames and directory structure,
- preserve key sequence token semantics,
- preserve final status and exit-code gate logic (`20`, `21`),
- preserve JSON field names in `run_meta.json` and report JSON,
- preserve suite exit behavior (1 when any run fails).

## Report Rendering Strategy

- JSON report generated with typed structs (no jq),
- HTML rendered with deterministic string builder + escaped content,
- status classes `ok` / `fail`, media embeds when file exists.

## Profile Strategy

- embed built-in profile env files with `include_str!` for deterministic availability,
- parse simple `key="value"` / `key=value` lines,
- support external profile directory override if required later.

## Error Handling Strategy

Central `DoctorError` enum with clear variants:

- invalid args/profile,
- missing dependency command,
- command execution failure,
- IO/JSON/HTTP failures,
- failed run gate conditions.

Subcommands return `Result<()>`; top-level main maps error to exit status 1.

## Testing Strategy

Unit tests:

- key token parsing/translation
- path normalization
- duration literal behavior
- profile parsing and overrides
- run_meta serialization roundtrip
- report generation basic structure

Integration tests (crate-local):

- `capture --dry-run` writes expected artifacts
- `suite` manifest generation with mocked runs (if feasible without VHS)

## Conformance/Parity Verification

- compare generated tape commands against legacy for representative profiles,
- compare run_meta field presence and status transitions,
- compare suite/report output schema,
- run CLI smoke checks mirroring doctor flow.

## Performance and Safety

- keep implementation synchronous and deterministic,
- avoid threads unless required (seeder spawn during capture mirrors legacy behavior),
- no unsafe code,
- avoid shell interpolation; pass arguments via `Command` for robustness.
