//! Deterministic multi-modal profiler orchestration and hotspot extraction.
//!
//! This module turns externally-collected profiling samples (CPU self-time,
//! allocation pressure, and syscall overhead) into a stable hotspot table with
//! normalized cross-modal attribution, candidate optimization levers, evidence
//! logs, and [`benchmark_regression_gate`](crate::benchmark_regression_gate)-
//! compatible observations.
//!
//! Process execution stays outside the library boundary: the orchestration plan
//! records the profiler invocations (flamegraph, allocation profiler, syscall
//! summary) so CI and E2E scripts can run their preferred tooling, while the
//! library normalizes the observed samples into reproducible artifacts. For a
//! fixed plan and a fixed set of samples the output is byte-for-byte identical.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::benchmark_regression_gate::{BenchmarkHotspotObservation, BenchmarkProfileKey};

/// Schema version for profile orchestration plans and reports.
pub const PROFILE_ORCHESTRATION_SCHEMA_VERSION: &str = "profile-orchestration-v1";

/// Profiling modality attributing one dimension of runtime cost.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ProfileModality {
    /// CPU self-time (flamegraph-style sampling).
    #[default]
    Cpu,
    /// Allocation pressure (bytes allocated / retained).
    Allocation,
    /// Syscall overhead (count or time spent in kernel).
    Syscall,
}

impl ProfileModality {
    /// Canonical orchestration order used for deterministic iteration.
    pub const ALL: [Self; 3] = [Self::Cpu, Self::Allocation, Self::Syscall];

    /// Stable lowercase tag used in identifiers and evidence.
    #[must_use]
    pub fn category_tag(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Allocation => "allocation",
            Self::Syscall => "syscall",
        }
    }

    /// Human-facing unit label for the modality's raw weight.
    #[must_use]
    pub fn weight_unit(self) -> &'static str {
        match self {
            Self::Cpu => "self_time_us",
            Self::Allocation => "bytes",
            Self::Syscall => "syscalls",
        }
    }
}

/// One profiler invocation recorded in the orchestration plan.
///
/// The command is *described*, not executed, here. A runner produces the raw
/// artifact at `output_artifact`, and the resulting samples are fed back into
/// [`ProfileOrchestrator::orchestrate`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfilerInvocation {
    pub modality: ProfileModality,
    pub tool: String,
    pub args: Vec<String>,
    pub output_artifact: String,
    pub fingerprint_sha256: String,
}

impl ProfilerInvocation {
    #[must_use]
    pub fn new(
        modality: ProfileModality,
        tool: impl Into<String>,
        args: Vec<String>,
        output_artifact: impl Into<String>,
    ) -> Self {
        let tool = tool.into();
        let output_artifact = output_artifact.into();
        let fingerprint_sha256 = stable_hash(&InvocationHashInput {
            modality,
            tool: tool.as_str(),
            args: &args,
            output_artifact: output_artifact.as_str(),
        });
        Self {
            modality,
            tool,
            args,
            output_artifact,
            fingerprint_sha256,
        }
    }

    /// Deterministic shell-style replay command for this invocation.
    #[must_use]
    pub fn replay_command(&self) -> String {
        let rendered = std::iter::once(self.tool.clone())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        format!("{rendered} # artifact={}", self.output_artifact)
    }
}

#[derive(Serialize)]
struct InvocationHashInput<'a> {
    modality: ProfileModality,
    tool: &'a str,
    args: &'a [String],
    output_artifact: &'a str,
}

/// Orchestration plan: target identity plus the profiler invocations to run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileOrchestrationPlan {
    pub schema_version: String,
    pub plan_id: String,
    pub target_id: String,
    pub profile: BenchmarkProfileKey,
    pub deterministic_seed: u64,
    pub invocations: Vec<ProfilerInvocation>,
}

impl ProfileOrchestrationPlan {
    #[must_use]
    pub fn new(
        target_id: impl Into<String>,
        profile: BenchmarkProfileKey,
        deterministic_seed: u64,
        mut invocations: Vec<ProfilerInvocation>,
    ) -> Self {
        invocations.sort_by(|left, right| {
            left.modality
                .cmp(&right.modality)
                .then_with(|| left.tool.cmp(&right.tool))
                .then_with(|| left.output_artifact.cmp(&right.output_artifact))
        });
        let target_id = target_id.into();
        let invocation_fingerprints = invocations
            .iter()
            .map(|invocation| invocation.fingerprint_sha256.clone())
            .collect::<Vec<_>>();
        let plan_id = format!(
            "profile-plan-{}",
            short_hash(&stable_hash(&PlanHashInput {
                schema_version: PROFILE_ORCHESTRATION_SCHEMA_VERSION,
                target_id: target_id.as_str(),
                profile: &profile,
                deterministic_seed,
                invocation_fingerprints: &invocation_fingerprints,
            }))
        );
        Self {
            schema_version: PROFILE_ORCHESTRATION_SCHEMA_VERSION.to_string(),
            plan_id,
            target_id,
            profile,
            deterministic_seed,
            invocations,
        }
    }

    /// Replay command that re-runs the whole orchestration for this plan.
    #[must_use]
    pub fn replay_command(&self) -> String {
        format!(
            "doctor_frankentui profile-orchestrate --plan-id {} --target {} --seed {}",
            self.plan_id, self.target_id, self.deterministic_seed
        )
    }

    /// All per-invocation replay commands in deterministic order.
    #[must_use]
    pub fn invocation_replay_commands(&self) -> Vec<String> {
        self.invocations
            .iter()
            .map(ProfilerInvocation::replay_command)
            .collect()
    }

    /// Map each modality to the artifact path that attributes it (if declared).
    fn artifact_for_modality(&self) -> BTreeMap<ProfileModality, Vec<String>> {
        let mut map: BTreeMap<ProfileModality, Vec<String>> = BTreeMap::new();
        for invocation in &self.invocations {
            map.entry(invocation.modality)
                .or_default()
                .push(invocation.output_artifact.clone());
        }
        for artifacts in map.values_mut() {
            *artifacts = sorted_unique(std::mem::take(artifacts));
        }
        map
    }
}

#[derive(Serialize)]
struct PlanHashInput<'a> {
    schema_version: &'a str,
    target_id: &'a str,
    profile: &'a BenchmarkProfileKey,
    deterministic_seed: u64,
    invocation_fingerprints: &'a [String],
}

/// One attributed profiling sample for a single symbol/site in one modality.
///
/// `self_weight` carries the modality's native unit (CPU microseconds,
/// allocation bytes, syscall count). `total_weight_hint` lets the runner pin the
/// modality total when some cost is unattributed (`[unknown]` frames); when
/// present and larger than the attributed sum it is used as the denominator so
/// percentages never exceed 100%.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileSample {
    pub modality: ProfileModality,
    pub symbol: String,
    pub source_location: String,
    pub self_weight: f64,
    pub total_weight_hint: Option<f64>,
    pub call_count: u64,
}

impl ProfileSample {
    #[must_use]
    pub fn new(
        modality: ProfileModality,
        symbol: impl Into<String>,
        source_location: impl Into<String>,
        self_weight: f64,
        call_count: u64,
    ) -> Self {
        Self {
            modality,
            symbol: symbol.into(),
            source_location: source_location.into(),
            self_weight: self_weight.max(0.0),
            total_weight_hint: None,
            call_count,
        }
    }

    #[must_use]
    pub fn with_total_weight_hint(mut self, total_weight_hint: f64) -> Self {
        self.total_weight_hint = Some(total_weight_hint.max(0.0));
        self
    }

    fn site_key(&self) -> (String, String) {
        (self.symbol.clone(), self.source_location.clone())
    }
}

/// Blend weights for combining per-modality fractions into a single score.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ProfileModalityWeights {
    pub cpu: f64,
    pub allocation: f64,
    pub syscall: f64,
}

impl Default for ProfileModalityWeights {
    fn default() -> Self {
        Self {
            cpu: 0.5,
            allocation: 0.3,
            syscall: 0.2,
        }
    }
}

impl ProfileModalityWeights {
    #[must_use]
    pub fn weight_for(self, modality: ProfileModality) -> f64 {
        match modality {
            ProfileModality::Cpu => self.cpu.max(0.0),
            ProfileModality::Allocation => self.allocation.max(0.0),
            ProfileModality::Syscall => self.syscall.max(0.0),
        }
    }
}

/// Orchestrator configuration controlling surfacing and scoring.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ProfileOrchestrationConfig {
    /// Number of hotspots surfaced in `top_hotspots` (acceptance: top-5).
    pub top_n: usize,
    /// Blend weights for cross-modal normalized attribution.
    pub modality_weights: ProfileModalityWeights,
    /// Multiplier mapping attribution fraction onto the opportunity-score scale.
    pub opportunity_scale: f64,
    /// Per-extra-modality bonus applied to the diversity factor.
    pub diversity_step: f64,
}

impl Default for ProfileOrchestrationConfig {
    fn default() -> Self {
        Self {
            top_n: 5,
            modality_weights: ProfileModalityWeights::default(),
            opportunity_scale: 10.0,
            diversity_step: 0.25,
        }
    }
}

impl ProfileOrchestrationConfig {
    #[must_use]
    pub fn with_top_n(mut self, top_n: usize) -> Self {
        self.top_n = top_n;
        self
    }

    #[must_use]
    pub fn with_modality_weights(mut self, modality_weights: ProfileModalityWeights) -> Self {
        self.modality_weights = modality_weights;
        self
    }

    #[must_use]
    pub fn with_opportunity_scale(mut self, opportunity_scale: f64) -> Self {
        self.opportunity_scale = opportunity_scale.max(0.0);
        self
    }

    #[must_use]
    pub fn with_diversity_step(mut self, diversity_step: f64) -> Self {
        self.diversity_step = diversity_step.max(0.0);
        self
    }

    fn fingerprint(&self) -> String {
        stable_hash(&ConfigHashInput {
            top_n: self.top_n,
            modality_weights: self.modality_weights,
            opportunity_scale: self.opportunity_scale,
            diversity_step: self.diversity_step,
        })
    }
}

#[derive(Serialize)]
struct ConfigHashInput {
    top_n: usize,
    modality_weights: ProfileModalityWeights,
    opportunity_scale: f64,
    diversity_step: f64,
}

/// Deterministic per-modality rollup of attributed weight.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileModalitySummary {
    pub modality: ProfileModality,
    pub attributed_weight: f64,
    pub effective_total_weight: f64,
    pub unattributed_weight: f64,
    pub site_count: usize,
    pub top_site_percent: f64,
    pub evidence_artifacts: Vec<String>,
}

/// One extracted hotspot with cross-modal normalized attribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileHotspot {
    pub hotspot_id: String,
    pub rank: u32,
    pub symbol: String,
    pub source_location: String,
    pub dominant_modality: ProfileModality,
    pub category_tags: Vec<String>,
    pub cpu_percent: f64,
    pub allocation_percent: f64,
    pub syscall_percent: f64,
    /// Cross-modal share of total cost. Across all hotspots these sum to 100%
    /// when there is any attributable weight, and to 0% in the degenerate case
    /// where every sample has zero weight.
    pub normalized_attribution_percent: f64,
    pub opportunity_score: f64,
    pub call_count: u64,
    pub candidate_levers: Vec<String>,
    pub evidence_artifacts: Vec<String>,
}

impl ProfileHotspot {
    /// Percent-of-total for a specific modality (0.0 when absent).
    #[must_use]
    pub fn modality_percent(&self, modality: ProfileModality) -> f64 {
        match modality {
            ProfileModality::Cpu => self.cpu_percent,
            ProfileModality::Allocation => self.allocation_percent,
            ProfileModality::Syscall => self.syscall_percent,
        }
    }

    /// Convert to a [`BenchmarkHotspotObservation`] for the regression gate.
    ///
    /// `self_time_percent` carries the cross-modal normalized attribution so the
    /// observation is meaningful for allocation/syscall-dominant hotspots too.
    #[must_use]
    pub fn to_benchmark_observation(
        &self,
        profile: BenchmarkProfileKey,
    ) -> BenchmarkHotspotObservation {
        BenchmarkHotspotObservation::new(
            self.hotspot_id.clone(),
            profile,
            format!("{} ({})", self.symbol, self.source_location),
            self.rank,
            self.normalized_attribution_percent,
            self.opportunity_score,
            self.evidence_artifacts.join(";"),
        )
    }

    fn lever(&self) -> HotspotLever {
        HotspotLever {
            hotspot_id: self.hotspot_id.clone(),
            rank: self.rank,
            symbol: self.symbol.clone(),
            source_location: self.source_location.clone(),
            dominant_modality: self.dominant_modality,
            normalized_attribution_percent: self.normalized_attribution_percent,
            opportunity_score: self.opportunity_score,
            candidate_levers: self.candidate_levers.clone(),
        }
    }
}

/// JSONL-ready evidence record for one surfaced hotspot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileEvidenceLog {
    pub orchestration_id: String,
    pub plan_id: String,
    pub hotspot_id: String,
    pub rank: u32,
    pub symbol: String,
    pub source_location: String,
    pub dominant_modality: ProfileModality,
    pub category_tags: Vec<String>,
    pub normalized_attribution_percent: f64,
    pub opportunity_score: f64,
    pub candidate_levers: Vec<String>,
    pub evidence_artifacts: Vec<String>,
    pub replay_command: String,
}

/// Lever attached to an optimization decision record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HotspotLever {
    pub hotspot_id: String,
    pub rank: u32,
    pub symbol: String,
    pub source_location: String,
    pub dominant_modality: ProfileModality,
    pub normalized_attribution_percent: f64,
    pub opportunity_score: f64,
    pub candidate_levers: Vec<String>,
}

/// Decision record linking surfaced hotspots to levers and replay commands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileOptimizationDecisionRecord {
    pub orchestration_id: String,
    pub plan_id: String,
    pub target_id: String,
    pub hotspot_levers: Vec<HotspotLever>,
    pub linked_hotspot_ids: Vec<String>,
    pub replay_commands: Vec<String>,
    pub evidence_artifacts: Vec<String>,
}

/// Exported JSON stats artifact (deterministic content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileJsonStatsArtifact {
    pub path: String,
    pub sha256: String,
    pub content: String,
}

/// Full orchestration report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileOrchestrationReport {
    pub schema_version: String,
    pub orchestration_id: String,
    pub plan: ProfileOrchestrationPlan,
    pub config: ProfileOrchestrationConfig,
    pub modality_summaries: Vec<ProfileModalitySummary>,
    pub hotspots: Vec<ProfileHotspot>,
    pub top_hotspots: Vec<ProfileHotspot>,
    pub evidence_logs: Vec<ProfileEvidenceLog>,
    pub exported_json_stats: ProfileJsonStatsArtifact,
}

impl ProfileOrchestrationReport {
    /// Surfaced hotspot identifiers in rank order.
    #[must_use]
    pub fn linked_hotspot_ids(&self) -> Vec<String> {
        self.top_hotspots
            .iter()
            .map(|hotspot| hotspot.hotspot_id.clone())
            .collect()
    }

    /// Surfaced hotspots as regression-gate observations.
    #[must_use]
    pub fn benchmark_observations(&self) -> Vec<BenchmarkHotspotObservation> {
        self.top_hotspots
            .iter()
            .map(|hotspot| hotspot.to_benchmark_observation(self.plan.profile.clone()))
            .collect()
    }

    /// Optimization decision record tying hotspots to levers and replay commands.
    #[must_use]
    pub fn optimization_decision_record(&self) -> ProfileOptimizationDecisionRecord {
        let mut replay_commands = vec![self.plan.replay_command()];
        replay_commands.extend(self.plan.invocation_replay_commands());
        let evidence_artifacts = sorted_unique(
            self.top_hotspots
                .iter()
                .flat_map(|hotspot| hotspot.evidence_artifacts.iter().cloned())
                .collect(),
        );
        ProfileOptimizationDecisionRecord {
            orchestration_id: self.orchestration_id.clone(),
            plan_id: self.plan.plan_id.clone(),
            target_id: self.plan.target_id.clone(),
            hotspot_levers: self
                .top_hotspots
                .iter()
                .map(ProfileHotspot::lever)
                .collect(),
            linked_hotspot_ids: self.linked_hotspot_ids(),
            replay_commands,
            evidence_artifacts,
        }
    }
}

/// Profiler orchestration runner.
#[derive(Debug, Clone, Default)]
pub struct ProfileOrchestrator {
    config: ProfileOrchestrationConfig,
}

impl ProfileOrchestrator {
    #[must_use]
    pub fn new(config: ProfileOrchestrationConfig) -> Self {
        Self { config }
    }

    /// Normalize `samples` against `plan` into a deterministic hotspot report.
    #[must_use]
    pub fn orchestrate(
        &self,
        plan: ProfileOrchestrationPlan,
        samples: Vec<ProfileSample>,
    ) -> ProfileOrchestrationReport {
        let modality_artifacts = plan.artifact_for_modality();
        let modality_rollups = build_modality_rollups(&samples);
        let modality_summaries = build_modality_summaries(&modality_rollups, &modality_artifacts);
        let mut hotspots = self.build_hotspots(&plan, &modality_rollups, &modality_artifacts);
        rank_hotspots(&mut hotspots);

        let top_hotspots = hotspots
            .iter()
            .take(self.config.top_n)
            .cloned()
            .collect::<Vec<_>>();

        let orchestration_id = orchestration_id_for(&plan, &self.config, &hotspots);
        let evidence_logs = top_hotspots
            .iter()
            .map(|hotspot| evidence_log_for(&orchestration_id, &plan, hotspot))
            .collect::<Vec<_>>();
        let exported_json_stats = export_json_stats(
            &orchestration_id,
            &plan,
            self.config,
            &modality_summaries,
            &hotspots,
        );

        ProfileOrchestrationReport {
            schema_version: PROFILE_ORCHESTRATION_SCHEMA_VERSION.to_string(),
            orchestration_id,
            plan,
            config: self.config,
            modality_summaries,
            hotspots,
            top_hotspots,
            evidence_logs,
            exported_json_stats,
        }
    }

    fn build_hotspots(
        &self,
        plan: &ProfileOrchestrationPlan,
        rollups: &BTreeMap<ProfileModality, ModalityRollup>,
        modality_artifacts: &BTreeMap<ProfileModality, Vec<String>>,
    ) -> Vec<ProfileHotspot> {
        // Collect every (symbol, source_location) site across modalities.
        let mut site_keys = BTreeSet::new();
        for rollup in rollups.values() {
            for key in rollup.sites.keys() {
                site_keys.insert(key.clone());
            }
        }

        // First pass: compute per-site blended weight for cross-site normalization.
        let mut blended_weights: BTreeMap<(String, String), f64> = BTreeMap::new();
        let mut total_blended = 0.0;
        for key in &site_keys {
            let blended = self.blended_weight(key, rollups);
            total_blended += blended;
            blended_weights.insert(key.clone(), blended);
        }

        // Second pass: materialize hotspots.
        let mut hotspots = Vec::new();
        for key in &site_keys {
            let (symbol, source_location) = key;
            let mut percents = BTreeMap::new();
            let mut present = Vec::new();
            let mut call_count = 0u64;
            let mut artifacts = Vec::new();
            for modality in ProfileModality::ALL {
                let Some(rollup) = rollups.get(&modality) else {
                    continue;
                };
                let Some(site) = rollup.sites.get(key) else {
                    continue;
                };
                let percent = 100.0 * safe_fraction(site.weight, rollup.effective_total);
                percents.insert(modality, percent);
                present.push(modality);
                call_count = call_count.saturating_add(site.call_count);
                if let Some(paths) = modality_artifacts.get(&modality) {
                    artifacts.extend(paths.iter().cloned());
                } else {
                    artifacts.push(format!("{}:unattributed", modality.category_tag()));
                }
            }

            let blended = blended_weights.get(key).copied().unwrap_or(0.0);
            let normalized_attribution_percent = 100.0 * safe_fraction(blended, total_blended);
            let diversity_factor =
                1.0 + self.config.diversity_step * f64::from(present_extra(present.len()));
            let opportunity_score = (normalized_attribution_percent / 100.0)
                * self.config.opportunity_scale
                * diversity_factor;

            let dominant_modality = dominant_modality(&percents);
            let category_tags = present
                .iter()
                .map(|modality| modality.category_tag().to_string())
                .collect::<Vec<_>>();
            let candidate_levers = candidate_levers_for(dominant_modality, &category_tags);
            let hotspot_id = hotspot_id_for(&plan.plan_id, symbol, source_location, &category_tags);

            hotspots.push(ProfileHotspot {
                hotspot_id,
                rank: 0,
                symbol: symbol.clone(),
                source_location: source_location.clone(),
                dominant_modality,
                category_tags,
                cpu_percent: percents.get(&ProfileModality::Cpu).copied().unwrap_or(0.0),
                allocation_percent: percents
                    .get(&ProfileModality::Allocation)
                    .copied()
                    .unwrap_or(0.0),
                syscall_percent: percents
                    .get(&ProfileModality::Syscall)
                    .copied()
                    .unwrap_or(0.0),
                normalized_attribution_percent,
                opportunity_score,
                call_count,
                candidate_levers,
                evidence_artifacts: sorted_unique(artifacts),
            });
        }
        hotspots
    }

    fn blended_weight(
        &self,
        key: &(String, String),
        rollups: &BTreeMap<ProfileModality, ModalityRollup>,
    ) -> f64 {
        let mut blended = 0.0;
        for modality in ProfileModality::ALL {
            let Some(rollup) = rollups.get(&modality) else {
                continue;
            };
            let Some(site) = rollup.sites.get(key) else {
                continue;
            };
            let fraction = safe_fraction(site.weight, rollup.effective_total);
            blended += self.config.modality_weights.weight_for(modality) * fraction;
        }
        blended
    }
}

struct SiteWeight {
    weight: f64,
    call_count: u64,
}

struct ModalityRollup {
    attributed_weight: f64,
    effective_total: f64,
    sites: BTreeMap<(String, String), SiteWeight>,
}

fn build_modality_rollups(samples: &[ProfileSample]) -> BTreeMap<ProfileModality, ModalityRollup> {
    let mut rollups: BTreeMap<ProfileModality, ModalityRollup> = BTreeMap::new();
    let mut hints: BTreeMap<ProfileModality, f64> = BTreeMap::new();

    for sample in samples {
        let rollup = rollups
            .entry(sample.modality)
            .or_insert_with(|| ModalityRollup {
                attributed_weight: 0.0,
                effective_total: 0.0,
                sites: BTreeMap::new(),
            });
        rollup.attributed_weight += sample.self_weight;
        let site = rollup.sites.entry(sample.site_key()).or_insert(SiteWeight {
            weight: 0.0,
            call_count: 0,
        });
        site.weight += sample.self_weight;
        site.call_count = site.call_count.saturating_add(sample.call_count);

        if let Some(hint) = sample.total_weight_hint {
            let entry = hints.entry(sample.modality).or_insert(0.0);
            *entry = entry.max(hint);
        }
    }

    for (modality, rollup) in &mut rollups {
        let hint = hints.get(modality).copied().unwrap_or(0.0);
        rollup.effective_total = rollup.attributed_weight.max(hint);
    }

    rollups
}

fn build_modality_summaries(
    rollups: &BTreeMap<ProfileModality, ModalityRollup>,
    modality_artifacts: &BTreeMap<ProfileModality, Vec<String>>,
) -> Vec<ProfileModalitySummary> {
    let mut summaries = Vec::new();
    for modality in ProfileModality::ALL {
        let Some(rollup) = rollups.get(&modality) else {
            continue;
        };
        let top_site_weight = rollup
            .sites
            .values()
            .map(|site| site.weight)
            .reduce(f64::max)
            .unwrap_or(0.0);
        let top_site_percent = 100.0 * safe_fraction(top_site_weight, rollup.effective_total);
        let evidence_artifacts = modality_artifacts
            .get(&modality)
            .cloned()
            .unwrap_or_else(|| vec![format!("{}:unattributed", modality.category_tag())]);
        summaries.push(ProfileModalitySummary {
            modality,
            attributed_weight: rollup.attributed_weight,
            effective_total_weight: rollup.effective_total,
            unattributed_weight: (rollup.effective_total - rollup.attributed_weight).max(0.0),
            site_count: rollup.sites.len(),
            top_site_percent,
            evidence_artifacts,
        });
    }
    summaries
}

fn rank_hotspots(hotspots: &mut [ProfileHotspot]) {
    hotspots.sort_by(|left, right| {
        right
            .opportunity_score
            .total_cmp(&left.opportunity_score)
            .then_with(|| {
                right
                    .normalized_attribution_percent
                    .total_cmp(&left.normalized_attribution_percent)
            })
            .then_with(|| left.hotspot_id.cmp(&right.hotspot_id))
    });
    for (index, hotspot) in hotspots.iter_mut().enumerate() {
        hotspot.rank = u32::try_from(index + 1).unwrap_or(u32::MAX);
    }
}

fn dominant_modality(percents: &BTreeMap<ProfileModality, f64>) -> ProfileModality {
    // Pick the highest-percent modality among those actually PRESENT for this
    // site (`percents` only holds present modalities). Ties resolve to the first
    // in canonical order (BTreeMap iterates Cpu < Allocation < Syscall). Falls
    // back to Cpu only when no modality is present (degenerate input).
    let mut best: Option<(ProfileModality, f64)> = None;
    for (&modality, &percent) in percents {
        match best {
            Some((_, best_percent)) if percent <= best_percent => {}
            _ => best = Some((modality, percent)),
        }
    }
    best.map_or(ProfileModality::Cpu, |(modality, _)| modality)
}

fn candidate_levers_for(dominant: ProfileModality, category_tags: &[String]) -> Vec<String> {
    let mut levers = match dominant {
        ProfileModality::Cpu => vec![
            "reduce-redundant-computation",
            "hoist-invariant-work",
            "batch-and-vectorize",
        ],
        ProfileModality::Allocation => vec![
            "reuse-buffers",
            "pool-allocations",
            "avoid-intermediate-clones",
        ],
        ProfileModality::Syscall => {
            vec!["batch-io", "buffer-writes", "coalesce-syscalls"]
        }
    }
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();

    if category_tags.iter().any(|tag| tag == "cpu") && dominant != ProfileModality::Cpu {
        levers.push("audit-cpu-self-time".to_string());
    }
    if category_tags.iter().any(|tag| tag == "allocation")
        && dominant != ProfileModality::Allocation
    {
        levers.push("audit-allocation-pressure".to_string());
    }
    if category_tags.iter().any(|tag| tag == "syscall") && dominant != ProfileModality::Syscall {
        levers.push("audit-syscall-overhead".to_string());
    }

    dedup_preserve_order(levers)
}

fn evidence_log_for(
    orchestration_id: &str,
    plan: &ProfileOrchestrationPlan,
    hotspot: &ProfileHotspot,
) -> ProfileEvidenceLog {
    ProfileEvidenceLog {
        orchestration_id: orchestration_id.to_string(),
        plan_id: plan.plan_id.clone(),
        hotspot_id: hotspot.hotspot_id.clone(),
        rank: hotspot.rank,
        symbol: hotspot.symbol.clone(),
        source_location: hotspot.source_location.clone(),
        dominant_modality: hotspot.dominant_modality,
        category_tags: hotspot.category_tags.clone(),
        normalized_attribution_percent: hotspot.normalized_attribution_percent,
        opportunity_score: hotspot.opportunity_score,
        candidate_levers: hotspot.candidate_levers.clone(),
        evidence_artifacts: hotspot.evidence_artifacts.clone(),
        replay_command: plan.replay_command(),
    }
}

fn orchestration_id_for(
    plan: &ProfileOrchestrationPlan,
    config: &ProfileOrchestrationConfig,
    hotspots: &[ProfileHotspot],
) -> String {
    let hotspot_digests = hotspots
        .iter()
        .map(|hotspot| {
            format!(
                "{}:{}:{:.6}:{:.6}",
                hotspot.rank,
                hotspot.hotspot_id,
                hotspot.normalized_attribution_percent,
                hotspot.opportunity_score
            )
        })
        .collect::<Vec<_>>();
    let hash = stable_hash(&OrchestrationIdInput {
        plan_id: plan.plan_id.as_str(),
        config_fingerprint: config.fingerprint().as_str(),
        hotspot_digests: &hotspot_digests,
    });
    format!("profile-orch-{}", short_hash(&hash))
}

#[derive(Serialize)]
struct OrchestrationIdInput<'a> {
    plan_id: &'a str,
    config_fingerprint: &'a str,
    hotspot_digests: &'a [String],
}

fn export_json_stats(
    orchestration_id: &str,
    plan: &ProfileOrchestrationPlan,
    config: ProfileOrchestrationConfig,
    modality_summaries: &[ProfileModalitySummary],
    hotspots: &[ProfileHotspot],
) -> ProfileJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        orchestration_id: &'a str,
        plan_id: &'a str,
        config: ProfileOrchestrationConfig,
        modality_summaries: &'a [ProfileModalitySummary],
        hotspots: &'a [ProfileHotspot],
    }

    let payload = Export {
        schema_version: PROFILE_ORCHESTRATION_SCHEMA_VERSION,
        orchestration_id,
        plan_id: plan.plan_id.as_str(),
        config,
        modality_summaries,
        hotspots,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    ProfileJsonStatsArtifact {
        path: format!("{orchestration_id}/profile_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn hotspot_id_for(
    plan_id: &str,
    symbol: &str,
    source_location: &str,
    category_tags: &[String],
) -> String {
    let hash = stable_hash(&HotspotIdInput {
        plan_id,
        symbol,
        source_location,
        category_tags,
    });
    format!("hotspot-{}", short_hash(&hash))
}

#[derive(Serialize)]
struct HotspotIdInput<'a> {
    plan_id: &'a str,
    symbol: &'a str,
    source_location: &'a str,
    category_tags: &'a [String],
}

fn present_extra(present: usize) -> u32 {
    u32::try_from(present.saturating_sub(1)).unwrap_or(0)
}

fn safe_fraction(numerator: f64, denominator: f64) -> f64 {
    if denominator.abs() <= f64::EPSILON {
        0.0
    } else {
        numerator / denominator
    }
}

fn dedup_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            output.push(value);
        }
    }
    output
}

fn sorted_unique(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

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

    fn profile() -> BenchmarkProfileKey {
        BenchmarkProfileKey::new("benchmark_capture", "fixture-scroll")
    }

    fn plan() -> ProfileOrchestrationPlan {
        ProfileOrchestrationPlan::new(
            "workload-scroll-heavy",
            profile(),
            42,
            vec![
                ProfilerInvocation::new(
                    ProfileModality::Cpu,
                    "cargo flamegraph",
                    vec!["--bench".to_string(), "scroll".to_string()],
                    "profiles/cpu.flamegraph.json",
                ),
                ProfilerInvocation::new(
                    ProfileModality::Allocation,
                    "heaptrack",
                    vec!["doctor_frankentui".to_string()],
                    "profiles/alloc.heaptrack.json",
                ),
                ProfilerInvocation::new(
                    ProfileModality::Syscall,
                    "strace",
                    vec!["-c".to_string()],
                    "profiles/syscall.summary.txt",
                ),
            ],
        )
    }

    fn samples() -> Vec<ProfileSample> {
        vec![
            // CPU: render_diff dominates self-time.
            ProfileSample::new(
                ProfileModality::Cpu,
                "render_diff",
                "ftui-render/src/diff.rs:120",
                700.0,
                4_000,
            ),
            ProfileSample::new(
                ProfileModality::Cpu,
                "wrap_text",
                "ftui-text/src/wrap.rs:42",
                300.0,
                1_200,
            ),
            // Allocation: wrap_text allocates heavily; render_diff a little.
            ProfileSample::new(
                ProfileModality::Allocation,
                "wrap_text",
                "ftui-text/src/wrap.rs:42",
                800_000.0,
                1_200,
            ),
            ProfileSample::new(
                ProfileModality::Allocation,
                "render_diff",
                "ftui-render/src/diff.rs:120",
                200_000.0,
                4_000,
            ),
            // Syscall: flush_writer dominates kernel time.
            ProfileSample::new(
                ProfileModality::Syscall,
                "flush_writer",
                "ftui-runtime/src/terminal_writer.rs:88",
                1_500.0,
                1_500,
            ),
        ]
    }

    fn orchestrate_default() -> ProfileOrchestrationReport {
        ProfileOrchestrator::default().orchestrate(plan(), samples())
    }

    #[test]
    fn orchestration_is_deterministic_for_fixed_inputs() {
        let first = orchestrate_default();
        let second = orchestrate_default();
        assert_eq!(first.orchestration_id, second.orchestration_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.plan.plan_id, second.plan.plan_id);
        assert_eq!(first.hotspots, second.hotspots);
    }

    #[test]
    fn top_hotspots_have_stable_ids_and_ranks() {
        let report = orchestrate_default();
        assert!(!report.top_hotspots.is_empty());
        assert!(report.top_hotspots.len() <= 5);
        for (index, hotspot) in report.top_hotspots.iter().enumerate() {
            assert_eq!(hotspot.rank as usize, index + 1);
            assert!(hotspot.hotspot_id.starts_with("hotspot-"));
            assert!(!hotspot.hotspot_id.is_empty());
        }
        // Ranks are strictly increasing and opportunity scores non-increasing.
        let mut previous_score = f64::INFINITY;
        for hotspot in &report.hotspots {
            assert!(hotspot.opportunity_score <= previous_score + 1e-9);
            previous_score = hotspot.opportunity_score;
        }
    }

    #[test]
    fn modality_percentages_are_percent_of_total() {
        let report = orchestrate_default();
        // CPU sums to 100% across the two CPU sites.
        let cpu_sum: f64 = report
            .hotspots
            .iter()
            .map(|hotspot| hotspot.cpu_percent)
            .sum();
        assert!((cpu_sum - 100.0).abs() < 1e-6, "cpu_sum={cpu_sum}");
        // render_diff is 70% of CPU self-time.
        let render = report
            .hotspots
            .iter()
            .find(|hotspot| hotspot.symbol == "render_diff")
            .expect("render_diff hotspot present");
        assert!((render.cpu_percent - 70.0).abs() < 1e-6);
        // wrap_text is 80% of allocation pressure.
        let wrap = report
            .hotspots
            .iter()
            .find(|hotspot| hotspot.symbol == "wrap_text")
            .expect("wrap_text hotspot present");
        assert!((wrap.allocation_percent - 80.0).abs() < 1e-6);
    }

    #[test]
    fn normalized_attribution_sums_to_one_hundred() {
        let report = orchestrate_default();
        let total: f64 = report
            .hotspots
            .iter()
            .map(|hotspot| hotspot.normalized_attribution_percent)
            .sum();
        assert!((total - 100.0).abs() < 1e-6, "total={total}");
    }

    #[test]
    fn multi_modal_hotspot_records_category_tags_and_dominant() {
        let report = orchestrate_default();
        let render = report
            .hotspots
            .iter()
            .find(|hotspot| hotspot.symbol == "render_diff")
            .expect("render_diff hotspot present");
        // render_diff appears in CPU + allocation modalities.
        assert!(render.category_tags.contains(&"cpu".to_string()));
        assert!(render.category_tags.contains(&"allocation".to_string()));
        assert_eq!(render.dominant_modality, ProfileModality::Cpu);
        // flush_writer is syscall-only.
        let flush = report
            .hotspots
            .iter()
            .find(|hotspot| hotspot.symbol == "flush_writer")
            .expect("flush_writer hotspot present");
        assert_eq!(flush.category_tags, vec!["syscall".to_string()]);
        assert_eq!(flush.dominant_modality, ProfileModality::Syscall);
    }

    #[test]
    fn candidate_levers_match_dominant_modality() {
        let report = orchestrate_default();
        let flush = report
            .hotspots
            .iter()
            .find(|hotspot| hotspot.symbol == "flush_writer")
            .expect("flush_writer hotspot present");
        assert!(flush.candidate_levers.contains(&"batch-io".to_string()));
        let wrap = report
            .hotspots
            .iter()
            .find(|hotspot| hotspot.symbol == "wrap_text")
            .expect("wrap_text hotspot present");
        // wrap_text is allocation-dominant but also CPU-present → cross-modal audit lever.
        assert!(wrap.candidate_levers.contains(&"reuse-buffers".to_string()));
        assert!(
            wrap.candidate_levers
                .contains(&"audit-cpu-self-time".to_string())
        );
    }

    #[test]
    fn top_hotspot_opportunity_score_clears_gate_threshold() {
        let report = orchestrate_default();
        let best = report
            .top_hotspots
            .first()
            .expect("at least one hotspot surfaced");
        // The dominant hotspot should clear the gate's default 2.0 threshold.
        assert!(
            best.opportunity_score >= 2.0,
            "best opportunity_score={}",
            best.opportunity_score
        );
    }

    #[test]
    fn benchmark_observations_carry_profile_and_scores() {
        let report = orchestrate_default();
        let observations = report.benchmark_observations();
        assert_eq!(observations.len(), report.top_hotspots.len());
        let first = observations.first().expect("observation present");
        assert_eq!(first.profile, profile());
        assert!(first.symbol_or_path.contains('('));
        assert!(first.opportunity_score > 0.0);
        assert_eq!(first.rank, 1);
        assert!(!first.hotspot_id.is_empty());
    }

    #[test]
    fn decision_record_links_levers_and_replay_commands() {
        let report = orchestrate_default();
        let record = report.optimization_decision_record();
        assert_eq!(record.plan_id, report.plan.plan_id);
        assert_eq!(record.hotspot_levers.len(), report.top_hotspots.len());
        assert_eq!(record.linked_hotspot_ids, report.linked_hotspot_ids());
        // Replay commands include the orchestration command plus each invocation.
        assert!(
            record
                .replay_commands
                .iter()
                .any(|command| command.contains("profile-orchestrate"))
        );
        assert!(
            record
                .replay_commands
                .iter()
                .any(|command| command.contains("cargo flamegraph"))
        );
        assert!(!record.evidence_artifacts.is_empty());
        assert!(
            record
                .evidence_artifacts
                .iter()
                .any(|artifact| artifact.contains("flamegraph"))
        );
    }

    #[test]
    fn evidence_logs_are_emitted_for_surfaced_hotspots() {
        let report = orchestrate_default();
        assert_eq!(report.evidence_logs.len(), report.top_hotspots.len());
        let log = report.evidence_logs.first().expect("evidence log present");
        assert_eq!(log.orchestration_id, report.orchestration_id);
        assert!(log.replay_command.contains("profile-orchestrate"));
        assert!(!log.candidate_levers.is_empty());
        assert!(log.opportunity_score > 0.0);
    }

    #[test]
    fn modality_summaries_cover_each_present_modality() {
        let report = orchestrate_default();
        assert_eq!(report.modality_summaries.len(), 3);
        let cpu = report
            .modality_summaries
            .iter()
            .find(|summary| summary.modality == ProfileModality::Cpu)
            .expect("cpu summary present");
        assert!((cpu.attributed_weight - 1_000.0).abs() < 1e-6);
        assert!((cpu.top_site_percent - 70.0).abs() < 1e-6);
        assert_eq!(cpu.site_count, 2);
        assert!(
            cpu.evidence_artifacts
                .iter()
                .any(|a| a.contains("flamegraph"))
        );
    }

    #[test]
    fn unattributed_total_hint_caps_percentages() {
        let plan = ProfileOrchestrationPlan::new(
            "workload-hint",
            profile(),
            7,
            vec![ProfilerInvocation::new(
                ProfileModality::Cpu,
                "cargo flamegraph",
                vec![],
                "profiles/cpu.json",
            )],
        );
        let samples = vec![
            ProfileSample::new(ProfileModality::Cpu, "a", "a.rs:1", 300.0, 1)
                .with_total_weight_hint(1_000.0),
            ProfileSample::new(ProfileModality::Cpu, "b", "b.rs:1", 200.0, 1)
                .with_total_weight_hint(1_000.0),
        ];
        let report = ProfileOrchestrator::default().orchestrate(plan, samples);
        // Attributed = 500, but hint pins the total at 1000 → a is 30%, not 60%.
        let a = report
            .hotspots
            .iter()
            .find(|hotspot| hotspot.symbol == "a")
            .expect("hotspot a present");
        assert!(
            (a.cpu_percent - 30.0).abs() < 1e-6,
            "a.cpu_percent={}",
            a.cpu_percent
        );
        let cpu = report
            .modality_summaries
            .first()
            .expect("cpu summary present");
        assert!((cpu.unattributed_weight - 500.0).abs() < 1e-6);
    }

    #[test]
    fn empty_samples_produce_a_valid_empty_report() {
        let report = ProfileOrchestrator::default().orchestrate(plan(), Vec::new());
        assert!(report.hotspots.is_empty());
        assert!(report.top_hotspots.is_empty());
        assert!(report.evidence_logs.is_empty());
        assert!(report.modality_summaries.is_empty());
        assert!(!report.exported_json_stats.sha256.is_empty());
        // Decision record still references replay commands without panicking.
        let record = report.optimization_decision_record();
        assert!(record.hotspot_levers.is_empty());
        assert!(!record.replay_commands.is_empty());
    }

    #[test]
    fn top_n_config_limits_surfaced_hotspots() {
        let config = ProfileOrchestrationConfig::default().with_top_n(1);
        let report = ProfileOrchestrator::new(config).orchestrate(plan(), samples());
        assert_eq!(report.top_hotspots.len(), 1);
        assert_eq!(report.evidence_logs.len(), 1);
        // The single surfaced hotspot is the top-ranked one.
        assert_eq!(report.top_hotspots[0].rank, 1);
    }

    #[test]
    fn invocations_are_sorted_for_stable_plan_ids() {
        let forward = ProfileOrchestrationPlan::new(
            "workload",
            profile(),
            1,
            vec![
                ProfilerInvocation::new(ProfileModality::Cpu, "a", vec![], "cpu.json"),
                ProfilerInvocation::new(ProfileModality::Syscall, "b", vec![], "sys.txt"),
            ],
        );
        let reversed = ProfileOrchestrationPlan::new(
            "workload",
            profile(),
            1,
            vec![
                ProfilerInvocation::new(ProfileModality::Syscall, "b", vec![], "sys.txt"),
                ProfilerInvocation::new(ProfileModality::Cpu, "a", vec![], "cpu.json"),
            ],
        );
        assert_eq!(forward.plan_id, reversed.plan_id);
        assert_eq!(forward.invocations, reversed.invocations);
    }

    #[test]
    fn diversity_bonus_raises_multi_modal_scores() {
        // A site present in all three modalities at equal within-modal share
        // should score above a single-modality site with the same normalized
        // attribution because of the diversity factor.
        let plan = ProfileOrchestrationPlan::new("w", profile(), 1, Vec::new());
        let multi = vec![
            ProfileSample::new(ProfileModality::Cpu, "m", "m.rs:1", 50.0, 1),
            ProfileSample::new(ProfileModality::Allocation, "m", "m.rs:1", 50.0, 1),
            ProfileSample::new(ProfileModality::Syscall, "m", "m.rs:1", 50.0, 1),
            ProfileSample::new(ProfileModality::Cpu, "s", "s.rs:1", 50.0, 1),
        ];
        let report = ProfileOrchestrator::default().orchestrate(plan, multi);
        let m = report
            .hotspots
            .iter()
            .find(|hotspot| hotspot.symbol == "m")
            .expect("m present");
        assert_eq!(m.category_tags.len(), 3);
        assert!(m.opportunity_score > 0.0);
        // m should rank first (multi-modal + higher blended weight).
        assert_eq!(report.hotspots[0].symbol, "m");
    }
}
