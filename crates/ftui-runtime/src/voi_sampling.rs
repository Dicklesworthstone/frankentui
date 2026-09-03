#![forbid(unsafe_code)]

//! Value-of-Information (VOI) Sampling Policy for expensive measurements.
//!
//! This module decides **when** to sample costly latency/cost measurements so
//! overhead stays low while guarantees remain intact.
//!
//! # Mathematical Model
//!
//! We treat "violation" observations as Bernoulli random variables:
//!
//! ```text
//! X_t ∈ {0,1},  X_t = 1  ⇔  measurement violates SLA threshold
//! ```
//!
//! We maintain a Beta prior/posterior over the violation probability `p`:
//!
//! ```text
//! p ~ Beta(α, β)
//! α ← α + X_t
//! β ← β + (1 − X_t)
//! ```
//!
//! The posterior variance is:
//!
//! ```text
//! Var[p] = αβ / ((α+β)^2 (α+β+1))
//! ```
//!
//! ## Expected VOI (Variance Reduction)
//!
//! The expected variance **after one more sample** is:
//!
//! ```text
//! E[Var[p] | one sample] =
//!   p̂ · Var[Beta(α+1,β)] + (1−p̂) · Var[Beta(α,β+1)]
//! ```
//!
//! where `p̂ = α / (α+β)` is the posterior mean.
//!
//! The **value of information** (VOI) is the expected reduction:
//!
//! ```text
//! VOI = Var[p] − E[Var[p] | one sample]  ≥ 0
//! ```
//!
//! ## Anytime-Valid Safety (E-Process Layer)
//!
//! We optionally track an e-process over the same Bernoulli stream to keep
//! decisions anytime-valid. Sampling decisions depend only on **past** data,
//! so the e-process remains valid under adaptive sampling:
//!
//! ```text
//! W_0 = 1
//! W_t = W_{t-1} × (1 + λ (X_t − μ₀))
//! ```
//!
//! where `μ₀` is the baseline violation rate under H₀ and λ is a betting
//! fraction (clamped for stability).
//!
//! ## Decision Rule (Explainable)
//!
//! We compute a scalar **score**:
//!
//! ```text
//! score = VOI × value_scale × (1 + boundary_weight × boundary_score)
//! boundary_score = 1 / (1 + |log W − log W*|)
//! ```
//!
//! where `W* = 1/α` is the e-value threshold.
//!
//! Then:
//! 1) If `max_interval` exceeded ⇒ **sample** (forced).
//! 2) If `min_interval` not met ⇒ **skip** (guard).
//! 3) Else **sample** iff `score ≥ cost`.
//!
//! This yields a deterministic, explainable policy that preferentially samples
//! when uncertainty is high **and** evidence is near the decision boundary.
//!
//! # Perf JSONL Schema
//!
//! The microbench emits JSONL lines per decision:
//!
//! ```text
//! {"test":"voi_sampling","case":"decision","idx":N,"elapsed_ns":N,"sample":true,"violated":false,"e_value":1.23}
//! ```
//!
//! # Key Invariants
//!
//! 1. **Deterministic**: same inputs → same decisions.
//! 2. **VOI non-negative**: expected variance reduction ≥ 0.
//! 3. **Anytime-valid**: e-process remains valid under adaptive sampling.
//! 4. **Bounded silence**: max-interval forces periodic sampling.
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | α,β ≤ 0 | Clamp to ε | Avoid invalid Beta |
//! | μ₀ ≤ 0 or ≥ 1 | Clamp to (ε, 1−ε) | Avoid degenerate e-process |
//! | λ out of range | Clamp to valid range | Prevent negative wealth |
//! | cost ≤ 0 | Clamp to ε | Avoid divide-by-zero in evidence |
//! | max_interval = 0 | Disabled | Explicit opt-out |
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::voi_sampling::{VoiConfig, VoiSampler};
//! use std::time::Instant;
//!
//! let mut sampler = VoiSampler::new(VoiConfig::default());
//! let decision = sampler.decide(Instant::now());
//! if decision.should_sample {
//!     let violated = false; // measure and evaluate
//!     sampler.observe(violated);
//! }
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use web_time::{Duration, Instant};

const EPS: f64 = 1e-12;
const MU_0_MIN: f64 = 1e-6;
const MU_0_MAX: f64 = 1.0 - 1e-6;
const LAMBDA_EPS: f64 = 1e-9;
const E_MIN: f64 = 1e-12;
const E_MAX: f64 = 1e12;
const VAR_MAX: f64 = 0.25; // Max Beta variance as α,β → 0

// ---------------------------------------------------------------------------
// Monotonic counters (exported for observability dashboards / tests)
// ---------------------------------------------------------------------------

static VOI_SAMPLES_TAKEN_TOTAL: AtomicU64 = AtomicU64::new(0);
static VOI_SAMPLES_SKIPPED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Total VOI samples taken (monotonic counter for metrics export).
#[must_use]
pub fn voi_samples_taken_total() -> u64 {
    VOI_SAMPLES_TAKEN_TOTAL.load(Ordering::Relaxed)
}

/// Total VOI samples skipped (monotonic counter for metrics export).
#[must_use]
pub fn voi_samples_skipped_total() -> u64 {
    VOI_SAMPLES_SKIPPED_TOTAL.load(Ordering::Relaxed)
}

/// Configuration for the VOI sampling policy.
#[derive(Debug, Clone)]
pub struct VoiConfig {
    /// Significance level α for the e-process threshold (W* = 1/α).
    /// Default: 0.05.
    pub alpha: f64,

    /// Beta prior α for violation probability. Default: 1.0.
    pub prior_alpha: f64,

    /// Beta prior β for violation probability. Default: 1.0.
    pub prior_beta: f64,

    /// Baseline violation rate μ₀ under H₀. Default: 0.05.
    pub mu_0: f64,

    /// E-process betting fraction λ. Default: 0.5 (clamped).
    pub lambda: f64,

    /// Value scaling factor for VOI. Default: 1.0.
    pub value_scale: f64,

    /// Weight for boundary proximity. Default: 1.0.
    pub boundary_weight: f64,

    /// Sampling cost (in normalized units). Default: 0.01.
    pub sample_cost: f64,

    /// Minimum interval between samples (ms). Default: 0.
    pub min_interval_ms: u64,

    /// Maximum interval between samples (ms). 0 disables time forcing.
    /// Default: 250.
    pub max_interval_ms: u64,

    /// Minimum events between samples. Default: 0.
    pub min_interval_events: u64,

    /// Maximum events between samples. 0 disables event forcing.
    /// Default: 20.
    pub max_interval_events: u64,

    /// Enable JSONL-compatible logging.
    pub enable_logging: bool,

    /// Maximum log entries to retain.
    pub max_log_entries: usize,
}

impl Default for VoiConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            prior_alpha: 1.0,
            prior_beta: 1.0,
            mu_0: 0.05,
            lambda: 0.5,
            value_scale: 1.0,
            boundary_weight: 1.0,
            sample_cost: 0.01,
            min_interval_ms: 0,
            max_interval_ms: 250,
            min_interval_events: 0,
            max_interval_events: 20,
            enable_logging: false,
            max_log_entries: 2048,
        }
    }
}

/// Sampling decision with full evidence.
#[derive(Debug, Clone)]
pub struct VoiDecision {
    pub event_idx: u64,
    pub should_sample: bool,
    pub forced_by_interval: bool,
    pub blocked_by_min_interval: bool,
    pub voi_gain: f64,
    pub score: f64,
    pub cost: f64,
    pub log_bayes_factor: f64,
    pub posterior_mean: f64,
    pub posterior_variance: f64,
    pub e_value: f64,
    pub e_threshold: f64,
    pub boundary_score: f64,
    pub events_since_sample: u64,
    pub time_since_sample_ms: f64,
    pub reason: &'static str,
}

impl VoiDecision {
    /// Serialize decision to JSONL.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"event":"voi_decision","idx":{},"should_sample":{},"forced":{},"blocked":{},"voi_gain":{:.6},"score":{:.6},"cost":{:.6},"log_bayes_factor":{:.4},"posterior_mean":{:.6},"posterior_variance":{:.6},"e_value":{:.6},"e_threshold":{:.6},"boundary_score":{:.6},"events_since_sample":{},"time_since_sample_ms":{:.3},"reason":"{}"}}"#,
            self.event_idx,
            self.should_sample,
            self.forced_by_interval,
            self.blocked_by_min_interval,
            self.voi_gain,
            self.score,
            self.cost,
            self.log_bayes_factor,
            self.posterior_mean,
            self.posterior_variance,
            self.e_value,
            self.e_threshold,
            self.boundary_score,
            self.events_since_sample,
            self.time_since_sample_ms,
            self.reason
        )
    }
}

/// Observation result after a sample is taken.
#[derive(Debug, Clone)]
pub struct VoiObservation {
    pub event_idx: u64,
    pub sample_idx: u64,
    pub violated: bool,
    pub posterior_mean: f64,
    pub posterior_variance: f64,
    pub alpha: f64,
    pub beta: f64,
    pub e_value: f64,
    pub e_threshold: f64,
}

impl VoiObservation {
    /// Serialize observation to JSONL.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"event":"voi_observe","idx":{},"sample_idx":{},"violated":{},"posterior_mean":{:.6},"posterior_variance":{:.6},"alpha":{:.3},"beta":{:.3},"e_value":{:.6},"e_threshold":{:.6}}}"#,
            self.event_idx,
            self.sample_idx,
            self.violated,
            self.posterior_mean,
            self.posterior_variance,
            self.alpha,
            self.beta,
            self.e_value,
            self.e_threshold
        )
    }
}

/// Log entry for VOI sampling.
#[derive(Debug, Clone)]
pub enum VoiLogEntry {
    Decision(VoiDecision),
    Observation(VoiObservation),
}

impl VoiLogEntry {
    /// Serialize log entry to JSONL.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        match self {
            Self::Decision(decision) => decision.to_jsonl(),
            Self::Observation(obs) => obs.to_jsonl(),
        }
    }
}

/// Summary statistics for VOI sampling.
#[derive(Debug, Clone)]
pub struct VoiSummary {
    pub total_events: u64,
    pub total_samples: u64,
    pub forced_samples: u64,
    pub skipped_events: u64,
    pub current_mean: f64,
    pub current_variance: f64,
    pub e_value: f64,
    pub e_threshold: f64,
    pub avg_events_between_samples: f64,
    pub avg_ms_between_samples: f64,
}

/// Snapshot of VOI sampler state for debug overlays.
#[derive(Debug, Clone)]
pub struct VoiSamplerSnapshot {
    pub captured_ms: u64,
    pub alpha: f64,
    pub beta: f64,
    pub posterior_mean: f64,
    pub posterior_variance: f64,
    pub expected_variance_after: f64,
    pub voi_gain: f64,
    pub last_decision: Option<VoiDecision>,
    pub last_observation: Option<VoiObservation>,
    pub recent_logs: Vec<VoiLogEntry>,
}

/// VOI-driven sampler with Beta-Bernoulli posterior and e-process control.
#[derive(Debug, Clone)]
pub struct VoiSampler {
    config: VoiConfig,
    alpha: f64,
    beta: f64,
    mu_0: f64,
    lambda: f64,
    e_value: f64,
    e_threshold: f64,
    event_idx: u64,
    sample_idx: u64,
    forced_samples: u64,
    last_sample_event: u64,
    last_sample_time: Instant,
    start_time: Instant,
    last_decision_forced: bool,
    logs: VecDeque<VoiLogEntry>,
    last_decision: Option<VoiDecision>,
    last_observation: Option<VoiObservation>,
}

impl VoiSampler {
    /// Create a new VOI sampler with given config.
    pub fn new(config: VoiConfig) -> Self {
        Self::new_at(config, Instant::now())
    }

    /// Create a new VOI sampler at a specific time (for deterministic tests).
    pub fn new_at(config: VoiConfig, now: Instant) -> Self {
        let mut cfg = config;

        let prior_alpha = if cfg.prior_alpha.is_nan() {
            EPS
        } else {
            cfg.prior_alpha.max(EPS)
        };
        let prior_beta = if cfg.prior_beta.is_nan() {
            EPS
        } else {
            cfg.prior_beta.max(EPS)
        };
        let mu_0 = if cfg.mu_0.is_nan() {
            0.5
        } else {
            cfg.mu_0.clamp(MU_0_MIN, MU_0_MAX)
        };
        let lambda_max = (1.0 / (1.0 - mu_0)) - LAMBDA_EPS;
        let lambda = if cfg.lambda.is_nan() {
            LAMBDA_EPS
        } else {
            cfg.lambda.clamp(LAMBDA_EPS, lambda_max)
        };

        cfg.value_scale = if cfg.value_scale.is_nan() {
            EPS
        } else {
            cfg.value_scale.max(EPS)
        };
        cfg.boundary_weight = if cfg.boundary_weight.is_nan() {
            0.0
        } else {
            cfg.boundary_weight.max(0.0)
        };
        cfg.sample_cost = if cfg.sample_cost.is_nan() {
            EPS
        } else {
            cfg.sample_cost.max(EPS)
        };
        cfg.max_log_entries = cfg.max_log_entries.max(1);

        let e_threshold = 1.0 / cfg.alpha.max(EPS);

        Self {
            config: cfg,
            alpha: prior_alpha,
            beta: prior_beta,
            mu_0,
            lambda,
            e_value: 1.0,
            e_threshold,
            event_idx: 0,
            sample_idx: 0,
            forced_samples: 0,
            last_sample_event: 0,
            last_sample_time: now,
            start_time: now,
            last_decision_forced: false,
            logs: VecDeque::new(),
            last_decision: None,
            last_observation: None,
        }
    }

    /// Access the sampler configuration.
    #[must_use]
    pub fn config(&self) -> &VoiConfig {
        &self.config
    }

    /// Current posterior parameters (alpha, beta).
    #[must_use]
    pub fn posterior_params(&self) -> (f64, f64) {
        (self.alpha, self.beta)
    }

    /// Current posterior mean.
    #[must_use]
    pub fn posterior_mean(&self) -> f64 {
        beta_mean(self.alpha, self.beta)
    }

    /// Current posterior variance.
    #[must_use]
    pub fn posterior_variance(&self) -> f64 {
        beta_variance(self.alpha, self.beta)
    }

    /// Expected posterior variance after one additional sample.
    #[must_use]
    pub fn expected_variance_after(&self) -> f64 {
        expected_variance_after(self.alpha, self.beta)
    }

    /// Most recent decision, if any.
    #[must_use]
    pub fn last_decision(&self) -> Option<&VoiDecision> {
        self.last_decision.as_ref()
    }

    /// Most recent observation, if any.
    #[must_use]
    pub fn last_observation(&self) -> Option<&VoiObservation> {
        self.last_observation.as_ref()
    }

    /// Decide whether to sample at time `now`.
    pub fn decide(&mut self, now: Instant) -> VoiDecision {
        self.event_idx += 1;

        let events_since_sample = if self.sample_idx == 0 {
            self.event_idx
        } else {
            self.event_idx.saturating_sub(self.last_sample_event)
        };
        let time_since_sample = if now >= self.last_sample_time {
            now.saturating_duration_since(self.last_sample_time)
        } else {
            Duration::ZERO
        };

        let forced_by_events = self.config.max_interval_events > 0
            && events_since_sample >= self.config.max_interval_events;
        let forced_by_time = self.config.max_interval_ms > 0
            && time_since_sample >= Duration::from_millis(self.config.max_interval_ms);
        let forced = forced_by_events || forced_by_time;

        let blocked_by_events = self.sample_idx > 0
            && self.config.min_interval_events > 0
            && events_since_sample < self.config.min_interval_events;
        let blocked_by_time = self.sample_idx > 0
            && self.config.min_interval_ms > 0
            && time_since_sample < Duration::from_millis(self.config.min_interval_ms);
        let blocked = blocked_by_events || blocked_by_time;

        let variance = beta_variance(self.alpha, self.beta);
        let expected_after = expected_variance_after(self.alpha, self.beta);
        let voi_gain = (variance - expected_after).max(0.0);

        let boundary_score = boundary_score(self.e_value, self.e_threshold);
        let score = voi_gain
            * self.config.value_scale
            * (1.0 + self.config.boundary_weight * boundary_score);
        let cost = self.config.sample_cost;
        let log_bayes_factor = log10_ratio(score, cost);

        let should_sample = if forced {
            true
        } else if blocked {
            false
        } else {
            score >= cost
        };

        let reason = if forced {
            "forced_interval"
        } else if blocked {
            "min_interval"
        } else if should_sample {
            "voi_ge_cost"
        } else {
            "voi_lt_cost"
        };

        let decision = VoiDecision {
            event_idx: self.event_idx,
            should_sample,
            forced_by_interval: forced,
            blocked_by_min_interval: blocked,
            voi_gain,
            score,
            cost,
            log_bayes_factor,
            posterior_mean: beta_mean(self.alpha, self.beta),
            posterior_variance: variance,
            e_value: self.e_value,
            e_threshold: self.e_threshold,
            boundary_score,
            events_since_sample,
            time_since_sample_ms: time_since_sample.as_secs_f64() * 1000.0,
            reason,
        };

        self.last_decision = Some(decision.clone());
        self.last_decision_forced = forced;

        // --- Tracing observability (bd-37a.4) ---
        let _span = tracing::debug_span!(
            "voi.evaluate",
            decision_context = %reason,
            voi_estimate = %voi_gain,
            sample_cost = %cost,
            sample_decision = should_sample,
        )
        .entered();

        tracing::debug!(
            target: crate::telemetry_schema::TARGET_VOI,
            voi_gain = %voi_gain,
            score = %score,
            cost = %cost,
            log_bayes_factor = %log_bayes_factor,
            posterior_mean = %decision.posterior_mean,
            posterior_variance = %variance,
            boundary_score = %boundary_score,
            e_value = %self.e_value,
            reason = %reason,
            event_idx = self.event_idx,
            "voi calculation"
        );

        tracing::debug!(
            target: crate::telemetry_schema::TARGET_VOI,
            voi_estimate_value = %voi_gain,
            "voi estimate histogram"
        );

        if should_sample {
            VOI_SAMPLES_TAKEN_TOTAL.fetch_add(1, Ordering::Relaxed);
        } else {
            VOI_SAMPLES_SKIPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
        }

        if self.config.enable_logging {
            self.push_log(VoiLogEntry::Decision(decision.clone()));
        }

        decision
    }

    /// Record a sampled observation at time `now`.
    pub fn observe_at(&mut self, violated: bool, now: Instant) -> VoiObservation {
        self.sample_idx += 1;
        self.last_sample_event = self.event_idx;
        self.last_sample_time = now;
        if self.last_decision_forced {
            self.forced_samples += 1;
        }

        if violated {
            self.alpha += 1.0;
        } else {
            self.beta += 1.0;
        }

        self.update_eprocess(violated);

        let obs_posterior_mean = beta_mean(self.alpha, self.beta);
        let obs_posterior_variance = beta_variance(self.alpha, self.beta);
        let obs_voi_estimate =
            (obs_posterior_variance - expected_variance_after(self.alpha, self.beta)).max(0.0);

        let observation = VoiObservation {
            event_idx: self.event_idx,
            sample_idx: self.sample_idx,
            violated,
            posterior_mean: obs_posterior_mean,
            posterior_variance: obs_posterior_variance,
            alpha: self.alpha,
            beta: self.beta,
            e_value: self.e_value,
            e_threshold: self.e_threshold,
        };

        // --- TRACE: individual utility estimate after observation ---
        tracing::trace!(
            target: crate::telemetry_schema::TARGET_VOI,
            violated = violated,
            alpha = %self.alpha,
            beta = %self.beta,
            posterior_mean = %obs_posterior_mean,
            posterior_variance = %obs_posterior_variance,
            e_value = %self.e_value,
            voi_estimate_value = %obs_voi_estimate,
            sample_idx = self.sample_idx,
            "utility estimate after observation"
        );

        self.last_observation = Some(observation.clone());
        if self.config.enable_logging {
            self.push_log(VoiLogEntry::Observation(observation.clone()));
        }

        observation
    }

    /// Record a sampled observation using `Instant::now()`.
    pub fn observe(&mut self, violated: bool) -> VoiObservation {
        self.observe_at(violated, Instant::now())
    }

    /// Current summary statistics.
    #[must_use]
    pub fn summary(&self) -> VoiSummary {
        let skipped_events = self.event_idx.saturating_sub(self.sample_idx);
        let avg_events_between_samples = if self.sample_idx > 0 {
            self.event_idx as f64 / self.sample_idx as f64
        } else {
            0.0
        };
        let elapsed_ms = self.start_time.elapsed().as_secs_f64() * 1000.0;
        let avg_ms_between_samples = if self.sample_idx > 0 {
            elapsed_ms / self.sample_idx as f64
        } else {
            0.0
        };

        VoiSummary {
            total_events: self.event_idx,
            total_samples: self.sample_idx,
            forced_samples: self.forced_samples,
            skipped_events,
            current_mean: beta_mean(self.alpha, self.beta),
            current_variance: beta_variance(self.alpha, self.beta),
            e_value: self.e_value,
            e_threshold: self.e_threshold,
            avg_events_between_samples,
            avg_ms_between_samples,
        }
    }

    /// Access current logs.
    #[must_use]
    pub fn logs(&self) -> &VecDeque<VoiLogEntry> {
        &self.logs
    }

    /// Render logs as JSONL.
    #[must_use]
    pub fn logs_to_jsonl(&self) -> String {
        self.logs
            .iter()
            .map(VoiLogEntry::to_jsonl)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Create a snapshot of the sampler state for debug overlays.
    #[must_use]
    pub fn snapshot(&self, max_logs: usize, captured_ms: u64) -> VoiSamplerSnapshot {
        let expected_after = expected_variance_after(self.alpha, self.beta);
        let variance = beta_variance(self.alpha, self.beta);
        let voi_gain = (variance - expected_after).max(0.0);
        let mut recent_logs: Vec<VoiLogEntry> = self
            .logs
            .iter()
            .rev()
            .take(max_logs.max(1))
            .cloned()
            .collect();
        recent_logs.reverse();

        VoiSamplerSnapshot {
            captured_ms,
            alpha: self.alpha,
            beta: self.beta,
            posterior_mean: beta_mean(self.alpha, self.beta),
            posterior_variance: variance,
            expected_variance_after: expected_after,
            voi_gain,
            last_decision: self.last_decision.clone(),
            last_observation: self.last_observation.clone(),
            recent_logs,
        }
    }

    fn push_log(&mut self, entry: VoiLogEntry) {
        if self.logs.len() >= self.config.max_log_entries {
            self.logs.pop_front();
        }
        self.logs.push_back(entry);
    }

    fn update_eprocess(&mut self, violated: bool) {
        let x = if violated { 1.0 } else { 0.0 };
        let factor = 1.0 + self.lambda * (x - self.mu_0);
        let next = self.e_value * factor.max(EPS);
        self.e_value = next.clamp(E_MIN, E_MAX);
    }

    /// Increment forced sample counter (for testing/integration).
    pub fn mark_forced_sample(&mut self) {
        self.forced_samples += 1;
    }
}

fn beta_mean(alpha: f64, beta: f64) -> f64 {
    alpha / (alpha + beta)
}

fn beta_variance(alpha: f64, beta: f64) -> f64 {
    let sum = alpha + beta;
    if sum <= 0.0 {
        return 0.0;
    }
    let var = (alpha * beta) / (sum * sum * (sum + 1.0));
    var.min(VAR_MAX)
}

fn expected_variance_after(alpha: f64, beta: f64) -> f64 {
    let p = beta_mean(alpha, beta);
    let var_success = beta_variance(alpha + 1.0, beta);
    let var_failure = beta_variance(alpha, beta + 1.0);
    p * var_success + (1.0 - p) * var_failure
}

fn boundary_score(e_value: f64, threshold: f64) -> f64 {
    let e = e_value.max(EPS);
    let t = threshold.max(EPS);
    let gap = (e.ln() - t.ln()).abs();
    1.0 / (1.0 + gap)
}

fn log10_ratio(score: f64, cost: f64) -> f64 {
    let ratio = (score + EPS) / (cost + EPS);
    ratio.ln() / std::f64::consts::LN_10
}

// =============================================================================
// Deferred refinement scheduler (bd-2vr05.15.4.4)
// =============================================================================

/// Configuration for VOI-guided deferred refinement scheduling.
///
/// This scheduler only allocates work from spare frame budget and uses a
/// deterministic VOI score with fairness boosts to avoid starvation.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredRefinementConfig {
    /// Minimum spare budget to keep reserved for non-optional work.
    pub min_spare_budget_us: u64,
    /// Hard cap on optional refinements scheduled per frame.
    pub max_refinements_per_frame: usize,
    /// Minimum effective VOI required to schedule a refinement.
    pub voi_gain_cutoff: f64,
    /// Fairness boost added per skipped frame for a region.
    pub fairness_boost_per_skip: f64,
    /// Maximum fairness boost for a region.
    pub fairness_boost_cap: f64,
}

impl Default for DeferredRefinementConfig {
    fn default() -> Self {
        Self {
            min_spare_budget_us: 500,
            max_refinements_per_frame: 2,
            voi_gain_cutoff: 0.01,
            fairness_boost_per_skip: 0.02,
            fairness_boost_cap: 1.0,
        }
    }
}

/// Candidate optional refinement unit for a region/paragraph bucket.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RefinementCandidate {
    /// Deterministic region identifier.
    pub region_id: u64,
    /// Estimated runtime cost for this refinement.
    pub estimated_cost_us: u64,
    /// Predicted value-of-information gain (higher is better).
    pub voi_gain: f64,
}

/// Selected refinement with explainable scoring terms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RefinementSelection {
    pub region_id: u64,
    pub estimated_cost_us: u64,
    pub voi_gain: f64,
    pub fairness_boost: f64,
    pub effective_voi: f64,
    pub score: f64,
}

/// Frame-level refinement plan.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredRefinementPlan {
    pub frame_budget_us: u64,
    pub mandatory_work_us: u64,
    pub reserved_spare_us: u64,
    pub optional_budget_us: u64,
    pub spent_optional_us: u64,
    pub selected: Vec<RefinementSelection>,
}

impl DeferredRefinementPlan {
    /// Hard budget predicate for CI assertions.
    #[must_use]
    pub fn hard_budget_respected(&self) -> bool {
        self.mandatory_work_us
            .saturating_add(self.reserved_spare_us)
            .saturating_add(self.spent_optional_us)
            <= self.frame_budget_us
    }
}

/// Deterministic VOI-guided scheduler for optional deferred refinements.
#[derive(Debug, Clone)]
pub struct DeferredRefinementScheduler {
    config: DeferredRefinementConfig,
    skipped_frames: BTreeMap<u64, u32>,
}

impl DeferredRefinementScheduler {
    /// Create a scheduler from explicit config.
    #[must_use]
    pub fn new(config: DeferredRefinementConfig) -> Self {
        Self {
            config,
            skipped_frames: BTreeMap::new(),
        }
    }

    /// Access scheduler config.
    #[must_use]
    pub fn config(&self) -> &DeferredRefinementConfig {
        &self.config
    }

    /// Number of consecutive frames this region has been skipped.
    #[must_use]
    pub fn skipped_frames_for(&self, region_id: u64) -> u32 {
        self.skipped_frames.get(&region_id).copied().unwrap_or(0)
    }

    /// Compute a deterministic frame plan under a hard budget cap.
    ///
    /// - `frame_budget_us`: total frame budget.
    /// - `mandatory_work_us`: non-optional cost already committed.
    /// - `candidates`: optional refinements for this frame.
    pub fn plan_frame(
        &mut self,
        frame_budget_us: u64,
        mandatory_work_us: u64,
        candidates: &[RefinementCandidate],
    ) -> DeferredRefinementPlan {
        let reserved_spare_us = self.config.min_spare_budget_us;
        let available_after_mandatory = frame_budget_us.saturating_sub(mandatory_work_us);
        let optional_budget_us = available_after_mandatory.saturating_sub(reserved_spare_us);

        let mut scored = Vec::with_capacity(candidates.len());
        for candidate in candidates.iter().copied() {
            let skip_count = self.skipped_frames_for(candidate.region_id);
            let fairness_boost = (skip_count as f64 * self.config.fairness_boost_per_skip)
                .min(self.config.fairness_boost_cap);
            let voi_gain = if candidate.voi_gain.is_finite() {
                candidate.voi_gain.max(0.0)
            } else {
                0.0
            };
            let effective_voi = voi_gain + fairness_boost;
            let normalized_cost = candidate.estimated_cost_us.max(1) as f64;
            let score = effective_voi / normalized_cost;
            scored.push((
                candidate,
                fairness_boost,
                effective_voi,
                score,
                candidate.region_id,
            ));
        }

        // Deterministic ordering:
        // 1) score DESC, 2) effective_voi DESC, 3) region_id ASC.
        scored.sort_by(|a, b| {
            b.3.total_cmp(&a.3)
                .then_with(|| b.2.total_cmp(&a.2))
                .then_with(|| a.4.cmp(&b.4))
        });

        let mut remaining_optional_us = optional_budget_us;
        let mut selected = Vec::with_capacity(self.config.max_refinements_per_frame);
        let mut selected_ids = BTreeMap::<u64, ()>::new();

        for (candidate, fairness_boost, effective_voi, score, _) in scored {
            if selected.len() >= self.config.max_refinements_per_frame {
                break;
            }
            if effective_voi < self.config.voi_gain_cutoff {
                continue;
            }
            if candidate.estimated_cost_us > remaining_optional_us {
                continue;
            }
            selected.push(RefinementSelection {
                region_id: candidate.region_id,
                estimated_cost_us: candidate.estimated_cost_us,
                voi_gain: if candidate.voi_gain.is_finite() {
                    candidate.voi_gain.max(0.0)
                } else {
                    0.0
                },
                fairness_boost,
                effective_voi,
                score,
            });
            selected_ids.insert(candidate.region_id, ());
            remaining_optional_us =
                remaining_optional_us.saturating_sub(candidate.estimated_cost_us);
        }

        // Update fairness accounting for all regions visible this frame.
        for candidate in candidates {
            if selected_ids.contains_key(&candidate.region_id) {
                self.skipped_frames.insert(candidate.region_id, 0);
            } else {
                let next = self
                    .skipped_frames_for(candidate.region_id)
                    .saturating_add(1);
                self.skipped_frames.insert(candidate.region_id, next);
            }
        }

        let spent_optional_us = optional_budget_us.saturating_sub(remaining_optional_us);
        let plan = DeferredRefinementPlan {
            frame_budget_us,
            mandatory_work_us,
            reserved_spare_us,
            optional_budget_us,
            spent_optional_us,
            selected,
        };

        debug_assert!(plan.hard_budget_respected());
        plan
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry::LookupSpan;

    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= *byte as u64;
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
    }

    fn hash_u64(hash: &mut u64, value: u64) {
        hash_bytes(hash, &value.to_le_bytes());
    }

    fn hash_f64(hash: &mut u64, value: f64) {
        hash_u64(hash, value.to_bits());
    }

    fn decision_checksum(decisions: &[VoiDecision]) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        for decision in decisions {
            hash_u64(&mut hash, decision.event_idx);
            hash_u64(&mut hash, decision.should_sample as u64);
            hash_u64(&mut hash, decision.forced_by_interval as u64);
            hash_u64(&mut hash, decision.blocked_by_min_interval as u64);
            hash_f64(&mut hash, decision.voi_gain);
            hash_f64(&mut hash, decision.score);
            hash_f64(&mut hash, decision.cost);
            hash_f64(&mut hash, decision.log_bayes_factor);
            hash_f64(&mut hash, decision.posterior_mean);
            hash_f64(&mut hash, decision.posterior_variance);
            hash_f64(&mut hash, decision.e_value);
            hash_f64(&mut hash, decision.e_threshold);
            hash_f64(&mut hash, decision.boundary_score);
            hash_u64(&mut hash, decision.events_since_sample);
            hash_f64(&mut hash, decision.time_since_sample_ms);
        }
        hash
    }

    #[test]
    fn voi_gain_non_negative() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let decision = sampler.decide(Instant::now());
        assert!(decision.voi_gain >= 0.0);
    }

    #[test]
    fn forced_by_max_interval() {
        let config = VoiConfig {
            max_interval_events: 2,
            sample_cost: 1.0, // discourage sampling unless forced
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);
        let now = Instant::now();

        let d1 = sampler.decide(now);
        assert!(!d1.forced_by_interval);

        let d2 = sampler.decide(now + Duration::from_millis(1));
        assert!(d2.forced_by_interval);
        assert!(d2.should_sample);
    }

    #[test]
    fn min_interval_blocks_sampling_after_first() {
        let config = VoiConfig {
            min_interval_events: 5,
            sample_cost: 0.0, // otherwise would sample
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);

        let first = sampler.decide(Instant::now());
        assert!(first.should_sample);
        sampler.observe(false);

        let second = sampler.decide(Instant::now());
        assert!(second.blocked_by_min_interval);
        assert!(!second.should_sample);
    }

    #[test]
    fn variance_shrinks_with_samples() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let mut now = Instant::now();
        let mut variances = Vec::new();
        for _ in 0..5 {
            let decision = sampler.decide(now);
            if decision.should_sample {
                sampler.observe_at(false, now);
            }
            variances.push(beta_variance(sampler.alpha, sampler.beta));
            now += Duration::from_millis(1);
        }
        for window in variances.windows(2) {
            assert!(window[1] <= window[0] + 1e-9);
        }
    }

    #[test]
    fn decision_checksum_is_stable() {
        let config = VoiConfig {
            sample_cost: 0.01,
            ..Default::default()
        };
        let mut now = Instant::now();
        let mut sampler = VoiSampler::new_at(config, now);

        let mut state: u64 = 42;
        let mut decisions = Vec::new();

        for _ in 0..32 {
            let decision = sampler.decide(now);
            let violated = lcg_next(&mut state).is_multiple_of(10);
            if decision.should_sample {
                sampler.observe_at(violated, now);
            }
            decisions.push(decision);
            now += Duration::from_millis(5 + (lcg_next(&mut state) % 7));
        }

        let checksum = decision_checksum(&decisions);
        assert_eq!(checksum, 0x0b51_d8b6_47a7_b00c);
    }

    #[test]
    fn logs_render_jsonl() {
        let config = VoiConfig {
            enable_logging: true,
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);
        let decision = sampler.decide(Instant::now());
        if decision.should_sample {
            sampler.observe(false);
        }
        let jsonl = sampler.logs_to_jsonl();
        assert!(jsonl.contains("\"event\":\"voi_decision\""));
    }

    proptest! {
        #[test]
        fn prop_voi_gain_non_negative(alpha in 0.01f64..10.0, beta in 0.01f64..10.0) {
            let var = beta_variance(alpha, beta);
            let expected_after = expected_variance_after(alpha, beta);
            prop_assert!(var + 1e-12 >= expected_after);
        }

        #[test]
        fn prop_e_value_stays_positive(seq in proptest::collection::vec(any::<bool>(), 1..50)) {
            let mut sampler = VoiSampler::new(VoiConfig::default());
            let mut now = Instant::now();
            for violated in seq {
                let decision = sampler.decide(now);
                if decision.should_sample {
                    sampler.observe_at(violated, now);
                }
                now += Duration::from_millis(1);
                prop_assert!(sampler.e_value >= E_MIN - 1e-12);
            }
        }
    }

    // =========================================================================
    // Perf microbench (JSONL + budget gate)
    // =========================================================================

    #[test]
    fn perf_voi_sampling_budget() {
        use std::io::Write as _;

        const RUNS: usize = 60;
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let mut now = Instant::now();
        let mut samples = Vec::with_capacity(RUNS);
        let mut jsonl = Vec::new();

        for i in 0..RUNS {
            let start = Instant::now();
            let decision = sampler.decide(now);
            let violated = i % 11 == 0;
            if decision.should_sample {
                sampler.observe_at(violated, now);
            }
            let elapsed_ns = start.elapsed().as_nanos() as u64;
            samples.push(elapsed_ns);

            writeln!(
                &mut jsonl,
                "{{\"test\":\"voi_sampling\",\"case\":\"decision\",\"idx\":{},\
\"elapsed_ns\":{},\"sample\":{},\"violated\":{},\"e_value\":{:.6}}}",
                i, elapsed_ns, decision.should_sample, violated, sampler.e_value
            )
            .expect("jsonl write failed");

            now += Duration::from_millis(1);
        }

        fn percentile(samples: &mut [u64], p: f64) -> u64 {
            samples.sort_unstable();
            let idx = ((samples.len() as f64 - 1.0) * p).round() as usize;
            samples[idx]
        }

        let mut samples_sorted = samples.clone();
        let _p50 = percentile(&mut samples_sorted, 0.50);
        let p95 = percentile(&mut samples_sorted, 0.95);
        let p99 = percentile(&mut samples_sorted, 0.99);

        let (budget_p95, budget_p99) = if cfg!(debug_assertions) {
            (200_000, 400_000)
        } else {
            (20_000, 40_000)
        };

        assert!(p95 <= budget_p95, "p95 {p95}ns exceeds {budget_p95}ns");
        assert!(p99 <= budget_p99, "p99 {p99}ns exceeds {budget_p99}ns");

        let text = String::from_utf8(jsonl).expect("jsonl utf8");
        print!("{text}");
        assert_eq!(text.lines().count(), RUNS);
    }

    // =========================================================================
    // Deterministic JSONL output for E2E harness
    // =========================================================================

    #[test]
    fn e2e_deterministic_jsonl() {
        use std::io::Write as _;

        let seed = std::env::var("VOI_SEED")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        let config = VoiConfig {
            enable_logging: false,
            ..Default::default()
        };
        let mut now = Instant::now();
        let mut sampler = VoiSampler::new_at(config, now);
        let mut state = seed;
        let mut decisions = Vec::new();
        let mut jsonl = Vec::new();

        for idx in 0..40u64 {
            let decision = sampler.decide(now);
            let violated = lcg_next(&mut state).is_multiple_of(7);
            if decision.should_sample {
                sampler.observe_at(violated, now);
            }
            decisions.push(decision.clone());

            writeln!(
                &mut jsonl,
                "{{\"event\":\"voi_decision\",\"seed\":{},\"idx\":{},\
\"sample\":{},\"violated\":{},\"voi_gain\":{:.6}}}",
                seed, idx, decision.should_sample, violated, decision.voi_gain
            )
            .expect("jsonl write failed");

            now += Duration::from_millis(3 + (lcg_next(&mut state) % 5));
        }

        let checksum = decision_checksum(&decisions);
        writeln!(
            &mut jsonl,
            "{{\"event\":\"voi_checksum\",\"seed\":{},\"checksum\":\"{checksum:016x}\",\"decisions\":{}}}",
            seed,
            decisions.len()
        )
        .expect("jsonl write failed");

        let text = String::from_utf8(jsonl).expect("jsonl utf8");
        print!("{text}");
        assert!(text.contains("\"event\":\"voi_checksum\""));
    }

    fn lcg_next(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        *state
    }

    // =========================================================================
    // Additional coverage tests
    // =========================================================================

    #[test]
    fn default_config_values() {
        let cfg = VoiConfig::default();
        assert!((cfg.alpha - 0.05).abs() < f64::EPSILON);
        assert!((cfg.prior_alpha - 1.0).abs() < f64::EPSILON);
        assert!((cfg.prior_beta - 1.0).abs() < f64::EPSILON);
        assert!((cfg.mu_0 - 0.05).abs() < f64::EPSILON);
        assert!((cfg.lambda - 0.5).abs() < f64::EPSILON);
        assert_eq!(cfg.max_interval_ms, 250);
        assert_eq!(cfg.max_interval_events, 20);
        assert_eq!(cfg.min_interval_ms, 0);
        assert_eq!(cfg.min_interval_events, 0);
        assert!(!cfg.enable_logging);
    }

    #[test]
    fn config_clamping_prior_alpha_beta() {
        let config = VoiConfig {
            prior_alpha: -1.0,
            prior_beta: 0.0,
            ..Default::default()
        };
        let sampler = VoiSampler::new(config);
        let (a, b) = sampler.posterior_params();
        assert!(a > 0.0, "alpha should be clamped above zero");
        assert!(b > 0.0, "beta should be clamped above zero");
    }

    #[test]
    fn config_clamping_mu_0() {
        let config = VoiConfig {
            mu_0: -0.5,
            ..Default::default()
        };
        let sampler = VoiSampler::new(config);
        let mean = sampler.posterior_mean();
        assert!((0.0..=1.0).contains(&mean));
    }

    #[test]
    fn config_clamping_sample_cost() {
        let config = VoiConfig {
            sample_cost: -1.0,
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);
        let d = sampler.decide(Instant::now());
        assert!(d.cost > 0.0, "cost should be clamped above zero");
    }

    #[test]
    fn accessor_config() {
        let config = VoiConfig {
            alpha: 0.1,
            ..Default::default()
        };
        let sampler = VoiSampler::new(config);
        assert!((sampler.config().alpha - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn accessor_posterior_params() {
        let config = VoiConfig {
            prior_alpha: 3.0,
            prior_beta: 7.0,
            ..Default::default()
        };
        let sampler = VoiSampler::new(config);
        let (a, b) = sampler.posterior_params();
        assert!((a - 3.0).abs() < f64::EPSILON);
        assert!((b - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accessor_posterior_mean() {
        let config = VoiConfig {
            prior_alpha: 2.0,
            prior_beta: 8.0,
            ..Default::default()
        };
        let sampler = VoiSampler::new(config);
        // mean = 2/(2+8) = 0.2
        assert!((sampler.posterior_mean() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn accessor_posterior_variance() {
        let sampler = VoiSampler::new(VoiConfig::default());
        let var = sampler.posterior_variance();
        assert!(var >= 0.0);
        assert!(var <= 0.25); // max variance for Beta
    }

    #[test]
    fn accessor_expected_variance_after() {
        let sampler = VoiSampler::new(VoiConfig::default());
        let before = sampler.posterior_variance();
        let after = sampler.expected_variance_after();
        assert!(
            after <= before + 1e-12,
            "expected variance after should not exceed current"
        );
    }

    #[test]
    fn last_decision_initially_none() {
        let sampler = VoiSampler::new(VoiConfig::default());
        assert!(sampler.last_decision().is_none());
    }

    #[test]
    fn last_decision_after_decide() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        sampler.decide(Instant::now());
        assert!(sampler.last_decision().is_some());
    }

    #[test]
    fn last_observation_initially_none() {
        let sampler = VoiSampler::new(VoiConfig::default());
        assert!(sampler.last_observation().is_none());
    }

    #[test]
    fn last_observation_after_observe() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        sampler.decide(Instant::now());
        sampler.observe(false);
        assert!(sampler.last_observation().is_some());
        assert!(!sampler.last_observation().unwrap().violated);
    }

    #[test]
    fn observe_violation_updates_alpha() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let (a_before, _) = sampler.posterior_params();
        sampler.decide(Instant::now());
        sampler.observe(true);
        let (a_after, _) = sampler.posterior_params();
        assert!((a_after - a_before - 1.0).abs() < 1e-9);
    }

    #[test]
    fn observe_no_violation_updates_beta() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let (_, b_before) = sampler.posterior_params();
        sampler.decide(Instant::now());
        sampler.observe(false);
        let (_, b_after) = sampler.posterior_params();
        assert!((b_after - b_before - 1.0).abs() < 1e-9);
    }

    #[test]
    fn e_value_positive_after_violations() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let mut now = Instant::now();
        for _ in 0..10 {
            sampler.decide(now);
            sampler.observe_at(true, now);
            now += Duration::from_millis(1);
        }
        let summary = sampler.summary();
        assert!(summary.e_value > 0.0);
    }

    #[test]
    fn summary_initial_state() {
        let sampler = VoiSampler::new(VoiConfig::default());
        let summary = sampler.summary();
        assert_eq!(summary.total_events, 0);
        assert_eq!(summary.total_samples, 0);
        assert_eq!(summary.forced_samples, 0);
        assert_eq!(summary.skipped_events, 0);
        assert!((summary.avg_events_between_samples).abs() < f64::EPSILON);
    }

    #[test]
    fn summary_after_observations() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let now = Instant::now();
        sampler.decide(now);
        sampler.observe_at(false, now);
        sampler.decide(now + Duration::from_millis(10));
        let summary = sampler.summary();
        assert_eq!(summary.total_events, 2);
        assert_eq!(summary.total_samples, 1);
        assert_eq!(summary.skipped_events, 1);
    }

    #[test]
    fn mark_forced_sample_increments() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        assert_eq!(sampler.summary().forced_samples, 0);
        sampler.mark_forced_sample();
        sampler.mark_forced_sample();
        assert_eq!(sampler.summary().forced_samples, 2);
    }

    #[test]
    fn snapshot_captures_state() {
        let mut sampler = VoiSampler::new(VoiConfig {
            enable_logging: true,
            ..Default::default()
        });
        let now = Instant::now();
        sampler.decide(now);
        sampler.observe_at(false, now);

        let snap = sampler.snapshot(10, 42);
        assert_eq!(snap.captured_ms, 42);
        assert!(snap.alpha > 0.0);
        assert!(snap.beta > 0.0);
        assert!((0.0..=1.0).contains(&snap.posterior_mean));
        assert!(snap.last_decision.is_some());
        assert!(snap.last_observation.is_some());
    }

    #[test]
    fn log_rotation_respects_max_entries() {
        let config = VoiConfig {
            enable_logging: true,
            max_log_entries: 3,
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);
        let mut now = Instant::now();

        for _ in 0..10 {
            let d = sampler.decide(now);
            if d.should_sample {
                sampler.observe_at(false, now);
            }
            now += Duration::from_millis(300);
        }

        assert!(sampler.logs().len() <= 3);
    }

    #[test]
    fn logs_empty_when_logging_disabled() {
        let config = VoiConfig {
            enable_logging: false,
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);
        sampler.decide(Instant::now());
        assert!(sampler.logs().is_empty());
    }

    #[test]
    fn decision_jsonl_format() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let decision = sampler.decide(Instant::now());
        let jsonl = decision.to_jsonl();
        assert!(jsonl.starts_with('{'));
        assert!(jsonl.ends_with('}'));
        assert!(jsonl.contains("\"event\":\"voi_decision\""));
        assert!(jsonl.contains("\"should_sample\":"));
        assert!(jsonl.contains("\"reason\":"));
    }

    #[test]
    fn observation_jsonl_format() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        sampler.decide(Instant::now());
        let obs = sampler.observe(false);
        let jsonl = obs.to_jsonl();
        assert!(jsonl.starts_with('{'));
        assert!(jsonl.ends_with('}'));
        assert!(jsonl.contains("\"event\":\"voi_observe\""));
        assert!(jsonl.contains("\"violated\":false"));
    }

    #[test]
    fn log_entry_jsonl_decision_variant() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        let decision = sampler.decide(Instant::now());
        let entry = VoiLogEntry::Decision(decision);
        let jsonl = entry.to_jsonl();
        assert!(jsonl.contains("\"event\":\"voi_decision\""));
    }

    #[test]
    fn log_entry_jsonl_observation_variant() {
        let mut sampler = VoiSampler::new(VoiConfig::default());
        sampler.decide(Instant::now());
        let obs = sampler.observe(true);
        let entry = VoiLogEntry::Observation(obs);
        let jsonl = entry.to_jsonl();
        assert!(jsonl.contains("\"event\":\"voi_observe\""));
        assert!(jsonl.contains("\"violated\":true"));
    }

    #[test]
    fn time_based_max_interval_forces_sample() {
        let config = VoiConfig {
            max_interval_ms: 100,
            max_interval_events: 0, // disable event forcing
            sample_cost: 100.0,     // very high cost to prevent VOI sampling
            ..Default::default()
        };
        let now = Instant::now();
        let mut sampler = VoiSampler::new_at(config, now);

        // First decision (no prior sample, but event interval might force)
        let _d1 = sampler.decide(now + Duration::from_millis(1));
        sampler.observe_at(false, now + Duration::from_millis(1));

        // Second decision well within time window
        let d2 = sampler.decide(now + Duration::from_millis(10));
        assert!(!d2.forced_by_interval, "should not force within 100ms");

        // Decision after 100+ ms
        let d3 = sampler.decide(now + Duration::from_millis(110));
        assert!(d3.forced_by_interval, "should force after 100ms");
        assert!(d3.should_sample);
    }

    #[test]
    fn time_based_min_interval_blocks() {
        let config = VoiConfig {
            min_interval_ms: 50,
            min_interval_events: 0,
            ..Default::default()
        };
        let now = Instant::now();
        let mut sampler = VoiSampler::new_at(config, now);

        // First sample
        let d1 = sampler.decide(now);
        assert!(d1.should_sample);
        sampler.observe_at(false, now);

        // Too soon
        let d2 = sampler.decide(now + Duration::from_millis(10));
        assert!(d2.blocked_by_min_interval);
        assert!(!d2.should_sample);

        // After min interval
        let d3 = sampler.decide(now + Duration::from_millis(60));
        assert!(!d3.blocked_by_min_interval);
    }

    #[test]
    fn decision_reason_strings() {
        // forced
        let config = VoiConfig {
            max_interval_events: 1,
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);
        let d = sampler.decide(Instant::now());
        assert_eq!(d.reason, "forced_interval");
    }

    #[test]
    fn decision_reason_min_interval() {
        let config = VoiConfig {
            min_interval_events: 100,
            sample_cost: 0.0,
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);
        sampler.decide(Instant::now());
        sampler.observe(false);
        let d = sampler.decide(Instant::now());
        assert_eq!(d.reason, "min_interval");
    }

    #[test]
    fn beta_mean_basic() {
        assert!((beta_mean(1.0, 1.0) - 0.5).abs() < 1e-9);
        assert!((beta_mean(2.0, 8.0) - 0.2).abs() < 1e-9);
        assert!((beta_mean(5.0, 5.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn beta_variance_basic() {
        // Var(Beta(1,1)) = 1*1 / (4*3) = 1/12
        let var = beta_variance(1.0, 1.0);
        assert!((var - 1.0 / 12.0).abs() < 1e-9);
    }

    #[test]
    fn beta_variance_degenerate() {
        assert!((beta_variance(0.0, 0.0)).abs() < f64::EPSILON);
        assert!((beta_variance(-1.0, -1.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn boundary_score_at_threshold() {
        // When e_value equals threshold, gap = 0, score = 1.0
        let score = boundary_score(20.0, 20.0);
        assert!((score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn boundary_score_far_from_threshold() {
        // Large gap → small score
        let score = boundary_score(1.0, 1e6);
        assert!(score < 0.1);
    }

    #[test]
    fn logs_to_jsonl_multiple_entries() {
        let config = VoiConfig {
            enable_logging: true,
            ..Default::default()
        };
        let mut sampler = VoiSampler::new(config);
        let mut now = Instant::now();
        for _ in 0..5 {
            let d = sampler.decide(now);
            if d.should_sample {
                sampler.observe_at(false, now);
            }
            now += Duration::from_millis(300);
        }
        let jsonl = sampler.logs_to_jsonl();
        let line_count = jsonl.lines().count();
        assert!(
            line_count >= 2,
            "should have at least 2 log lines, got {line_count}"
        );
    }

    // =========================================================================
    // Tracing capture infrastructure
    // =========================================================================

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    struct CapturedSpan {
        name: String,
        target: String,
        level: tracing::Level,
        fields: HashMap<String, String>,
        parent_name: Option<String>,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    struct CapturedEvent {
        level: tracing::Level,
        target: String,
        message: String,
        fields: HashMap<String, String>,
        parent_span_name: Option<String>,
    }

    struct SpanCapture {
        spans: Arc<Mutex<Vec<CapturedSpan>>>,
        events: Arc<Mutex<Vec<CapturedEvent>>>,
        span_index: Arc<Mutex<HashMap<u64, usize>>>,
    }

    impl SpanCapture {
        fn new() -> (Self, CaptureHandle) {
            let spans = Arc::new(Mutex::new(Vec::new()));
            let events = Arc::new(Mutex::new(Vec::new()));
            let span_index = Arc::new(Mutex::new(HashMap::new()));

            let handle = CaptureHandle {
                spans: spans.clone(),
                events: events.clone(),
            };

            (
                Self {
                    spans,
                    events,
                    span_index,
                },
                handle,
            )
        }
    }

    struct CaptureHandle {
        spans: Arc<Mutex<Vec<CapturedSpan>>>,
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl CaptureHandle {
        fn spans(&self) -> Vec<CapturedSpan> {
            self.spans.lock().unwrap().clone()
        }

        fn events(&self) -> Vec<CapturedEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    struct FieldVisitor(Vec<(String, String)>);

    impl tracing::field::Visit for FieldVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0
                .push((field.name().to_string(), format!("{value:?}")));
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.0.push((field.name().to_string(), value.to_string()));
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.0.push((field.name().to_string(), value.to_string()));
        }

        fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
            self.0.push((field.name().to_string(), value.to_string()));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push((field.name().to_string(), value.to_string()));
        }

        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            self.0.push((field.name().to_string(), value.to_string()));
        }
    }

    impl<S> tracing_subscriber::Layer<S> for SpanCapture
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            id: &tracing::span::Id,
            ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = FieldVisitor(Vec::new());
            attrs.record(&mut visitor);

            let parent_name = ctx
                .current_span()
                .id()
                .and_then(|pid| ctx.span(pid))
                .map(|span_ref| span_ref.name().to_string());

            let mut fields: HashMap<String, String> = visitor.0.into_iter().collect();
            for field in attrs.metadata().fields() {
                fields.entry(field.name().to_string()).or_default();
            }

            let mut spans = self.spans.lock().unwrap();
            let idx = spans.len();
            spans.push(CapturedSpan {
                name: attrs.metadata().name().to_string(),
                target: attrs.metadata().target().to_string(),
                level: *attrs.metadata().level(),
                fields,
                parent_name,
            });

            self.span_index.lock().unwrap().insert(id.into_u64(), idx);
        }

        fn on_record(
            &self,
            id: &tracing::span::Id,
            values: &tracing::span::Record<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = FieldVisitor(Vec::new());
            values.record(&mut visitor);

            let index = self.span_index.lock().unwrap();
            if let Some(&idx) = index.get(&id.into_u64()) {
                let mut spans = self.spans.lock().unwrap();
                if let Some(span) = spans.get_mut(idx) {
                    for (k, v) in visitor.0 {
                        span.fields.insert(k, v);
                    }
                }
            }
        }

        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = FieldVisitor(Vec::new());
            event.record(&mut visitor);

            let fields: HashMap<String, String> = visitor.0.clone().into_iter().collect();
            let message = visitor
                .0
                .iter()
                .find(|(k, _)| k == "message")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();

            let parent_span_name = ctx
                .current_span()
                .id()
                .and_then(|id| ctx.span(id))
                .map(|span_ref| span_ref.name().to_string());

            self.events.lock().unwrap().push(CapturedEvent {
                level: *event.metadata().level(),
                target: event.metadata().target().to_string(),
                message,
                fields,
                parent_span_name,
            });
        }
    }

    fn with_captured_tracing<F>(f: F) -> CaptureHandle
    where
        F: FnOnce(),
    {
        let (layer, handle) = SpanCapture::new();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, f);
        handle
    }

    // =========================================================================
    // Tracing span field assertions (bd-37a.4)
    // =========================================================================

    #[test]
    fn span_voi_evaluate_has_required_fields() {
        let handle = with_captured_tracing(|| {
            let mut sampler = VoiSampler::new(VoiConfig::default());
            sampler.decide(Instant::now());
        });

        let spans = handle.spans();
        let voi_spans: Vec<_> = spans.iter().filter(|s| s.name == "voi.evaluate").collect();
        assert!(
            !voi_spans.is_empty(),
            "expected at least one voi.evaluate span, got none"
        );

        let span = &voi_spans[0];
        assert!(
            span.fields.contains_key("decision_context"),
            "missing decision_context field"
        );
        assert!(
            span.fields.contains_key("voi_estimate"),
            "missing voi_estimate field"
        );
        assert!(
            span.fields.contains_key("sample_cost"),
            "missing sample_cost field"
        );
        assert!(
            span.fields.contains_key("sample_decision"),
            "missing sample_decision field"
        );
    }

    #[test]
    fn span_voi_evaluate_decision_context_values() {
        let handle = with_captured_tracing(|| {
            // Force a high-cost scenario so first decision is not forced
            let config = VoiConfig {
                max_interval_events: 0,
                max_interval_ms: 0,
                sample_cost: 1000.0,
                ..Default::default()
            };
            let mut sampler = VoiSampler::new(config);
            sampler.decide(Instant::now());
        });

        let spans = handle.spans();
        let voi_spans: Vec<_> = spans.iter().filter(|s| s.name == "voi.evaluate").collect();
        assert!(!voi_spans.is_empty());

        let ctx = &voi_spans[0].fields["decision_context"];
        assert!(
            ctx == "voi_lt_cost" || ctx == "voi_ge_cost",
            "unexpected context: {ctx}"
        );
    }

    #[test]
    fn span_voi_evaluate_forced_interval_context() {
        let handle = with_captured_tracing(|| {
            let config = VoiConfig {
                max_interval_events: 1,
                ..Default::default()
            };
            let mut sampler = VoiSampler::new(config);
            sampler.decide(Instant::now());
        });

        let spans = handle.spans();
        let voi_spans: Vec<_> = spans.iter().filter(|s| s.name == "voi.evaluate").collect();
        assert!(!voi_spans.is_empty());
        assert_eq!(voi_spans[0].fields["decision_context"], "forced_interval");
    }

    // =========================================================================
    // DEBUG log assertions
    // =========================================================================

    #[test]
    fn debug_log_voi_calculation() {
        let handle = with_captured_tracing(|| {
            let mut sampler = VoiSampler::new(VoiConfig::default());
            sampler.decide(Instant::now());
        });

        let events = handle.events();
        let debug_events: Vec<_> = events
            .iter()
            .filter(|e| {
                e.level == tracing::Level::DEBUG
                    && e.target == crate::telemetry_schema::TARGET_VOI
                    && e.fields.contains_key("voi_gain")
            })
            .collect();

        assert!(
            !debug_events.is_empty(),
            "expected at least one DEBUG voi calculation event"
        );

        let evt = &debug_events[0];
        assert!(evt.fields.contains_key("score"), "missing score field");
        assert!(evt.fields.contains_key("cost"), "missing cost field");
        assert!(
            evt.fields.contains_key("posterior_mean"),
            "missing posterior_mean"
        );
        assert!(
            evt.fields.contains_key("boundary_score"),
            "missing boundary_score"
        );
    }

    #[test]
    fn debug_log_voi_estimate_histogram() {
        let handle = with_captured_tracing(|| {
            let mut sampler = VoiSampler::new(VoiConfig::default());
            sampler.decide(Instant::now());
        });

        let events = handle.events();
        let hist_events: Vec<_> = events
            .iter()
            .filter(|e| {
                e.level == tracing::Level::DEBUG
                    && e.target == crate::telemetry_schema::TARGET_VOI
                    && e.fields.contains_key("voi_estimate_value")
            })
            .collect();

        assert!(
            !hist_events.is_empty(),
            "expected voi_estimate_value histogram event"
        );
    }

    // =========================================================================
    // TRACE log assertions (utility estimates)
    // =========================================================================

    #[test]
    fn trace_log_utility_estimate_after_observation() {
        let handle = with_captured_tracing(|| {
            let mut sampler = VoiSampler::new(VoiConfig::default());
            let now = Instant::now();
            sampler.decide(now);
            sampler.observe_at(false, now);
        });

        let events = handle.events();
        let trace_events: Vec<_> = events
            .iter()
            .filter(|e| {
                e.level == tracing::Level::TRACE
                    && e.target == crate::telemetry_schema::TARGET_VOI
                    && e.fields.contains_key("voi_estimate_value")
            })
            .collect();

        assert!(
            !trace_events.is_empty(),
            "expected TRACE utility estimate event after observe"
        );

        let evt = &trace_events[0];
        assert!(evt.fields.contains_key("alpha"), "missing alpha");
        assert!(evt.fields.contains_key("beta"), "missing beta");
        assert!(
            evt.fields.contains_key("posterior_mean"),
            "missing posterior_mean"
        );
        assert!(evt.fields.contains_key("e_value"), "missing e_value");
    }

    // =========================================================================
    // Counter verification
    // =========================================================================

    #[test]
    fn counters_increment_on_sample_decision() {
        let handle = with_captured_tracing(|| {
            let mut sampler = VoiSampler::new(VoiConfig::default());
            let mut now = Instant::now();
            for _ in 0..5 {
                let d = sampler.decide(now);
                if d.should_sample {
                    sampler.observe_at(false, now);
                }
                now += Duration::from_millis(100);
            }
        });

        // Verify the tracing events were emitted (indirect counter check).
        // We can't reliably check global atomic counters in parallel tests,
        // so we verify the DEBUG events contain voi_gain (one per decide call).
        let events = handle.events();
        let calc_events: Vec<_> = events
            .iter()
            .filter(|e| {
                e.level == tracing::Level::DEBUG
                    && e.target == crate::telemetry_schema::TARGET_VOI
                    && e.fields.contains_key("voi_gain")
            })
            .collect();

        assert_eq!(
            calc_events.len(),
            5,
            "expected 5 voi calculation events for 5 decide() calls"
        );
    }

    #[test]
    fn counter_accessors_are_callable() {
        // Verify the counter accessor functions exist and return non-panicking values.
        let taken = voi_samples_taken_total();
        let skipped = voi_samples_skipped_total();
        // Verify the sum doesn't overflow (sanity check).
        let _ = taken.checked_add(skipped).expect("counter overflow");
    }

    #[test]
    fn counters_increase_monotonically() {
        let before_taken = voi_samples_taken_total();
        let before_skipped = voi_samples_skipped_total();

        let mut sampler = VoiSampler::new(VoiConfig::default());
        let mut now = Instant::now();
        for _ in 0..10 {
            let d = sampler.decide(now);
            if d.should_sample {
                sampler.observe_at(false, now);
            }
            now += Duration::from_millis(100);
        }

        let after_taken = voi_samples_taken_total();
        let after_skipped = voi_samples_skipped_total();

        // After 10 decisions, at least some should have been taken or skipped.
        assert!(
            (after_taken + after_skipped) >= (before_taken + before_skipped) + 10,
            "expected at least 10 counter increments total, \
             taken: {before_taken}→{after_taken}, skipped: {before_skipped}→{after_skipped}"
        );
    }

    // =========================================================================
    // Deferred refinement scheduler tests (bd-2vr05.15.4.4)
    // =========================================================================

    #[test]
    fn deferred_scheduler_respects_hard_budget() {
        let mut scheduler = DeferredRefinementScheduler::new(DeferredRefinementConfig {
            min_spare_budget_us: 200,
            max_refinements_per_frame: 3,
            voi_gain_cutoff: 0.01,
            fairness_boost_per_skip: 0.02,
            fairness_boost_cap: 0.5,
        });

        let candidates = [
            RefinementCandidate {
                region_id: 1,
                estimated_cost_us: 600,
                voi_gain: 0.25,
            },
            RefinementCandidate {
                region_id: 2,
                estimated_cost_us: 500,
                voi_gain: 0.21,
            },
            RefinementCandidate {
                region_id: 3,
                estimated_cost_us: 300,
                voi_gain: 0.08,
            },
        ];

        let plan = scheduler.plan_frame(3_000, 1_900, &candidates);
        assert!(plan.hard_budget_respected());
        assert!(plan.spent_optional_us <= plan.optional_budget_us);
        assert!(
            plan.mandatory_work_us
                .saturating_add(plan.reserved_spare_us)
                .saturating_add(plan.spent_optional_us)
                <= 3_000
        );
    }

    #[test]
    fn deferred_scheduler_is_deterministic_for_identical_inputs() {
        let config = DeferredRefinementConfig {
            min_spare_budget_us: 100,
            max_refinements_per_frame: 2,
            voi_gain_cutoff: 0.01,
            fairness_boost_per_skip: 0.03,
            fairness_boost_cap: 0.6,
        };
        let mut a = DeferredRefinementScheduler::new(config.clone());
        let mut b = DeferredRefinementScheduler::new(config);

        let candidates = [
            RefinementCandidate {
                region_id: 11,
                estimated_cost_us: 450,
                voi_gain: 0.13,
            },
            RefinementCandidate {
                region_id: 22,
                estimated_cost_us: 500,
                voi_gain: 0.11,
            },
            RefinementCandidate {
                region_id: 33,
                estimated_cost_us: 350,
                voi_gain: 0.07,
            },
        ];

        for _ in 0..25 {
            let pa = a.plan_frame(2_800, 1_600, &candidates);
            let pb = b.plan_frame(2_800, 1_600, &candidates);
            assert_eq!(pa, pb);
        }
    }

    #[test]
    fn deferred_scheduler_fairness_avoids_starvation() {
        let mut scheduler = DeferredRefinementScheduler::new(DeferredRefinementConfig {
            min_spare_budget_us: 400,
            max_refinements_per_frame: 1,
            voi_gain_cutoff: 0.01,
            fairness_boost_per_skip: 0.05,
            fairness_boost_cap: 2.0,
        });

        let candidates = [
            RefinementCandidate {
                region_id: 100,
                estimated_cost_us: 700,
                voi_gain: 0.20,
            },
            RefinementCandidate {
                region_id: 200,
                estimated_cost_us: 700,
                voi_gain: 0.02,
            },
        ];

        let mut low_region_selected = 0u32;
        for _ in 0..30 {
            let plan = scheduler.plan_frame(4_000, 2_700, &candidates);
            assert!(plan.hard_budget_respected());
            if plan.selected.iter().any(|s| s.region_id == 200) {
                low_region_selected = low_region_selected.saturating_add(1);
            }
        }

        assert!(
            low_region_selected > 0,
            "fairness boosting should eventually schedule the lower-VOI region"
        );
    }
}
