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

#[cfg(feature = "experimental")]
pub mod abstract_interpretation;
#[cfg(feature = "experimental")]
pub mod accessibility_diff;
#[cfg(feature = "experimental")]
pub mod adaptive_schedule;
#[cfg(feature = "experimental")]
pub mod adversarial_fixtures;
#[cfg(feature = "experimental")]
pub mod alien_kernel_tests;
#[cfg(feature = "experimental")]
pub mod alien_kernel_unit_tests;
#[cfg(feature = "experimental")]
pub mod archetype_scoreboard;
#[cfg(feature = "experimental")]
pub mod artifact_coding_tests;
#[cfg(feature = "experimental")]
pub mod backend_capability;
#[cfg(feature = "experimental")]
pub mod baseline_capture;
#[cfg(feature = "experimental")]
pub mod benchmark_harness;
#[cfg(feature = "experimental")]
pub mod benchmark_regression_gate;
#[cfg(feature = "experimental")]
pub mod capability_gap;
pub mod capture;
#[cfg(feature = "experimental")]
pub mod cegis_synthesis;
#[cfg(feature = "experimental")]
pub mod certification_report;
#[cfg(feature = "experimental")]
pub mod chaos_drill;
#[cfg(feature = "experimental")]
pub mod ci_outputs;
pub mod cli;
#[cfg(feature = "experimental")]
pub mod cli_explain_tests;
#[cfg(feature = "experimental")]
pub mod code_emission;
#[cfg(feature = "experimental")]
pub mod codegen_optimize;
#[cfg(feature = "experimental")]
pub mod composition_matrix;
#[cfg(feature = "experimental")]
pub mod composition_semantics;
#[cfg(feature = "experimental")]
pub mod concolic_differential;
#[cfg(feature = "experimental")]
pub mod controller_composition;
#[cfg(feature = "experimental")]
pub mod corpus;
#[cfg(feature = "experimental")]
pub mod corpus_integrity_tests;
#[cfg(feature = "experimental")]
pub mod counterfactual_audit;
#[cfg(feature = "experimental")]
pub mod coverage_prioritizer;
#[cfg(feature = "experimental")]
pub mod decision_loss_policy;
#[cfg(feature = "experimental")]
pub mod deep_assurance_gauntlet;
#[cfg(feature = "experimental")]
pub mod degradation_policy;
pub mod doctor;
#[cfg(feature = "experimental")]
pub mod drift_monitor;
#[cfg(feature = "experimental")]
pub mod ecosystem_scan;
#[cfg(feature = "experimental")]
pub mod effect_canonical;
#[cfg(feature = "experimental")]
pub mod effect_translator;
#[cfg(feature = "experimental")]
pub mod egraph_optimizer;
#[cfg(feature = "experimental")]
pub mod entry_header_compiler;
pub mod error;
#[cfg(feature = "experimental")]
pub mod explain;
#[cfg(feature = "experimental")]
pub mod failure_mode_risk;
#[cfg(feature = "experimental")]
pub mod feedback_ingestion;
#[cfg(feature = "experimental")]
pub mod fixture_taxonomy;
#[cfg(feature = "experimental")]
pub mod flagship_migrations;
#[cfg(feature = "experimental")]
pub mod formal_assurance_gauntlet;
#[cfg(feature = "experimental")]
pub mod galaxy_brain_cards;
#[cfg(feature = "experimental")]
pub mod galaxy_brain_ux;
#[cfg(feature = "experimental")]
pub mod gap_triage;
#[cfg(feature = "experimental")]
pub mod golden_isomorphism;
#[cfg(feature = "experimental")]
pub mod governance_validator_tests;
#[cfg(feature = "experimental")]
pub mod graveyard_control_tests;
#[cfg(feature = "experimental")]
pub mod graveyard_gauntlet;
#[cfg(feature = "experimental")]
pub mod graveyard_verify;
#[cfg(feature = "experimental")]
pub mod graveyardctl;
#[cfg(feature = "experimental")]
pub mod guarantee_assumption_registry;
#[cfg(feature = "experimental")]
pub mod guarantee_layer;
pub mod harness;
#[cfg(feature = "experimental")]
pub mod hazard_regime_model;
#[cfg(feature = "experimental")]
pub mod hierarchical_fusion;
pub mod import;
pub mod intent_inference;
#[cfg(feature = "experimental")]
pub mod ir_explainer;
#[cfg(feature = "experimental")]
pub mod ir_normalize;
#[cfg(feature = "experimental")]
pub mod ir_versioning;
pub mod keyseq;
#[cfg(feature = "experimental")]
pub mod killer_demo;
#[cfg(feature = "experimental")]
pub mod lowering;
#[cfg(feature = "experimental")]
pub mod mapping_atlas;
#[cfg(feature = "experimental")]
pub mod metamorphic_oracle;
#[cfg(feature = "experimental")]
pub mod migration_config;
pub mod migration_ir;
#[cfg(feature = "experimental")]
pub mod milestone_policy;
#[cfg(feature = "experimental")]
pub mod module_graph;
#[cfg(feature = "experimental")]
pub mod multi_round_drill;
#[cfg(feature = "experimental")]
pub mod nightly_evaluation;
#[cfg(feature = "experimental")]
pub mod nightly_stress;
#[cfg(feature = "experimental")]
pub mod one_lever_policy;
#[cfg(feature = "experimental")]
pub mod operator_workflows;
#[cfg(feature = "experimental")]
pub mod opportunity_scorer;
#[cfg(feature = "experimental")]
pub mod optimization_control_tests;
#[cfg(feature = "experimental")]
pub mod optimization_gauntlet;
#[cfg(feature = "experimental")]
pub mod optimization_model_tests;
#[cfg(feature = "experimental")]
pub mod paper_verification;
pub mod performance_diff;
#[cfg(feature = "experimental")]
pub mod portfolio_governance_tests;
#[cfg(feature = "experimental")]
pub mod portfolio_scheduler;
#[cfg(feature = "experimental")]
pub mod posterior_core;
pub mod profile;
#[cfg(feature = "experimental")]
pub mod profile_orchestrator;
#[cfg(feature = "experimental")]
pub mod proof_artifacts;
#[cfg(feature = "experimental")]
pub mod recommendation_contract;
pub mod redact;
#[cfg(feature = "experimental")]
pub mod release_candidate_gate;
pub mod report;
#[cfg(feature = "experimental")]
pub mod reprofile_loop;
#[cfg(feature = "experimental")]
pub mod reverse_round_governance;
#[cfg(feature = "experimental")]
pub mod rollout_gate_tests;
pub mod runmeta;
pub mod sandbox;
pub mod seed;
pub mod semantic_contract;
#[cfg(feature = "experimental")]
pub mod semantic_diff;
#[cfg(feature = "experimental")]
pub mod sequential_fdr;
#[cfg(feature = "experimental")]
pub mod shadow_run;
#[cfg(feature = "experimental")]
pub mod state_effects;
#[cfg(feature = "experimental")]
pub mod state_event_translator;
#[cfg(feature = "experimental")]
pub mod style_semantics;
#[cfg(feature = "experimental")]
pub mod style_translator;
pub mod suite;
pub mod supervised_orchestration;
#[cfg(feature = "experimental")]
pub mod symptom_router;
pub mod tape;
#[cfg(feature = "experimental")]
pub mod test_budget_allocator;
#[cfg(feature = "experimental")]
pub mod tier_escalator;
#[cfg(feature = "experimental")]
pub mod tier_escalator_tests;
pub mod trace;
#[cfg(feature = "experimental")]
pub mod translation_planner;
#[cfg(feature = "experimental")]
pub mod tsx_parser;
pub mod util;
#[cfg(feature = "experimental")]
pub mod view_layout_translator;
pub mod visual_diff;
#[cfg(feature = "experimental")]
pub mod voi_probe_planner;

pub use cli::run_from_env;
pub use error::{DoctorError, Result};
pub use harness::{BaselineResult, FixtureRunner, Harness, HarnessConfig, ScriptedEvent};
pub use intent_inference::{IntentInferenceResult, infer_intents};

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::{Parser, error::ErrorKind};

    use crate::cli::Cli;

    const CORE_MODULES: &[&str] = &[
        "capture",
        "cli",
        "doctor",
        "error",
        "harness",
        "import",
        "intent_inference",
        "keyseq",
        "migration_ir",
        "performance_diff",
        "profile",
        "redact",
        "report",
        "runmeta",
        "sandbox",
        "seed",
        "semantic_contract",
        "suite",
        "supervised_orchestration",
        "tape",
        "trace",
        "util",
        "visual_diff",
    ];

    const EXPERIMENTAL_MODULES: &[&str] = &[
        "abstract_interpretation",
        "accessibility_diff",
        "adaptive_schedule",
        "adversarial_fixtures",
        "alien_kernel_tests",
        "alien_kernel_unit_tests",
        "archetype_scoreboard",
        "artifact_coding_tests",
        "backend_capability",
        "baseline_capture",
        "benchmark_harness",
        "benchmark_regression_gate",
        "capability_gap",
        "cegis_synthesis",
        "certification_report",
        "chaos_drill",
        "ci_outputs",
        "cli_explain_tests",
        "code_emission",
        "codegen_optimize",
        "composition_matrix",
        "composition_semantics",
        "concolic_differential",
        "controller_composition",
        "corpus",
        "corpus_integrity_tests",
        "counterfactual_audit",
        "coverage_prioritizer",
        "decision_loss_policy",
        "deep_assurance_gauntlet",
        "degradation_policy",
        "drift_monitor",
        "ecosystem_scan",
        "effect_canonical",
        "effect_translator",
        "egraph_optimizer",
        "entry_header_compiler",
        "explain",
        "failure_mode_risk",
        "feedback_ingestion",
        "fixture_taxonomy",
        "flagship_migrations",
        "formal_assurance_gauntlet",
        "galaxy_brain_cards",
        "galaxy_brain_ux",
        "gap_triage",
        "golden_isomorphism",
        "governance_validator_tests",
        "graveyard_control_tests",
        "graveyard_gauntlet",
        "graveyard_verify",
        "graveyardctl",
        "guarantee_assumption_registry",
        "guarantee_layer",
        "hazard_regime_model",
        "hierarchical_fusion",
        "ir_explainer",
        "ir_normalize",
        "ir_versioning",
        "killer_demo",
        "lowering",
        "mapping_atlas",
        "metamorphic_oracle",
        "migration_config",
        "milestone_policy",
        "module_graph",
        "multi_round_drill",
        "nightly_evaluation",
        "nightly_stress",
        "one_lever_policy",
        "operator_workflows",
        "opportunity_scorer",
        "optimization_control_tests",
        "optimization_gauntlet",
        "optimization_model_tests",
        "paper_verification",
        "portfolio_governance_tests",
        "portfolio_scheduler",
        "posterior_core",
        "profile_orchestrator",
        "proof_artifacts",
        "recommendation_contract",
        "release_candidate_gate",
        "reprofile_loop",
        "reverse_round_governance",
        "rollout_gate_tests",
        "semantic_diff",
        "sequential_fdr",
        "shadow_run",
        "state_effects",
        "state_event_translator",
        "style_semantics",
        "style_translator",
        "symptom_router",
        "test_budget_allocator",
        "tier_escalator",
        "tier_escalator_tests",
        "translation_planner",
        "tsx_parser",
        "view_layout_translator",
        "voi_probe_planner",
    ];

    #[test]
    fn core_module_list_matches_products_doc() {
        let products_doc = include_str!("../../../docs/doctor-frankentui-products.md");
        let lib_src = include_str!("lib.rs");

        let mut doc_core = BTreeSet::new();
        let mut doc_experimental = BTreeSet::new();

        let mut in_module_table = false;
        for line in products_doc.lines() {
            if line.starts_with("## ") {
                in_module_table = line.trim() == "## Module Classification Table";
                continue;
            }
            if !in_module_table || !line.starts_with("| `") {
                continue;
            }
            let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
            if cells.len() < 3 {
                continue;
            }
            let module = cells[0].trim_matches('`');
            let product = cells[1].trim_matches('`');
            let gate = cells[2].trim_matches('`');
            match product {
                "core" => {
                    assert_eq!(gate, "default", "core module {module} must be default");
                    doc_core.insert(module.to_string());
                }
                "migrate" | "graveyard" | "gates" => {
                    assert_eq!(
                        gate, "experimental",
                        "non-core module {module} must be experimental"
                    );
                    doc_experimental.insert(module.to_string());
                }
                other => panic!("unexpected product classification '{other}' for {module}"),
            }
        }

        let expected_core: BTreeSet<String> =
            CORE_MODULES.iter().map(|s| (*s).to_string()).collect();
        let expected_experimental: BTreeSet<String> = EXPERIMENTAL_MODULES
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        assert_eq!(doc_core, expected_core, "core module set mismatch");
        assert_eq!(
            doc_experimental, expected_experimental,
            "experimental module set mismatch"
        );

        // Also parse lib.rs declarations directly so any ungated experimental pub mod fails.
        let mut prev_cfg_experimental = false;
        let mut declared_core = BTreeSet::new();
        let mut declared_experimental = BTreeSet::new();
        for line in lib_src.lines() {
            let trimmed = line.trim();
            if trimmed == "#[cfg(test)]" {
                break;
            }
            if trimmed == "#[cfg(feature = \"experimental\")]" {
                prev_cfg_experimental = true;
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("pub mod ")
                && let Some(mod_name) = rest.strip_suffix(';')
            {
                if prev_cfg_experimental {
                    declared_experimental.insert(mod_name.to_string());
                } else {
                    declared_core.insert(mod_name.to_string());
                }
                prev_cfg_experimental = false;
            } else if !trimmed.is_empty() {
                prev_cfg_experimental = false;
            }
        }

        assert_eq!(declared_core, doc_core);
        assert_eq!(declared_experimental, doc_experimental);
    }

    #[test]
    fn cli_core_subcommands_route() {
        let core_subcommands = [
            "capture",
            "replay",
            "seed-demo",
            "suite",
            "migrate",
            "report",
            "doctor",
            "certify",
            "import",
            "plan",
            "list-profiles",
            "performance-diff",
            "visual-diff",
        ];
        for sub in core_subcommands {
            let err = Cli::try_parse_from(["doctor_frankentui", sub, "--help"])
                .expect_err("--help exits via clap DisplayHelp");
            assert_eq!(
                err.kind(),
                ErrorKind::DisplayHelp,
                "core subcommand '{sub}' should route to DisplayHelp, got {err:?}"
            );
        }

        let experimental_subcommands = [
            "alien-uplift",
            "graveyard-verify",
            "graveyardctl",
            "voi-plan",
            "sequential-fdr",
            "portfolio-schedule",
            "chaos-drill",
            "nightly-eval",
            "adaptive-schedule",
            "nightly-stress",
            "optimization-gauntlet",
            "feedback-report",
            "ci-outputs",
            "hazard-regime",
            "formal-assurance",
            "graveyard-gauntlet",
            "deep-assurance",
            "operator-workflows",
            "release-candidate",
            "multi-round-drill",
            "flagship-migrations",
            "killer-demo",
            "galaxy-ux",
        ];
        for sub in experimental_subcommands {
            let err = Cli::try_parse_from(["doctor_frankentui", sub, "--help"])
                .expect_err("subcommand --help or unrecognized");
            if cfg!(feature = "experimental") {
                assert_eq!(
                    err.kind(),
                    ErrorKind::DisplayHelp,
                    "experimental subcommand '{sub}' should route when feature is enabled"
                );
            } else {
                assert_eq!(
                    err.kind(),
                    ErrorKind::InvalidSubcommand,
                    "experimental subcommand '{sub}' must be rejected without --features experimental"
                );
            }
        }
    }
}
