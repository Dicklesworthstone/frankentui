#![forbid(unsafe_code)]

//! Behavior-preservation proof oracles for optimization validation (bd-js3c3).
//!
//! This module defines the equivalence contracts that optimization work must
//! satisfy:
//!
//! - **Equivalence dimensions**: What must match exactly, what may differ with justification
//! - **Proof templates**: Structured evidence for behavioral preservation
//! - **Replay commands**: Counterexample reproduction from failed proofs
//! - **Difference classification**: Presentation-only vs semantic divergence
//!
//! # Status: vocabulary, not machinery
//!
//! These are definitions. Nothing here captures a checksum, runs a candidate,
//! or compares two runs — the module imports only `std`, and no caller anywhere
//! in the workspace consumes it. An earlier version of this doc said it "builds
//! on `golden.rs` and `shadow_run.rs`"; it never did, and a reader who believed
//! that would think optimizations were being checked against these contracts
//! automatically. They are not. `golden.rs` and `shadow_run.rs` are where the
//! executable equivalence checking actually lives, and they do not know this
//! module exists. Closing that gap is `bd-g00-root-epic-ewths.11.5`.
//!
//! # Design rationale
//!
//! Optimizations that skip work or change scheduling can accidentally alter
//! visible output, timing-sensitive semantics, or failure artifacts. This
//! module names the properties such a proof layer would have to establish.
//!
//! # Equivalence dimensions
//!
//! ```text
//! EquivalenceDimension
//! ├── RenderOutput     — frame buffer checksums (strict)
//! ├── CellContent      — per-cell character content (strict)
//! ├── CellStyle        — per-cell fg/bg/attrs (strict unless relaxed)
//! ├── EventOrdering    — model update sequence (strict)
//! ├── SubscriptionSet  — active subscription IDs (strict)
//! ├── CommandSequence  — commands returned from update (strict)
//! ├── TieBreaking      — deterministic ordering under ambiguity (strict)
//! ├── ShutdownBehavior — exit path and cleanup (strict)
//! ├── ArtifactContent  — evidence file content (strict)
//! └── TimingBounds     — performance within SLO (relaxed with evidence)
//! ```

use std::collections::HashSet;

/// A dimension of behavioral equivalence.
///
/// Each dimension defines what aspect of behavior must be preserved
/// across an optimization change. Dimensions are either strict (must
/// match exactly) or relaxed (may differ with documented justification).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EquivalenceDimension {
    /// Frame buffer checksums must match (BLAKE3).
    RenderOutput,
    /// Per-cell character content must match.
    CellContent,
    /// Per-cell foreground/background/attributes must match.
    CellStyle,
    /// Model update event sequence must match.
    EventOrdering,
    /// Set of active subscription IDs must match.
    SubscriptionSet,
    /// Commands returned from model updates must match.
    CommandSequence,
    /// Deterministic ordering under ambiguity must match.
    TieBreaking,
    /// Exit path and cleanup behavior must match.
    ShutdownBehavior,
    /// Evidence artifact content must match.
    ArtifactContent,
    /// Performance must remain within SLO bounds (relaxed).
    TimingBounds,
}

impl EquivalenceDimension {
    /// Whether this dimension requires strict equality.
    ///
    /// Strict dimensions must produce identical output. Relaxed dimensions
    /// may differ if the difference is documented and justified.
    #[must_use]
    pub const fn is_strict(&self) -> bool {
        !matches!(self, Self::TimingBounds)
    }

    /// Human-readable description of what this dimension checks.
    #[must_use]
    pub const fn description(&self) -> &'static str {
        match self {
            Self::RenderOutput => "Frame buffer checksums (BLAKE3) must be identical",
            Self::CellContent => "Per-cell character content must be identical",
            Self::CellStyle => "Per-cell fg/bg/attrs must be identical",
            Self::EventOrdering => "Model update event sequence must be identical",
            Self::SubscriptionSet => "Active subscription IDs must be identical",
            Self::CommandSequence => "Commands from model updates must be identical",
            Self::TieBreaking => "Deterministic ordering under ambiguity must be identical",
            Self::ShutdownBehavior => "Exit path and cleanup must be identical",
            Self::ArtifactContent => "Evidence artifact content must be identical",
            Self::TimingBounds => "Performance must remain within SLO bounds (allowed to differ)",
        }
    }

    /// Which domain this dimension belongs to.
    #[must_use]
    pub const fn domain(&self) -> EquivalenceDomain {
        match self {
            Self::RenderOutput | Self::CellContent | Self::CellStyle => EquivalenceDomain::Render,
            Self::EventOrdering
            | Self::SubscriptionSet
            | Self::CommandSequence
            | Self::TieBreaking
            | Self::ShutdownBehavior => EquivalenceDomain::Runtime,
            Self::ArtifactContent => EquivalenceDomain::Doctor,
            Self::TimingBounds => EquivalenceDomain::Performance,
        }
    }

    /// All equivalence dimensions.
    pub const ALL: &'static [EquivalenceDimension] = &[
        Self::RenderOutput,
        Self::CellContent,
        Self::CellStyle,
        Self::EventOrdering,
        Self::SubscriptionSet,
        Self::CommandSequence,
        Self::TieBreaking,
        Self::ShutdownBehavior,
        Self::ArtifactContent,
        Self::TimingBounds,
    ];
}

/// Domain that an equivalence dimension belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EquivalenceDomain {
    /// Render pipeline: buffer, diff, presenter.
    Render,
    /// Runtime: event loop, subscriptions, commands.
    Runtime,
    /// Doctor: evidence artifacts, subprocess orchestration.
    Doctor,
    /// Performance: timing, throughput, resource usage.
    Performance,
}

/// Classification of a difference found during proof validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DifferenceClass {
    /// Semantic difference: behavior changed in a user-visible way.
    /// MUST be fixed or explicitly justified.
    Semantic,
    /// Presentation-only difference: output looks different but behavior
    /// is equivalent (e.g., trailing whitespace, ANSI reset ordering).
    /// MAY be accepted with documentation.
    PresentationOnly,
    /// Performance difference: timing changed but correctness preserved.
    /// Acceptable if within SLO bounds.
    PerformanceOnly,
}

/// A structured proof of behavioral preservation.
///
/// This is the evidence template that optimization PRs must fill out
/// when golden checksums change.
#[derive(Debug, Clone)]
pub struct BehaviorProof {
    /// What optimization was made.
    pub change_description: String,
    /// Why the new output is equivalent to the old output.
    pub equivalence_justification: String,
    /// Dimensions that were verified.
    pub verified_dimensions: HashSet<EquivalenceDimension>,
    /// Dimensions that intentionally changed (with justification).
    pub relaxed_dimensions: Vec<RelaxedDimension>,
    /// Old golden checksums.
    pub old_checksums: Vec<String>,
    /// New golden checksums.
    pub new_checksums: Vec<String>,
    /// Replay command for reproduction.
    pub replay_command: String,
    /// Scenario name and seed for deterministic replay.
    pub scenario: String,
    /// Random seed used.
    pub seed: u64,
    /// Viewport dimensions.
    pub viewport: (u16, u16),
}

/// A dimension that was intentionally relaxed with justification.
#[derive(Debug, Clone)]
pub struct RelaxedDimension {
    /// Which dimension was relaxed.
    pub dimension: EquivalenceDimension,
    /// Why the difference is acceptable.
    pub justification: String,
    /// Classification of the difference.
    pub difference_class: DifferenceClass,
}

/// Result of validating a behavior proof.
#[derive(Debug, Clone)]
pub struct ProofValidation {
    /// Whether the proof is complete and valid.
    pub valid: bool,
    /// Missing strict dimensions that should have been verified.
    pub missing_dimensions: Vec<EquivalenceDimension>,
    /// Issues found during validation.
    pub issues: Vec<String>,
}

/// Validate a behavior proof for completeness.
///
/// Checks that:
/// - All strict dimensions are either verified or explicitly relaxed
/// - Relaxed dimensions have justification and classification
/// - Replay command is non-empty
/// - Checksums are present
#[must_use]
pub fn validate_proof(proof: &BehaviorProof) -> ProofValidation {
    let mut issues = Vec::new();
    let mut missing = Vec::new();

    // Check all strict dimensions are covered.
    let relaxed_dims: HashSet<EquivalenceDimension> = proof
        .relaxed_dimensions
        .iter()
        .map(|r| r.dimension)
        .collect();

    for dim in EquivalenceDimension::ALL {
        if dim.is_strict()
            && !proof.verified_dimensions.contains(dim)
            && !relaxed_dims.contains(dim)
        {
            missing.push(*dim);
        }
    }

    if !missing.is_empty() {
        issues.push(format!(
            "Missing coverage for {} strict dimension(s)",
            missing.len()
        ));
    }

    // Check relaxed dimensions have justification.
    for relaxed in &proof.relaxed_dimensions {
        if relaxed.justification.is_empty() {
            issues.push(format!(
                "Relaxed dimension {:?} has empty justification",
                relaxed.dimension
            ));
        }
    }

    // Check replay command.
    if proof.replay_command.is_empty() {
        issues.push("Replay command is empty".to_string());
    }

    // Check checksums.
    if proof.old_checksums.is_empty() && proof.new_checksums.is_empty() {
        issues.push("Both old and new checksums are empty".to_string());
    }

    let valid = issues.is_empty();
    ProofValidation {
        valid,
        missing_dimensions: missing,
        issues,
    }
}

/// Generate a replay command for reproducing a proof scenario.
///
/// Returns a shell command that can reproduce the exact test conditions.
#[must_use]
pub fn replay_command(scenario: &str, seed: u64, viewport: (u16, u16)) -> String {
    format!(
        "GOLDEN_SEED={seed} cargo test -p ftui-harness -- {scenario} --nocapture \
         # viewport: {w}x{h}",
        seed = seed,
        scenario = scenario,
        w = viewport.0,
        h = viewport.1,
    )
}

/// Dimensions required for each equivalence domain.
#[must_use]
pub fn dimensions_for_domain(domain: EquivalenceDomain) -> Vec<EquivalenceDimension> {
    EquivalenceDimension::ALL
        .iter()
        .filter(|d| d.domain() == domain)
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_dimensions_have_descriptions() {
        for dim in EquivalenceDimension::ALL {
            assert!(
                !dim.description().is_empty(),
                "{:?} must have a description",
                dim
            );
        }
    }

    #[test]
    fn all_dimensions_have_domains() {
        for dim in EquivalenceDimension::ALL {
            let _domain = dim.domain(); // no panic
        }
    }

    #[test]
    fn strict_dimensions_are_majority() {
        let strict_count = EquivalenceDimension::ALL
            .iter()
            .filter(|d| d.is_strict())
            .count();
        assert!(
            strict_count >= 8,
            "most dimensions should be strict, got {} strict out of {}",
            strict_count,
            EquivalenceDimension::ALL.len()
        );
    }

    #[test]
    fn timing_is_relaxed() {
        assert!(
            !EquivalenceDimension::TimingBounds.is_strict(),
            "timing must be relaxed — optimizations change timing by definition"
        );
    }

    #[test]
    fn render_domain_has_output_and_content() {
        let render_dims = dimensions_for_domain(EquivalenceDomain::Render);
        assert!(render_dims.contains(&EquivalenceDimension::RenderOutput));
        assert!(render_dims.contains(&EquivalenceDimension::CellContent));
        assert!(render_dims.contains(&EquivalenceDimension::CellStyle));
    }

    #[test]
    fn runtime_domain_has_ordering_and_shutdown() {
        let runtime_dims = dimensions_for_domain(EquivalenceDomain::Runtime);
        assert!(runtime_dims.contains(&EquivalenceDimension::EventOrdering));
        assert!(runtime_dims.contains(&EquivalenceDimension::ShutdownBehavior));
    }

    #[test]
    fn doctor_domain_has_artifact_content() {
        let doctor_dims = dimensions_for_domain(EquivalenceDomain::Doctor);
        assert!(doctor_dims.contains(&EquivalenceDimension::ArtifactContent));
    }

    #[test]
    fn all_domains_covered() {
        let domains: HashSet<EquivalenceDomain> = EquivalenceDimension::ALL
            .iter()
            .map(|d| d.domain())
            .collect();
        assert!(domains.contains(&EquivalenceDomain::Render));
        assert!(domains.contains(&EquivalenceDomain::Runtime));
        assert!(domains.contains(&EquivalenceDomain::Doctor));
        assert!(domains.contains(&EquivalenceDomain::Performance));
    }

    #[test]
    fn replay_command_includes_seed_and_scenario() {
        let cmd = replay_command("test_resize_80x24", 42, (80, 24));
        assert!(cmd.contains("GOLDEN_SEED=42"));
        assert!(cmd.contains("test_resize_80x24"));
        assert!(cmd.contains("80x24"));
    }

    #[test]
    fn validate_complete_proof_passes() {
        let proof = BehaviorProof {
            change_description: "Optimize buffer diff to skip unchanged rows".to_string(),
            equivalence_justification: "Only unchanged rows are skipped; output is identical"
                .to_string(),
            verified_dimensions: EquivalenceDimension::ALL
                .iter()
                .filter(|d| d.is_strict())
                .copied()
                .collect(),
            relaxed_dimensions: vec![RelaxedDimension {
                dimension: EquivalenceDimension::TimingBounds,
                justification: "Optimization reduces frame time by ~15%".to_string(),
                difference_class: DifferenceClass::PerformanceOnly,
            }],
            old_checksums: vec!["blake3:abc123".to_string()],
            new_checksums: vec!["blake3:abc123".to_string()],
            replay_command: "GOLDEN_SEED=42 cargo test -p ftui-harness -- resize".to_string(),
            scenario: "resize_80x24".to_string(),
            seed: 42,
            viewport: (80, 24),
        };

        let result = validate_proof(&proof);
        assert!(
            result.valid,
            "complete proof should pass: {:?}",
            result.issues
        );
        assert!(result.missing_dimensions.is_empty());
    }

    #[test]
    fn validate_incomplete_proof_fails() {
        let proof = BehaviorProof {
            change_description: "Some optimization".to_string(),
            equivalence_justification: "Trust me".to_string(),
            verified_dimensions: HashSet::new(), // nothing verified!
            relaxed_dimensions: vec![],
            old_checksums: vec!["old".to_string()],
            new_checksums: vec!["new".to_string()],
            replay_command: "cargo test".to_string(),
            scenario: "test".to_string(),
            seed: 0,
            viewport: (80, 24),
        };

        let result = validate_proof(&proof);
        assert!(!result.valid, "incomplete proof should fail");
        assert!(
            !result.missing_dimensions.is_empty(),
            "should report missing dimensions"
        );
    }

    #[test]
    fn validate_proof_without_replay_command_fails() {
        let proof = BehaviorProof {
            change_description: "test".to_string(),
            equivalence_justification: "test".to_string(),
            verified_dimensions: EquivalenceDimension::ALL
                .iter()
                .filter(|d| d.is_strict())
                .copied()
                .collect(),
            relaxed_dimensions: vec![],
            old_checksums: vec!["a".to_string()],
            new_checksums: vec!["a".to_string()],
            replay_command: String::new(), // empty!
            scenario: "test".to_string(),
            seed: 0,
            viewport: (80, 24),
        };

        let result = validate_proof(&proof);
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("Replay command")));
    }

    #[test]
    fn validate_relaxed_without_justification_fails() {
        let mut verified: HashSet<EquivalenceDimension> = EquivalenceDimension::ALL
            .iter()
            .filter(|d| d.is_strict())
            .copied()
            .collect();
        verified.remove(&EquivalenceDimension::RenderOutput);

        let proof = BehaviorProof {
            change_description: "test".to_string(),
            equivalence_justification: "test".to_string(),
            verified_dimensions: verified,
            relaxed_dimensions: vec![RelaxedDimension {
                dimension: EquivalenceDimension::RenderOutput,
                justification: String::new(), // empty justification!
                difference_class: DifferenceClass::PresentationOnly,
            }],
            old_checksums: vec!["a".to_string()],
            new_checksums: vec!["b".to_string()],
            replay_command: "cargo test".to_string(),
            scenario: "test".to_string(),
            seed: 0,
            viewport: (80, 24),
        };

        let result = validate_proof(&proof);
        assert!(!result.valid);
        assert!(
            result
                .issues
                .iter()
                .any(|i| i.contains("empty justification"))
        );
    }

    #[test]
    fn difference_classes_are_distinct() {
        assert_ne!(DifferenceClass::Semantic, DifferenceClass::PresentationOnly);
        assert_ne!(
            DifferenceClass::PresentationOnly,
            DifferenceClass::PerformanceOnly
        );
        assert_ne!(DifferenceClass::Semantic, DifferenceClass::PerformanceOnly);
    }

    #[test]
    fn total_dimensions_count() {
        assert_eq!(
            EquivalenceDimension::ALL.len(),
            10,
            "should have exactly 10 equivalence dimensions"
        );
    }
}
