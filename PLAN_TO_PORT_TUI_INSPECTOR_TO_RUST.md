# PLAN_TO_PORT_TUI_INSPECTOR_TO_RUST.md

## Why This Port

`/dp/tui_inspector` currently provides useful, deterministic TUI capture workflows, but the implementation is a set of shell scripts coupled to external tooling (`bash`, `jq`, `curl`, `vhs`, `ttyd`, `ffmpeg`).

Porting to Rust inside FrankenTUI will:

- integrate inspection flows directly into this workspace,
- remove shell/jq fragility,
- provide typed configs + metadata schemas,
- make behavior easier for coding agents to call and reason about,
- keep deterministic artifact outputs for CI and local diagnostics.

## Scope

The new crate will be named `doctor_franktentui` and will be added under `crates/doctor_franktentui`.

Target parity with `/dp/tui_inspector` includes:

- profile-driven capture orchestration,
- VHS tape generation from key-token sequences,
- optional live JSON-RPC seeding,
- suite execution across multiple profiles,
- report generation (JSON + HTML),
- doctor checks for environment/wiring,
- deterministic run artifacts (`run_meta.json`, `run_summary.txt`, logs, tape, media).

## Explicit Exclusions

Functional exclusions for v1: **none** (goal is complete functional parity).

Non-goal exclusions (implementation-level):

- do not re-implement `vhs`, `ttyd`, `ffmpeg`, or `ffprobe` internals in Rust,
- do not vendor those binaries; keep them as external runtime dependencies,
- do not add backward-compat shell wrappers inside FrankenTUI (Rust CLI is canonical).

## Legacy Source of Truth

- `/dp/tui_inspector/README.md`
- `/dp/tui_inspector/scripts/capture_mcp_agent_mail_tui.sh`
- `/dp/tui_inspector/scripts/capture_mcp_agent_mail_analytics.sh`
- `/dp/tui_inspector/scripts/seed_mcp_agent_mail_demo.sh`
- `/dp/tui_inspector/scripts/run_mcp_agent_mail_tui_suite.sh`
- `/dp/tui_inspector/scripts/generate_tui_inspector_report.sh`
- `/dp/tui_inspector/scripts/doctor_tui_inspector.sh`
- `/dp/tui_inspector/profiles/*.env`
- `/dp/tui_inspector/tapes/mcp-agent-mail-analytics.tape`

## Reference Integration Targets in FrankenTUI

- `crates/ftui-harness` (snapshot/conformance patterns)
- `crates/ftui-pty` (PTY capture utilities)
- workspace conventions in root `Cargo.toml`

## Phase Plan

1. Planning/spec docs
- create this plan,
- create extracted structure spec,
- create proposed Rust architecture,
- create feature parity tracker.

2. Crate bootstrap
- add `crates/doctor_franktentui` workspace member,
- create `Cargo.toml`, `src/lib.rs`, `src/main.rs`, module layout,
- enforce Rust 2024 and `#![forbid(unsafe_code)]`.

3. Core implementation
- shared domain models (`RunMeta`, `SuiteManifest`, profile config),
- key token parser + VHS tape builder,
- capture runner with artifact writing + strict failure gates,
- seed-demo JSON-RPC client,
- suite orchestrator,
- report generator,
- doctor checks.

4. Tests
- parser/token translation tests,
- profile parsing/defaulting tests,
- metadata/report generation tests,
- CLI validation tests.

5. Verification and handoff
- run required quality gates (`cargo check`, `cargo clippy`, `cargo fmt --check`),
- update `FEATURE_PARITY.md` with actual status,
- document residual gaps (if any) and next steps.

## Success Criteria

- `doctor_franktentui` can produce a capture run with equivalent artifacts and status handling,
- all legacy profile workflows are representable,
- suite mode aggregates run results and emits JSON/HTML reports,
- seeding flow hits the same JSON-RPC methods with equivalent semantics,
- doctor checks detect missing dependencies and wiring issues,
- workspace builds/lints/formats cleanly.
