//! Golden-output checksum oracle + isomorphism-proof artifact pipeline
//! (bd-3bxhj.10/optimization bd-3bxhj.8.17).
//!
//! Optimization changes must be *behavior-preserving* unless an exception is
//! explicitly declared. This module provides the deterministic machinery that
//! enforces that contract:
//!
//! 1. **Golden-output capture pipeline** — [`capture_golden_manifest`] records a
//!    content-addressed checksum manifest over a set of representative fixture
//!    outputs ([`GoldenOutput`]). The manifest is the frozen baseline an
//!    optimization is measured against.
//! 2. **Isomorphism-proof template compiler** — [`compile_isomorphism_proof`]
//!    compiles a structured [`IsomorphismProof`] that decides, per fixture,
//!    whether the candidate output is byte-identical to the golden output, or
//!    equal only after applying the change's *declared* equivalence invariants
//!    (ordering, tie-breaks, floating-point tolerance, RNG-seed independence).
//!    The proof links back to the `change_id`, the fixture set, and the
//!    golden/candidate checksum-verification results.
//! 3. **Verification gate** — [`GoldenIsomorphismGate::verify`] turns the proof
//!    into a go/no-go promotion decision: any fixture whose behavior changed
//!    without a valid [`GoldenPolicyException`] produces a blocking finding, so
//!    the optimization cannot be promoted.
//!
//! Every gate finding carries `proof_id`, the per-fixture `checksum_verdict`,
//! the invariant status, and a one-command counterexample `replay_command`, so
//! failures are forensically reproducible. The module is pure and deterministic
//! — it owns no I/O — so equivalent inputs always yield the same `oracle_id`,
//! `proof_id`, and `evidence_checksum`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version for the golden-isomorphism oracle artifacts.
pub const GOLDEN_ISOMORPHISM_SCHEMA_VERSION: &str = "golden-isomorphism-v1";

// ── Observable output model ─────────────────────────────────────────────────

/// One observable output record produced by a fixture.
///
/// Records are the unit the oracle checksums. Their fields are chosen so the
/// four behavior-preservation invariants can be applied as deterministic
/// normalizations before hashing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldenRecord {
    /// Canonical observable key (e.g. a row id, cell coordinate, or event name).
    pub key: String,
    /// Emission ordinal (position in the output stream).
    pub ordinal: u64,
    /// Discrete observable tokens (exact-match, never tolerance-compared).
    pub tokens: Vec<String>,
    /// Continuous observables (subject to the floating-point invariant).
    pub floats: Vec<f64>,
    /// RNG seed material attached to this record (subject to the RNG invariant).
    pub seed: Option<u64>,
}

impl GoldenRecord {
    /// A record with the given key and ordinal and no other observables.
    #[must_use]
    pub fn new(key: impl Into<String>, ordinal: u64) -> Self {
        Self {
            key: key.into(),
            ordinal,
            tokens: Vec::new(),
            floats: Vec::new(),
            seed: None,
        }
    }

    /// Attach discrete tokens.
    #[must_use]
    pub fn with_tokens(mut self, tokens: impl IntoIterator<Item = String>) -> Self {
        self.tokens = tokens.into_iter().collect();
        self
    }

    /// Attach continuous floats.
    #[must_use]
    pub fn with_floats(mut self, floats: impl IntoIterator<Item = f64>) -> Self {
        self.floats = floats.into_iter().collect();
        self
    }

    /// Attach RNG seed material.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }
}

/// The full observable output of one fixture under one workload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldenOutput {
    /// Fixture identity.
    pub fixture_id: String,
    /// Workload identity within the fixture.
    pub workload_id: String,
    /// The observable records, in emission order.
    pub records: Vec<GoldenRecord>,
}

impl GoldenOutput {
    /// A fixture output with the given identity and records.
    #[must_use]
    pub fn new(
        fixture_id: impl Into<String>,
        workload_id: impl Into<String>,
        records: Vec<GoldenRecord>,
    ) -> Self {
        Self {
            fixture_id: fixture_id.into(),
            workload_id: workload_id.into(),
            records,
        }
    }
}

// ── Equivalence invariants ──────────────────────────────────────────────────

/// A declared behavior-preservation invariant an optimization relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IsomorphismInvariant {
    /// The full emission order of records is not observable.
    Ordering,
    /// Only the order *among records sharing a key* is not observable.
    TieBreak,
    /// Floats are equivalent within `tolerance_decimals` decimal places.
    FloatingPoint {
        /// Number of decimal places preserved before comparison.
        tolerance_decimals: u8,
    },
    /// RNG seed material is not part of observable behavior.
    RngSeed,
}

/// The coarse identity of an invariant (ignores the floating-point tolerance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantKind {
    /// [`IsomorphismInvariant::Ordering`].
    Ordering,
    /// [`IsomorphismInvariant::TieBreak`].
    TieBreak,
    /// [`IsomorphismInvariant::FloatingPoint`].
    FloatingPoint,
    /// [`IsomorphismInvariant::RngSeed`].
    RngSeed,
}

impl IsomorphismInvariant {
    /// The coarse kind of this invariant.
    #[must_use]
    pub fn kind(self) -> InvariantKind {
        match self {
            Self::Ordering => InvariantKind::Ordering,
            Self::TieBreak => InvariantKind::TieBreak,
            Self::FloatingPoint { .. } => InvariantKind::FloatingPoint,
            Self::RngSeed => InvariantKind::RngSeed,
        }
    }

    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ordering => "ordering",
            Self::TieBreak => "tie_break",
            Self::FloatingPoint { .. } => "floating_point",
            Self::RngSeed => "rng_seed",
        }
    }
}

/// Whether a declared invariant held and whether it was load-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantStatus {
    /// The proof's overall equivalence held and this invariant was required for
    /// it (removing it would break the equivalence).
    HoldsLoadBearing,
    /// The proof's overall equivalence held but this invariant was not required.
    HoldsUnused,
    /// The proof's overall equivalence did not hold under the declared set.
    Violated,
}

impl InvariantStatus {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HoldsLoadBearing => "holds_load_bearing",
            Self::HoldsUnused => "holds_unused",
            Self::Violated => "violated",
        }
    }
}

/// The resolved set of normalizations to apply before hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InvariantSet {
    ordering: bool,
    tie_break: bool,
    rng_seed: bool,
    fp_decimals: Option<u8>,
}

impl InvariantSet {
    /// The empty set: raw, byte-identical comparison.
    const fn raw() -> Self {
        Self {
            ordering: false,
            tie_break: false,
            rng_seed: false,
            fp_decimals: None,
        }
    }

    fn from_invariants(invariants: &[IsomorphismInvariant]) -> Self {
        let mut set = Self::raw();
        for invariant in invariants {
            match invariant {
                IsomorphismInvariant::Ordering => set.ordering = true,
                IsomorphismInvariant::TieBreak => set.tie_break = true,
                IsomorphismInvariant::RngSeed => set.rng_seed = true,
                IsomorphismInvariant::FloatingPoint { tolerance_decimals } => {
                    set.fp_decimals = Some(*tolerance_decimals);
                }
            }
        }
        set
    }

    fn without(self, kind: InvariantKind) -> Self {
        let mut set = self;
        match kind {
            InvariantKind::Ordering => set.ordering = false,
            InvariantKind::TieBreak => set.tie_break = false,
            InvariantKind::RngSeed => set.rng_seed = false,
            InvariantKind::FloatingPoint => set.fp_decimals = None,
        }
        set
    }
}

/// The canonical, normalized projection of a record used for hashing + sorting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct NormalizedRecord {
    key: String,
    ordinal: Option<u64>,
    tokens: Vec<String>,
    floats: Vec<i128>,
    seed: Option<u64>,
}

fn quantize(value: f64, decimals: u8) -> i128 {
    let scale = 10f64.powi(i32::from(decimals));
    (value * scale).round() as i128
}

fn normalized_projection(output: &GoldenOutput, set: InvariantSet) -> Vec<NormalizedRecord> {
    let drop_ordinal = set.ordering || set.tie_break;
    let mut records: Vec<NormalizedRecord> = output
        .records
        .iter()
        .map(|record| NormalizedRecord {
            key: record.key.clone(),
            ordinal: if drop_ordinal {
                None
            } else {
                Some(record.ordinal)
            },
            tokens: record.tokens.clone(),
            floats: record
                .floats
                .iter()
                .map(|&value| match set.fp_decimals {
                    Some(decimals) => quantize(value, decimals),
                    None => i128::from(value.to_bits()),
                })
                .collect(),
            seed: if set.rng_seed { None } else { record.seed },
        })
        .collect();

    if set.ordering {
        records.sort();
    } else if set.tie_break {
        // Preserve the order of distinct keys (only within-key order is free).
        let mut first_seen: BTreeMap<String, usize> = BTreeMap::new();
        for record in &records {
            let next = first_seen.len();
            first_seen.entry(record.key.clone()).or_insert(next);
        }
        records.sort_by(|a, b| {
            first_seen[&a.key]
                .cmp(&first_seen[&b.key])
                .then_with(|| a.cmp(b))
        });
    }

    records
}

fn output_checksum(output: &GoldenOutput, set: InvariantSet) -> String {
    stable_hash(&normalized_projection(output, set))
}

// ── Golden manifest (captured baseline) ─────────────────────────────────────

/// A captured, content-addressed checksum digest for one fixture's golden output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenOutputDigest {
    /// Fixture identity.
    pub fixture_id: String,
    /// Workload identity.
    pub workload_id: String,
    /// Raw (byte-identical) checksum of the fixture's output.
    pub raw_checksum: String,
    /// Number of records captured.
    pub record_count: usize,
}

/// The frozen golden baseline: a checksum manifest over representative fixtures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenManifest {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic manifest identifier.
    pub manifest_id: String,
    /// The change/revision this golden baseline was captured at.
    pub baseline_change_id: String,
    /// Per-fixture digests, keyed by fixture id.
    pub digests: BTreeMap<String, GoldenOutputDigest>,
    /// Aggregate checksum over every per-fixture digest.
    pub manifest_checksum: String,
}

/// Capture a golden checksum manifest over a set of fixture outputs.
#[must_use]
pub fn capture_golden_manifest(
    baseline_change_id: impl Into<String>,
    outputs: &[GoldenOutput],
) -> GoldenManifest {
    let baseline_change_id = baseline_change_id.into();
    let raw = InvariantSet::raw();
    let mut digests = BTreeMap::new();
    for output in outputs {
        digests.insert(
            output.fixture_id.clone(),
            GoldenOutputDigest {
                fixture_id: output.fixture_id.clone(),
                workload_id: output.workload_id.clone(),
                raw_checksum: output_checksum(output, raw),
                record_count: output.records.len(),
            },
        );
    }
    let manifest_checksum = stable_hash(&digests);
    let manifest_id = format!(
        "golden-manifest-{}",
        short_hash(&stable_hash(&ManifestIdInput {
            schema_version: GOLDEN_ISOMORPHISM_SCHEMA_VERSION,
            baseline_change_id: &baseline_change_id,
            manifest_checksum: &manifest_checksum,
        }))
    );
    GoldenManifest {
        schema_version: GOLDEN_ISOMORPHISM_SCHEMA_VERSION.to_string(),
        manifest_id,
        baseline_change_id,
        digests,
        manifest_checksum,
    }
}

#[derive(Serialize)]
struct ManifestIdInput<'a> {
    schema_version: &'a str,
    baseline_change_id: &'a str,
    manifest_checksum: &'a str,
}

// ── Optimization change + policy exceptions ─────────────────────────────────

/// A declared exception that permits a behavior change for one fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenPolicyException {
    /// The fixture whose behavior change is permitted.
    pub fixture_id: String,
    /// Rationale for the exception.
    pub reason: String,
    /// Evidence references backing the exception (must be non-empty).
    pub evidence_refs: Vec<String>,
}

impl GoldenPolicyException {
    /// A policy exception for the given fixture.
    #[must_use]
    pub fn new(
        fixture_id: impl Into<String>,
        reason: impl Into<String>,
        evidence_refs: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            fixture_id: fixture_id.into(),
            reason: reason.into(),
            evidence_refs: evidence_refs.into_iter().collect(),
        }
    }

    /// Whether the exception is well-formed (non-empty reason + evidence).
    #[must_use]
    pub fn is_evidence_backed(&self) -> bool {
        !self.reason.trim().is_empty() && !self.evidence_refs.is_empty()
    }
}

/// An optimization change under evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizationChange {
    /// The change identity.
    pub change_id: String,
    /// The baseline change the golden manifest was captured at.
    pub baseline_change_id: String,
    /// The single optimization lever this change pulls.
    pub lever: String,
    /// Human-readable description.
    pub description: String,
    /// Equivalence invariants this change relies on to claim isomorphism.
    pub declared_invariants: Vec<IsomorphismInvariant>,
    /// Declared exceptions permitting specific behavior changes.
    pub policy_exceptions: Vec<GoldenPolicyException>,
}

impl OptimizationChange {
    /// A change with the given identity and lever.
    #[must_use]
    pub fn new(
        change_id: impl Into<String>,
        baseline_change_id: impl Into<String>,
        lever: impl Into<String>,
    ) -> Self {
        Self {
            change_id: change_id.into(),
            baseline_change_id: baseline_change_id.into(),
            lever: lever.into(),
            description: String::new(),
            declared_invariants: Vec::new(),
            policy_exceptions: Vec::new(),
        }
    }

    /// Set the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Declare an equivalence invariant.
    #[must_use]
    pub fn with_invariant(mut self, invariant: IsomorphismInvariant) -> Self {
        self.declared_invariants.push(invariant);
        self
    }

    /// Declare a policy exception.
    #[must_use]
    pub fn with_exception(mut self, exception: GoldenPolicyException) -> Self {
        self.policy_exceptions.push(exception);
        self
    }

    /// The declared invariants deduplicated by kind, in declaration order.
    #[must_use]
    fn deduped_invariants(&self) -> Vec<IsomorphismInvariant> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for invariant in &self.declared_invariants {
            if seen.insert(invariant.kind()) {
                out.push(*invariant);
            }
        }
        out
    }
}

// ── Isomorphism proof artifact ──────────────────────────────────────────────

/// The per-fixture checksum verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumVerdict {
    /// Byte-identical: golden and candidate raw checksums match.
    Match,
    /// Equal only after applying the declared invariants.
    NormalizedMatch,
    /// Behavior changed: not equal even under the declared invariants.
    Mismatch,
}

impl ChecksumVerdict {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::NormalizedMatch => "normalized_match",
            Self::Mismatch => "mismatch",
        }
    }

    /// Whether behavior is preserved under this verdict.
    #[must_use]
    pub fn behavior_preserved(self) -> bool {
        matches!(self, Self::Match | Self::NormalizedMatch)
    }
}

/// The per-fixture checksum-verification result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureVerdict {
    /// Fixture identity.
    pub fixture_id: String,
    /// Raw golden checksum.
    pub golden_raw_checksum: String,
    /// Raw candidate checksum.
    pub candidate_raw_checksum: String,
    /// Golden checksum under the declared invariants.
    pub golden_normalized_checksum: String,
    /// Candidate checksum under the declared invariants.
    pub candidate_normalized_checksum: String,
    /// The verdict.
    pub verdict: ChecksumVerdict,
}

/// The status of one declared invariant across the whole proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantCheck {
    /// The declared invariant.
    pub invariant: IsomorphismInvariant,
    /// Its status.
    pub status: InvariantStatus,
}

/// A counterexample: the first fixture whose behavior changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenCounterexample {
    /// Fixture identity.
    pub fixture_id: String,
    /// Golden checksum (under declared invariants).
    pub golden_checksum: String,
    /// Candidate checksum (under declared invariants).
    pub candidate_checksum: String,
    /// Human-readable detail.
    pub detail: String,
    /// One-command replay reference.
    pub replay_command: String,
}

/// A compiled isomorphism proof for one optimization change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsomorphismProof {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic proof identifier.
    pub proof_id: String,
    /// The change this proof certifies.
    pub change_id: String,
    /// The baseline change the golden manifest was captured at.
    pub baseline_change_id: String,
    /// The fixture set covered by the proof, in stable order.
    pub fixture_ids: Vec<String>,
    /// Aggregate golden checksum over the covered fixtures.
    pub golden_manifest_checksum: String,
    /// Aggregate candidate checksum over the covered fixtures.
    pub candidate_manifest_checksum: String,
    /// Per-declared-invariant status.
    pub invariant_checks: Vec<InvariantCheck>,
    /// Per-fixture checksum-verification results.
    pub fixture_verdicts: Vec<FixtureVerdict>,
    /// Whether behavior is preserved for every covered fixture.
    pub behavior_preserved: bool,
    /// The first behavior-changing fixture, if any.
    pub counterexample: Option<GoldenCounterexample>,
    /// One-command replay reference for the whole proof.
    pub replay_command: String,
}

fn fixture_replay_command(change_id: &str, fixture_id: &str, proof_id: &str) -> String {
    format!(
        "doctor_frankentui golden-isomorphism-verify --change {change_id} \
         --fixture {fixture_id} --proof {proof_id}"
    )
}

/// Compile an isomorphism proof comparing candidate outputs against golden
/// outputs under the change's declared invariants.
#[must_use]
pub fn compile_isomorphism_proof(
    change: &OptimizationChange,
    golden: &[GoldenOutput],
    candidate: &[GoldenOutput],
) -> IsomorphismProof {
    let declared = change.deduped_invariants();
    let set = InvariantSet::from_invariants(&declared);
    let raw = InvariantSet::raw();

    let golden_map: BTreeMap<&str, &GoldenOutput> =
        golden.iter().map(|o| (o.fixture_id.as_str(), o)).collect();
    let candidate_map: BTreeMap<&str, &GoldenOutput> = candidate
        .iter()
        .map(|o| (o.fixture_id.as_str(), o))
        .collect();

    // Covered fixtures = those present in both, in stable (sorted) order.
    let fixture_ids: Vec<String> = golden_map
        .keys()
        .filter(|id| candidate_map.contains_key(*id))
        .map(|id| (*id).to_string())
        .collect();

    let mut fixture_verdicts = Vec::new();
    let mut golden_digest: BTreeMap<&str, String> = BTreeMap::new();
    let mut candidate_digest: BTreeMap<&str, String> = BTreeMap::new();
    for id in &fixture_ids {
        let g = golden_map[id.as_str()];
        let c = candidate_map[id.as_str()];
        let golden_raw = output_checksum(g, raw);
        let candidate_raw = output_checksum(c, raw);
        let golden_norm = output_checksum(g, set);
        let candidate_norm = output_checksum(c, set);
        let verdict = if golden_raw == candidate_raw {
            ChecksumVerdict::Match
        } else if golden_norm == candidate_norm {
            ChecksumVerdict::NormalizedMatch
        } else {
            ChecksumVerdict::Mismatch
        };
        golden_digest.insert(id.as_str(), golden_raw.clone());
        candidate_digest.insert(id.as_str(), candidate_raw.clone());
        fixture_verdicts.push(FixtureVerdict {
            fixture_id: id.clone(),
            golden_raw_checksum: golden_raw,
            candidate_raw_checksum: candidate_raw,
            golden_normalized_checksum: golden_norm,
            candidate_normalized_checksum: candidate_norm,
            verdict,
        });
    }

    let behavior_preserved = fixture_verdicts
        .iter()
        .all(|v| v.verdict.behavior_preserved());

    let golden_manifest_checksum = stable_hash(&golden_digest);
    let candidate_manifest_checksum = stable_hash(&candidate_digest);

    let proof_id = format!(
        "isomorphism-proof-{}",
        short_hash(&stable_hash(&ProofIdInput {
            schema_version: GOLDEN_ISOMORPHISM_SCHEMA_VERSION,
            change_id: &change.change_id,
            golden_manifest_checksum: &golden_manifest_checksum,
            candidate_manifest_checksum: &candidate_manifest_checksum,
        }))
    );

    // Per-invariant status via leave-one-out load-bearing analysis.
    let invariant_checks = declared
        .iter()
        .map(|invariant| {
            let status = if !behavior_preserved {
                InvariantStatus::Violated
            } else if preserved_under(
                &fixture_ids,
                &golden_map,
                &candidate_map,
                set.without(invariant.kind()),
            ) {
                InvariantStatus::HoldsUnused
            } else {
                InvariantStatus::HoldsLoadBearing
            };
            InvariantCheck {
                invariant: *invariant,
                status,
            }
        })
        .collect();

    let counterexample = fixture_verdicts
        .iter()
        .find(|v| v.verdict == ChecksumVerdict::Mismatch)
        .map(|v| GoldenCounterexample {
            fixture_id: v.fixture_id.clone(),
            golden_checksum: v.golden_normalized_checksum.clone(),
            candidate_checksum: v.candidate_normalized_checksum.clone(),
            detail: format!(
                "fixture `{}` behavior changed under the declared invariants",
                v.fixture_id
            ),
            replay_command: fixture_replay_command(&change.change_id, &v.fixture_id, &proof_id),
        });

    let replay_command = format!(
        "doctor_frankentui golden-isomorphism-verify --proof {proof_id} --change {}",
        change.change_id
    );

    IsomorphismProof {
        schema_version: GOLDEN_ISOMORPHISM_SCHEMA_VERSION.to_string(),
        proof_id,
        change_id: change.change_id.clone(),
        baseline_change_id: change.baseline_change_id.clone(),
        fixture_ids,
        golden_manifest_checksum,
        candidate_manifest_checksum,
        invariant_checks,
        fixture_verdicts,
        behavior_preserved,
        counterexample,
        replay_command,
    }
}

fn preserved_under(
    fixture_ids: &[String],
    golden_map: &BTreeMap<&str, &GoldenOutput>,
    candidate_map: &BTreeMap<&str, &GoldenOutput>,
    set: InvariantSet,
) -> bool {
    let raw = InvariantSet::raw();
    fixture_ids.iter().all(|id| {
        let g = golden_map[id.as_str()];
        let c = candidate_map[id.as_str()];
        output_checksum(g, raw) == output_checksum(c, raw)
            || output_checksum(g, set) == output_checksum(c, set)
    })
}

#[derive(Serialize)]
struct ProofIdInput<'a> {
    schema_version: &'a str,
    change_id: &'a str,
    golden_manifest_checksum: &'a str,
    candidate_manifest_checksum: &'a str,
}

// ── Verification gate ───────────────────────────────────────────────────────

/// Gate severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoldenSeverity {
    /// Blocks promotion.
    Block,
    /// Advisory only.
    Warn,
}

impl GoldenSeverity {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Warn => "warn",
        }
    }
}

/// A gate finding code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoldenFindingCode {
    /// A behavior change without a covering policy exception.
    UncoveredBehaviorChange,
    /// A behavior change covered by a valid policy exception.
    CoveredBehaviorChange,
    /// A policy exception that is not evidence-backed.
    InvalidPolicyException,
    /// A policy exception for a fixture whose behavior did not change.
    UnusedPolicyException,
    /// A golden fixture with no corresponding candidate output.
    MissingCandidateOutput,
    /// A candidate output with no corresponding golden fixture.
    ExtraneousCandidateOutput,
}

impl GoldenFindingCode {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UncoveredBehaviorChange => "uncovered_behavior_change",
            Self::CoveredBehaviorChange => "covered_behavior_change",
            Self::InvalidPolicyException => "invalid_policy_exception",
            Self::UnusedPolicyException => "unused_policy_exception",
            Self::MissingCandidateOutput => "missing_candidate_output",
            Self::ExtraneousCandidateOutput => "extraneous_candidate_output",
        }
    }
}

/// A gate finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenFinding {
    /// The finding code.
    pub code: GoldenFindingCode,
    /// Severity.
    pub severity: GoldenSeverity,
    /// The change under evaluation.
    pub change_id: String,
    /// The proof this finding references.
    pub proof_id: String,
    /// The fixture this finding is about.
    pub fixture_id: String,
    /// The per-fixture checksum verdict (`n/a` for coverage findings).
    pub checksum_verdict: String,
    /// Human-readable detail.
    pub detail: String,
    /// Remediation hint.
    pub remediation: String,
    /// One-command counterexample replay reference.
    pub replay_command: String,
}

/// Verification-gate configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenIsomorphismConfig {
    /// Whether policy exceptions are honored at all.
    pub allow_policy_exceptions: bool,
    /// Whether policy exceptions must carry non-empty evidence to be valid.
    pub require_exception_evidence: bool,
    /// Whether the golden and candidate fixture sets must match exactly.
    pub require_full_fixture_coverage: bool,
}

impl Default for GoldenIsomorphismConfig {
    fn default() -> Self {
        Self {
            allow_policy_exceptions: true,
            require_exception_evidence: true,
            require_full_fixture_coverage: true,
        }
    }
}

/// Aggregate gate counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenIsomorphismSummary {
    /// Fixtures compared (present in both golden and candidate).
    pub total_fixtures: usize,
    /// Byte-identical fixtures.
    pub matched: usize,
    /// Fixtures equal only under the declared invariants.
    pub normalized_matched: usize,
    /// Fixtures whose behavior changed.
    pub mismatched: usize,
    /// Behavior changes covered by a valid exception.
    pub covered_changes: usize,
    /// Behavior changes with no covering exception.
    pub uncovered_changes: usize,
    /// Blocking findings.
    pub blocking_findings: usize,
    /// Advisory findings.
    pub advisory_findings: usize,
    /// Whether behavior is preserved for every covered fixture.
    pub behavior_preserved: bool,
    /// Whether the optimization may be promoted.
    pub promotion_allowed: bool,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// The full golden-isomorphism verification report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldenIsomorphismReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic oracle/report identifier.
    pub oracle_id: String,
    /// The configuration that produced this report.
    pub config: GoldenIsomorphismConfig,
    /// The change under evaluation.
    pub change_id: String,
    /// The compiled isomorphism proof.
    pub proof: IsomorphismProof,
    /// Gate findings, in stable order.
    pub findings: Vec<GoldenFinding>,
    /// Aggregate summary.
    pub summary: GoldenIsomorphismSummary,
    /// Whether the optimization may be promoted.
    pub promotion_allowed: bool,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: GoldenJsonStatsArtifact,
    /// Replay command for the whole report.
    pub replay_command: String,
    /// SHA-256 fingerprint of the findings (output checksum).
    pub evidence_checksum: String,
}

impl GoldenIsomorphismReport {
    /// All blocking findings, in stable order.
    #[must_use]
    pub fn blocking(&self) -> Vec<&GoldenFinding> {
        self.findings
            .iter()
            .filter(|f| f.severity == GoldenSeverity::Block)
            .collect()
    }
}

/// The golden-output checksum oracle + isomorphism verification gate.
#[derive(Debug, Clone, Default)]
pub struct GoldenIsomorphismGate {
    config: GoldenIsomorphismConfig,
}

impl GoldenIsomorphismGate {
    /// Build a gate from configuration.
    #[must_use]
    pub fn new(config: GoldenIsomorphismConfig) -> Self {
        Self { config }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &GoldenIsomorphismConfig {
        &self.config
    }

    /// Verify a candidate against the golden baseline under the change's
    /// declared invariants and produce a deterministic promotion decision.
    #[must_use]
    pub fn verify(
        &self,
        change: &OptimizationChange,
        golden: &[GoldenOutput],
        candidate: &[GoldenOutput],
    ) -> GoldenIsomorphismReport {
        let proof = compile_isomorphism_proof(change, golden, candidate);
        let golden_ids: BTreeSet<&str> = golden.iter().map(|o| o.fixture_id.as_str()).collect();
        let candidate_ids: BTreeSet<&str> =
            candidate.iter().map(|o| o.fixture_id.as_str()).collect();

        // Valid exceptions: well-formed (per config) and honored (per config).
        let mut findings = Vec::new();
        let mut valid_exception_fixtures: BTreeSet<&str> = BTreeSet::new();
        for exception in &change.policy_exceptions {
            let evidence_ok =
                !self.config.require_exception_evidence || exception.is_evidence_backed();
            let well_formed = !exception.reason.trim().is_empty() && evidence_ok;
            if self.config.allow_policy_exceptions && well_formed {
                valid_exception_fixtures.insert(exception.fixture_id.as_str());
            } else {
                findings.push(GoldenFinding {
                    code: GoldenFindingCode::InvalidPolicyException,
                    severity: GoldenSeverity::Block,
                    change_id: change.change_id.clone(),
                    proof_id: proof.proof_id.clone(),
                    fixture_id: exception.fixture_id.clone(),
                    checksum_verdict: "n/a".to_string(),
                    detail: format!(
                        "policy exception for `{}` is not evidence-backed or exceptions are disabled",
                        exception.fixture_id
                    ),
                    remediation: "attach a non-empty rationale and at least one evidence reference"
                        .to_string(),
                    replay_command: fixture_replay_command(
                        &change.change_id,
                        &exception.fixture_id,
                        &proof.proof_id,
                    ),
                });
            }
        }

        // Coverage: golden and candidate fixture sets must match.
        if self.config.require_full_fixture_coverage {
            for id in golden_ids.difference(&candidate_ids) {
                findings.push(self.coverage_finding(
                    GoldenFindingCode::MissingCandidateOutput,
                    change,
                    &proof,
                    id,
                    "golden fixture has no candidate output",
                    "produce a candidate output for this fixture before promotion",
                ));
            }
            for id in candidate_ids.difference(&golden_ids) {
                findings.push(self.coverage_finding(
                    GoldenFindingCode::ExtraneousCandidateOutput,
                    change,
                    &proof,
                    id,
                    "candidate output has no golden baseline",
                    "capture a golden baseline for this fixture or drop it from the candidate",
                ));
            }
        }

        // Per-fixture behavior decisions.
        let mut matched = 0;
        let mut normalized_matched = 0;
        let mut mismatched = 0;
        let mut covered_changes = 0;
        let mut uncovered_changes = 0;
        for verdict in &proof.fixture_verdicts {
            let covered = valid_exception_fixtures.contains(verdict.fixture_id.as_str());
            match verdict.verdict {
                ChecksumVerdict::Match => {
                    matched += 1;
                    if covered {
                        findings.push(self.unused_exception_finding(change, &proof, verdict));
                    }
                }
                ChecksumVerdict::NormalizedMatch => {
                    normalized_matched += 1;
                    if covered {
                        findings.push(self.unused_exception_finding(change, &proof, verdict));
                    }
                }
                ChecksumVerdict::Mismatch => {
                    mismatched += 1;
                    if covered {
                        covered_changes += 1;
                        findings.push(GoldenFinding {
                            code: GoldenFindingCode::CoveredBehaviorChange,
                            severity: GoldenSeverity::Warn,
                            change_id: change.change_id.clone(),
                            proof_id: proof.proof_id.clone(),
                            fixture_id: verdict.fixture_id.clone(),
                            checksum_verdict: verdict.verdict.as_str().to_string(),
                            detail: format!(
                                "fixture `{}` behavior changed but is covered by a policy exception",
                                verdict.fixture_id
                            ),
                            remediation: "confirm the exception's rationale still holds".to_string(),
                            replay_command: fixture_replay_command(
                                &change.change_id,
                                &verdict.fixture_id,
                                &proof.proof_id,
                            ),
                        });
                    } else {
                        uncovered_changes += 1;
                        findings.push(GoldenFinding {
                            code: GoldenFindingCode::UncoveredBehaviorChange,
                            severity: GoldenSeverity::Block,
                            change_id: change.change_id.clone(),
                            proof_id: proof.proof_id.clone(),
                            fixture_id: verdict.fixture_id.clone(),
                            checksum_verdict: verdict.verdict.as_str().to_string(),
                            detail: format!(
                                "fixture `{}` behavior changed without a declared invariant or policy exception",
                                verdict.fixture_id
                            ),
                            remediation:
                                "declare the equivalence invariant that makes this an isomorphism, \
                                 or file an evidence-backed policy exception"
                                    .to_string(),
                            replay_command: fixture_replay_command(
                                &change.change_id,
                                &verdict.fixture_id,
                                &proof.proof_id,
                            ),
                        });
                    }
                }
            }
        }

        let blocking_findings = findings
            .iter()
            .filter(|f| f.severity == GoldenSeverity::Block)
            .count();
        let advisory_findings = findings.len() - blocking_findings;
        let promotion_allowed = blocking_findings == 0;

        let summary = GoldenIsomorphismSummary {
            total_fixtures: proof.fixture_verdicts.len(),
            matched,
            normalized_matched,
            mismatched,
            covered_changes,
            uncovered_changes,
            blocking_findings,
            advisory_findings,
            behavior_preserved: proof.behavior_preserved,
            promotion_allowed,
        };

        let evidence_checksum = stable_hash(&findings);
        let oracle_id = format!(
            "golden-isomorphism-{}",
            short_hash(&stable_hash(&OracleIdInput {
                schema_version: GOLDEN_ISOMORPHISM_SCHEMA_VERSION,
                change_id: &change.change_id,
                proof_id: &proof.proof_id,
                evidence_checksum: &evidence_checksum,
            }))
        );
        let replay_command = format!(
            "doctor_frankentui golden-isomorphism-verify --oracle-id {oracle_id} --change {}",
            change.change_id
        );
        let exported_json_stats = export_json_stats(
            &oracle_id,
            &self.config,
            &change.change_id,
            &proof,
            &summary,
            &findings,
        );

        GoldenIsomorphismReport {
            schema_version: GOLDEN_ISOMORPHISM_SCHEMA_VERSION.to_string(),
            oracle_id,
            config: self.config.clone(),
            change_id: change.change_id.clone(),
            proof,
            findings,
            summary,
            promotion_allowed,
            exported_json_stats,
            replay_command,
            evidence_checksum,
        }
    }

    fn coverage_finding(
        &self,
        code: GoldenFindingCode,
        change: &OptimizationChange,
        proof: &IsomorphismProof,
        fixture_id: &str,
        detail: &str,
        remediation: &str,
    ) -> GoldenFinding {
        GoldenFinding {
            code,
            severity: GoldenSeverity::Block,
            change_id: change.change_id.clone(),
            proof_id: proof.proof_id.clone(),
            fixture_id: fixture_id.to_string(),
            checksum_verdict: "n/a".to_string(),
            detail: format!("{detail}: `{fixture_id}`"),
            remediation: remediation.to_string(),
            replay_command: fixture_replay_command(&change.change_id, fixture_id, &proof.proof_id),
        }
    }

    fn unused_exception_finding(
        &self,
        change: &OptimizationChange,
        proof: &IsomorphismProof,
        verdict: &FixtureVerdict,
    ) -> GoldenFinding {
        GoldenFinding {
            code: GoldenFindingCode::UnusedPolicyException,
            severity: GoldenSeverity::Warn,
            change_id: change.change_id.clone(),
            proof_id: proof.proof_id.clone(),
            fixture_id: verdict.fixture_id.clone(),
            checksum_verdict: verdict.verdict.as_str().to_string(),
            detail: format!(
                "policy exception for `{}` is unused; the fixture's behavior did not change",
                verdict.fixture_id
            ),
            remediation: "remove the stale policy exception".to_string(),
            replay_command: fixture_replay_command(
                &change.change_id,
                &verdict.fixture_id,
                &proof.proof_id,
            ),
        }
    }
}

#[derive(Serialize)]
struct OracleIdInput<'a> {
    schema_version: &'a str,
    change_id: &'a str,
    proof_id: &'a str,
    evidence_checksum: &'a str,
}

fn export_json_stats(
    oracle_id: &str,
    config: &GoldenIsomorphismConfig,
    change_id: &str,
    proof: &IsomorphismProof,
    summary: &GoldenIsomorphismSummary,
    findings: &[GoldenFinding],
) -> GoldenJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        oracle_id: &'a str,
        change_id: &'a str,
        config: &'a GoldenIsomorphismConfig,
        summary: &'a GoldenIsomorphismSummary,
        proof: &'a IsomorphismProof,
        findings: &'a [GoldenFinding],
    }
    let payload = Export {
        schema_version: GOLDEN_ISOMORPHISM_SCHEMA_VERSION,
        oracle_id,
        change_id,
        config,
        summary,
        proof,
        findings,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    GoldenJsonStatsArtifact {
        path: format!("{oracle_id}/golden_isomorphism_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Hashing helpers (mirrors the crate's deterministic-stack idiom) ─────────

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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn golden_outputs() -> Vec<GoldenOutput> {
        vec![
            GoldenOutput::new(
                "fixture-scroll",
                "scroll-1k",
                vec![
                    GoldenRecord::new("row-0", 0)
                        .with_tokens(["a".to_string()])
                        .with_floats([1.0]),
                    GoldenRecord::new("row-1", 1)
                        .with_tokens(["b".to_string()])
                        .with_floats([2.5]),
                ],
            ),
            GoldenOutput::new(
                "fixture-grid",
                "grid-render",
                vec![
                    GoldenRecord::new("cell-0", 0).with_floats([0.10]),
                    GoldenRecord::new("cell-1", 1).with_floats([0.20]),
                ],
            ),
        ]
    }

    fn change(id: &str) -> OptimizationChange {
        OptimizationChange::new(id, "baseline-0", "diff-strategy")
            .with_description("swap diff scan strategy")
    }

    // ── Capture pipeline ─────────────────────────────────────────────────

    #[test]
    fn capture_manifest_is_deterministic() {
        let first = capture_golden_manifest("baseline-0", &golden_outputs());
        let second = capture_golden_manifest("baseline-0", &golden_outputs());
        assert_eq!(first, second);
        assert_eq!(first.manifest_checksum, second.manifest_checksum);
        assert_eq!(first.digests.len(), 2);
        assert!(first.digests.contains_key("fixture-scroll"));
    }

    #[test]
    fn capture_manifest_changes_with_output() {
        let base = capture_golden_manifest("baseline-0", &golden_outputs());
        let mut mutated = golden_outputs();
        mutated[0].records[0].tokens = vec!["z".to_string()];
        let other = capture_golden_manifest("baseline-0", &mutated);
        assert_ne!(base.manifest_checksum, other.manifest_checksum);
    }

    // ── Happy path: identical candidate ──────────────────────────────────

    #[test]
    fn identical_candidate_promotes() {
        let golden = golden_outputs();
        let report = GoldenIsomorphismGate::default().verify(&change("c1"), &golden, &golden);
        assert!(report.promotion_allowed);
        assert!(report.proof.behavior_preserved);
        assert!(report.blocking().is_empty());
        assert_eq!(report.summary.matched, 2);
        assert_eq!(report.summary.mismatched, 0);
        assert!(report.proof.counterexample.is_none());
    }

    // ── Ordering invariant ───────────────────────────────────────────────

    #[test]
    fn reorder_with_ordering_invariant_is_isomorphic() {
        let golden = golden_outputs();
        let mut candidate = golden_outputs();
        candidate[0].records.reverse();
        let change = change("c-order").with_invariant(IsomorphismInvariant::Ordering);
        let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        assert!(report.promotion_allowed);
        assert!(report.proof.behavior_preserved);
        assert_eq!(report.summary.normalized_matched, 1);
        let ordering = report
            .proof
            .invariant_checks
            .iter()
            .find(|c| c.invariant.kind() == InvariantKind::Ordering)
            .expect("ordering check");
        assert_eq!(ordering.status, InvariantStatus::HoldsLoadBearing);
    }

    #[test]
    fn reorder_without_ordering_invariant_blocks() {
        let golden = golden_outputs();
        let mut candidate = golden_outputs();
        candidate[0].records.reverse();
        let report =
            GoldenIsomorphismGate::default().verify(&change("c-no-order"), &golden, &candidate);
        assert!(!report.promotion_allowed);
        assert!(!report.proof.behavior_preserved);
        let counter = report
            .proof
            .counterexample
            .as_ref()
            .expect("counterexample");
        assert_eq!(counter.fixture_id, "fixture-scroll");
        assert!(counter.replay_command.contains("golden-isomorphism-verify"));
        assert!(
            report
                .blocking()
                .iter()
                .any(|f| f.code == GoldenFindingCode::UncoveredBehaviorChange)
        );
    }

    // ── Tie-break invariant ──────────────────────────────────────────────

    #[test]
    fn within_key_reorder_passes_under_tie_break() {
        let golden = vec![GoldenOutput::new(
            "f",
            "w",
            vec![
                GoldenRecord::new("k", 0).with_tokens(["x".to_string()]),
                GoldenRecord::new("k", 1).with_tokens(["y".to_string()]),
            ],
        )];
        let mut candidate = golden.clone();
        candidate[0].records.reverse();
        let change = change("c-tie").with_invariant(IsomorphismInvariant::TieBreak);
        let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        assert!(report.promotion_allowed);
        assert_eq!(report.summary.normalized_matched, 1);
    }

    #[test]
    fn cross_key_reorder_not_masked_by_tie_break() {
        let golden = vec![GoldenOutput::new(
            "f",
            "w",
            vec![GoldenRecord::new("k0", 0), GoldenRecord::new("k1", 1)],
        )];
        let mut candidate = golden.clone();
        candidate[0].records.reverse(); // swaps two DISTINCT keys
        let change = change("c-tie2").with_invariant(IsomorphismInvariant::TieBreak);
        let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        // Tie-break preserves the order of distinct keys, so this is a real change.
        assert!(!report.promotion_allowed);
        assert_eq!(report.summary.mismatched, 1);
    }

    // ── Floating-point invariant ─────────────────────────────────────────

    #[test]
    fn float_drift_within_tolerance_is_isomorphic() {
        let golden = golden_outputs();
        let mut candidate = golden_outputs();
        candidate[1].records[0].floats = vec![0.1004]; // golden is 0.10
        let change = change("c-fp").with_invariant(IsomorphismInvariant::FloatingPoint {
            tolerance_decimals: 2,
        });
        let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        assert!(report.promotion_allowed, "0.1004 rounds to 0.10 at 2 dp");
        assert_eq!(report.summary.normalized_matched, 1);
    }

    #[test]
    fn float_drift_beyond_tolerance_blocks() {
        let golden = golden_outputs();
        let mut candidate = golden_outputs();
        candidate[1].records[0].floats = vec![0.20]; // golden is 0.10
        let change = change("c-fp2").with_invariant(IsomorphismInvariant::FloatingPoint {
            tolerance_decimals: 2,
        });
        let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        assert!(!report.promotion_allowed);
        assert_eq!(report.summary.mismatched, 1);
    }

    #[test]
    fn float_drift_without_fp_invariant_blocks() {
        let golden = golden_outputs();
        let mut candidate = golden_outputs();
        candidate[1].records[0].floats = vec![0.1000001];
        let report = GoldenIsomorphismGate::default().verify(&change("c-fp3"), &golden, &candidate);
        assert!(!report.promotion_allowed);
    }

    // ── RNG-seed invariant ───────────────────────────────────────────────

    #[test]
    fn seed_change_is_isomorphic_under_rng_invariant() {
        let golden = vec![GoldenOutput::new(
            "f",
            "w",
            vec![GoldenRecord::new("k", 0).with_seed(7)],
        )];
        let mut candidate = golden.clone();
        candidate[0].records[0].seed = Some(42);
        let change = change("c-seed").with_invariant(IsomorphismInvariant::RngSeed);
        let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        assert!(report.promotion_allowed);
        assert_eq!(report.summary.normalized_matched, 1);
    }

    // ── Policy exceptions ────────────────────────────────────────────────

    #[test]
    fn behavior_change_with_valid_exception_promotes() {
        let golden = golden_outputs();
        let mut candidate = golden_outputs();
        candidate[0].records[0].tokens = vec!["changed".to_string()];
        let change = change("c-ex").with_exception(GoldenPolicyException::new(
            "fixture-scroll",
            "intentional content tweak",
            ["bd-3bxhj.8.17-evidence".to_string()],
        ));
        let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        assert!(report.promotion_allowed);
        assert_eq!(report.summary.covered_changes, 1);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == GoldenFindingCode::CoveredBehaviorChange)
        );
    }

    #[test]
    fn behavior_change_with_evidence_free_exception_blocks() {
        let golden = golden_outputs();
        let mut candidate = golden_outputs();
        candidate[0].records[0].tokens = vec!["changed".to_string()];
        let change = change("c-ex2").with_exception(GoldenPolicyException::new(
            "fixture-scroll",
            "no evidence attached",
            std::iter::empty(),
        ));
        let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        assert!(!report.promotion_allowed);
        assert!(
            report
                .blocking()
                .iter()
                .any(|f| f.code == GoldenFindingCode::InvalidPolicyException)
        );
    }

    #[test]
    fn exception_for_unchanged_fixture_is_advisory() {
        let golden = golden_outputs();
        let change = change("c-ex3").with_exception(GoldenPolicyException::new(
            "fixture-scroll",
            "stale exception",
            ["e1".to_string()],
        ));
        let report = GoldenIsomorphismGate::default().verify(&change, &golden, &golden);
        assert!(report.promotion_allowed);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == GoldenFindingCode::UnusedPolicyException
                    && f.severity == GoldenSeverity::Warn)
        );
    }

    // ── Coverage ─────────────────────────────────────────────────────────

    #[test]
    fn missing_candidate_output_blocks() {
        let golden = golden_outputs();
        let candidate = vec![golden[0].clone()];
        let report =
            GoldenIsomorphismGate::default().verify(&change("c-miss"), &golden, &candidate);
        assert!(!report.promotion_allowed);
        assert!(
            report
                .blocking()
                .iter()
                .any(|f| f.code == GoldenFindingCode::MissingCandidateOutput)
        );
    }

    #[test]
    fn extraneous_candidate_output_blocks() {
        let golden = vec![golden_outputs()[0].clone()];
        let candidate = golden_outputs();
        let report =
            GoldenIsomorphismGate::default().verify(&change("c-extra"), &golden, &candidate);
        assert!(!report.promotion_allowed);
        assert!(
            report
                .blocking()
                .iter()
                .any(|f| f.code == GoldenFindingCode::ExtraneousCandidateOutput)
        );
    }

    // ── Acceptance criteria ──────────────────────────────────────────────

    #[test]
    fn proof_links_change_id_and_fixture_set() {
        let golden = golden_outputs();
        let report = GoldenIsomorphismGate::default().verify(&change("c-link"), &golden, &golden);
        assert_eq!(report.proof.change_id, "c-link");
        assert_eq!(report.proof.baseline_change_id, "baseline-0");
        assert_eq!(report.proof.fixture_ids.len(), 2);
        assert_eq!(report.proof.fixture_verdicts.len(), 2);
        assert!(!report.proof.golden_manifest_checksum.is_empty());
        assert!(!report.proof.candidate_manifest_checksum.is_empty());
    }

    #[test]
    fn every_finding_carries_proof_id_and_replay_command() {
        let golden = golden_outputs();
        let mut candidate = golden_outputs();
        candidate[0].records.reverse();
        let report =
            GoldenIsomorphismGate::default().verify(&change("c-fields"), &golden, &candidate);
        assert!(!report.findings.is_empty());
        for finding in &report.findings {
            assert!(!finding.proof_id.is_empty());
            assert_eq!(finding.proof_id, report.proof.proof_id);
            assert!(!finding.checksum_verdict.is_empty());
            assert!(!finding.remediation.is_empty());
            assert!(finding.replay_command.contains("golden-isomorphism-verify"));
            assert!(finding.replay_command.contains(&finding.fixture_id));
        }
    }

    // ── Determinism ──────────────────────────────────────────────────────

    #[test]
    fn report_is_deterministic() {
        let golden = golden_outputs();
        let mut candidate = golden_outputs();
        candidate[0].records.reverse();
        let change = change("c-det").with_invariant(IsomorphismInvariant::Ordering);
        let first = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        let second = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
        assert_eq!(first.oracle_id, second.oracle_id);
        assert_eq!(first.proof.proof_id, second.proof.proof_id);
        assert_eq!(first.evidence_checksum, second.evidence_checksum);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first, second);
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let golden = golden_outputs();
        let report = GoldenIsomorphismGate::default().verify(&change("c-serde"), &golden, &golden);
        let json = serde_json::to_string(&report).expect("serialize");
        let restored: GoldenIsomorphismReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.oracle_id, report.oracle_id);
        assert_eq!(restored.proof, report.proof);
        assert_eq!(restored.findings, report.findings);
        assert_eq!(restored.evidence_checksum, report.evidence_checksum);
    }

    #[test]
    fn json_stats_checksum_is_self_consistent() {
        let golden = golden_outputs();
        let report = GoldenIsomorphismGate::default().verify(&change("c-json"), &golden, &golden);
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert_eq!(report.evidence_checksum, stable_hash(&report.findings));
    }

    // ── Property tests ───────────────────────────────────────────────────

    fn outputs_from_seeds(seeds: &[u8]) -> Vec<GoldenOutput> {
        let records = seeds
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                GoldenRecord::new(format!("k{i}"), i as u64)
                    .with_tokens([format!("t{s}")])
                    .with_floats([f64::from(s)])
            })
            .collect();
        vec![GoldenOutput::new("f", "w", records)]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Identical golden==candidate always promotes with behavior preserved.
        #[test]
        fn prop_identity_always_promotes(seeds in proptest::collection::vec(0u8..32, 0..8)) {
            let golden = outputs_from_seeds(&seeds);
            let report = GoldenIsomorphismGate::default().verify(&change("p1"), &golden, &golden);
            prop_assert!(report.promotion_allowed);
            prop_assert!(report.proof.behavior_preserved);
            prop_assert_eq!(report.summary.mismatched, 0);
        }

        /// The gate is deterministic: same inputs → byte-identical report.
        #[test]
        fn prop_report_is_byte_stable(
            seeds in proptest::collection::vec(0u8..32, 1..8),
            id in "[a-z]{1,6}",
        ) {
            let golden = outputs_from_seeds(&seeds);
            let mut candidate = outputs_from_seeds(&seeds);
            candidate[0].records.reverse();
            let change = change(&id).with_invariant(IsomorphismInvariant::Ordering);
            let first = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
            let second = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
            prop_assert_eq!(&first.oracle_id, &second.oracle_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.findings, &second.findings);
        }

        /// With no exceptions declared, promotion is allowed iff behavior is
        /// preserved for every fixture (and coverage is complete).
        #[test]
        fn prop_promotion_matches_behavior_preservation(
            seeds in proptest::collection::vec(0u8..32, 1..8),
            reverse in any::<bool>(),
            declare_ordering in any::<bool>(),
        ) {
            let golden = outputs_from_seeds(&seeds);
            let mut candidate = outputs_from_seeds(&seeds);
            if reverse {
                candidate[0].records.reverse();
            }
            let mut change = change("p3");
            if declare_ordering {
                change = change.with_invariant(IsomorphismInvariant::Ordering);
            }
            let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
            prop_assert_eq!(report.promotion_allowed, report.proof.behavior_preserved);
        }

        /// A valid policy exception for the (single) fixture always permits
        /// promotion, even when behavior changed.
        #[test]
        fn prop_valid_exception_always_promotes(
            seeds in proptest::collection::vec(1u8..32, 1..8),
        ) {
            let golden = outputs_from_seeds(&seeds);
            let mut candidate = outputs_from_seeds(&seeds);
            candidate[0].records[0].tokens = vec!["mutated".to_string()];
            let change = change("p4").with_exception(GoldenPolicyException::new(
                "f",
                "intentional",
                ["evidence".to_string()],
            ));
            let report = GoldenIsomorphismGate::default().verify(&change, &golden, &candidate);
            prop_assert!(report.promotion_allowed);
        }
    }
}
