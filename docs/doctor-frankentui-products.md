# `doctor_frankentui` Product & Module Map

Per owner decision `bd-g00-root-epic-ewths.28.1` (Option B: feature-gate inside `crates/doctor_frankentui`), `doctor_frankentui` separates its default verification core from three experimental product families gated behind `--features experimental`:

1. **`core`** (default build, 23 library modules + `lib.rs` / `main.rs`): PTY/VHS capture, MCP demo seeding, multi-profile suite execution, HTML/JSON reporting, environment self-certification, intake planning, performance/visual diff replay, intent inference, and semantic contracts.
2. **`migrate`** (`#[cfg(feature = "experimental")]`, 31 modules): TSX/React-to-FrankenTUI migration compiler, lowering/translation passes, code emission, and certification/proof artifact generation.
3. **`graveyard`** (`#[cfg(feature = "experimental")]`, 39 modules): Alien-graveyard research-governance kernels, sequential FDR, VOI probe planning, portfolio scheduling, hazard/BOCPD regime modeling, and galaxy-brain explainability UX.
4. **`gates`** (`#[cfg(feature = "experimental")]`, 31 modules): Nightly/stress evaluation harnesses, chaos drills, multi-round optimization gauntlets, and release-candidate gates.

## CLI Subcommands

### Core Subcommands (always enabled)

| Subcommand | Alias | Handler Module | Purpose |
| --- | --- | --- | --- |
| `replay` | `capture` | `capture` | Replay deterministic capture workflows for a profile |
| `seed-demo` | — | `seed` | Seed MCP demo data via JSON-RPC |
| `migrate` | `suite` | `suite` | Run replay suites across multiple profiles |
| `report` | — | `report` | Generate HTML and JSON reports from a suite directory |
| `certify` | `doctor` | `doctor` | Certify environment wiring and capture readiness |
| `plan` | `import` | `import` | Plan deterministic import intake and snapshot materialization |
| `list-profiles` | — | `capture` | Print built-in profile names |
| `performance-diff` | — | `performance_diff` | Replay archived performance sample comparisons |
| `visual-diff` | — | `visual_diff` | Replay archived visual frame comparisons |

### Experimental Subcommands (`--features experimental`)

`alien-uplift`, `graveyard-verify`, `graveyardctl`, `voi-plan`, `sequential-fdr`, `portfolio-schedule`, `chaos-drill`, `nightly-eval`, `adaptive-schedule`, `nightly-stress`, `optimization-gauntlet`, `feedback-report`, `ci-outputs`, `hazard-regime`, `formal-assurance`, `graveyard-gauntlet`, `deep-assurance`, `operator-workflows`, `release-candidate`, `multi-round-drill`, `flagship-migrations`, `killer-demo`, `galaxy-ux`.

## Module Classification Table

Every `pub mod` in `crates/doctor_frankentui/src/lib.rs` is classified below and enforced by `tests::core_module_list_matches_products_doc`:

| Module | Product | Feature Gate |
| --- | --- | --- |
| `abstract_interpretation` | `graveyard` | `experimental` |
| `accessibility_diff` | `migrate` | `experimental` |
| `adaptive_schedule` | `gates` | `experimental` |
| `adversarial_fixtures` | `gates` | `experimental` |
| `alien_kernel_tests` | `graveyard` | `experimental` |
| `alien_kernel_unit_tests` | `graveyard` | `experimental` |
| `archetype_scoreboard` | `graveyard` | `experimental` |
| `artifact_coding_tests` | `graveyard` | `experimental` |
| `backend_capability` | `migrate` | `experimental` |
| `baseline_capture` | `gates` | `experimental` |
| `benchmark_harness` | `gates` | `experimental` |
| `benchmark_regression_gate` | `gates` | `experimental` |
| `capability_gap` | `migrate` | `experimental` |
| `capture` | `core` | default |
| `cegis_synthesis` | `graveyard` | `experimental` |
| `certification_report` | `migrate` | `experimental` |
| `chaos_drill` | `gates` | `experimental` |
| `ci_outputs` | `migrate` | `experimental` |
| `cli` | `core` | default |
| `cli_explain_tests` | `graveyard` | `experimental` |
| `code_emission` | `migrate` | `experimental` |
| `codegen_optimize` | `migrate` | `experimental` |
| `composition_matrix` | `graveyard` | `experimental` |
| `composition_semantics` | `migrate` | `experimental` |
| `concolic_differential` | `graveyard` | `experimental` |
| `controller_composition` | `graveyard` | `experimental` |
| `corpus` | `gates` | `experimental` |
| `corpus_integrity_tests` | `gates` | `experimental` |
| `counterfactual_audit` | `graveyard` | `experimental` |
| `coverage_prioritizer` | `gates` | `experimental` |
| `decision_loss_policy` | `graveyard` | `experimental` |
| `deep_assurance_gauntlet` | `gates` | `experimental` |
| `degradation_policy` | `gates` | `experimental` |
| `doctor` | `core` | default |
| `drift_monitor` | `gates` | `experimental` |
| `ecosystem_scan` | `graveyard` | `experimental` |
| `effect_canonical` | `migrate` | `experimental` |
| `effect_translator` | `migrate` | `experimental` |
| `egraph_optimizer` | `graveyard` | `experimental` |
| `entry_header_compiler` | `graveyard` | `experimental` |
| `error` | `core` | default |
| `explain` | `migrate` | `experimental` |
| `failure_mode_risk` | `graveyard` | `experimental` |
| `feedback_ingestion` | `migrate` | `experimental` |
| `fixture_taxonomy` | `gates` | `experimental` |
| `flagship_migrations` | `migrate` | `experimental` |
| `formal_assurance_gauntlet` | `gates` | `experimental` |
| `galaxy_brain_cards` | `graveyard` | `experimental` |
| `galaxy_brain_ux` | `graveyard` | `experimental` |
| `gap_triage` | `migrate` | `experimental` |
| `golden_isomorphism` | `graveyard` | `experimental` |
| `governance_validator_tests` | `graveyard` | `experimental` |
| `graveyard_control_tests` | `graveyard` | `experimental` |
| `graveyard_gauntlet` | `graveyard` | `experimental` |
| `graveyard_verify` | `graveyard` | `experimental` |
| `graveyardctl` | `graveyard` | `experimental` |
| `guarantee_assumption_registry` | `graveyard` | `experimental` |
| `guarantee_layer` | `graveyard` | `experimental` |
| `harness` | `core` | default |
| `hazard_regime_model` | `graveyard` | `experimental` |
| `hierarchical_fusion` | `graveyard` | `experimental` |
| `import` | `core` | default |
| `intent_inference` | `core` | default |
| `ir_explainer` | `migrate` | `experimental` |
| `ir_normalize` | `migrate` | `experimental` |
| `ir_versioning` | `migrate` | `experimental` |
| `keyseq` | `core` | default |
| `killer_demo` | `graveyard` | `experimental` |
| `lowering` | `migrate` | `experimental` |
| `mapping_atlas` | `migrate` | `experimental` |
| `metamorphic_oracle` | `graveyard` | `experimental` |
| `migration_config` | `migrate` | `experimental` |
| `migration_ir` | `core` | default |
| `milestone_policy` | `gates` | `experimental` |
| `module_graph` | `migrate` | `experimental` |
| `multi_round_drill` | `gates` | `experimental` |
| `nightly_evaluation` | `gates` | `experimental` |
| `nightly_stress` | `gates` | `experimental` |
| `one_lever_policy` | `gates` | `experimental` |
| `operator_workflows` | `migrate` | `experimental` |
| `opportunity_scorer` | `gates` | `experimental` |
| `optimization_control_tests` | `gates` | `experimental` |
| `optimization_gauntlet` | `gates` | `experimental` |
| `optimization_model_tests` | `gates` | `experimental` |
| `paper_verification` | `graveyard` | `experimental` |
| `performance_diff` | `core` | default |
| `portfolio_governance_tests` | `graveyard` | `experimental` |
| `portfolio_scheduler` | `graveyard` | `experimental` |
| `posterior_core` | `graveyard` | `experimental` |
| `profile` | `core` | default |
| `profile_orchestrator` | `gates` | `experimental` |
| `proof_artifacts` | `migrate` | `experimental` |
| `recommendation_contract` | `graveyard` | `experimental` |
| `redact` | `core` | default |
| `release_candidate_gate` | `gates` | `experimental` |
| `report` | `core` | default |
| `reprofile_loop` | `gates` | `experimental` |
| `reverse_round_governance` | `graveyard` | `experimental` |
| `rollout_gate_tests` | `gates` | `experimental` |
| `runmeta` | `core` | default |
| `sandbox` | `core` | default |
| `seed` | `core` | default |
| `semantic_contract` | `core` | default |
| `semantic_diff` | `migrate` | `experimental` |
| `sequential_fdr` | `graveyard` | `experimental` |
| `shadow_run` | `gates` | `experimental` |
| `state_effects` | `migrate` | `experimental` |
| `state_event_translator` | `migrate` | `experimental` |
| `style_semantics` | `migrate` | `experimental` |
| `style_translator` | `migrate` | `experimental` |
| `suite` | `core` | default |
| `supervised_orchestration` | `core` | default |
| `symptom_router` | `graveyard` | `experimental` |
| `tape` | `core` | default |
| `test_budget_allocator` | `gates` | `experimental` |
| `tier_escalator` | `gates` | `experimental` |
| `tier_escalator_tests` | `gates` | `experimental` |
| `trace` | `core` | default |
| `translation_planner` | `migrate` | `experimental` |
| `tsx_parser` | `migrate` | `experimental` |
| `util` | `core` | default |
| `view_layout_translator` | `migrate` | `experimental` |
| `visual_diff` | `core` | default |
| `voi_probe_planner` | `graveyard` | `experimental` |
