# Doctor FrankenTUI Feature Parity

Status key:

- `complete`: implemented and verified
- `partial`: implemented with known gaps
- `planned`: specified but not yet implemented

## Legacy-to-Rust Feature Matrix (`tui_inspector` -> `doctor_frankentui`)

| Capability | Legacy Source | Rust Status | Notes |
|---|---|---|---|
| Profile-driven capture runner | `capture_mcp_agent_mail_tui.sh` | complete | `doctor_frankentui capture` supports profile merge, tape generation, metadata, dry-run/full run |
| `--list-profiles` behavior | capture script | complete | `capture --list-profiles` and top-level `list-profiles` |
| Key token parser (`sleep`, `tab`, `text:*`, etc.) | capture script `emit_token` | complete | parity translation implemented in `keyseq.rs` with unit tests |
| Tape generation with runtime command wiring | capture script | complete | generated in `tape.rs`; supports generic runtime command (`--app-command`) plus legacy env wiring fallback |
| Optional live seeding during capture | capture + seeder scripts | complete | capture spawns `seed-demo` subprocess with delay and logs |
| Snapshot extraction and strict gate | capture script | complete | `ffmpeg` extraction + `--snapshot-required` gate (exit 21) |
| Seeder strict validation (`messages`, `timeout`) | seeder script | complete | clap numeric range validation and transport/error guards |
| JSON-RPC seed method sequence | seeder script | complete | `ensure_project`, `register_agent`×2, `send_message` loop, `fetch_inbox`, `search_messages`, best-effort reservation |
| Suite orchestration | `run_mcp_agent_mail_tui_suite.sh` | complete | `suite` command with `--fail-fast`, `--keep-going`, and runtime command overrides |
| Suite manifest generation | suite script | complete | `suite_manifest.json` emitted with counts + run records |
| Report JSON generation | report script | complete | typed JSON summary, no `jq` dependency |
| Report HTML generation with media cards | report script | complete | HTML card rendering with status colors, links, embeds |
| Doctor checks + optional full smoke | doctor script | complete | `doctor` command performs command/path/help/profile/dry-run/full checks |
| Compatibility wrapper behavior | analytics wrapper script | complete | represented as `capture --profile analytics-empty` |
| FrankenTUI-first defaults | New behavior | complete | default `--app-command` is `cargo run -q -p ftui-demo-showcase` and default `--project-dir` is `/data/projects/frankentui` |

## Alien-Graveyard Upgrades (Intentional Improvements)

| Upgrade | Status | Why |
|---|---|---|
| Structured evidence ledger for decisions | complete | `evidence_ledger.jsonl` captures decision records with policy/trace linkage |
| Explicit budgeted mode with fallback triggers | complete | `--capture-timeout-seconds` + timeout-triggered fallback metadata |
| Deterministic conservative mode toggle | complete | `--conservative` and `DOCTOR_FRANKENTUI_CONSERVATIVE=1` force safe behavior |
| Decision record IDs (`trace_id`, `decision_id`) | complete | trace-scoped IDs emitted in run metadata and ledger |

## Extreme-Optimization Upgrades (Intentional Improvements)

| Upgrade | Status | Why |
|---|---|---|
| Baseline benchmark against legacy dry-run | complete | hyperfine showed Rust dry-run mean 3.2ms vs legacy 88.4ms (~27.23x faster) |
| Hotspot-focused implementation sequence | complete | script runtime and jq/curl process overhead removed in core path |
| Deterministic output equivalence checks | partial | tape/metadata schema parity validated; full fixture diff campaign pending |

## Cross-Project Crate Reuse

| Imported crate | Source project | Status | Integration |
|---|---|---|---|
| `fastapi-output = 0.2.0` | `/dp/fastapi_rust` | complete | Auto-selected status rendering + agent/CI/TTY detection surfaced in doctor/capture/suite/report flows |
| `sqlmodel-console = 0.2.0` | `/dp/sqlmodel_rust` | complete | Output-mode detection (`plain/rich/json`) used for machine-readable JSON emission and run metadata enrichment |

## Verification Checklist

- [x] `cargo check --workspace --all-targets`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo fmt --check`
- [x] Dry-run capture smoke for `doctor_frankentui`
- [ ] Seed-demo smoke against reachable MCP server (when available)

## Known Remaining Work

- Run full live seeding smoke against a running MCP endpoint in this environment.
- Add conformance fixtures comparing legacy and Rust output JSON for a fixed capture scenario.

## Status (2026-10)

- **Default verification core (`default = []`)**: 23 core library modules exposing 9 core CLI subcommands and 4 aliases (`replay`/`capture`, `seed-demo`, `migrate`/`suite`, `report`, `certify`/`doctor`, `plan`/`import`, `list-profiles`, `performance-diff`, `visual-diff`), pinned by `core_module_list_matches_products_doc` and `cli_core_subcommands_route`, with default unit + integration tests verified under the 300s core test-time budget (`scripts/doctor_frankentui_core_time_budget.sh`, measured `33.865s`).
- **Experimental product subcommands (`--features experimental`)**: 101 modules across `migrate` (31), `graveyard` (39), and `gates` (31) exposing 23 experimental CLI subcommands gated behind `--features experimental` and documented in [`docs/doctor-frankentui-products.md`](../doctor-frankentui-products.md).
- **DSR verification stack (`frankentui-doctor-verification` in `.config/dsr-quality.yaml`)**: executes `doctor_frankentui_happy_e2e.sh` (real `vhs` + `ffmpeg` + `ffprobe` capture), `doctor_frankentui_failure_e2e.sh`, `doctor_frankentui_replay_triage.py`, 3-iteration `doctor_frankentui_determinism_soak.sh` (`total_divergences == 0`), `doctor_frankentui_coverage.sh` (`overall = 91.32% >= 68.0%`, `core_engine = 92.81% >= 74.0%`, `full_stack_orchestration = 90.02% >= 60.0%`, `diagnostics_and_contracts = 96.42% >= 70.0%`), and `doctor_frankentui_validate_jsonl.py --bundle-root /tmp/doctor_frankentui_ci --write-artifact-map`.
