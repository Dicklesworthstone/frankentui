//! FrankenTermJS release-readiness program (bd-2vr05.12).
//!
//! Converts implementation progress into safe-delivery artifacts, as data
//! rather than prose: the xterm.js **parity scorecard** with blocker tracking
//! (.12.1), the **browser support matrix** with pass criteria and fallback
//! expectations (.12.2), the **staged rollout plan** with rollback triggers
//! and telemetry checkpoints (.12.3), and the **go/no-go checklist** (.12.5).
//! Everything is deterministic, unit-tested, and serializes to stable JSON so
//! `scripts/frankenterm_js_release_rehearsal_e2e.sh` (.12.6) can bundle it
//! into the signoff packet next to the harvested compat/stress evidence.
//!
//! **Honesty note:** the `frankenterm-web` WASM packaging crate is
//! out-of-tree in this checkout. Every parity area names the in-tree
//! evidence arm that proves its behavior (the `FTUI_*_COMPAT` /
//! `FTUI_*_MATRIX` harnesses), and the standing packaging blocker is tracked
//! openly in the scorecard instead of being papered over: readiness for the
//! browser-facing stages is gated on it.

use core::fmt;

/// Schema version for release-readiness artifacts.
pub const RELEASE_READINESS_SCHEMA_VERSION: &str = "frankenterm-release-readiness-v1";

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_str_list(items: &[&str]) -> String {
    items
        .iter()
        .map(|s| format!("\"{}\"", escape_json(s)))
        .collect::<Vec<_>>()
        .join(",")
}

// ============================================================================
// Parity scorecard (.12.1)
// ============================================================================

/// Parity status for one functional area, versus xterm.js expectations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParityStatus {
    /// Behavior matches the xterm.js expectation (possibly exceeding it).
    Full,
    /// Core works; named gaps remain (see blockers/risks).
    Partial,
    /// Intentionally different, documented in the migration guide
    /// (divergence is a feature, not a gap — e.g. drain-driven events).
    DivergentByDesign,
}

impl ParityStatus {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::DivergentByDesign => "divergent_by_design",
        }
    }
}

impl fmt::Display for ParityStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A tracked release blocker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    /// Stable snake_case id.
    pub id: &'static str,
    /// What blocks and why it matters.
    pub summary: &'static str,
    /// Earliest rollout stage this blocker gates (everything from that
    /// stage onward is not ready while the blocker is open).
    pub gates_stage: RolloutStage,
}

/// One parity area row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityArea {
    /// Stable snake_case area id.
    pub id: &'static str,
    /// Human description of the covered surface.
    pub surface: &'static str,
    /// Parity verdict.
    pub status: ParityStatus,
    /// In-tree evidence arm (crate::test-target) proving the behavior.
    pub evidence: &'static str,
    /// Ids of blockers affecting this area (subset of the scorecard's list).
    pub blocker_ids: Vec<&'static str>,
    /// Risk annotations reviewers must weigh.
    pub risks: Vec<&'static str>,
}

/// The xterm.js parity scorecard: single source of truth for parity
/// coverage, blockers, and risk annotations.
#[derive(Debug, Clone)]
pub struct ParityScorecard {
    /// Parity rows, in stable order.
    pub areas: Vec<ParityArea>,
    /// Open blockers, in stable order.
    pub blockers: Vec<Blocker>,
}

impl ParityScorecard {
    /// The canonical scorecard for the current tree.
    #[must_use]
    pub fn canonical() -> Self {
        let packaging = Blocker {
            id: "wasm_packaging_out_of_tree",
            summary: "the frankenterm-web WASM packaging crate (stable JS wrapper) is \
                      out-of-tree in this checkout; browser-real E2E and npm delivery \
                      cannot be certified from this repository alone",
            gates_stage: RolloutStage::CanaryCohort,
        };
        Self {
            areas: vec![
                ParityArea {
                    id: "lifecycle_rendering",
                    surface: "init/resize/fit/render/destroy + renderer backends (dom-first)",
                    status: ParityStatus::Full,
                    evidence: "ftui-web::frankenterm_js_runtime_options_e2e",
                    blocker_ids: vec![],
                    risks: vec!["webgpu path certified only against simulated engines in-tree"],
                },
                ParityArea {
                    id: "input_ime",
                    surface: "keyboard/mouse/touch/paste + IME composition pipeline",
                    status: ParityStatus::Full,
                    evidence: "ftui-web::frankenterm_js_a11y_e2e",
                    blocker_ids: vec![],
                    risks: vec![],
                },
                ParityArea {
                    id: "events_model",
                    surface: "typed host events + errors (drain-driven, not push-driven)",
                    status: ParityStatus::DivergentByDesign,
                    evidence: "ftui-web::frankenterm_js_sdk_contract_compat",
                    blocker_ids: vec![],
                    risks: vec!["hosts porting onData habits must adopt the drain loop"],
                },
                ParityArea {
                    id: "attach_transport",
                    surface: "websocket attach state machine + flow control + reconnect",
                    status: ParityStatus::Full,
                    evidence: "ftui-pty::frankenterm_js_security_reliability_compat",
                    blocker_ids: vec![],
                    risks: vec![],
                },
                ParityArea {
                    id: "viewport_scrollback",
                    surface: "patches, viewport, scrollback, snapshot framing",
                    status: ParityStatus::Full,
                    evidence: "ftui-web::frankenterm_js_release_stress_e2e",
                    blocker_ids: vec![],
                    risks: vec![],
                },
                ParityArea {
                    id: "search_selection_links",
                    surface: "search, selection, OSC 8 links with open-policy",
                    status: ParityStatus::Full,
                    evidence: "ftui-web::frankenterm_js_markers_compat",
                    blocker_ids: vec![],
                    risks: vec![],
                },
                ParityArea {
                    id: "accessibility",
                    surface: "screen-reader mode, announcements, reduced motion/contrast",
                    status: ParityStatus::Full,
                    evidence: "ftui-web::frankenterm_js_a11y_e2e",
                    blocker_ids: vec![],
                    risks: vec![],
                },
                ParityArea {
                    id: "addons",
                    surface: "fit / image / ligatures / markers (xterm.js addon equivalents)",
                    status: ParityStatus::Full,
                    evidence: "ftui-text::frankenterm_js_ligature_parity_compat",
                    blocker_ids: vec![],
                    risks: vec![
                        "image parity certified at protocol level (sixel/iTerm2), not pixel level",
                    ],
                },
                ParityArea {
                    id: "sdk_typing_adapters",
                    surface: "stable API surface, .d.ts, first-party vanilla/React adapters",
                    status: ParityStatus::Full,
                    evidence: "ftui-web::frankenterm_js_sdk_validation_e2e",
                    blocker_ids: vec![],
                    risks: vec![],
                },
                ParityArea {
                    id: "browser_delivery",
                    surface: "npm package, browser-real E2E, WASM bundle size budget",
                    status: ParityStatus::Partial,
                    evidence: "ftui-web::frankenterm_js_sdk_contract_compat",
                    blocker_ids: vec!["wasm_packaging_out_of_tree"],
                    risks: vec![
                        "everything above is engine-level certified; the JS wrapper \
                                 must re-run the conformance gates when re-vendored",
                    ],
                },
            ],
            blockers: vec![packaging],
        }
    }

    /// Areas currently blocked (transitively via their blocker ids).
    #[must_use]
    pub fn blocked_areas(&self) -> Vec<&ParityArea> {
        self.areas
            .iter()
            .filter(|a| !a.blocker_ids.is_empty())
            .collect()
    }

    /// Deterministic JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let areas = self
            .areas
            .iter()
            .map(|a| {
                format!(
                    "{{\"id\":\"{}\",\"surface\":\"{}\",\"status\":\"{}\",\"evidence\":\"{}\",\"blockers\":[{}],\"risks\":[{}]}}",
                    a.id,
                    escape_json(a.surface),
                    a.status.as_str(),
                    a.evidence,
                    json_str_list(&a.blocker_ids),
                    json_str_list(&a.risks),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let blockers = self
            .blockers
            .iter()
            .map(|b| {
                format!(
                    "{{\"id\":\"{}\",\"summary\":\"{}\",\"gates_stage\":\"{}\"}}",
                    b.id,
                    escape_json(b.summary),
                    b.gates_stage.as_str(),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema\":\"{RELEASE_READINESS_SCHEMA_VERSION}\",\"artifact\":\"parity_scorecard\",\"areas\":[{areas}],\"blockers\":[{blockers}]}}"
        )
    }
}

// ============================================================================
// Browser support matrix (.12.2)
// ============================================================================

/// Support tier for one browser/renderer cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportTier {
    /// Must pass the full conformance + compat lanes.
    Supported,
    /// Expected to work via the documented fallback; failures triage as bugs
    /// but do not block release.
    BestEffort,
    /// Explicitly not targeted; the fallback chain must still land on a
    /// supported renderer.
    Unsupported,
}

impl SupportTier {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::BestEffort => "best_effort",
            Self::Unsupported => "unsupported",
        }
    }
}

/// One browser row of the support matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSupport {
    /// Browser family (engine-level: the in-tree lanes are engine-simulated).
    pub browser: &'static str,
    /// Minimum version line the criteria apply to.
    pub min_version: &'static str,
    /// Tier per renderer, in the fixed order dom/canvas/webgl/webgpu.
    pub renderer_tiers: [SupportTier; 4],
    /// Pass criteria + fallback expectation.
    pub criteria: &'static str,
}

/// The browser support matrix with explicit pass/fail criteria.
#[must_use]
pub fn browser_support_matrix() -> Vec<BrowserSupport> {
    vec![
        BrowserSupport {
            browser: "chromium",
            min_version: "120",
            renderer_tiers: [
                SupportTier::Supported,
                SupportTier::Supported,
                SupportTier::Supported,
                SupportTier::Supported,
            ],
            criteria: "all conformance + compat lanes green; webgpu is the preferred \
                       high-performance backend, dom the guaranteed fallback",
        },
        BrowserSupport {
            browser: "firefox",
            min_version: "121",
            renderer_tiers: [
                SupportTier::Supported,
                SupportTier::Supported,
                SupportTier::Supported,
                SupportTier::BestEffort,
            ],
            criteria: "webgpu best-effort behind the capability probe; automatic downgrade \
                       to webgl/canvas must be observable in the option-update events",
        },
        BrowserSupport {
            browser: "webkit_safari",
            min_version: "17",
            renderer_tiers: [
                SupportTier::Supported,
                SupportTier::Supported,
                SupportTier::BestEffort,
                SupportTier::BestEffort,
            ],
            criteria: "dom/canvas supported; gpu backends best-effort; IME + clipboard \
                       policies must pass the a11y/IME matrix on the dom renderer",
        },
        BrowserSupport {
            browser: "other_engines",
            min_version: "-",
            renderer_tiers: [
                SupportTier::BestEffort,
                SupportTier::BestEffort,
                SupportTier::Unsupported,
                SupportTier::Unsupported,
            ],
            criteria: "the capability probe must always land the fallback chain on dom; \
                       a boot failure on ANY engine is release-blocking (boot guarantee)",
        },
    ]
}

/// Deterministic JSON for the support matrix.
#[must_use]
pub fn browser_support_matrix_json() -> String {
    let rows = browser_support_matrix()
        .iter()
        .map(|r| {
            let tiers = r
                .renderer_tiers
                .iter()
                .map(|t| format!("\"{}\"", t.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{\"browser\":\"{}\",\"min_version\":\"{}\",\"renderers\":{{\"order\":[\"dom\",\"canvas\",\"webgl\",\"webgpu\"],\"tiers\":[{tiers}]}},\"criteria\":\"{}\"}}",
                r.browser,
                r.min_version,
                escape_json(r.criteria),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":\"{RELEASE_READINESS_SCHEMA_VERSION}\",\"artifact\":\"browser_support_matrix\",\"rows\":[{rows}]}}"
    )
}

// ============================================================================
// Staged rollout + rollback (.12.3)
// ============================================================================

/// Progressive rollout stages, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RolloutStage {
    /// Maintainer-facing dogfood builds.
    InternalDogfood,
    /// Opt-in flag for early adopters.
    OptInFlag,
    /// Percentage canary of real hosts.
    CanaryCohort,
    /// FrankenTermJS is the default terminal.
    DefaultEnablement,
}

impl RolloutStage {
    /// All stages in rollout order.
    pub const ALL: [RolloutStage; 4] = [
        RolloutStage::InternalDogfood,
        RolloutStage::OptInFlag,
        RolloutStage::CanaryCohort,
        RolloutStage::DefaultEnablement,
    ];

    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InternalDogfood => "internal_dogfood",
            Self::OptInFlag => "opt_in_flag",
            Self::CanaryCohort => "canary_cohort",
            Self::DefaultEnablement => "default_enablement",
        }
    }
}

/// One stage of the rollout plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutStagePlan {
    /// The stage.
    pub stage: RolloutStage,
    /// What must be true before entering the stage.
    pub entry_criteria: Vec<&'static str>,
    /// Automatic rollback triggers while in the stage (telemetry-derived).
    pub rollback_triggers: Vec<&'static str>,
    /// Telemetry checkpoints operators watch (JSONL event vocabularies).
    pub telemetry_checkpoints: Vec<&'static str>,
}

/// The canonical staged rollout + rollback plan.
#[must_use]
pub fn rollout_plan() -> Vec<RolloutStagePlan> {
    vec![
        RolloutStagePlan {
            stage: RolloutStage::InternalDogfood,
            entry_criteria: vec![
                "conformance CI gates green (frankenterm-conformance-gates job)",
                "parity scorecard has no red areas (partial allowed with named risks)",
            ],
            rollback_triggers: vec![
                "any semantic drift classification in the conformance/diff lanes",
            ],
            telemetry_checkpoints: vec![
                "attach.transition timelines (drainAttachTransitionsJsonl)",
                "adapter_transition/adapter_misuse JSONL (FTUI_SDK_ADAPTER_COMPAT vocabulary)",
            ],
        },
        RolloutStagePlan {
            stage: RolloutStage::OptInFlag,
            entry_criteria: vec![
                "stress/soak campaign executed with documented limits (FTUI_RELEASE_STRESS)",
                "migration guide + API reference published and cross-linked",
            ],
            rollback_triggers: vec![
                "attach.protocol_error rate above the stress-campaign baseline envelope",
                "queue.overflow sustained on default buffer policy under real host load",
            ],
            telemetry_checkpoints: vec![
                "SdkErrorKind code rates by class (errors JSONL)",
                "event-drain latency vs the 16ms cadence recommendation",
            ],
        },
        RolloutStagePlan {
            stage: RolloutStage::CanaryCohort,
            entry_criteria: vec![
                "wasm_packaging_out_of_tree blocker resolved (JS wrapper re-vendored + \
                 conformance gates re-run against it)",
                "browser support matrix criteria verified on real supported browsers",
            ],
            rollback_triggers: vec![
                "canary error-rate delta vs control beyond the agreed threshold",
                "unrecoverable renderer-fallback loops (downgrade chain not settling)",
            ],
            telemetry_checkpoints: vec![
                "per-browser renderer distribution (capability probe outcomes)",
                "rollout checkpoint events per rehearsal script vocabulary",
            ],
        },
        RolloutStagePlan {
            stage: RolloutStage::DefaultEnablement,
            entry_criteria: vec![
                "go/no-go checklist fully signed off (all machine-checkable items green)",
                "canary cohort stable for the agreed observation window",
            ],
            rollback_triggers: vec![
                "any release-blocking regression: flip the default back (the opt-out \
                 path must remain functional through at least one release)",
            ],
            telemetry_checkpoints: vec!["aggregate error budget across SdkErrorKind classes"],
        },
    ]
}

/// Deterministic JSON for the rollout plan.
#[must_use]
pub fn rollout_plan_json() -> String {
    let stages = rollout_plan()
        .iter()
        .map(|s| {
            format!(
                "{{\"stage\":\"{}\",\"entry_criteria\":[{}],\"rollback_triggers\":[{}],\"telemetry_checkpoints\":[{}]}}",
                s.stage.as_str(),
                json_str_list(&s.entry_criteria),
                json_str_list(&s.rollback_triggers),
                json_str_list(&s.telemetry_checkpoints),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":\"{RELEASE_READINESS_SCHEMA_VERSION}\",\"artifact\":\"rollout_plan\",\"stages\":[{stages}]}}"
    )
}

/// Readiness verdict for a stage given the current scorecard: a stage is
/// ready only when no open blocker gates it or an earlier stage.
#[must_use]
pub fn stage_ready(scorecard: &ParityScorecard, stage: RolloutStage) -> bool {
    scorecard.blockers.iter().all(|b| b.gates_stage > stage)
}

// ============================================================================
// Go/no-go checklist (.12.5)
// ============================================================================

/// Checklist item category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecklistCategory {
    /// Automated test gate (machine-checkable).
    TestGate,
    /// Documentation readiness.
    Docs,
    /// Human/operational signoff.
    Operational,
}

impl ChecklistCategory {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TestGate => "test_gate",
            Self::Docs => "docs",
            Self::Operational => "operational",
        }
    }
}

/// One go/no-go checklist item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecklistItem {
    /// Stable snake_case id.
    pub id: &'static str,
    /// Category.
    pub category: ChecklistCategory,
    /// What must be verified.
    pub requirement: &'static str,
    /// Where the evidence lives (test target, script, doc, or role).
    pub evidence: &'static str,
}

/// The final release checklist tying test gates, docs, and signoff.
#[must_use]
pub fn go_no_go_checklist() -> Vec<ChecklistItem> {
    vec![
        ChecklistItem {
            id: "conformance_gates_green",
            category: ChecklistCategory::TestGate,
            requirement: "conformance/differential/fuzz CI gates green on the release rev",
            evidence: "ci.yml::frankenterm-conformance-gates",
        },
        ChecklistItem {
            id: "compat_arms_green",
            category: ChecklistCategory::TestGate,
            requirement: "every FTUI_*_COMPAT arm green (markers, parser hooks, image, \
                          ligatures, security/reliability, sdk contract, sdk adapters)",
            evidence: "scripts/frankenterm_js_release_rehearsal_e2e.sh",
        },
        ChecklistItem {
            id: "stress_campaign_executed",
            category: ChecklistCategory::TestGate,
            requirement: "stress/soak campaign run with limits documented and within envelope",
            evidence: "ftui-web::frankenterm_js_release_stress_e2e",
        },
        ChecklistItem {
            id: "parity_scorecard_no_open_blockers",
            category: ChecklistCategory::TestGate,
            requirement: "parity scorecard reports zero open blockers for the target stage",
            evidence: "ftui-web::release_readiness (parity_scorecard artifact)",
        },
        ChecklistItem {
            id: "docs_aligned",
            category: ChecklistCategory::Docs,
            requirement: "contract, API reference, migration guide, and examples lockstep \
                          tests green (docs cannot drift from runtime)",
            evidence: "docs/frankenterm-js-sdk-reference.md + lockstep guards",
        },
        ChecklistItem {
            id: "rollout_plan_agreed",
            category: ChecklistCategory::Operational,
            requirement: "stage entry criteria, rollback triggers, and telemetry \
                          checkpoints reviewed by the release owner",
            evidence: "release_readiness::rollout_plan",
        },
        ChecklistItem {
            id: "rehearsal_signoff_packet",
            category: ChecklistCategory::Operational,
            requirement: "release rehearsal executed; signoff packet archived with all \
                          JSONL evidence and this checklist",
            evidence: "scripts/frankenterm_js_release_rehearsal_e2e.sh",
        },
    ]
}

/// Deterministic JSON for the checklist.
#[must_use]
pub fn go_no_go_checklist_json() -> String {
    let items = go_no_go_checklist()
        .iter()
        .map(|i| {
            format!(
                "{{\"id\":\"{}\",\"category\":\"{}\",\"requirement\":\"{}\",\"evidence\":\"{}\"}}",
                i.id,
                i.category.as_str(),
                escape_json(i.requirement),
                escape_json(i.evidence),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":\"{RELEASE_READINESS_SCHEMA_VERSION}\",\"artifact\":\"go_no_go_checklist\",\"items\":[{items}]}}"
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scorecard_blockers_and_areas_are_consistent() {
        let card = ParityScorecard::canonical();
        assert!(card.areas.len() >= 10, "scorecard must cover the surface");
        let blocker_ids: Vec<&str> = card.blockers.iter().map(|b| b.id).collect();
        for area in &card.areas {
            for id in &area.blocker_ids {
                assert!(
                    blocker_ids.contains(id),
                    "area {} references unknown blocker {id}",
                    area.id
                );
            }
            assert!(
                area.evidence.contains("::"),
                "evidence must name crate::target: {}",
                area.evidence
            );
        }
        // The packaging blocker is tracked honestly and gates the canary.
        let blocked = card.blocked_areas();
        assert!(
            blocked.iter().any(|a| a.id == "browser_delivery"),
            "browser delivery must be tracked as blocked while packaging is out-of-tree"
        );
    }

    /// Every evidence hook must point at a REAL integration-test target in
    /// the workspace: a scorecard citing a nonexistent (or wrong-crate)
    /// suite would make readiness claims nobody can verify.
    #[test]
    fn scorecard_evidence_targets_exist_in_the_workspace() {
        let workspace_crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ dir");
        for area in ParityScorecard::canonical().areas {
            let (krate, target) = area
                .evidence
                .split_once("::")
                .expect("evidence is crate::target");
            let path = workspace_crates
                .join(krate)
                .join("tests")
                .join(format!("{target}.rs"));
            assert!(
                path.is_file(),
                "area {}: evidence target does not exist: {}",
                area.id,
                path.display()
            );
        }
    }

    #[test]
    fn stage_readiness_respects_blocker_gates() {
        let card = ParityScorecard::canonical();
        assert!(stage_ready(&card, RolloutStage::InternalDogfood));
        assert!(stage_ready(&card, RolloutStage::OptInFlag));
        assert!(
            !stage_ready(&card, RolloutStage::CanaryCohort),
            "canary must be gated by the packaging blocker"
        );
        assert!(!stage_ready(&card, RolloutStage::DefaultEnablement));

        // Resolving the blocker opens the later stages.
        let mut resolved = card.clone();
        resolved.blockers.clear();
        assert!(stage_ready(&resolved, RolloutStage::DefaultEnablement));
    }

    #[test]
    fn rollout_plan_covers_every_stage_in_order_with_rollback() {
        let plan = rollout_plan();
        assert_eq!(plan.len(), RolloutStage::ALL.len());
        for (idx, stage_plan) in plan.iter().enumerate() {
            assert_eq!(stage_plan.stage, RolloutStage::ALL[idx], "stage order");
            assert!(!stage_plan.entry_criteria.is_empty());
            assert!(
                !stage_plan.rollback_triggers.is_empty(),
                "every stage needs rollback triggers: {}",
                stage_plan.stage.as_str()
            );
            assert!(!stage_plan.telemetry_checkpoints.is_empty());
        }
    }

    #[test]
    fn support_matrix_always_lands_on_a_supported_renderer() {
        for row in browser_support_matrix() {
            assert_ne!(
                row.renderer_tiers[0],
                SupportTier::Unsupported,
                "{}: the dom fallback must never be unsupported (boot guarantee)",
                row.browser
            );
        }
    }

    #[test]
    fn checklist_items_are_unique_and_machine_locatable() {
        let items = go_no_go_checklist();
        let mut ids: Vec<&str> = items.iter().map(|i| i.id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "checklist ids must be unique");
        assert!(
            items
                .iter()
                .any(|i| i.category == ChecklistCategory::TestGate),
        );
        assert!(items.iter().any(|i| i.category == ChecklistCategory::Docs));
        assert!(
            items
                .iter()
                .any(|i| i.category == ChecklistCategory::Operational),
        );
    }

    /// Emits every readiness artifact for the rehearsal script to harvest
    /// into the signoff packet (`FTUI_RELEASE_READINESS ` JSONL envelope).
    #[test]
    fn artifacts_emit_for_signoff_packet() {
        for artifact in [
            ParityScorecard::canonical().to_json(),
            browser_support_matrix_json(),
            rollout_plan_json(),
            go_no_go_checklist_json(),
        ] {
            println!("FTUI_RELEASE_READINESS {artifact}");
        }
    }

    #[test]
    fn all_artifacts_serialize_deterministically_and_parse() {
        let artifacts = [
            ParityScorecard::canonical().to_json(),
            browser_support_matrix_json(),
            rollout_plan_json(),
            go_no_go_checklist_json(),
        ];
        let again = [
            ParityScorecard::canonical().to_json(),
            browser_support_matrix_json(),
            rollout_plan_json(),
            go_no_go_checklist_json(),
        ];
        assert_eq!(artifacts, again, "artifacts must be byte-identical");
        for artifact in &artifacts {
            let parsed: serde_json::Value =
                serde_json::from_str(artifact).expect("artifact JSON parses");
            assert_eq!(
                parsed["schema"].as_str(),
                Some(RELEASE_READINESS_SCHEMA_VERSION)
            );
            assert!(parsed["artifact"].as_str().is_some());
        }
    }
}
