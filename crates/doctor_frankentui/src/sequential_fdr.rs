//! Sequential multiple-testing control with e-BH + alpha-investing wealth
//! management (bd-3bxhj.10.39).
//!
//! The OpenTUI→FrankenTUI migration pipeline evaluates *many* hypotheses over
//! time and across asynchronous gate families (quality regressions, performance
//! claims, accessibility parity, determinism checks). Classical fixed-horizon
//! p-value logic breaks under optional stopping and arbitrary asynchronous gate
//! order. This module provides anytime-valid, multi-hypothesis control that
//! preserves statistical validity while staying operationally practical and
//! fully deterministic.
//!
//! # Null orientation (read this first)
//!
//! Each *test* is a **claim that the migration is safe on one dimension**
//! (e.g. "rendering is determinism-equivalent", "no p99 latency regression").
//! The null is the *pessimistic* hypothesis:
//!
//! - `H0`: the migration is **unsafe / regressed** on this dimension.
//! - A large **e-value** is strong evidence the dimension is **safe**.
//! - **Rejecting `H0`** (a "discovery") means **certifying the dimension safe**.
//! - **False Discovery Rate** therefore bounds the fraction of dimensions that
//!   were *certified safe but are actually unsafe* — exactly the quantity a
//!   release gate must control.
//!
//! Under this orientation, **conservative == non-permissive**: when evidence
//! budget is exhausted or evidence is adversarially noisy, the safe action is to
//! **withhold certification** (block promotion), never to silently pass. This is
//! the direction every fallback in this module takes.
//!
//! # Pipeline (`evalue -> ebh -> invest -> govern`)
//!
//! A deterministic four-stage pipeline mirroring the crate's
//! [`crate::voi_probe_planner`] idiom:
//!
//! - **evalue**: construct a per-test e-value `e >= 0` (with `E[e] <= 1` under
//!   `H0`). Either supplied directly, or built from an observation stream by
//!   reusing [`crate::guarantee_layer::run_eprocess`] (terminal wealth `W_T` is a
//!   valid e-value). Implausibly-large e-values are clamped to a sane cap and the
//!   test is flagged **adversarial** (a conservative, surfaced event).
//! - **ebh**: the **primary** controller — **e-BH** (Wang & Ramdas, 2022) over
//!   all currently-pending tests. e-BH controls FDR `<= alpha` under *arbitrary*
//!   dependence, so it is valid no matter the asynchronous execution order of the
//!   gate families.
//! - **invest**: the **secondary** online controller — a per-family Foster–Stine
//!   **alpha-investing** wealth process. Each family maintains a wealth ledger; a
//!   test is certified online iff its e-value clears the wealth-derived
//!   threshold; wealth is spent on every test and earned back on certifications.
//!   When a family's wealth is exhausted (or the evidence is adversarial), the
//!   test is forced to a **conservative withhold**.
//! - **govern**: per-family wealth governance — clamp policy, a
//!   conservation-preserving fairness **transfer** that prevents low-volume
//!   families from being starved, and conservative-mode accounting that is
//!   **never silent**.
//!
//! # Authoritative certification
//!
//! The **e-BH** reject set is the FDR-controlled certified-safe set
//! (`E[V/R] <= alpha`). The alpha-investing layer is an online **conservative
//! governor**: it can only *withhold* an e-BH certification (when a family is
//! wealth-exhausted or its evidence is adversarial), never add one. Withholding
//! removes certifications, so the absolute number of false certifications `V`
//! only decreases — the combined behavior is never less safe than e-BH. This
//! module therefore reports both `ebh_certified` (the FDR-controlled set) and
//! `final_certified` (after conservative withholds), and never claims the
//! post-override set itself controls FDR at `alpha` (it controls a quantity no
//! larger in absolute false certifications).
//!
//! # Acceptance criteria
//!
//! - **AC1** (validity under optional stopping + async order): e-BH is the
//!   primary controller and is valid under arbitrary dependence; the per-family
//!   alpha-investing wealth process composes safely.
//! - **AC2** (deterministic, replay-identical): there is no RNG. Tests are sorted
//!   into a canonical order; the wealth recursion is pure arithmetic over that
//!   order; the ledger is **float-free** (every numeric term is a fixed-decimal
//!   string) so it derives [`Eq`] and replays byte-identically.
//! - **AC3** (budget depletion -> explicit conservative mode, never silent):
//!   every wealth-exhaustion and adversarial-evidence event is surfaced as a
//!   conservative ledger line, and the gate fails closed if any is silent.
//! - **AC4** (machine-checkable clauses): every certify/withhold line carries
//!   `evalue`, `rejection_threshold`, `wealth_before`, `wealth_after`,
//!   `family`, and a `clause_consistent` flag verifying the decision matches the
//!   arithmetic.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::guarantee_layer::{EProcessConfig, run_eprocess};

/// Schema version for the in-memory sequential-FDR report.
pub const SEQUENTIAL_FDR_SCHEMA_VERSION: &str = "sequential-fdr-v1";

/// Schema version for the materialized sequential-FDR pipeline artifacts.
pub const SEQUENTIAL_FDR_PIPELINE_SCHEMA_VERSION: &str = "sequential-fdr-pipeline-v1";

/// Numeric epsilon for wealth/threshold comparisons.
const EPS: f64 = 1e-9;

// ── Hashing helpers (mirrors the crate's deterministic-stack idiom) ──────────

fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

/// Deterministic fixed-decimal rendering. Keeps the ledger float-free (so it
/// derives `Eq`) while preserving six fractional digits of precision. `-0.0` and
/// non-finite values are normalized so the ledger replays byte-identically.
fn fmt6(x: f64) -> String {
    if !x.is_finite() {
        return "0.000000".to_string();
    }
    let rendered = format!("{x:.6}");
    if rendered == "-0.000000" {
        "0.000000".to_string()
    } else {
        rendered
    }
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// A migration gate family. Tests arrive into a family asynchronously and each
/// family owns an independent alpha-investing wealth ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateFamily {
    /// Rendering/behavioral quality-regression checks.
    Quality,
    /// Latency/throughput performance claims.
    Performance,
    /// Accessibility parity claims.
    Accessibility,
    /// Determinism / shadow-run equivalence checks.
    Determinism,
}

impl GateFamily {
    /// All families in canonical order.
    pub const ALL: [GateFamily; 4] = [
        GateFamily::Quality,
        GateFamily::Performance,
        GateFamily::Accessibility,
        GateFamily::Determinism,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quality => "quality",
            Self::Performance => "performance",
            Self::Accessibility => "accessibility",
            Self::Determinism => "determinism",
        }
    }
}

/// A pipeline stage (one per ledger line family).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FdrStage {
    /// Per-test e-value construction (direct or from an observation stream).
    Evalue,
    /// Batch e-BH rejection decision over all pending tests.
    Ebh,
    /// Per-test online alpha-investing wealth update (per family).
    Invest,
    /// Per-family wealth governance + conservative-mode accounting.
    Govern,
}

impl FdrStage {
    /// All stages in canonical (pipeline) order.
    pub const ALL: [FdrStage; 4] = [
        FdrStage::Evalue,
        FdrStage::Ebh,
        FdrStage::Invest,
        FdrStage::Govern,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evalue => "evalue",
            Self::Ebh => "ebh",
            Self::Invest => "invest",
            Self::Govern => "govern",
        }
    }
}

/// Which controller a ledger line reflects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FdrProfile {
    /// e-BH batch controller (primary; FDR `<= alpha` under arbitrary dependence).
    EBenjaminiHochberg,
    /// Foster–Stine alpha-investing online controller (secondary; mFDR).
    AlphaInvesting,
    /// No controller applies to this line (evalue / govern lines).
    NotApplicable,
}

impl FdrProfile {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EBenjaminiHochberg => "e_benjamini_hochberg",
            Self::AlphaInvesting => "alpha_investing",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// The decision for a test on a given line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestDecision {
    /// Reject `H0` — certify the dimension safe (a discovery).
    Certify,
    /// Retain `H0` — withhold certification (the conservative default).
    Withhold,
    /// The test input was structurally malformed and fails the gate.
    Invalid,
    /// No decision applies to this line (evalue / govern lines).
    NotApplicable,
}

impl TestDecision {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Certify => "certify",
            Self::Withhold => "withhold",
            Self::Invalid => "invalid",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// Why a test was forced to a conservative withhold (`not_conservative` for the
/// normal path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConservativeReason {
    /// Not a conservative event (normal certify/withhold).
    NotConservative,
    /// The family's alpha-investing wealth was exhausted when this test arrived.
    WealthExhausted,
    /// The evidence was adversarially noisy (e-value clamped or out-of-range
    /// observations): the test is withheld and surfaced.
    AdversarialEvidence,
}

impl ConservativeReason {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotConservative => "not_conservative",
            Self::WealthExhausted => "wealth_exhausted",
            Self::AdversarialEvidence => "adversarial_evidence",
        }
    }

    /// Whether this is a (surfaced) conservative event.
    #[must_use]
    pub fn is_conservative(self) -> bool {
        !matches!(self, Self::NotConservative)
    }
}

/// How a test's e-value was sourced (for the evalue line).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A directly-supplied e-value.
    Direct,
    /// An e-value built from an observation stream via the e-process.
    Observations,
}

impl EvidenceKind {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Observations => "observations",
        }
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// The evidence backing a test's e-value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EvidenceSource {
    /// A directly-supplied e-value `e >= 0` (`E[e] <= 1` under `H0`).
    Evalue(f64),
    /// A sequential stream of per-step **safety indicators** in `[0, 1]` (high =
    /// safe). Run through [`run_eprocess`] (testing `H0: mean <= mu0`), the
    /// terminal wealth `W_T` is the test's e-value.
    Observations(Vec<f64>),
}

/// A single hypothesis test scoped to a gate family, in arrival order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FdrTest {
    /// Stable identifier of the test.
    pub test_id: String,
    /// The gate family the test belongs to.
    pub family: GateFamily,
    /// Online arrival index within the stream (canonical tie-break).
    pub arrival_index: u32,
    /// The evidence backing the e-value.
    pub evidence: EvidenceSource,
}

impl FdrTest {
    /// Construct a test with a directly-supplied e-value.
    #[must_use]
    pub fn with_evalue(
        test_id: impl Into<String>,
        family: GateFamily,
        arrival_index: u32,
        evalue: f64,
    ) -> Self {
        Self {
            test_id: test_id.into(),
            family,
            arrival_index,
            evidence: EvidenceSource::Evalue(evalue),
        }
    }

    /// Construct a test whose e-value is built from an observation stream.
    #[must_use]
    pub fn with_observations(
        test_id: impl Into<String>,
        family: GateFamily,
        arrival_index: u32,
        observations: Vec<f64>,
    ) -> Self {
        Self {
            test_id: test_id.into(),
            family,
            arrival_index,
            evidence: EvidenceSource::Observations(observations),
        }
    }

    /// Validate structural well-formedness; returns the first failing reason.
    #[must_use]
    fn validate(&self) -> Option<&'static str> {
        if self.test_id.trim().is_empty() {
            return Some("empty test_id");
        }
        match &self.evidence {
            EvidenceSource::Evalue(e) => {
                if !e.is_finite() || *e < 0.0 {
                    return Some("e-value must be finite and non-negative");
                }
            }
            EvidenceSource::Observations(obs) => {
                if obs.is_empty() {
                    return Some("observation stream must be non-empty");
                }
                if obs.iter().any(|o| !o.is_finite()) {
                    return Some("observation stream contains a non-finite value");
                }
            }
        }
        None
    }
}

/// Sequential-FDR controller configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SequentialFdrConfig {
    /// Target FDR level for the e-BH controller (and the alpha-investing budget).
    pub alpha: f64,
    /// Sane cap for e-values; an e-value above this is treated as adversarial and
    /// clamped to the cap (prevents one "infinite certainty" claim dominating).
    pub evalue_cap: f64,
    /// Initial alpha-investing wealth per family (`<= alpha` for the mFDR
    /// guarantee).
    pub initial_wealth: f64,
    /// Fraction of current wealth invested per test (`gamma` in `(0, 1)`).
    pub invest_fraction: f64,
    /// Alpha-investing payout earned per online certification (`omega <= alpha`).
    pub reward: f64,
    /// Wealth at or below which a family is exhausted (cannot invest -> withhold).
    pub wealth_floor: f64,
    /// Upper clamp on a family's wealth (clamp policy; prevents unbounded growth).
    pub wealth_cap: f64,
    /// A family whose end-of-round wealth is below this is "starved" and eligible
    /// for a fairness transfer.
    pub starvation_floor: f64,
    /// `mu0` for the e-process used to build e-values from observation streams.
    pub eprocess_mu0: f64,
}

impl Default for SequentialFdrConfig {
    fn default() -> Self {
        Self {
            alpha: 0.10,
            evalue_cap: 1_000_000.0,
            initial_wealth: 0.05,
            invest_fraction: 0.5,
            reward: 0.10,
            wealth_floor: 1e-3,
            wealth_cap: 1.0,
            starvation_floor: 0.01,
            eprocess_mu0: 0.10,
        }
    }
}

impl SequentialFdrConfig {
    /// Set the target FDR level.
    #[must_use]
    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha;
        self
    }

    /// Set the per-family initial wealth.
    #[must_use]
    pub fn with_initial_wealth(mut self, initial_wealth: f64) -> Self {
        self.initial_wealth = initial_wealth;
        self
    }

    /// Set the e-value adversarial clamp cap.
    #[must_use]
    pub fn with_evalue_cap(mut self, evalue_cap: f64) -> Self {
        self.evalue_cap = evalue_cap;
        self
    }

    /// Set the wealth floor (exhaustion threshold).
    #[must_use]
    pub fn with_wealth_floor(mut self, wealth_floor: f64) -> Self {
        self.wealth_floor = wealth_floor;
        self
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One line of the sequential-FDR evidence ledger.
///
/// Float-free (numeric terms are fixed-decimal `String`s), so the line derives
/// `Eq` and serializes byte-identically across runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequentialFdrLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id (stable for fixed tests/config).
    pub run_id: String,
    /// The pipeline stage this line belongs to.
    pub stage: FdrStage,
    /// The controller this line reflects.
    pub profile: FdrProfile,
    /// Test id (or `family:<family>` on govern lines).
    pub test_id: String,
    /// The gate family this line concerns.
    pub family: GateFamily,
    /// Evidence kind (`None` on non-evalue lines).
    pub evidence_kind: Option<EvidenceKind>,
    /// The decision on this line.
    pub decision: TestDecision,
    /// Conservative reason (`not_conservative` for the normal path).
    pub conservative_reason: ConservativeReason,
    /// The (clamped) e-value (fixed-decimal).
    pub evalue: String,
    /// The rejection threshold the e-value is compared against (fixed-decimal):
    /// the e-BH cutoff on ebh lines, `1/alpha_i` on invest lines, `0` otherwise.
    pub rejection_threshold: String,
    /// Wealth before this test (fixed-decimal; invest/govern lines).
    pub wealth_before: String,
    /// Wealth after this test (fixed-decimal; invest/govern lines).
    pub wealth_after: String,
    /// Alpha invested at this test (fixed-decimal; invest lines).
    pub alpha_invested: String,
    /// Wealth transferred *into* this family (fixed-decimal; govern lines).
    pub transfer_in: String,
    /// Wealth transferred *out of* this family (fixed-decimal; govern lines).
    pub transfer_out: String,
    /// 1-based rank of this test in the e-BH ordering (ebh lines; `0` otherwise).
    pub ebh_rank: u32,
    /// e-BH `k*` (number of rejections; ebh lines repeat the global value).
    pub ebh_k_star: u32,
    /// Total tests considered by e-BH (ebh lines repeat the global value).
    pub ebh_total: u32,
    /// Whether the decision is consistent with the recorded arithmetic (AC4).
    pub clause_consistent: bool,
    /// Human-readable decision clause / detail.
    pub detail: String,
    /// Remediation command set for surfaced defects (empty when clean).
    pub remediation: Vec<String>,
    /// Deterministic command to reproduce this stage.
    pub reproduction_command: String,
}

/// Whether a ledger line has every mandatory field populated.
fn required_fields_present(line: &SequentialFdrLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.test_id.is_empty()
        && !line.evalue.is_empty()
        && !line.rejection_threshold.is_empty()
        && !line.wealth_before.is_empty()
        && !line.wealth_after.is_empty()
        && !line.alpha_invested.is_empty()
        && !line.transfer_in.is_empty()
        && !line.transfer_out.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[SequentialFdrLedgerEntry]) -> String {
    let mut out = String::new();
    for entry in ledger {
        match serde_json::to_string(entry) {
            Ok(line) => out.push_str(&line),
            Err(error) => out.push_str(&error.to_string()),
        }
        out.push('\n');
    }
    out
}

/// Grouped fields for assembling one ledger line (keeps helper arity low).
struct LineParts {
    stage: FdrStage,
    profile: FdrProfile,
    test_id: String,
    family: GateFamily,
    evidence_kind: Option<EvidenceKind>,
    decision: TestDecision,
    conservative_reason: ConservativeReason,
    evalue: f64,
    rejection_threshold: f64,
    wealth_before: f64,
    wealth_after: f64,
    alpha_invested: f64,
    transfer_in: f64,
    transfer_out: f64,
    ebh_rank: u32,
    ebh_k_star: u32,
    ebh_total: u32,
    clause_consistent: bool,
    detail: String,
    remediation: Vec<String>,
}

// ── Internal scoring / planning ──────────────────────────────────────────────

/// A test with its constructed e-value (computed once, reused across stages).
#[derive(Debug, Clone)]
struct Scored {
    test: FdrTest,
    evidence_kind: EvidenceKind,
    /// The constructed e-value *before* the adversarial clamp.
    raw_evalue: f64,
    /// The e-value used by the controllers (clamped to `evalue_cap`).
    evalue: f64,
    /// Whether the e-value was clamped or the evidence was flagged adversarial.
    adversarial: bool,
    /// A human-readable note about how the e-value was sourced.
    source_note: String,
    valid: bool,
    invalid_reason: Option<&'static str>,
}

/// The e-BH batch outcome.
#[derive(Debug, Clone)]
struct EbhOutcome {
    /// `certified[i]` for each scored test (input order).
    certified: Vec<bool>,
    /// 1-based e-BH rank for each scored test (input order).
    rank: Vec<u32>,
    /// `k*` (number of rejections).
    k_star: usize,
    /// The e-value cutoff: certify iff `evalue >= threshold`.
    threshold: f64,
    /// Total valid tests considered.
    total: usize,
}

/// The per-test online alpha-investing outcome.
#[derive(Debug, Clone)]
struct InvestStep {
    wealth_before: f64,
    alpha_invested: f64,
    threshold: f64,
    online_certified: bool,
    conservative_reason: ConservativeReason,
    wealth_after: f64,
}

/// Per-family governance + wealth trajectory.
#[derive(Debug, Clone)]
struct FamilyGovernance {
    family: GateFamily,
    tests: usize,
    online_certified: usize,
    online_withheld: usize,
    conservative_count: usize,
    exhausted_count: usize,
    adversarial_count: usize,
    wealth_start: f64,
    /// Wealth after all online tests, before the fairness transfer.
    wealth_pre_transfer: f64,
    transfer_in: f64,
    transfer_out: f64,
    /// Wealth handed to the next round (post-transfer, clamped).
    wealth_end: f64,
}

/// The full planning outcome.
#[derive(Debug, Clone)]
struct PlanOutcome {
    ebh: EbhOutcome,
    invest: Vec<InvestStep>,
    /// Final per-test conservative reason (adversarial dominates exhaustion).
    conservative_reason: Vec<ConservativeReason>,
    /// `final_certified[i]` = e-BH certified AND not conservatively overridden.
    final_certified: Vec<bool>,
    governance: Vec<FamilyGovernance>,
    invalid: usize,
    adversarial: usize,
    ebh_certified_count: usize,
    final_certified_count: usize,
    conservative_overrides: usize,
    wealth_conserved: bool,
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The sequential-FDR controller engine.
#[derive(Debug, Clone)]
pub struct SequentialFdrController {
    run_id: String,
    label: String,
    config: SequentialFdrConfig,
    /// Tests in canonical `(family, arrival_index, test_id)` order.
    tests: Vec<FdrTest>,
}

impl SequentialFdrController {
    /// Construct a controller over a test stream. Tests are sorted into a
    /// canonical order so the decisions are deterministic regardless of input
    /// order.
    #[must_use]
    pub fn new(label: impl Into<String>, config: SequentialFdrConfig, tests: Vec<FdrTest>) -> Self {
        let label = label.into();
        let mut tests = tests;
        tests.sort_by(|a, b| {
            a.family
                .cmp(&b.family)
                .then(a.arrival_index.cmp(&b.arrival_index))
                .then(a.test_id.cmp(&b.test_id))
        });

        #[derive(Serialize)]
        struct IdSeed<'a> {
            label: &'a str,
            alpha: String,
            initial_wealth: String,
            test_ids: Vec<&'a str>,
            families: Vec<&'a str>,
            arrivals: Vec<u32>,
        }
        let id_seed = stable_hash(&IdSeed {
            label: &label,
            alpha: fmt6(config.alpha),
            initial_wealth: fmt6(config.initial_wealth),
            test_ids: tests.iter().map(|t| t.test_id.as_str()).collect(),
            families: tests.iter().map(|t| t.family.as_str()).collect(),
            arrivals: tests.iter().map(|t| t.arrival_index).collect(),
        });
        let run_id = format!("seq-fdr-{}", short_hash(&id_seed));

        Self {
            run_id,
            label,
            config,
            tests,
        }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn reproduction_command(&self, stage: FdrStage) -> String {
        format!(
            "doctor_frankentui --machine json sequential-fdr --stage {} --label {}",
            stage.as_str(),
            self.label
        )
    }

    /// Construct the e-value for every test (the `evalue` stage logic).
    fn score_all(&self) -> Vec<Scored> {
        self.tests.iter().map(|test| self.score_one(test)).collect()
    }

    fn score_one(&self, test: &FdrTest) -> Scored {
        let invalid_reason = test.validate();
        let (evidence_kind, raw_evalue, mut adversarial, mut source_note, valid, invalid_reason) =
            match (&test.evidence, invalid_reason) {
                (_, Some(reason)) => (
                    match &test.evidence {
                        EvidenceSource::Evalue(_) => EvidenceKind::Direct,
                        EvidenceSource::Observations(_) => EvidenceKind::Observations,
                    },
                    0.0,
                    false,
                    format!("invalid: {reason}"),
                    false,
                    Some(reason),
                ),
                (EvidenceSource::Evalue(e), None) => (
                    EvidenceKind::Direct,
                    *e,
                    false,
                    "direct e-value".to_string(),
                    true,
                    None,
                ),
                (EvidenceSource::Observations(obs), None) => {
                    let cfg = EProcessConfig::default().with_mu0(self.config.eprocess_mu0);
                    match run_eprocess(&cfg, obs) {
                        Ok(result) => {
                            // Out-of-range observations are clamped and flagged by
                            // the e-process; treat that as adversarial evidence.
                            let flagged = !result.assumption_violations.is_empty();
                            let note = format!(
                                "e-process W_T over {} observations (mu0={})",
                                result.observation_count,
                                fmt6(self.config.eprocess_mu0)
                            );
                            (
                                EvidenceKind::Observations,
                                result.final_wealth,
                                flagged,
                                note,
                                true,
                                None,
                            )
                        }
                        Err(_) => (
                            EvidenceKind::Observations,
                            0.0,
                            false,
                            "e-process construction failed".to_string(),
                            false,
                            Some("e-process construction failed"),
                        ),
                    }
                }
            };

        // Adversarial clamp: an implausibly-large (or non-finite) e-value is
        // clamped to the cap and the test is flagged — a surfaced conservative
        // event, never silently trusted. Clamping only *reduces* the value
        // (`min(e, cap)`), so it preserves the e-value property
        // (`E[min(e, cap)] <= E[e] <= 1`) and keeps e-BH's FDR guarantee intact.
        let mut evalue = raw_evalue;
        if valid {
            if !evalue.is_finite() {
                evalue = self.config.evalue_cap;
                adversarial = true;
                source_note = format!("{source_note}; non-finite e-value clamped");
            } else if evalue > self.config.evalue_cap {
                evalue = self.config.evalue_cap;
                adversarial = true;
                source_note = format!("{source_note}; e-value exceeded cap, clamped");
            }
        } else {
            evalue = 0.0;
        }

        Scored {
            test: test.clone(),
            evidence_kind,
            raw_evalue,
            evalue,
            adversarial,
            source_note,
            valid,
            invalid_reason,
        }
    }

    /// The e-BH (Wang & Ramdas, 2022) batch decision over the valid tests.
    ///
    /// Sort e-values descending; `k* = max{ k : e_[k] >= K/(alpha*k) }`; reject
    /// the top `k*`. Equivalent to BH on `p_i = 1/e_i`, controlling FDR `<= alpha`
    /// under *arbitrary* dependence. The reject set equals `{ i : e_i >= K/(alpha*k*) }`.
    fn ebh(&self, scored: &[Scored]) -> EbhOutcome {
        let alpha = self.config.alpha;
        // Only valid tests participate in the e-BH ordering.
        let valid_idx: Vec<usize> = (0..scored.len()).filter(|&i| scored[i].valid).collect();
        let k = valid_idx.len();
        let mut certified = vec![false; scored.len()];
        let mut rank = vec![0u32; scored.len()];

        if k == 0 || alpha <= 0.0 {
            return EbhOutcome {
                certified,
                rank,
                k_star: 0,
                threshold: 0.0,
                total: 0,
            };
        }

        // Order valid tests by e-value descending; canonical-index tie-break.
        let mut order = valid_idx.clone();
        order.sort_by(|&a, &b| {
            scored[b]
                .evalue
                .total_cmp(&scored[a].evalue)
                .then(scored[a].test.test_id.cmp(&scored[b].test.test_id))
                .then(a.cmp(&b))
        });
        for (position, &idx) in order.iter().enumerate() {
            rank[idx] = u32::try_from(position + 1).unwrap_or(u32::MAX);
        }

        let kf = k as f64;
        // k* = the largest rank whose ordered e-value clears K/(alpha*rank).
        let mut k_star = 0usize;
        for (position, &idx) in order.iter().enumerate() {
            let rank_k = (position + 1) as f64;
            let cutoff = kf / (alpha * rank_k);
            if scored[idx].evalue >= cutoff {
                k_star = position + 1;
            }
        }

        // Uniform cutoff: certify iff evalue >= threshold reproduces the top-k*
        // reject set exactly (in both the k*>0 and k*==0 cases).
        let threshold = if k_star > 0 {
            kf / (alpha * k_star as f64)
        } else {
            kf / alpha
        };
        for &idx in &valid_idx {
            if scored[idx].evalue >= threshold {
                certified[idx] = true;
            }
        }

        EbhOutcome {
            certified,
            rank,
            k_star,
            threshold,
            total: k,
        }
    }

    /// The per-family Foster–Stine alpha-investing wealth process.
    ///
    /// Per family, process tests in canonical (arrival) order with wealth `W`
    /// (`W(0) = initial_wealth`). For each test allocate `alpha_i = gamma*W`
    /// (clamped to `(0, alpha]` and `<= W`); certify online iff `e_i >= 1/alpha_i`;
    /// on a certification `W <- W - alpha_i + reward`, otherwise
    /// `W <- W - alpha_i/(1 - alpha_i)`. A family at/below the wealth floor is
    /// exhausted (cannot invest) and the test is conservatively withheld;
    /// adversarial evidence is likewise withheld. Returns one step per scored
    /// test (input order).
    fn invest(&self, scored: &[Scored]) -> (Vec<InvestStep>, Vec<FamilyGovernance>) {
        let cfg = &self.config;
        let mut steps: Vec<InvestStep> = scored
            .iter()
            .map(|_| InvestStep {
                wealth_before: 0.0,
                alpha_invested: 0.0,
                threshold: 0.0,
                online_certified: false,
                conservative_reason: ConservativeReason::NotConservative,
                wealth_after: 0.0,
            })
            .collect();

        let mut governance = Vec::new();
        for family in GateFamily::ALL {
            // Valid tests of this family, already in canonical order.
            let members: Vec<usize> = (0..scored.len())
                .filter(|&i| scored[i].valid && scored[i].test.family == family)
                .collect();
            if members.is_empty() {
                continue;
            }
            let mut wealth = cfg.initial_wealth;
            let wealth_start = wealth;
            let mut online_certified = 0usize;
            let mut online_withheld = 0usize;
            let mut exhausted_count = 0usize;
            let mut adversarial_count = 0usize;
            for &i in &members {
                let s = &scored[i];
                let wealth_before = wealth;
                let exhausted = wealth_before <= cfg.wealth_floor + EPS;
                let alpha_i = if exhausted {
                    0.0
                } else {
                    (cfg.invest_fraction * wealth_before)
                        .min(cfg.alpha)
                        .min(wealth_before)
                };
                let threshold = if alpha_i > EPS { 1.0 / alpha_i } else { 0.0 };

                let (online_certified_i, reason, wealth_next) = if exhausted {
                    exhausted_count += 1;
                    (false, ConservativeReason::WealthExhausted, wealth_before)
                } else if s.adversarial {
                    adversarial_count += 1;
                    // Conservative withhold; spend the retain cost.
                    let next = wealth_before - alpha_i / (1.0 - alpha_i);
                    (false, ConservativeReason::AdversarialEvidence, next)
                } else if s.evalue >= threshold {
                    // Online certification: earn the payout.
                    let next = wealth_before - alpha_i + cfg.reward;
                    (true, ConservativeReason::NotConservative, next)
                } else {
                    // Normal retain: pay the investment cost.
                    let next = wealth_before - alpha_i / (1.0 - alpha_i);
                    (false, ConservativeReason::NotConservative, next)
                };

                // Clamp policy: keep wealth in [0, wealth_cap].
                wealth = wealth_next.clamp(0.0, cfg.wealth_cap);
                if online_certified_i {
                    online_certified += 1;
                } else {
                    online_withheld += 1;
                }
                steps[i] = InvestStep {
                    wealth_before,
                    alpha_invested: alpha_i,
                    threshold,
                    online_certified: online_certified_i,
                    conservative_reason: reason,
                    wealth_after: wealth,
                };
            }
            governance.push(FamilyGovernance {
                family,
                tests: members.len(),
                online_certified,
                online_withheld,
                conservative_count: exhausted_count + adversarial_count,
                exhausted_count,
                adversarial_count,
                wealth_start,
                wealth_pre_transfer: wealth,
                transfer_in: 0.0,
                transfer_out: 0.0,
                wealth_end: wealth,
            });
        }
        (steps, governance)
    }

    /// Conservation-preserving fairness transfer: starved families (end wealth
    /// below the starvation floor) receive a bounded transfer from the richest
    /// donor (end wealth above `2*initial_wealth`). Total wealth is conserved, so
    /// the global alpha-investing budget is preserved while low-volume families
    /// are not permanently starved. Deterministic (canonical family order).
    fn apply_fairness_transfers(&self, governance: &mut [FamilyGovernance]) -> bool {
        let cfg = &self.config;
        let donor_threshold = 2.0 * cfg.initial_wealth;
        let total_before: f64 = governance.iter().map(|g| g.wealth_pre_transfer).sum();

        // Canonical order: families are already in GateFamily::ALL order.
        for starved in 0..governance.len() {
            if governance[starved].wealth_end >= cfg.starvation_floor - EPS {
                continue;
            }
            let deficit = cfg.starvation_floor - governance[starved].wealth_end;
            if deficit <= EPS {
                continue;
            }
            // Richest eligible donor (deterministic: max wealth, family tie-break).
            let donor = governance
                .iter()
                .enumerate()
                .filter(|(j, g)| *j != starved && g.wealth_end > donor_threshold + EPS)
                .max_by(|(ja, a), (jb, b)| a.wealth_end.total_cmp(&b.wealth_end).then(jb.cmp(ja)))
                .map(|(j, _)| j);
            let Some(donor) = donor else { continue };
            let donor_excess = governance[donor].wealth_end - donor_threshold;
            let transfer = deficit.min(donor_excess).max(0.0);
            if transfer <= EPS {
                continue;
            }
            governance[donor].wealth_end -= transfer;
            governance[donor].transfer_out += transfer;
            governance[starved].wealth_end += transfer;
            governance[starved].transfer_in += transfer;
        }

        let total_after: f64 = governance.iter().map(|g| g.wealth_end).sum();
        (total_before - total_after).abs() <= 1e-6
    }

    #[allow(clippy::too_many_lines)]
    fn plan(&self, scored: &[Scored]) -> PlanOutcome {
        let ebh = self.ebh(scored);
        let (invest, mut governance) = self.invest(scored);
        let wealth_conserved = self.apply_fairness_transfers(&mut governance);

        // Final per-test conservative reason (adversarial dominates exhaustion).
        let conservative_reason: Vec<ConservativeReason> = scored
            .iter()
            .zip(&invest)
            .map(|(s, step)| {
                if s.adversarial {
                    ConservativeReason::AdversarialEvidence
                } else {
                    step.conservative_reason
                }
            })
            .collect();

        // Authoritative certification: e-BH certified AND not conservatively
        // overridden. Conservative events can only *withhold*, never add.
        let final_certified: Vec<bool> = (0..scored.len())
            .map(|i| ebh.certified[i] && !conservative_reason[i].is_conservative())
            .collect();

        let invalid = scored.iter().filter(|s| !s.valid).count();
        let adversarial = scored.iter().filter(|s| s.adversarial).count();
        let ebh_certified_count = ebh.certified.iter().filter(|&&c| c).count();
        let final_certified_count = final_certified.iter().filter(|&&c| c).count();
        let conservative_overrides = (0..scored.len())
            .filter(|&i| ebh.certified[i] && !final_certified[i])
            .count();

        PlanOutcome {
            ebh,
            invest,
            conservative_reason,
            final_certified,
            governance,
            invalid,
            adversarial,
            ebh_certified_count,
            final_certified_count,
            conservative_overrides,
            wealth_conserved,
        }
    }

    fn assemble(&self, parts: LineParts) -> SequentialFdrLedgerEntry {
        SequentialFdrLedgerEntry {
            schema_version: SEQUENTIAL_FDR_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            stage: parts.stage,
            profile: parts.profile,
            test_id: parts.test_id,
            family: parts.family,
            evidence_kind: parts.evidence_kind,
            decision: parts.decision,
            conservative_reason: parts.conservative_reason,
            evalue: fmt6(parts.evalue),
            rejection_threshold: fmt6(parts.rejection_threshold),
            wealth_before: fmt6(parts.wealth_before),
            wealth_after: fmt6(parts.wealth_after),
            alpha_invested: fmt6(parts.alpha_invested),
            transfer_in: fmt6(parts.transfer_in),
            transfer_out: fmt6(parts.transfer_out),
            ebh_rank: parts.ebh_rank,
            ebh_k_star: parts.ebh_k_star,
            ebh_total: parts.ebh_total,
            clause_consistent: parts.clause_consistent,
            detail: parts.detail,
            remediation: parts.remediation,
            reproduction_command: self.reproduction_command(parts.stage),
        }
    }

    fn evalue_lines(&self, scored: &[Scored]) -> Vec<SequentialFdrLedgerEntry> {
        scored
            .iter()
            .map(|s| {
                let detail = if let Some(reason) = s.invalid_reason {
                    format!("invalid test: {reason}")
                } else if s.adversarial {
                    format!(
                        "adversarial evidence: raw={} clamped={} ({})",
                        fmt6(s.raw_evalue),
                        fmt6(s.evalue),
                        s.source_note
                    )
                } else {
                    format!("evalue={} ({})", fmt6(s.evalue), s.source_note)
                };
                let decision = if s.valid {
                    TestDecision::NotApplicable
                } else {
                    TestDecision::Invalid
                };
                let conservative_reason = if s.adversarial {
                    ConservativeReason::AdversarialEvidence
                } else {
                    ConservativeReason::NotConservative
                };
                let remediation = if !s.valid {
                    vec![format!("fix malformed test input {}", s.test.test_id)]
                } else if s.adversarial {
                    vec![format!(
                        "investigate adversarial evidence for {} (e-value clamped to cap)",
                        s.test.test_id
                    )]
                } else {
                    Vec::new()
                };
                self.assemble(LineParts {
                    stage: FdrStage::Evalue,
                    profile: FdrProfile::NotApplicable,
                    test_id: s.test.test_id.clone(),
                    family: s.test.family,
                    evidence_kind: Some(s.evidence_kind),
                    decision,
                    conservative_reason,
                    evalue: s.evalue,
                    rejection_threshold: 0.0,
                    wealth_before: 0.0,
                    wealth_after: 0.0,
                    alpha_invested: 0.0,
                    transfer_in: 0.0,
                    transfer_out: 0.0,
                    ebh_rank: 0,
                    ebh_k_star: 0,
                    ebh_total: 0,
                    clause_consistent: true,
                    detail,
                    remediation,
                })
            })
            .collect()
    }

    fn ebh_lines(&self, scored: &[Scored], plan: &PlanOutcome) -> Vec<SequentialFdrLedgerEntry> {
        let ebh = &plan.ebh;
        scored
            .iter()
            .enumerate()
            .filter(|(_, s)| s.valid)
            .map(|(i, s)| {
                let certified = ebh.certified[i];
                let decision = if certified {
                    TestDecision::Certify
                } else {
                    TestDecision::Withhold
                };
                // AC4: certify iff e-value clears the e-BH cutoff.
                let clause_consistent = certified == (s.evalue >= ebh.threshold);
                let detail = format!(
                    "{}: evalue={} {} threshold={} (e-BH rank={}, k*={}, K={}, alpha={})",
                    decision.as_str(),
                    fmt6(s.evalue),
                    if certified { ">=" } else { "<" },
                    fmt6(ebh.threshold),
                    ebh.rank[i],
                    ebh.k_star,
                    ebh.total,
                    fmt6(self.config.alpha)
                );
                self.assemble(LineParts {
                    stage: FdrStage::Ebh,
                    profile: FdrProfile::EBenjaminiHochberg,
                    test_id: s.test.test_id.clone(),
                    family: s.test.family,
                    evidence_kind: None,
                    decision,
                    conservative_reason: ConservativeReason::NotConservative,
                    evalue: s.evalue,
                    rejection_threshold: ebh.threshold,
                    wealth_before: 0.0,
                    wealth_after: 0.0,
                    alpha_invested: 0.0,
                    transfer_in: 0.0,
                    transfer_out: 0.0,
                    ebh_rank: ebh.rank[i],
                    ebh_k_star: u32::try_from(ebh.k_star).unwrap_or(u32::MAX),
                    ebh_total: u32::try_from(ebh.total).unwrap_or(u32::MAX),
                    clause_consistent,
                    detail,
                    remediation: Vec::new(),
                })
            })
            .collect()
    }

    fn invest_lines(&self, scored: &[Scored], plan: &PlanOutcome) -> Vec<SequentialFdrLedgerEntry> {
        scored
            .iter()
            .enumerate()
            .filter(|(_, s)| s.valid)
            .map(|(i, s)| {
                let step = &plan.invest[i];
                let reason = step.conservative_reason;
                let decision = if step.online_certified {
                    TestDecision::Certify
                } else {
                    TestDecision::Withhold
                };
                // AC4: certify iff (alpha_i > 0, not conservative, e >= 1/alpha_i).
                let arithmetic_certify = reason == ConservativeReason::NotConservative
                    && step.alpha_invested > EPS
                    && s.evalue >= step.threshold;
                let clause_consistent = step.online_certified == arithmetic_certify;
                let detail = match reason {
                    ConservativeReason::WealthExhausted => format!(
                        "withhold (wealth exhausted): wealth_before={} <= floor={}",
                        fmt6(step.wealth_before),
                        fmt6(self.config.wealth_floor)
                    ),
                    ConservativeReason::AdversarialEvidence => format!(
                        "withhold (adversarial evidence): evalue={} not trusted; wealth_after={}",
                        fmt6(s.evalue),
                        fmt6(step.wealth_after)
                    ),
                    ConservativeReason::NotConservative => format!(
                        "{}: evalue={} {} threshold=1/alpha_i={}; wealth {}->{}",
                        decision.as_str(),
                        fmt6(s.evalue),
                        if step.online_certified { ">=" } else { "<" },
                        fmt6(step.threshold),
                        fmt6(step.wealth_before),
                        fmt6(step.wealth_after)
                    ),
                };
                let remediation = match reason {
                    ConservativeReason::WealthExhausted => vec![format!(
                        "raise initial_wealth / reward for family {} (wealth exhausted)",
                        s.test.family.as_str()
                    )],
                    ConservativeReason::AdversarialEvidence => vec![format!(
                        "investigate adversarial evidence for {}",
                        s.test.test_id
                    )],
                    ConservativeReason::NotConservative => Vec::new(),
                };
                self.assemble(LineParts {
                    stage: FdrStage::Invest,
                    profile: FdrProfile::AlphaInvesting,
                    test_id: s.test.test_id.clone(),
                    family: s.test.family,
                    evidence_kind: None,
                    decision,
                    conservative_reason: reason,
                    evalue: s.evalue,
                    rejection_threshold: step.threshold,
                    wealth_before: step.wealth_before,
                    wealth_after: step.wealth_after,
                    alpha_invested: step.alpha_invested,
                    transfer_in: 0.0,
                    transfer_out: 0.0,
                    ebh_rank: 0,
                    ebh_k_star: 0,
                    ebh_total: 0,
                    clause_consistent,
                    detail,
                    remediation,
                })
            })
            .collect()
    }

    fn govern_lines(&self, plan: &PlanOutcome) -> Vec<SequentialFdrLedgerEntry> {
        plan.governance
            .iter()
            .map(|g| {
                let conservative = g.conservative_count > 0;
                let detail = format!(
                    "family={} tests={} online_certified={} withheld={} conservative={} (exhausted={}, adversarial={}); wealth {}->{} (transfer_in={}, transfer_out={})",
                    g.family.as_str(),
                    g.tests,
                    g.online_certified,
                    g.online_withheld,
                    g.conservative_count,
                    g.exhausted_count,
                    g.adversarial_count,
                    fmt6(g.wealth_start),
                    fmt6(g.wealth_end),
                    fmt6(g.transfer_in),
                    fmt6(g.transfer_out)
                );
                let remediation = if conservative {
                    vec![format!(
                        "family {} hit conservative mode {}x; raise budget or investigate evidence",
                        g.family.as_str(),
                        g.conservative_count
                    )]
                } else {
                    Vec::new()
                };
                self.assemble(LineParts {
                    stage: FdrStage::Govern,
                    profile: FdrProfile::NotApplicable,
                    test_id: format!("family:{}", g.family.as_str()),
                    family: g.family,
                    evidence_kind: None,
                    decision: TestDecision::NotApplicable,
                    conservative_reason: if conservative {
                        // Surface the dominant reason for the family.
                        if g.adversarial_count >= g.exhausted_count {
                            ConservativeReason::AdversarialEvidence
                        } else {
                            ConservativeReason::WealthExhausted
                        }
                    } else {
                        ConservativeReason::NotConservative
                    },
                    evalue: 0.0,
                    rejection_threshold: 0.0,
                    wealth_before: g.wealth_start,
                    wealth_after: g.wealth_end,
                    alpha_invested: 0.0,
                    transfer_in: g.transfer_in,
                    transfer_out: g.transfer_out,
                    ebh_rank: 0,
                    ebh_k_star: 0,
                    ebh_total: 0,
                    clause_consistent: true,
                    detail,
                    remediation,
                })
            })
            .collect()
    }

    fn full_ledger(&self, scored: &[Scored], plan: &PlanOutcome) -> Vec<SequentialFdrLedgerEntry> {
        let mut ledger = Vec::new();
        ledger.extend(self.evalue_lines(scored));
        ledger.extend(self.ebh_lines(scored, plan));
        ledger.extend(self.invest_lines(scored, plan));
        ledger.extend(self.govern_lines(plan));
        ledger
    }

    /// Build a report for a stage view (`None` = all stages; the gate applies).
    #[must_use]
    pub fn run(&self, stage: Option<FdrStage>) -> SequentialFdrReport {
        let scored = self.score_all();
        let plan = self.plan(&scored);
        let full = self.full_ledger(&scored, &plan);

        let ledger: Vec<SequentialFdrLedgerEntry> = match stage {
            None => full,
            Some(stage) => full
                .into_iter()
                .filter(|line| line.stage == stage)
                .collect(),
        };

        let gate_applies = stage.is_none();
        let required_fields_complete = ledger.iter().all(required_fields_present);
        let clauses_consistent = ledger.iter().all(|l| l.clause_consistent);

        // Conservative integrity (AC3): every conservative test is surfaced on a
        // govern line, and no conservative test was finally certified.
        let conservative_tests = plan
            .conservative_reason
            .iter()
            .filter(|r| r.is_conservative())
            .count();
        let conservative_surfaced: usize =
            plan.governance.iter().map(|g| g.conservative_count).sum();
        let no_conservative_certified = (0..scored.len())
            .all(|i| !(plan.conservative_reason[i].is_conservative() && plan.final_certified[i]));
        let conservative_integrity_ok =
            conservative_surfaced == conservative_tests && no_conservative_certified;

        // FDR monotonicity (final certified must be a subset of e-BH certified).
        let fdr_monotone_ok =
            (0..scored.len()).all(|i| !plan.final_certified[i] || plan.ebh.certified[i]);

        // e-BH self-check: exactly k* certifications, all at/above threshold.
        let ebh_count_matches = plan.ebh_certified_count == plan.ebh.k_star;

        let gate_passes = if gate_applies {
            required_fields_complete
                && plan.invalid == 0
                && clauses_consistent
                && conservative_integrity_ok
                && fdr_monotone_ok
                && ebh_count_matches
                && plan.wealth_conserved
        } else {
            true
        };

        let stage_label = stage.map_or("all", FdrStage::as_str).to_string();
        let stages_covered: BTreeSet<FdrStage> = ledger.iter().map(|l| l.stage).collect();
        let families: BTreeSet<GateFamily> = plan.governance.iter().map(|g| g.family).collect();

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "seq-fdr-{}",
            short_hash(&stable_hash(&ReportIdInput {
                run_id: &self.run_id,
                stage: &stage_label,
                evidence_checksum: &evidence_checksum,
            }))
        );
        let replay_command = format!(
            "doctor_frankentui --machine json sequential-fdr --stage {stage_label} --label {}",
            self.label
        );

        let summary = SequentialFdrSummary {
            schema_version: SEQUENTIAL_FDR_SCHEMA_VERSION.to_string(),
            report_id: report_id.clone(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            stage: stage_label.clone(),
            evidence_checksum: evidence_checksum.clone(),
            alpha: fmt6(self.config.alpha),
            total_tests: self.tests.len(),
            total_ledger_lines: ledger.len(),
            stages_covered: stages_covered.len(),
            families: families.len(),
            ebh_total: plan.ebh.total,
            ebh_k_star: plan.ebh.k_star,
            ebh_threshold: fmt6(plan.ebh.threshold),
            ebh_certified: plan.ebh_certified_count,
            final_certified: plan.final_certified_count,
            conservative_overrides: plan.conservative_overrides,
            invalid: plan.invalid,
            adversarial: plan.adversarial,
            conservative_events: conservative_tests,
            conservative_surfaced,
            required_fields_complete,
            clauses_consistent,
            conservative_integrity_ok,
            fdr_monotone_ok,
            ebh_count_matches,
            wealth_conserved: plan.wealth_conserved,
            gate_applies,
            gate_passes,
            replay_command: replay_command.clone(),
        };

        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);

        SequentialFdrReport {
            schema_version: SEQUENTIAL_FDR_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            stage: stage_label,
            evidence_checksum,
            ledger,
            summary,
            gate_applies,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    run_id: &'a str,
    stage: &'a str,
    evidence_checksum: &'a str,
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one sequential-FDR run.
///
/// Numeric aggregates are fixed-decimal strings so the summary derives `Eq` and
/// serializes byte-identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequentialFdrSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// The stage view (`all` or a single stage).
    pub stage: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// Target FDR level (fixed-decimal).
    pub alpha: String,
    /// Total tests in the stream.
    pub total_tests: usize,
    /// Total emitted ledger lines.
    pub total_ledger_lines: usize,
    /// Distinct stages covered by the emitted ledger.
    pub stages_covered: usize,
    /// Distinct gate families with tests.
    pub families: usize,
    /// Tests considered by e-BH (valid tests).
    pub ebh_total: usize,
    /// e-BH `k*` (number of rejections).
    pub ebh_k_star: usize,
    /// e-BH e-value cutoff (fixed-decimal).
    pub ebh_threshold: String,
    /// Tests certified by e-BH (the FDR-controlled set).
    pub ebh_certified: usize,
    /// Tests certified after conservative withholds (the authoritative set).
    pub final_certified: usize,
    /// e-BH certifications downgraded by the conservative governor.
    pub conservative_overrides: usize,
    /// Malformed tests (fail the gate).
    pub invalid: usize,
    /// Tests with adversarial (clamped/out-of-range) evidence.
    pub adversarial: usize,
    /// Tests that hit a conservative event (exhausted or adversarial).
    pub conservative_events: usize,
    /// Conservative events surfaced on govern lines (must equal `conservative_events`).
    pub conservative_surfaced: usize,
    /// Whether every ledger line has all mandatory fields populated.
    pub required_fields_complete: bool,
    /// Whether every decision matches its recorded arithmetic (AC4).
    pub clauses_consistent: bool,
    /// Whether every conservative event was surfaced and none was certified (AC3).
    pub conservative_integrity_ok: bool,
    /// Whether the final certified set is a subset of the e-BH set.
    pub fdr_monotone_ok: bool,
    /// Whether the e-BH certified count equals `k*`.
    pub ebh_count_matches: bool,
    /// Whether the fairness transfers conserved total wealth.
    pub wealth_conserved: bool,
    /// Whether the gate applies to this stage view.
    pub gate_applies: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequentialFdrStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory sequential-FDR report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequentialFdrReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// The stage view (`all` or a single stage).
    pub stage: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// The emitted evidence ledger (float-free).
    pub ledger: Vec<SequentialFdrLedgerEntry>,
    /// Aggregate summary.
    pub summary: SequentialFdrSummary,
    /// Whether the gate applies to this stage view.
    pub gate_applies: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: SequentialFdrStatsArtifact,
}

impl SequentialFdrReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &SequentialFdrSummary,
    ledger: &[SequentialFdrLedgerEntry],
) -> SequentialFdrStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a SequentialFdrSummary,
        ledger: &'a [SequentialFdrLedgerEntry],
    }

    let payload = Export {
        schema_version: SEQUENTIAL_FDR_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    SequentialFdrStatsArtifact {
        path: format!("{report_id}/sequential_fdr_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Default corpus + report builders ─────────────────────────────────────────

/// The default test corpus: a deterministic, all-justified stream that drives a
/// green gate. Eight direct e-values across the four families (strong evidence
/// first within each family so the online and batch controllers agree), with no
/// adversarial evidence and no wealth exhaustion. Negative corpora used in tests
/// drive the conservative / red paths.
#[must_use]
pub fn default_test_corpus() -> Vec<FdrTest> {
    vec![
        // Quality: strong regression-free certification, then a weak claim.
        FdrTest::with_evalue("q.frame.regression", GateFamily::Quality, 0, 500.0),
        FdrTest::with_evalue("q.markup.parity", GateFamily::Quality, 1, 8.0),
        // Performance.
        FdrTest::with_evalue("p.latency.p99", GateFamily::Performance, 0, 300.0),
        FdrTest::with_evalue("p.throughput", GateFamily::Performance, 1, 3.0),
        // Accessibility.
        FdrTest::with_evalue("a.contrast.ratio", GateFamily::Accessibility, 0, 200.0),
        FdrTest::with_evalue("a.focus.order", GateFamily::Accessibility, 1, 2.0),
        // Determinism: two strong checks (the second still clears the online bar).
        FdrTest::with_evalue("d.replay.checksum", GateFamily::Determinism, 0, 100.0),
        FdrTest::with_evalue("d.shadow.parity", GateFamily::Determinism, 1, 30.0),
    ]
}

/// Run the full controller over the default corpus with the default config.
#[must_use]
pub fn run_sequential_fdr_report(label: &str) -> SequentialFdrReport {
    SequentialFdrController::new(label, SequentialFdrConfig::default(), default_test_corpus())
        .run(None)
}

/// Run a single stage view over the default corpus.
#[must_use]
pub fn run_sequential_fdr_report_for_stage(
    label: &str,
    stage: Option<FdrStage>,
) -> SequentialFdrReport {
    SequentialFdrController::new(label, SequentialFdrConfig::default(), default_test_corpus())
        .run(stage)
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized sequential-FDR pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct SequentialFdrPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
    /// The stage view to materialize (`None` = all stages; the gate applies).
    pub stage: Option<FdrStage>,
    /// Controller configuration.
    pub controller: SequentialFdrConfig,
    /// The test corpus.
    pub corpus: Vec<FdrTest>,
}

impl Default for SequentialFdrPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "sequential_fdr".to_string(),
            label: "sequential-fdr/e2e".to_string(),
            stage: None,
            controller: SequentialFdrConfig::default(),
            corpus: default_test_corpus(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequentialFdrArtifact {
    /// Logical artifact name.
    pub name: String,
    /// Relative file name within the run directory.
    pub file: String,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// Byte length of the file content.
    pub bytes: u64,
}

/// The outcome of running and materializing the pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequentialFdrPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL evidence ledger.
    pub ledger_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the JSON-stats artifact.
    pub stats_path: String,
    /// The machine-readable summary.
    pub summary: SequentialFdrSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<SequentialFdrArtifact>,
}

fn artifact_of(file: &str, content: &str) -> SequentialFdrArtifact {
    SequentialFdrArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the controller and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// Writes a JSONL evidence ledger (one [`SequentialFdrLedgerEntry`] per line), a
/// `pipeline_summary.json`, an `artifact_manifest.json`, and the JSON-stats
/// artifact. Artifacts are always written (so red-path triage is possible); the
/// returned summary's `gate_passes` reflects the gate.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_sequential_fdr_pipeline(
    run_root: &Path,
    config: &SequentialFdrPipelineConfig,
) -> crate::error::Result<SequentialFdrPipelineOutcome> {
    let report =
        SequentialFdrController::new(&config.label, config.controller, config.corpus.clone())
            .run(config.stage);

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "sequential_fdr_stats.json";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&run_dir.join(stats_file), &stats_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    let artifacts = vec![
        artifact_of(ledger_file, &ledger_content),
        artifact_of(stats_file, &stats_content),
        artifact_of(summary_file, &summary_content),
    ];

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        stage: &'a str,
        gate_applies: bool,
        gate_passes: bool,
        artifacts: &'a [SequentialFdrArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: SEQUENTIAL_FDR_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        stage: &report.stage,
        gate_applies: report.gate_applies,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(SequentialFdrPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// The stage selector for the `sequential-fdr` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SequentialFdrStageArg {
    /// Run all stages and apply the gate (default; the CI gate).
    All,
    /// Per-test e-value construction.
    Evalue,
    /// Batch e-BH rejection decisions.
    Ebh,
    /// Per-test online alpha-investing wealth updates.
    Invest,
    /// Per-family wealth governance.
    Govern,
}

impl SequentialFdrStageArg {
    fn to_stage(self) -> Option<FdrStage> {
        match self {
            Self::All => None,
            Self::Evalue => Some(FdrStage::Evalue),
            Self::Ebh => Some(FdrStage::Ebh),
            Self::Invest => Some(FdrStage::Invest),
            Self::Govern => Some(FdrStage::Govern),
        }
    }
}

/// CLI arguments for the `sequential-fdr` command.
#[derive(Debug, clap::Args)]
pub struct SequentialFdrArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/sequential_fdr"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "sequential_fdr")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "sequential-fdr/e2e")]
    pub label: String,

    /// Which stage(s) to run. `all` (default) applies the gate.
    #[arg(long = "stage", value_enum, default_value_t = SequentialFdrStageArg::All)]
    pub stage: SequentialFdrStageArg,
}

/// Run the `sequential-fdr` command: materialize the pipeline over the default
/// corpus and apply the gate (when the stage view is the full pipeline).
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (a malformed test, an inconsistent clause, a silent conservative event,
/// a non-monotone certification, an e-BH count mismatch, or a wealth-conservation
/// violation), or an I/O error if artifacts cannot be materialized.
pub fn run_sequential_fdr(args: SequentialFdrArgs) -> crate::error::Result<()> {
    let config = SequentialFdrPipelineConfig {
        run_name: args.run_name,
        label: args.label,
        stage: args.stage.to_stage(),
        controller: SequentialFdrConfig::default(),
        corpus: default_test_corpus(),
    };
    let outcome = run_sequential_fdr_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("sequential fdr controller"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!("stage: {}", summary.stage));
        ui.info(&format!(
            "tests: {} | families: {} | ledger lines: {} | alpha: {}",
            summary.total_tests, summary.families, summary.total_ledger_lines, summary.alpha
        ));
        ui.info(&format!(
            "e-BH: k*={} certified={} (threshold={}) | final certified: {} | conservative overrides: {}",
            summary.ebh_k_star,
            summary.ebh_certified,
            summary.ebh_threshold,
            summary.final_certified,
            summary.conservative_overrides
        ));
        ui.info(&format!(
            "invalid: {} | adversarial: {} | conservative events: {} (surfaced: {})",
            summary.invalid,
            summary.adversarial,
            summary.conservative_events,
            summary.conservative_surfaced
        ));
        if summary.conservative_events > 0 {
            ui.info("conservative mode surfaced (wealth exhausted / adversarial evidence)");
        }
        if !summary.gate_applies {
            ui.info("gate not applicable to this stage view");
        } else if summary.gate_passes {
            ui.success("sequential-fdr gate PASSED");
        } else {
            ui.error("sequential-fdr gate FAILED");
        }
    }

    if summary.gate_applies && !summary.gate_passes {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "sequential-fdr gate failed: invalid={}, clauses_consistent={}, conservative_integrity_ok={}, fdr_monotone_ok={}, ebh_count_matches={}, wealth_conserved={}",
                summary.invalid,
                summary.clauses_consistent,
                summary.conservative_integrity_ok,
                summary.fdr_monotone_ok,
                summary.ebh_count_matches,
                summary.wealth_conserved
            ),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn controller_with(
        config: SequentialFdrConfig,
        corpus: Vec<FdrTest>,
    ) -> SequentialFdrController {
        SequentialFdrController::new("sequential-fdr/test", config, corpus)
    }

    #[test]
    fn default_corpus_gate_passes_and_covers_all_stages() {
        let report = run_sequential_fdr_report("sequential-fdr/test");
        assert!(report.gate_applies);
        assert!(report.gate_passes, "default corpus must be green");
        assert!(report.summary.required_fields_complete);
        assert_eq!(report.summary.invalid, 0);
        assert_eq!(report.summary.stages_covered, FdrStage::ALL.len());
        for line in &report.ledger {
            assert!(
                required_fields_present(line),
                "line missing fields: {line:?}"
            );
        }
    }

    #[test]
    fn ebh_certifies_the_expected_count() {
        // K=8, alpha=0.10 => K/alpha=80. Sorted e-values
        // [500,300,200,100,30,8,3,2]: e_[k] >= 80/k holds through k=5 (30>=16)
        // and fails at k=6 (8 < 13.33...), so k*=5, threshold=80/5=16.
        let report = run_sequential_fdr_report("sequential-fdr/test");
        assert_eq!(report.summary.ebh_k_star, 5);
        assert_eq!(report.summary.ebh_certified, 5);
        assert_eq!(report.summary.ebh_threshold, fmt6(16.0));
        assert!(report.summary.ebh_count_matches);
        // No conservative events on the green corpus -> final == e-BH.
        assert_eq!(report.summary.final_certified, 5);
        assert_eq!(report.summary.conservative_overrides, 0);
    }

    #[test]
    fn ebh_step_up_rescues_middle_ranks() {
        // The subtle step-up property: a rank that FAILS its own cutoff
        // K/(alpha*k) is still certified when a LATER rank passes. K=4,
        // alpha=0.5 => K/alpha=8; cutoffs (8, 4, 8/3=2.667, 2). Sorted e-values
        // [10, 3, 2.5, 2.1]: k=1 passes (10>=8), k=2 FAILS (3<4), k=3 FAILS
        // (2.5<2.667), k=4 passes (2.1>=2). k* = max passing = 4, threshold =
        // 8/4 = 2, so ALL FOUR certify even though ranks 2 and 3 failed their
        // own cutoffs (rescued by the step-up).
        let cfg = SequentialFdrConfig::default().with_alpha(0.5);
        let corpus = vec![
            FdrTest::with_evalue("q.a", GateFamily::Quality, 0, 10.0),
            FdrTest::with_evalue("q.b", GateFamily::Quality, 1, 3.0),
            FdrTest::with_evalue("q.c", GateFamily::Quality, 2, 2.5),
            FdrTest::with_evalue("q.d", GateFamily::Quality, 3, 2.1),
        ];
        let report = controller_with(cfg, corpus).run(None);
        assert_eq!(report.summary.ebh_k_star, 4, "step-up must reach k*=4");
        assert_eq!(report.summary.ebh_certified, 4, "all four rescued");
        assert_eq!(report.summary.ebh_threshold, fmt6(2.0));
        assert!(report.summary.ebh_count_matches);
        assert!(report.gate_passes);
        // Confirm the middle ranks (which failed their own cutoffs) are certified.
        for line in &report.ledger {
            if line.stage == FdrStage::Ebh {
                assert_eq!(
                    line.decision,
                    TestDecision::Certify,
                    "every rank is certified via step-up: {line:?}"
                );
            }
        }
    }

    #[test]
    fn ebh_clause_consistency_holds_for_every_line() {
        let report = run_sequential_fdr_report("sequential-fdr/test");
        let threshold: f64 = report.summary.ebh_threshold.parse().unwrap();
        for line in &report.ledger {
            if line.stage == FdrStage::Ebh {
                let e: f64 = line.evalue.parse().unwrap();
                let certified = line.decision == TestDecision::Certify;
                assert_eq!(certified, e >= threshold, "ebh clause: {line:?}");
                assert!(line.clause_consistent);
            }
        }
        assert!(report.summary.clauses_consistent);
    }

    #[test]
    fn invest_wealth_recursion_replays_from_ledger() {
        // AC2/AC4: re-derive each family's wealth trajectory from the invest
        // lines and confirm it matches the recorded wealth_after.
        let report = run_sequential_fdr_report("sequential-fdr/test");
        let cfg = SequentialFdrConfig::default();
        let mut by_family: BTreeMap<String, Vec<&SequentialFdrLedgerEntry>> = BTreeMap::new();
        for line in &report.ledger {
            if line.stage == FdrStage::Invest {
                by_family
                    .entry(line.family.as_str().to_string())
                    .or_default()
                    .push(line);
            }
        }
        for (_family, lines) in by_family {
            let mut wealth = cfg.initial_wealth;
            for line in lines {
                let before: f64 = line.wealth_before.parse().unwrap();
                assert!((before - wealth).abs() < 1e-6, "wealth_before mismatch");
                let alpha_i: f64 = line.alpha_invested.parse().unwrap();
                let certified = line.decision == TestDecision::Certify;
                let next = if certified {
                    wealth - alpha_i + cfg.reward
                } else {
                    wealth - alpha_i / (1.0 - alpha_i)
                };
                wealth = next.clamp(0.0, cfg.wealth_cap);
                let after: f64 = line.wealth_after.parse().unwrap();
                assert!(
                    (after - wealth).abs() < 1e-6,
                    "wealth_after mismatch: {line:?}"
                );
            }
        }
    }

    #[test]
    fn deterministic_report_and_ledger_across_runs() {
        let a = run_sequential_fdr_report("sequential-fdr/test");
        let b = run_sequential_fdr_report("sequential-fdr/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger); // float-free ledger derives Eq
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn test_input_order_does_not_change_decisions() {
        let mut corpus = default_test_corpus();
        corpus.reverse();
        let shuffled = controller_with(SequentialFdrConfig::default(), corpus).run(None);
        let canonical = run_sequential_fdr_report("sequential-fdr/e2e");
        // The canonical sort makes the decision content order-free.
        assert_eq!(
            shuffled.summary.ebh_certified,
            canonical.summary.ebh_certified
        );
        assert_eq!(
            shuffled.summary.final_certified,
            canonical.summary.final_certified
        );
        assert_eq!(
            shuffled.summary.ebh_threshold,
            canonical.summary.ebh_threshold
        );
    }

    #[test]
    fn observation_stream_builds_a_valid_evalue() {
        // Reuse of the e-process: an all-safe observation stream yields a large
        // terminal-wealth e-value (> 1, evidence of safety).
        let corpus = vec![FdrTest::with_observations(
            "q.stream",
            GateFamily::Quality,
            0,
            vec![1.0; 12],
        )];
        let report = controller_with(SequentialFdrConfig::default(), corpus).run(None);
        let evalue_line = report
            .ledger
            .iter()
            .find(|l| l.stage == FdrStage::Evalue)
            .unwrap();
        let e: f64 = evalue_line.evalue.parse().unwrap();
        assert!(e > 1.0, "all-safe stream must yield e-value > 1, got {e}");
        assert_eq!(evalue_line.evidence_kind, Some(EvidenceKind::Observations));
    }

    #[test]
    fn malformed_test_fails_the_gate() {
        let corpus = vec![
            FdrTest::with_evalue("ok", GateFamily::Quality, 0, 500.0),
            FdrTest::with_evalue("bad", GateFamily::Quality, 1, -1.0),
        ];
        let report = controller_with(SequentialFdrConfig::default(), corpus).run(None);
        assert_eq!(report.summary.invalid, 1);
        assert!(!report.gate_passes, "a malformed test must fail the gate");
        let invalid_line = report
            .ledger
            .iter()
            .find(|l| l.stage == FdrStage::Evalue && l.decision == TestDecision::Invalid)
            .unwrap();
        assert!(!invalid_line.remediation.is_empty());
    }

    #[test]
    fn adversarial_evidence_is_clamped_and_withheld_never_silent() {
        // An implausibly-large e-value is clamped, flagged adversarial, and
        // withheld; the conservative event is surfaced so the gate still passes.
        let cfg = SequentialFdrConfig::default().with_evalue_cap(1_000.0);
        let corpus = vec![FdrTest::with_evalue("q.spike", GateFamily::Quality, 0, 1e9)];
        let report = controller_with(cfg, corpus).run(None);
        assert_eq!(report.summary.adversarial, 1);
        assert_eq!(report.summary.conservative_events, 1);
        assert_eq!(report.summary.conservative_surfaced, 1);
        assert!(report.summary.conservative_integrity_ok);
        // The clamped, adversarial test must NOT be finally certified.
        assert_eq!(report.summary.final_certified, 0);
        assert!(
            report.gate_passes,
            "surfaced conservative event keeps the gate green"
        );
        // e-BH would have certified the (clamped) spike; the override withheld it.
        assert!(report.summary.ebh_certified >= report.summary.final_certified);
    }

    #[test]
    fn wealth_exhaustion_forces_surfaced_conservative_withhold() {
        // A tiny initial wealth with a streak of weak (un-rejected) tests starves
        // the family; once wealth is at the floor, further tests are
        // conservatively withheld and surfaced (never silently permissive).
        let cfg = SequentialFdrConfig::default()
            .with_initial_wealth(0.02)
            .with_wealth_floor(0.015);
        let corpus = vec![
            FdrTest::with_evalue("q.a", GateFamily::Quality, 0, 1.0),
            FdrTest::with_evalue("q.b", GateFamily::Quality, 1, 1.0),
            FdrTest::with_evalue("q.c", GateFamily::Quality, 2, 1.0),
        ];
        let report = controller_with(cfg, corpus).run(None);
        assert!(
            report.summary.conservative_events >= 1,
            "expected exhaustion"
        );
        assert_eq!(
            report.summary.conservative_events,
            report.summary.conservative_surfaced
        );
        assert!(report.summary.conservative_integrity_ok);
        let exhausted = report
            .ledger
            .iter()
            .any(|l| l.conservative_reason == ConservativeReason::WealthExhausted);
        assert!(exhausted, "a wealth-exhausted line must be emitted");
        assert!(report.gate_passes);
    }

    #[test]
    fn conservative_override_only_withholds_never_adds() {
        // final_certified is always a subset of ebh_certified.
        let cfg = SequentialFdrConfig::default().with_evalue_cap(1_000.0);
        let corpus = vec![
            FdrTest::with_evalue("q.spike", GateFamily::Quality, 0, 1e9),
            FdrTest::with_evalue("p.ok", GateFamily::Performance, 0, 500.0),
        ];
        let report = controller_with(cfg, corpus).run(None);
        assert!(report.summary.fdr_monotone_ok);
        assert!(report.summary.final_certified <= report.summary.ebh_certified);
    }

    #[test]
    fn fairness_transfer_conserves_total_wealth() {
        let report = run_sequential_fdr_report("sequential-fdr/test");
        assert!(report.summary.wealth_conserved);
        // Sum of transfer_in must equal sum of transfer_out across govern lines.
        let mut tin = 0.0f64;
        let mut tout = 0.0f64;
        for line in &report.ledger {
            if line.stage == FdrStage::Govern {
                tin += line.transfer_in.parse::<f64>().unwrap();
                tout += line.transfer_out.parse::<f64>().unwrap();
            }
        }
        assert!((tin - tout).abs() < 1e-6);
    }

    #[test]
    fn starved_family_receives_a_conservation_preserving_transfer() {
        // One rich family (a strong cert grows its wealth) and one starved family
        // (a long weak streak) -> a bounded transfer rebalances them, conserving
        // total wealth.
        let cfg = SequentialFdrConfig::default().with_initial_wealth(0.05);
        let corpus = vec![
            // Performance family ends rich (strong cert + payout).
            FdrTest::with_evalue("p.win", GateFamily::Performance, 0, 500.0),
            // Quality family ends starved (a streak of weak retains drains wealth
            // below the starvation floor without exhausting it).
            FdrTest::with_evalue("q.a", GateFamily::Quality, 0, 1.0),
            FdrTest::with_evalue("q.b", GateFamily::Quality, 1, 1.0),
            FdrTest::with_evalue("q.c", GateFamily::Quality, 2, 1.0),
        ];
        let report = controller_with(cfg, corpus).run(None);
        assert!(report.summary.wealth_conserved);
        let any_transfer = report
            .ledger
            .iter()
            .any(|l| l.stage == FdrStage::Govern && l.transfer_in.parse::<f64>().unwrap() > 0.0);
        assert!(any_transfer, "a starved family should receive a transfer");
    }

    #[test]
    fn single_stage_view_does_not_apply_the_gate() {
        let report =
            run_sequential_fdr_report_for_stage("sequential-fdr/test", Some(FdrStage::Ebh));
        assert!(!report.gate_applies);
        assert!(report.gate_passes);
        for line in &report.ledger {
            assert_eq!(line.stage, FdrStage::Ebh);
        }
    }

    #[test]
    fn empty_corpus_is_green_with_no_certifications() {
        let report = controller_with(SequentialFdrConfig::default(), Vec::new()).run(None);
        assert!(report.gate_passes);
        assert_eq!(report.summary.ebh_certified, 0);
        assert_eq!(report.summary.final_certified, 0);
        assert_eq!(report.summary.ebh_k_star, 0);
    }

    #[test]
    fn reward_grows_wealth_on_certification() {
        // A strong first test certifies online and earns the payout.
        let report = run_sequential_fdr_report("sequential-fdr/test");
        let first_quality = report
            .ledger
            .iter()
            .find(|l| l.stage == FdrStage::Invest && l.test_id == "q.frame.regression")
            .unwrap();
        assert_eq!(first_quality.decision, TestDecision::Certify);
        let before: f64 = first_quality.wealth_before.parse().unwrap();
        let after: f64 = first_quality.wealth_after.parse().unwrap();
        assert!(after > before, "certification must earn the payout");
    }
}
