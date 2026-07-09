#![forbid(unsafe_code)]

//! `doctor_frankentui` is the operator-facing workflow crate for capture,
//! certification, replay, suite execution, and migration planning.
//!
//! The crate deliberately mixes pure analysis modules with a small set of
//! orchestration-heavy command paths. Those orchestration paths are the primary
//! candidates for supervised Asupersync migration because they own blocking I/O,
//! retries, subprocess lifecycles, timeouts, evidence writes, and fallback
//! behavior.
//!
//! # Command Surface
//!
//! [`cli::run`] dispatches the operator-visible commands:
//!
//! - `replay` (`capture.rs`): full replay/capture workflow for a single profile.
//! - `seed-demo` (`seed.rs`): MCP bootstrap + demo message seeding.
//! - `migrate` (`suite.rs`): repeated replay runs plus manifest/report synthesis.
//! - `certify` (`doctor.rs`): environment and capture-stack certification with
//!   degraded-mode fallback.
//! - `report` (`report.rs`): artifact/report synthesis over completed suite runs.
//! - `plan` (`import.rs`): import-intake planning and snapshot materialization.
//!
//! # Workflow Inventory
//!
//! The orchestration topology is concentrated in a few modules:
//!
//! - [`seed`]: blocking HTTP/MCP bootstrap, retry policy, backoff sleeps, and
//!   server-readiness polling before project/agent/message setup.
//! - [`capture`]: the heaviest workflow. It resolves profile/config state,
//!   optionally starts seed-demo, prepares tapes/artifacts, spawns replay/VHS/tmux
//!   subprocesses, waits on timeouts, records decision ledgers, and finalizes
//!   media/snapshot artifacts.
//! - [`doctor`]: certification wrapper around command checks, capture smoke runs,
//!   degraded-capture classification, app-launch fallback, and summary emission.
//! - [`suite`]: profile fan-out, per-run process orchestration via the CLI,
//!   manifest/index generation, and final report invocation.
//! - [`report`]: post-processing over existing artifacts; lower supervision value
//!   than `capture`/`doctor`, but still part of the end-to-end operator contract.
//! - [`util`], [`runmeta`], [`tape`], and [`profile`]: shared plumbing for
//!   filesystem writes, timestamps, tape generation, and reproducible metadata.
//!
//! The rest of the crate is mostly pure translation/analysis logic and should
//! remain synchronous unless a later change proves otherwise.
//!
//! ## Command-To-Lane Topology
//!
//! The command inventory is more specific than "seed/capture/doctor/suite/report
//! exist." Each top-level CLI command already maps onto a small set of
//! supervision lanes, cancellation points, and artifact contracts:
//!
//! | Command | Primary modules | Highest-value supervised concerns | Existing evidence/artifact contract |
//! | --- | --- | --- | --- |
//! | `seed-demo` | [`seed`], [`util`] | MCP readiness polling, bounded retries, backoff sleeps, request/response logging, JSON-RPC failure classification | optional RPC log file plus deterministic endpoint/project/agent summary payload |
//! | `replay` / `capture` | [`capture`], [`tape`], [`runmeta`], [`trace`], [`util`] | seed-demo bootstrap, replay/VHS/tmux subprocess lifecycle, timeout enforcement, fallback classification, snapshot/media finalization, evidence-ledger writes | `run_meta.json`, `evidence_ledger.jsonl`, trace ids, output/snapshot paths, ttyd/tmux artifacts, fallback + capture-error reasons |
//! | `certify` / `doctor` | [`doctor`], [`capture`], [`util`] | command/help probes, capture smoke subprocesses, degraded-mode classification, optional tmux fallback, bounded app-smoke wait, summary synthesis | `doctor_summary.json`, capture smoke detail, degraded/fallback reason, tmux attach/session/pane artifacts |
//! | `migrate` / `suite` | [`suite`], [`report`], [`runmeta`] | profile fan-out, per-run CLI subprocess orchestration, fail-fast vs keep-going policy, manifest/index generation, report invocation | suite summary JSON, manifest with trace/fallback/capture-error index, report log/html/json outputs |
//! | `report` | [`report`], [`runmeta`] | deterministic aggregation over existing runs, artifact-link resolution, fallback/capture-error surfacing | HTML + JSON report with per-run trace ids, evidence ledger links, tmux/log artifacts |
//! | `plan` / `import` | [`import`] and translation modules | lower supervision value today; mostly deterministic analysis/snapshot materialization rather than long-running orchestration | import planning artifacts and deterministic snapshots |
//!
//! This mapping is the doctor-specific backbone for targeted Asupersync work:
//! only the commands above that own retries, subprocesses, deadlines, or
//! artifact assembly are in scope for replatforming.
//!
//! # Proposed Supervised Topology
//!
//! The migration target is a per-command supervision tree with explicit child
//! responsibilities and cancellation boundaries:
//!
//! 1. Root command scope
//!    Owns CLI args, run IDs, output mode, shared evidence sinks, and global
//!    deadline/cancellation propagation.
//! 2. Network/bootstrap lane
//!    Used by `seed-demo` and any capture paths that call it. Responsible for
//!    MCP health checks, bounded retries, backoff timing, and request/response
//!    logging.
//! 3. Subprocess lane
//!    Used by `capture`, `doctor`, and `suite`. Responsible for spawning,
//!    stdout/stderr capture, timeout enforcement, observer/tmux lifecycle, exit
//!    classification, and cleanup on cancellation.
//! 4. Artifact lane
//!    Responsible for run directory creation, summary/manifests/runmeta writes,
//!    append-only ledgers, and redaction-friendly evidence persistence.
//! 5. Aggregation lane
//!    Used by `suite`/`report` to merge per-run outcomes into manifest, summary,
//!    and operator-facing report artifacts.
//!
//! ## Boundary Inventory By Current Artifact Surface
//!
//! The current code already exposes the fields later migration work must keep
//! stable. `RunMeta` is the key per-run contract and currently carries:
//!
//! - identity: `trace_id`, `policy_id`, `profile`, `run_dir`
//! - fallback state: `fallback_active`, `fallback_reason`,
//!   `capture_error_reason`
//! - subprocess outcomes: `seed_exit_code`, `vhs_exit_code`,
//!   `host_vhs_exit_code`, snapshot/video existence + duration
//! - evidence/artifacts: `evidence_ledger`, output/snapshot paths,
//!   ttyd runtime/shim logs
//! - observer state: `tmux_session`, `tmux_attach_command`,
//!   `tmux_session_file`, `tmux_pane_capture`, `tmux_pane_log`
//!
//! `doctor.rs` then lifts the same artifact vocabulary into
//! `doctor_summary.json`, and `suite.rs` / `report.rs` preserve it in suite
//! manifests and human/operator reports. Any supervised migration that cannot
//! keep these relationships intact is changing the operator contract, not just
//! the executor internals.
//!
//! # Migration Sequence
//!
//! The code suggests a clear order of operations:
//!
//! 1. Inventory/topology definition in this crate-level doc and related tests.
//! 2. `seed.rs` migration first: smallest orchestration surface with concrete
//!    retry/wait logic and low blast radius.
//! 3. `capture.rs` and `doctor.rs` subprocess supervision next: they hold the
//!    highest operator pain and the richest fallback/timeout behavior.
//! 4. `suite.rs` fan-out/report composition after the lower-level lanes are
//!    supervised.
//! 5. Validation expansion last, once the new orchestration boundaries are
//!    stable enough to test deterministically.
//!
//! More concretely, the current repo suggests these implementation slices:
//!
//! 1. Convert `seed.rs` waits/retries into a supervised network/bootstrap lane
//!    while preserving retry logging and endpoint summary semantics.
//! 2. Replatform `capture.rs` subprocess + observer lifecycle as a supervised
//!    subprocess lane that still writes the same `RunMeta` and
//!    `evidence_ledger.jsonl` fields.
//! 3. Refine `doctor.rs` so degraded-capture classification and tmux fallback
//!    decisions become explicit child outcomes instead of ad hoc control flow.
//! 4. Lift `suite.rs` fan-out and report invocation onto the same lane model so
//!    suite summaries can compare per-run lane outcomes deterministically.
//! 5. Only after the above, expand shadow-run and replay validation over the
//!    stabilized evidence schema.
//!
//! # Invariants Worth Preserving
//!
//! Any migration should preserve these crate-level contracts:
//!
//! - CLI behavior and output schemas remain stable.
//! - All deadlines are explicit and observable in artifacts/logs.
//! - Cancellation and degraded-mode exits still emit actionable evidence.
//! - Replay/capture runs keep deterministic run IDs, manifests, and summary
//!   paths suitable for local triage and CI uploads.

pub mod abstract_interpretation;
pub mod accessibility_diff;
pub mod adaptive_schedule;
pub mod adversarial_fixtures;
pub mod alien_kernel_tests;
pub mod alien_kernel_unit_tests;
pub mod archetype_scoreboard;
pub mod artifact_coding_tests;
pub mod backend_capability;
pub mod baseline_capture;
pub mod benchmark_harness;
pub mod benchmark_regression_gate;
pub mod capability_gap;
pub mod capture;
pub mod cegis_synthesis;
pub mod certification_report;
pub mod chaos_drill;
pub mod ci_outputs;
pub mod cli;
pub mod cli_explain_tests;
pub mod code_emission;
pub mod codegen_optimize;
pub mod composition_matrix;
pub mod composition_semantics;
pub mod concolic_differential;
pub mod controller_composition;
pub mod corpus;
pub mod corpus_integrity_tests;
pub mod counterfactual_audit;
pub mod coverage_prioritizer;
pub mod decision_loss_policy;
pub mod deep_assurance_gauntlet;
pub mod degradation_policy;
pub mod doctor;
pub mod drift_monitor;
pub mod ecosystem_scan;
pub mod effect_canonical;
pub mod effect_translator;
pub mod egraph_optimizer;
pub mod entry_header_compiler;
pub mod error;
pub mod explain;
pub mod failure_mode_risk;
pub mod feedback_ingestion;
pub mod fixture_taxonomy;
pub mod flagship_migrations;
pub mod formal_assurance_gauntlet;
pub mod galaxy_brain_cards;
pub mod galaxy_brain_ux;
pub mod gap_triage;
pub mod golden_isomorphism;
pub mod governance_validator_tests;
pub mod graveyard_control_tests;
pub mod graveyard_gauntlet;
pub mod graveyard_verify;
pub mod graveyardctl;
pub mod guarantee_assumption_registry;
pub mod guarantee_layer;
pub mod harness;
pub mod hazard_regime_model;
pub mod hierarchical_fusion;
pub mod import;
pub mod intent_inference;
pub mod ir_explainer;
pub mod ir_normalize;
pub mod ir_versioning;
pub mod keyseq;
pub mod killer_demo;
pub mod lowering;
pub mod mapping_atlas;
pub mod metamorphic_oracle;
pub mod migration_config;
pub mod migration_ir;
pub mod milestone_policy;
pub mod module_graph;
pub mod multi_round_drill;
pub mod nightly_evaluation;
pub mod nightly_stress;
pub mod one_lever_policy;
pub mod operator_workflows;
pub mod opportunity_scorer;
pub mod optimization_control_tests;
pub mod optimization_gauntlet;
pub mod optimization_model_tests;
pub mod paper_verification;
pub mod performance_diff;
pub mod portfolio_governance_tests;
pub mod portfolio_scheduler;
pub mod posterior_core;
pub mod profile;
pub mod profile_orchestrator;
pub mod proof_artifacts;
pub mod recommendation_contract;
pub mod redact;
pub mod release_candidate_gate;
pub mod report;
pub mod reprofile_loop;
pub mod reverse_round_governance;
pub mod rollout_gate_tests;
pub mod runmeta;
pub mod sandbox;
pub mod seed;
pub mod semantic_contract;
pub mod semantic_diff;
pub mod sequential_fdr;
pub mod shadow_run;
pub mod state_effects;
pub mod state_event_translator;
pub mod style_semantics;
pub mod style_translator;
pub mod suite;
pub mod supervised_orchestration;
pub mod symptom_router;
pub mod tape;
pub mod test_budget_allocator;
pub mod tier_escalator;
pub mod tier_escalator_tests;
pub mod trace;
pub mod translation_planner;
pub mod tsx_parser;
pub mod util;
pub mod view_layout_translator;
pub mod visual_diff;
pub mod voi_probe_planner;

pub use cli::run_from_env;
pub use error::{DoctorError, Result};
