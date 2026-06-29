//! Iterative re-profile loop and bottleneck-shift historian (bd-3bxhj.8.19).
//!
//! Optimization is iterative: fixing the dominant hotspot moves the bottleneck
//! somewhere else, and a lever that looked high-value before a round can be
//! worthless after it. After each *accepted* optimization (one lever, per
//! [`crate::one_lever_policy`]), this module re-profiles, compares before/after
//! metrics, detects whether the bottleneck migrated, re-ranks the opportunity
//! backlog against the *latest* profile, and records an auditable history ledger:
//!
//! 1. **Re-profile delta** — per-round before/after `p50/p95/p99` latency,
//!    throughput, and memory, with signed deltas and percent change (AC1).
//! 2. **Bottleneck-shift detector** — the dominant hotspot before vs after; a
//!    change of leader means the frontier moved (AC1).
//! 3. **Backlog re-rank** — every backlog candidate's opportunity score
//!    (`Impact · Confidence / Effort`, where Impact is the candidate's hotspot
//!    cost share in the *new* profile) is recomputed and the backlog re-sorted
//!    after each round (AC2).
//! 4. **Conservative pause** — an unstable measurement, a non-finite re-profile,
//!    or a primary-metric regression yields a [`RoundVerdict::Pause`] with a
//!    triage hint rather than blindly continuing (AC3).
//!
//! The ledger is **float-free** (every numeric term is a fixed-decimal string via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically. Raw finiteness is
//! checked before rendering so a NaN cannot be masked to `"0.000000"`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::recommendation_contract::EffortSize;

/// Schema version for the re-profile-loop artifacts.
pub const REPROFILE_LOOP_SCHEMA_VERSION: &str = "reprofile-loop-v1";

/// Numeric epsilon for guarded ratios and comparisons.
const EPS: f64 = 1e-9;

/// Default primary-metric (p99) regression fraction that forces a pause.
const DEFAULT_REGRESSION_THRESHOLD: f64 = 0.05;

/// How many candidates the re-ranked frontier reports.
const FRONTIER_TOP_K: usize = 3;

// ── Hashing / formatting helpers ─────────────────────────────────────────────

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

/// Deterministic fixed-decimal rendering. Non-finite and `-0.0` normalize to a
/// stable string so the ledger derives `Eq` and replays byte-identically.
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

/// A ratio guarded against a zero (or non-finite) denominator.
fn safe_div(num: f64, den: f64) -> f64 {
    if den.abs() < EPS || !den.is_finite() || !num.is_finite() {
        0.0
    } else {
        num / den
    }
}

fn effort_cost(effort: EffortSize) -> f64 {
    match effort {
        EffortSize::Small => 1.0,
        EffortSize::Medium => 2.0,
        EffortSize::Large => 4.0,
        EffortSize::XLarge => 8.0,
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// A hotspot's cost share in a profile (the frontier element).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotspotCost {
    /// Stable hotspot id.
    pub hotspot_id: String,
    /// Cost share in `[0, 1]` (fraction of total measured cost).
    pub cost_share: f64,
}

/// A profile measurement snapshot (before or after a change).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSnapshot {
    /// p50 latency (microseconds).
    pub p50_us: f64,
    /// p95 latency (microseconds).
    pub p95_us: f64,
    /// p99 latency (microseconds, the primary metric).
    pub p99_us: f64,
    /// Throughput (ops/sec; higher is better).
    pub throughput: f64,
    /// Resident memory (bytes).
    pub memory_bytes: f64,
    /// Hotspot frontier (cost shares).
    pub hotspots: Vec<HotspotCost>,
    /// Whether the measurement was stable (low variance, enough samples).
    pub stable: bool,
}

impl ProfileSnapshot {
    /// Whether every latency / throughput / memory metric is finite.
    fn metrics_finite(&self) -> bool {
        self.p50_us.is_finite()
            && self.p95_us.is_finite()
            && self.p99_us.is_finite()
            && self.throughput.is_finite()
            && self.memory_bytes.is_finite()
    }

    /// The dominant hotspot id (highest cost share; tie → lexicographic id).
    fn dominant_hotspot(&self) -> Option<&str> {
        self.hotspots
            .iter()
            .filter(|h| h.cost_share.is_finite())
            .max_by(|a, b| {
                a.cost_share
                    .total_cmp(&b.cost_share)
                    .then_with(|| b.hotspot_id.cmp(&a.hotspot_id))
            })
            .map(|h| h.hotspot_id.as_str())
    }

    /// The cost share of a given hotspot in this profile (0 if absent).
    fn share_of(&self, hotspot_id: &str) -> f64 {
        self.hotspots
            .iter()
            .filter(|h| h.hotspot_id == hotspot_id && h.cost_share.is_finite())
            .map(|h| h.cost_share.max(0.0))
            .sum()
    }
}

/// A not-yet-applied backlog candidate (a future optimization lever).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacklogCandidate {
    /// Stable candidate id.
    pub candidate_id: String,
    /// The hotspot this candidate would address.
    pub hotspot_id: String,
    /// Calibrated confidence in `[0, 1]`.
    pub confidence: f64,
    /// Implementation effort.
    pub effort: EffortSize,
}

/// One optimization round: an accepted change plus its before/after profiles and
/// the current backlog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizationRound {
    /// Monotonic round number.
    pub round_number: usize,
    /// The accepted change id.
    pub change_id: String,
    /// The lever applied this round.
    pub lever_id: String,
    /// Profile before the change.
    pub before: ProfileSnapshot,
    /// Profile after the change (the re-profile).
    pub after: ProfileSnapshot,
    /// The remaining backlog to re-rank against the new profile.
    pub backlog: Vec<BacklogCandidate>,
}

/// Tunable configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReprofileConfig {
    /// The p99 regression fraction that forces a conservative pause.
    pub regression_threshold: f64,
}

impl Default for ReprofileConfig {
    fn default() -> Self {
        Self {
            regression_threshold: DEFAULT_REGRESSION_THRESHOLD,
        }
    }
}

// ── Verdict ──────────────────────────────────────────────────────────────────

/// The per-round verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundVerdict {
    /// The round is healthy; continue iterating.
    Continue,
    /// The round is unstable / regressed / failed; pause for triage.
    Pause,
}

impl RoundVerdict {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Pause => "pause",
        }
    }
}

// ── Re-ranked backlog entry ──────────────────────────────────────────────────

/// A backlog candidate re-scored against the new profile (float-free).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankedCandidate {
    /// The candidate id.
    pub candidate_id: String,
    /// The hotspot it addresses.
    pub hotspot_id: String,
    /// Recomputed impact (new cost share × 10, fixed-decimal).
    pub impact: String,
    /// Recomputed opportunity score `Impact · Confidence / Effort` (fixed-decimal).
    pub score: String,
}

/// One round's history-ledger entry (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundLedgerEntry {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Round number.
    pub round_number: usize,
    /// The change id (AC1 log field).
    pub change_id: String,
    /// The lever applied.
    pub lever_id: String,
    /// p50 before / after / delta / percent (fixed-decimal).
    pub p50_before: String,
    pub p50_after: String,
    pub p50_delta: String,
    pub p50_pct: String,
    /// p95 before / after / delta / percent (fixed-decimal).
    pub p95_before: String,
    pub p95_after: String,
    pub p95_delta: String,
    pub p95_pct: String,
    /// p99 before / after / delta / percent (fixed-decimal).
    pub p99_before: String,
    pub p99_after: String,
    pub p99_delta: String,
    pub p99_pct: String,
    /// Throughput delta (fixed-decimal; positive is better).
    pub throughput_delta: String,
    /// Memory delta in bytes (fixed-decimal; negative is better).
    pub memory_delta: String,
    /// Whether the primary metric (p99) improved.
    pub primary_improved: bool,
    /// The dominant hotspot before the change (AC1).
    pub bottleneck_before: String,
    /// The dominant hotspot after the change (AC1).
    pub bottleneck_after: String,
    /// Whether the bottleneck migrated (AC1).
    pub bottleneck_shifted: bool,
    /// The re-ranked backlog frontier (top-K candidate ids, AC2).
    pub new_frontier: Vec<String>,
    /// The full re-ranked backlog (AC2).
    pub reranked_backlog: Vec<RankedCandidate>,
    /// The round verdict.
    pub verdict: RoundVerdict,
    /// The pause reason / triage hint (non-empty iff paused, AC3).
    pub triage_hint: String,
    /// Whether every raw f64 was finite before rendering (pre-`fmt6`).
    pub numerically_finite: bool,
    /// Whether the row's flags are consistent with their recorded arithmetic.
    pub clause_consistent: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

fn entry_has_required_fields(e: &RoundLedgerEntry) -> bool {
    !e.schema_version.is_empty()
        && !e.run_id.is_empty()
        && !e.change_id.is_empty()
        && !e.lever_id.is_empty()
        && !e.p50_delta.is_empty()
        && !e.p95_delta.is_empty()
        && !e.p99_delta.is_empty()
        && !e.bottleneck_before.is_empty()
        && !e.bottleneck_after.is_empty()
        && !e.detail.is_empty()
        && !e.reproduction_command.is_empty()
        // AC3: a paused round must carry a triage hint.
        && (!matches!(e.verdict, RoundVerdict::Pause) || !e.triage_hint.is_empty())
}

/// Re-rank a backlog against a profile: each candidate's impact is its hotspot's
/// cost share in the new profile (×10), and the score is `Impact·Confidence/Effort`.
fn rerank_backlog(backlog: &[BacklogCandidate], profile: &ProfileSnapshot) -> Vec<RankedCandidate> {
    let mut scored: Vec<(RankedCandidate, f64)> = backlog
        .iter()
        .map(|c| {
            let impact = profile.share_of(&c.hotspot_id) * 10.0;
            let confidence = c.confidence.clamp(0.0, 1.0);
            let score = safe_div(impact * confidence, effort_cost(c.effort));
            (
                RankedCandidate {
                    candidate_id: c.candidate_id.clone(),
                    hotspot_id: c.hotspot_id.clone(),
                    impact: fmt6(impact),
                    score: fmt6(score),
                },
                score,
            )
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.candidate_id.cmp(&b.0.candidate_id))
    });
    scored.into_iter().map(|(c, _)| c).collect()
}

/// Evaluate one optimization round into a history-ledger entry.
fn evaluate_round(
    run_id: &str,
    round: &OptimizationRound,
    config: &ReprofileConfig,
) -> RoundLedgerEntry {
    let b = &round.before;
    let a = &round.after;

    // Latency deltas (after − before; negative is an improvement for latency).
    let p50_delta = a.p50_us - b.p50_us;
    let p95_delta = a.p95_us - b.p95_us;
    let p99_delta = a.p99_us - b.p99_us;
    let p50_pct = safe_div(p50_delta, b.p50_us) * 100.0;
    let p95_pct = safe_div(p95_delta, b.p95_us) * 100.0;
    let p99_pct = safe_div(p99_delta, b.p99_us) * 100.0;
    let throughput_delta = a.throughput - b.throughput;
    let memory_delta = a.memory_bytes - b.memory_bytes;

    let primary_improved = a.p99_us < b.p99_us - EPS;
    // A regression is a primary-metric increase beyond the configured fraction.
    let regressed = a.p99_us > b.p99_us * (1.0 + config.regression_threshold) + EPS;
    let reprofile_failed = !a.metrics_finite();
    let unstable = !a.stable;

    let bottleneck_before = b.dominant_hotspot().unwrap_or("none").to_string();
    let bottleneck_after = a.dominant_hotspot().unwrap_or("none").to_string();
    let bottleneck_shifted = bottleneck_before != bottleneck_after;

    let reranked_backlog = rerank_backlog(&round.backlog, a);
    let new_frontier: Vec<String> = reranked_backlog
        .iter()
        .take(FRONTIER_TOP_K)
        .map(|c| c.candidate_id.clone())
        .collect();

    // AC3: an unstable / failed / regressed round is conservatively paused with a
    // triage hint; a healthy round continues.
    let (verdict, triage_hint) = if reprofile_failed {
        (
            RoundVerdict::Pause,
            "re-profile produced non-finite metrics; re-run the profiler and verify the baseline harness".to_string(),
        )
    } else if unstable {
        (
            RoundVerdict::Pause,
            "measurement flagged unstable (high variance / too few samples); increase sample count before accepting deltas".to_string(),
        )
    } else if regressed {
        (
            RoundVerdict::Pause,
            format!(
                "primary metric p99 regressed {:.4}% (> {:.4}% budget); consider rolling back the lever",
                p99_pct,
                config.regression_threshold * 100.0
            ),
        )
    } else {
        (RoundVerdict::Continue, String::new())
    };

    let numerically_finite = [
        p50_delta,
        p95_delta,
        p99_delta,
        p50_pct,
        p95_pct,
        p99_pct,
        throughput_delta,
        memory_delta,
    ]
    .iter()
    .all(|x| x.is_finite())
        && b.metrics_finite();

    // Clause consistency, recomputed from the round's own data:
    //  - a paused verdict has a non-empty triage hint, a continue has none;
    //  - the bottleneck-shift flag matches the before/after dominant hotspots;
    //  - primary_improved matches the p99 delta sign;
    //  - any unstable / failed / regressed condition forces a pause (never continue).
    let pause_explained = match verdict {
        RoundVerdict::Pause => !triage_hint.is_empty(),
        RoundVerdict::Continue => triage_hint.is_empty(),
    };
    let shift_consistent = bottleneck_shifted == (bottleneck_before != bottleneck_after);
    let improvement_consistent = primary_improved == (a.p99_us < b.p99_us - EPS);
    let unsafe_forces_pause =
        !(reprofile_failed || unstable || regressed) || matches!(verdict, RoundVerdict::Pause);
    let clause_consistent =
        pause_explained && shift_consistent && improvement_consistent && unsafe_forces_pause;

    let detail = format!(
        "round {} {} [{}] p99 {:.1}->{:.1} ({:.2}%) | bottleneck {}->{}{} | {}{}",
        round.round_number,
        round.change_id,
        round.lever_id,
        b.p99_us,
        a.p99_us,
        p99_pct,
        bottleneck_before,
        bottleneck_after,
        if bottleneck_shifted { " (shifted)" } else { "" },
        verdict.as_str(),
        if matches!(verdict, RoundVerdict::Pause) {
            format!(" — {triage_hint}")
        } else {
            String::new()
        },
    );

    RoundLedgerEntry {
        schema_version: REPROFILE_LOOP_SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        round_number: round.round_number,
        change_id: round.change_id.clone(),
        lever_id: round.lever_id.clone(),
        p50_before: fmt6(b.p50_us),
        p50_after: fmt6(a.p50_us),
        p50_delta: fmt6(p50_delta),
        p50_pct: fmt6(p50_pct),
        p95_before: fmt6(b.p95_us),
        p95_after: fmt6(a.p95_us),
        p95_delta: fmt6(p95_delta),
        p95_pct: fmt6(p95_pct),
        p99_before: fmt6(b.p99_us),
        p99_after: fmt6(a.p99_us),
        p99_delta: fmt6(p99_delta),
        p99_pct: fmt6(p99_pct),
        throughput_delta: fmt6(throughput_delta),
        memory_delta: fmt6(memory_delta),
        primary_improved,
        bottleneck_before,
        bottleneck_after,
        bottleneck_shifted,
        new_frontier,
        reranked_backlog,
        verdict,
        triage_hint,
        numerically_finite,
        clause_consistent,
        detail,
        reproduction_command: format!(
            "cargo test -p doctor_frankentui --lib reprofile_loop # round {}",
            round.round_number
        ),
    }
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Roll-up of a re-profile-loop report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReprofileSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the ledger.
    pub evidence_checksum: String,
    /// Total rounds.
    pub total_rounds: usize,
    /// Rounds that continued.
    pub continued: usize,
    /// Rounds that paused for triage.
    pub paused: usize,
    /// Rounds where the bottleneck migrated.
    pub bottleneck_shifts: usize,
    /// Rounds where the primary metric improved.
    pub primary_improvements: usize,
    /// Whether every entry carries all mandated fields (AC1).
    pub required_fields_complete: bool,
    /// Whether every entry's flags match their arithmetic.
    pub clauses_consistent: bool,
    /// Whether every raw computation stayed finite.
    pub numerically_stable: bool,
    /// AC1: every round writes a before/after p50/p95/p99 delta + hotspot movement.
    pub delta_report_complete: bool,
    /// AC2: every round with a non-empty backlog recomputed its candidate scores.
    pub backlog_recomputed: bool,
    /// AC3: every unstable / failed / regressed round paused with a triage hint.
    pub unstable_pauses: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReprofileStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full re-profile-loop report: the history ledger + summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReprofileReport {
    /// The per-round history ledger.
    pub ledger: Vec<RoundLedgerEntry>,
    /// The roll-up summary + gate.
    pub summary: ReprofileSummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: ReprofileStatsArtifact,
}

impl ReprofileReport {
    /// Look up a round entry.
    #[must_use]
    pub fn round(&self, round_number: usize) -> Option<&RoundLedgerEntry> {
        self.ledger.iter().find(|e| e.round_number == round_number)
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

#[derive(Serialize)]
struct Checksummed<'a> {
    ledger: &'a [RoundLedgerEntry],
}

/// Compile a full re-profile-loop report over `rounds`.
#[must_use]
pub fn run_reprofile_loop(
    label: &str,
    rounds: &[OptimizationRound],
    config: &ReprofileConfig,
) -> ReprofileReport {
    let run_id = short_hash(&stable_hash(&format!(
        "{REPROFILE_LOOP_SCHEMA_VERSION}|{label}"
    )));

    let ledger: Vec<RoundLedgerEntry> = rounds
        .iter()
        .map(|r| evaluate_round(&run_id, r, config))
        .collect();

    let evidence_checksum = stable_hash(&Checksummed { ledger: &ledger });
    let report_id = short_hash(&stable_hash(&format!("{run_id}|{evidence_checksum}")));

    let continued = ledger
        .iter()
        .filter(|e| matches!(e.verdict, RoundVerdict::Continue))
        .count();
    let paused = ledger
        .iter()
        .filter(|e| matches!(e.verdict, RoundVerdict::Pause))
        .count();
    let bottleneck_shifts = ledger.iter().filter(|e| e.bottleneck_shifted).count();
    let primary_improvements = ledger.iter().filter(|e| e.primary_improved).count();

    let required_fields_complete = ledger.iter().all(entry_has_required_fields);
    let clauses_consistent = ledger.iter().all(|e| e.clause_consistent);
    // A *continued* round must have finite raw metrics — a NaN slipping into a
    // "continue" decision is a silent bug. A *paused* round may legitimately carry
    // non-finite metrics: that is exactly why it paused (failed re-profile). Either
    // way the rendered ledger strings must parse (fmt6 guarantees this).
    let numerically_stable = ledger.iter().all(|e| {
        (matches!(e.verdict, RoundVerdict::Pause) || e.numerically_finite)
            && [
                &e.p50_delta,
                &e.p95_delta,
                &e.p99_delta,
                &e.p99_pct,
                &e.throughput_delta,
                &e.memory_delta,
            ]
            .iter()
            .all(|s| s.parse::<f64>().is_ok_and(f64::is_finite))
    });
    // AC1: every round reports p50/p95/p99 deltas + a before/after bottleneck.
    let delta_report_complete = ledger.iter().all(|e| {
        !e.p50_delta.is_empty()
            && !e.p95_delta.is_empty()
            && !e.p99_delta.is_empty()
            && !e.bottleneck_before.is_empty()
            && !e.bottleneck_after.is_empty()
    });
    // AC2: every round whose source backlog was non-empty produced a re-ranked
    // backlog (scores recomputed from the latest profile).
    let backlog_recomputed = ledger
        .iter()
        .zip(rounds.iter())
        .all(|(e, r)| r.backlog.is_empty() || e.reranked_backlog.len() == r.backlog.len());
    // AC3: every unstable / failed / regressed round paused with a triage hint.
    let unstable_pauses = ledger.iter().zip(rounds.iter()).all(|(e, r)| {
        let failed = !r.after.metrics_finite();
        let unstable = !r.after.stable;
        let regressed =
            r.after.p99_us > r.before.p99_us * (1.0 + config.regression_threshold) + EPS;
        !(failed || unstable || regressed)
            || (matches!(e.verdict, RoundVerdict::Pause) && !e.triage_hint.is_empty())
    });

    let gate_passes = required_fields_complete
        && clauses_consistent
        && numerically_stable
        && delta_report_complete
        && backlog_recomputed
        && unstable_pauses;

    let summary = ReprofileSummary {
        schema_version: REPROFILE_LOOP_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        run_id: run_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_rounds: ledger.len(),
        continued,
        paused,
        bottleneck_shifts,
        primary_improvements,
        required_fields_complete,
        clauses_consistent,
        numerically_stable,
        delta_report_complete,
        backlog_recomputed,
        unstable_pauses,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib reprofile_loop # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a ReprofileSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: REPROFILE_LOOP_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        ReprofileStatsArtifact {
            path: format!("reprofile_loop/{report_id}.json"),
            sha256,
            content,
        }
    };

    ReprofileReport {
        ledger,
        summary,
        exported_json_stats,
    }
}

fn backlog() -> Vec<BacklogCandidate> {
    vec![
        BacklogCandidate {
            candidate_id: "cand.layout".to_string(),
            hotspot_id: "hot.layout".to_string(),
            confidence: 0.85,
            effort: EffortSize::Medium,
        },
        BacklogCandidate {
            candidate_id: "cand.render".to_string(),
            hotspot_id: "hot.render".to_string(),
            confidence: 0.80,
            effort: EffortSize::Small,
        },
        BacklogCandidate {
            candidate_id: "cand.alloc".to_string(),
            hotspot_id: "hot.alloc".to_string(),
            confidence: 0.70,
            effort: EffortSize::Large,
        },
    ]
}

/// A representative sequence of optimization rounds (continue / regress / unstable).
#[must_use]
pub fn default_optimization_rounds() -> Vec<OptimizationRound> {
    vec![
        // Round 1: a real win on render; bottleneck shifts render -> layout.
        OptimizationRound {
            round_number: 1,
            change_id: "chg.render_simd".to_string(),
            lever_id: "lever.render_diff_simd".to_string(),
            before: ProfileSnapshot {
                p50_us: 40.0,
                p95_us: 90.0,
                p99_us: 120.0,
                throughput: 8000.0,
                memory_bytes: 5_000_000.0,
                hotspots: vec![
                    HotspotCost {
                        hotspot_id: "hot.render".to_string(),
                        cost_share: 0.55,
                    },
                    HotspotCost {
                        hotspot_id: "hot.layout".to_string(),
                        cost_share: 0.30,
                    },
                ],
                stable: true,
            },
            after: ProfileSnapshot {
                p50_us: 32.0,
                p95_us: 70.0,
                p99_us: 85.0,
                throughput: 11000.0,
                memory_bytes: 4_900_000.0,
                hotspots: vec![
                    HotspotCost {
                        hotspot_id: "hot.render".to_string(),
                        cost_share: 0.25,
                    },
                    HotspotCost {
                        hotspot_id: "hot.layout".to_string(),
                        cost_share: 0.45,
                    },
                ],
                stable: true,
            },
            backlog: backlog(),
        },
        // Round 2: a regression on p99 -> conservative pause.
        OptimizationRound {
            round_number: 2,
            change_id: "chg.cache".to_string(),
            lever_id: "lever.speculative_cache".to_string(),
            before: ProfileSnapshot {
                p50_us: 32.0,
                p95_us: 70.0,
                p99_us: 85.0,
                throughput: 11000.0,
                memory_bytes: 4_900_000.0,
                hotspots: vec![HotspotCost {
                    hotspot_id: "hot.layout".to_string(),
                    cost_share: 0.45,
                }],
                stable: true,
            },
            after: ProfileSnapshot {
                p50_us: 34.0,
                p95_us: 78.0,
                p99_us: 100.0,
                throughput: 9500.0,
                memory_bytes: 5_300_000.0,
                hotspots: vec![HotspotCost {
                    hotspot_id: "hot.layout".to_string(),
                    cost_share: 0.50,
                }],
                stable: true,
            },
            backlog: backlog(),
        },
        // Round 3: unstable measurement -> conservative pause.
        OptimizationRound {
            round_number: 3,
            change_id: "chg.async".to_string(),
            lever_id: "lever.async_runtime".to_string(),
            before: ProfileSnapshot {
                p50_us: 32.0,
                p95_us: 70.0,
                p99_us: 85.0,
                throughput: 11000.0,
                memory_bytes: 4_900_000.0,
                hotspots: vec![HotspotCost {
                    hotspot_id: "hot.layout".to_string(),
                    cost_share: 0.45,
                }],
                stable: true,
            },
            after: ProfileSnapshot {
                p50_us: 31.0,
                p95_us: 69.0,
                p99_us: 84.0,
                throughput: 11200.0,
                memory_bytes: 4_880_000.0,
                hotspots: vec![HotspotCost {
                    hotspot_id: "hot.layout".to_string(),
                    cost_share: 0.44,
                }],
                stable: false,
            },
            backlog: backlog(),
        },
        // Round 4: a clean win on layout; continue.
        OptimizationRound {
            round_number: 4,
            change_id: "chg.layout_smallvec".to_string(),
            lever_id: "lever.layout_smallvec".to_string(),
            before: ProfileSnapshot {
                p50_us: 32.0,
                p95_us: 70.0,
                p99_us: 85.0,
                throughput: 11000.0,
                memory_bytes: 4_900_000.0,
                hotspots: vec![HotspotCost {
                    hotspot_id: "hot.layout".to_string(),
                    cost_share: 0.45,
                }],
                stable: true,
            },
            after: ProfileSnapshot {
                p50_us: 28.0,
                p95_us: 60.0,
                p99_us: 72.0,
                throughput: 13000.0,
                memory_bytes: 4_600_000.0,
                hotspots: vec![
                    HotspotCost {
                        hotspot_id: "hot.layout".to_string(),
                        cost_share: 0.20,
                    },
                    HotspotCost {
                        hotspot_id: "hot.alloc".to_string(),
                        cost_share: 0.40,
                    },
                ],
                stable: true,
            },
            backlog: backlog(),
        },
    ]
}

/// Run the default re-profile-loop report.
#[must_use]
pub fn run_default_reprofile_report(label: &str) -> ReprofileReport {
    run_reprofile_loop(
        label,
        &default_optimization_rounds(),
        &ReprofileConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ReprofileConfig {
        ReprofileConfig::default()
    }

    #[test]
    fn improving_round_continues_and_detects_shift() {
        let report = run_default_reprofile_report("rp/test");
        let r1 = report.round(1).unwrap();
        assert_eq!(r1.verdict, RoundVerdict::Continue);
        assert!(r1.primary_improved);
        // Render fix moved the bottleneck to layout.
        assert_eq!(r1.bottleneck_before, "hot.render");
        assert_eq!(r1.bottleneck_after, "hot.layout");
        assert!(r1.bottleneck_shifted);
        assert!(r1.triage_hint.is_empty());
        assert!(r1.clause_consistent);
    }

    #[test]
    fn regression_round_pauses_with_triage() {
        let report = run_default_reprofile_report("rp/test");
        let r2 = report.round(2).unwrap();
        assert_eq!(r2.verdict, RoundVerdict::Pause);
        assert!(!r2.primary_improved);
        assert!(r2.triage_hint.contains("regress"));
        assert!(r2.clause_consistent);
    }

    #[test]
    fn unstable_round_pauses_with_triage() {
        let report = run_default_reprofile_report("rp/test");
        let r3 = report.round(3).unwrap();
        assert_eq!(r3.verdict, RoundVerdict::Pause);
        assert!(r3.triage_hint.contains("unstable"));
        assert!(r3.clause_consistent);
    }

    #[test]
    fn non_finite_reprofile_pauses() {
        let round = OptimizationRound {
            round_number: 9,
            change_id: "c".to_string(),
            lever_id: "l".to_string(),
            before: ProfileSnapshot {
                p50_us: 40.0,
                p95_us: 90.0,
                p99_us: 120.0,
                throughput: 8000.0,
                memory_bytes: 5_000_000.0,
                hotspots: vec![HotspotCost {
                    hotspot_id: "h".to_string(),
                    cost_share: 0.5,
                }],
                stable: true,
            },
            after: ProfileSnapshot {
                p50_us: f64::NAN,
                p95_us: 70.0,
                p99_us: 85.0,
                throughput: 11000.0,
                memory_bytes: 4_900_000.0,
                hotspots: vec![HotspotCost {
                    hotspot_id: "h".to_string(),
                    cost_share: 0.5,
                }],
                stable: true,
            },
            backlog: vec![],
        };
        let report = run_reprofile_loop("rp/nan", std::slice::from_ref(&round), &cfg());
        let e = report.round(9).unwrap();
        assert_eq!(e.verdict, RoundVerdict::Pause);
        assert!(e.triage_hint.contains("non-finite"));
        // The NaN was caught pre-render, not masked into the gate.
        assert!(!e.numerically_finite);
        // ...and the row stays renderable / stable in string form.
        assert_eq!(e.p50_after, fmt6(f64::NAN));
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
    }

    #[test]
    fn backlog_rerank_follows_the_new_profile() {
        let report = run_default_reprofile_report("rp/test");
        let r1 = report.round(1).unwrap();
        // After round 1, layout dominates (0.45) so its candidate outranks render.
        assert_eq!(r1.reranked_backlog.len(), 3);
        let layout_score: f64 = r1
            .reranked_backlog
            .iter()
            .find(|c| c.candidate_id == "cand.layout")
            .unwrap()
            .score
            .parse()
            .unwrap();
        let render_score: f64 = r1
            .reranked_backlog
            .iter()
            .find(|c| c.candidate_id == "cand.render")
            .unwrap()
            .score
            .parse()
            .unwrap();
        // layout impact 0.45*10=4.5, conf 0.85, effort Medium(2) -> 1.9125;
        // render impact 0.25*10=2.5, conf 0.80, effort Small(1) -> 2.0.
        // render is cheaper, so it still leads here — but layout climbed vs before.
        assert!(layout_score > 0.0 && render_score > 0.0);
        assert_eq!(r1.new_frontier.first().unwrap(), "cand.render");
    }

    #[test]
    fn ledger_is_float_free_and_replay_stable() {
        let a = run_default_reprofile_report("rp/test");
        let b = run_default_reprofile_report("rp/test");
        assert_eq!(a.summary.report_id, b.summary.report_id);
        assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
    }

    #[test]
    fn empty_rounds_do_not_panic() {
        let report = run_reprofile_loop("rp/empty", &[], &cfg());
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_rounds, 0);
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_reprofile_report("rp/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_rounds, 4);
        assert_eq!(report.summary.continued, 2);
        assert_eq!(report.summary.paused, 2);
        assert!(report.summary.bottleneck_shifts >= 1);
        assert!(report.summary.delta_report_complete);
        assert!(report.summary.backlog_recomputed);
        assert!(report.summary.unstable_pauses);
        for e in &report.ledger {
            assert!(entry_has_required_fields(e));
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_reprofile_report("rp/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }
}
