#![forbid(unsafe_code)]

//! Queueing Theory Scheduler with SRPT/Smith-Rule Style Scheduling (bd-13pq.7).
//!
//! This module provides a fair, work-conserving task scheduler based on queueing theory
//! principles. It implements variants of SRPT (Shortest Remaining Processing Time) with
//! fairness constraints to prevent starvation.
//!
//! # Mathematical Model
//!
//! ## Scheduling Disciplines
//!
//! 1. **SRPT (Shortest Remaining Processing Time)**
//!    - Optimal for minimizing mean response time in M/G/1 queues
//!    - Preempts current job if a shorter job arrives
//!    - Problem: Can starve long jobs indefinitely
//!
//! 2. **Smith's Rule (Weighted SRPT)**
//!    - Priority = weight / remaining_time
//!    - Maximizes weighted throughput
//!    - Still suffers from starvation
//!
//! 3. **Fair SRPT (this implementation)**
//!    - Uses aging: priority increases with wait time
//!    - Ensures bounded wait time for all jobs
//!    - Trade-off: slightly worse mean response time for bounded starvation
//!
//! ## Queue Discipline
//!
//! Jobs are ordered by effective priority:
//! ```text
//! priority = (weight / remaining_time) + aging_factor * wait_time
//! ```
//!
//! Equivalent minimization form (used for evidence logging):
//! ```text
//! loss_proxy = 1 / max(priority, w_min)
//! ```
//!
//! This combines:
//! - Smith's rule: `weight / remaining_time`
//! - Aging: linear increase with wait time
//!
//! ## Fairness Guarantee (Aging-Based)
//!
//! With aging factor `a` and maximum job size `S_max`:
//! ```text
//! max_wait <= S_max * (1 + 1/a) / min_weight
//! ```
//!
//! # Key Invariants
//!
//! 1. **Work-conserving**: Server never idles when queue is non-empty
//! 2. **Priority ordering**: Queue is always sorted by effective priority
//! 3. **Bounded starvation**: All jobs complete within bounded time
//! 4. **Monotonic aging**: Wait time only increases while in queue
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | Zero weight | Use minimum weight | Prevent infinite priority |
//! | Zero remaining time | Complete immediately | Job is done |
//! | Queue overflow | Reject new jobs | Bounded memory |
//! | Clock drift | Use monotonic time | Avoid priority inversions |

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt::Write;

/// Default aging factor (0.1 = job gains priority of 1 unit after 10 time units).
const DEFAULT_AGING_FACTOR: f64 = 0.1;

/// Maximum queue size.
const MAX_QUEUE_SIZE: usize = 10_000;

/// Default minimum processing-time estimate (ms).
const DEFAULT_P_MIN_MS: f64 = 0.05;

/// Default maximum processing-time estimate (ms).
const DEFAULT_P_MAX_MS: f64 = 5_000.0;

/// Default minimum weight to prevent division issues.
const DEFAULT_W_MIN: f64 = 1e-6;

/// Default maximum weight cap.
const DEFAULT_W_MAX: f64 = 100.0;

/// Default weight when the source is `Default`.
const DEFAULT_WEIGHT_DEFAULT: f64 = 1.0;

/// Default weight when the source is `Unknown`.
const DEFAULT_WEIGHT_UNKNOWN: f64 = 1.0;

/// Default estimate (ms) when the source is `Default`.
const DEFAULT_ESTIMATE_DEFAULT_MS: f64 = 10.0;

/// Default estimate (ms) when the source is `Unknown`.
const DEFAULT_ESTIMATE_UNKNOWN_MS: f64 = 1_000.0;

/// Default starvation guard threshold (ms). 0 disables the guard.
const DEFAULT_WAIT_STARVE_MS: f64 = 500.0;

/// Default multiplier applied when starvation guard triggers.
const DEFAULT_STARVE_BOOST_RATIO: f64 = 1.5;

/// Configuration for the scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Aging factor: how fast priority increases with wait time.
    /// Higher = faster aging = more fairness, less optimality.
    /// Default: 0.1.
    pub aging_factor: f64,

    /// Minimum processing-time estimate (ms). Default: 0.05.
    pub p_min_ms: f64,

    /// Maximum processing-time estimate (ms). Default: 5000.
    pub p_max_ms: f64,

    /// Default estimate (ms) when estimate source is `Default`.
    /// Default: 10.0.
    pub estimate_default_ms: f64,

    /// Default estimate (ms) when estimate source is `Unknown`.
    /// Default: 1000.0.
    pub estimate_unknown_ms: f64,

    /// Minimum weight clamp. Default: 1e-6.
    pub w_min: f64,

    /// Maximum weight clamp. Default: 100.
    pub w_max: f64,

    /// Default weight when weight source is `Default`.
    /// Default: 1.0.
    pub weight_default: f64,

    /// Default weight when weight source is `Unknown`.
    /// Default: 1.0.
    pub weight_unknown: f64,

    /// Starvation guard threshold (ms). 0 disables the guard. Default: 500.
    pub wait_starve_ms: f64,

    /// Multiplier applied to base ratio when starvation guard triggers.
    /// Default: 1.5.
    pub starve_boost_ratio: f64,

    /// Enable Smith-rule weighting. If false, behaves like SRPT.
    pub smith_enabled: bool,

    /// Force FIFO ordering (arrival sequence) regardless of other settings.
    /// Useful for safety overrides and debugging.
    pub force_fifo: bool,

    /// Maximum queue size. Default: 10_000.
    pub max_queue_size: usize,

    /// Enable preemption. Default: true.
    pub preemptive: bool,

    /// Time quantum for round-robin fallback (when priorities are equal).
    /// Default: 10.0.
    pub time_quantum: f64,

    /// Enable logging. Default: false.
    pub enable_logging: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            aging_factor: DEFAULT_AGING_FACTOR,
            p_min_ms: DEFAULT_P_MIN_MS,
            p_max_ms: DEFAULT_P_MAX_MS,
            estimate_default_ms: DEFAULT_ESTIMATE_DEFAULT_MS,
            estimate_unknown_ms: DEFAULT_ESTIMATE_UNKNOWN_MS,
            w_min: DEFAULT_W_MIN,
            w_max: DEFAULT_W_MAX,
            weight_default: DEFAULT_WEIGHT_DEFAULT,
            weight_unknown: DEFAULT_WEIGHT_UNKNOWN,
            wait_starve_ms: DEFAULT_WAIT_STARVE_MS,
            starve_boost_ratio: DEFAULT_STARVE_BOOST_RATIO,
            smith_enabled: true,
            force_fifo: false,
            max_queue_size: MAX_QUEUE_SIZE,
            preemptive: true,
            time_quantum: 10.0,
            enable_logging: false,
        }
    }
}

impl SchedulerConfig {
    /// Effective scheduling mode with FIFO override.
    pub fn mode(&self) -> SchedulingMode {
        if self.force_fifo {
            SchedulingMode::Fifo
        } else if self.smith_enabled {
            SchedulingMode::Smith
        } else {
            SchedulingMode::Srpt
        }
    }
}

/// A job in the queue.
#[derive(Debug, Clone)]
pub struct Job {
    /// Unique job identifier.
    pub id: u64,

    /// Job weight (importance). Higher = more priority.
    pub weight: f64,

    /// Estimated remaining processing time.
    pub remaining_time: f64,

    /// Original estimated total time.
    pub total_time: f64,

    /// Time when job was submitted.
    pub arrival_time: f64,

    /// Monotonic arrival sequence (tie-breaker).
    pub arrival_seq: u64,

    /// Source of processing-time estimate.
    pub estimate_source: EstimateSource,

    /// Source of weight/importance.
    pub weight_source: WeightSource,

    /// Optional job name for debugging.
    pub name: Option<String>,
}

impl Job {
    /// Create a new job with given ID, weight, and estimated time.
    pub fn new(id: u64, weight: f64, estimated_time: f64) -> Self {
        let weight = if weight.is_nan() {
            DEFAULT_W_MIN
        } else if weight.is_infinite() {
            if weight.is_sign_positive() {
                DEFAULT_W_MAX
            } else {
                DEFAULT_W_MIN
            }
        } else {
            weight.clamp(DEFAULT_W_MIN, DEFAULT_W_MAX)
        };
        let estimated_time = if estimated_time.is_nan() {
            DEFAULT_P_MAX_MS
        } else if estimated_time.is_infinite() {
            if estimated_time.is_sign_positive() {
                DEFAULT_P_MAX_MS
            } else {
                DEFAULT_P_MIN_MS
            }
        } else {
            estimated_time.clamp(DEFAULT_P_MIN_MS, DEFAULT_P_MAX_MS)
        };
        Self {
            id,
            weight,
            remaining_time: estimated_time,
            total_time: estimated_time,
            arrival_time: 0.0,
            arrival_seq: 0,
            estimate_source: EstimateSource::Explicit,
            weight_source: WeightSource::Explicit,
            name: None,
        }
    }

    /// Create a job with a name.
    pub fn with_name(id: u64, weight: f64, estimated_time: f64, name: impl Into<String>) -> Self {
        let mut job = Self::new(id, weight, estimated_time);
        job.name = Some(name.into());
        job
    }

    /// Set estimate and weight sources.
    pub fn with_sources(
        mut self,
        weight_source: WeightSource,
        estimate_source: EstimateSource,
    ) -> Self {
        self.weight_source = weight_source;
        self.estimate_source = estimate_source;
        self
    }

    /// Fraction of job completed.
    pub fn progress(&self) -> f64 {
        if self.total_time <= 0.0 {
            1.0
        } else {
            1.0 - (self.remaining_time / self.total_time).clamp(0.0, 1.0)
        }
    }

    /// Is the job complete?
    pub fn is_complete(&self) -> bool {
        self.remaining_time <= 0.0
    }
}

/// Priority wrapper for the binary heap (max-heap, so we negate priority).
#[derive(Debug, Clone)]
struct PriorityJob {
    priority: f64,
    base_ratio: f64,
    job: Job,
    mode: SchedulingMode,
}

impl PartialEq for PriorityJob {
    fn eq(&self, other: &Self) -> bool {
        self.job.id == other.job.id
    }
}

impl Eq for PriorityJob {}

impl PartialOrd for PriorityJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PriorityJob {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.mode == SchedulingMode::Fifo || other.mode == SchedulingMode::Fifo {
            return other
                .job
                .arrival_seq
                .cmp(&self.job.arrival_seq)
                .then_with(|| other.job.id.cmp(&self.job.id));
        }
        // Higher priority comes first (max-heap)
        self.priority
            .total_cmp(&other.priority)
            // Tie-break 1: base ratio (w/p)
            .then_with(|| self.base_ratio.total_cmp(&other.base_ratio))
            // Tie-break 2: higher weight
            .then_with(|| self.job.weight.total_cmp(&other.job.weight))
            // Tie-break 3: shorter remaining time
            .then_with(|| other.job.remaining_time.total_cmp(&self.job.remaining_time))
            // Tie-break 4: earlier arrival sequence
            .then_with(|| other.job.arrival_seq.cmp(&self.job.arrival_seq))
            // Tie-break 5: lower job id
            .then_with(|| other.job.id.cmp(&self.job.id))
    }
}

/// Evidence for scheduling decisions.
#[derive(Debug, Clone)]
pub struct SchedulingEvidence {
    /// Current time.
    pub current_time: f64,

    /// Selected job ID (if any).
    pub selected_job_id: Option<u64>,

    /// Queue length.
    pub queue_length: usize,

    /// Mean wait time in queue.
    pub mean_wait_time: f64,

    /// Max wait time in queue.
    pub max_wait_time: f64,

    /// Reason for selection.
    pub reason: SelectionReason,

    /// Tie-break reason for the selected job (if applicable).
    pub tie_break_reason: Option<TieBreakReason>,

    /// Per-job evidence entries (ordered by scheduler priority).
    pub jobs: Vec<JobEvidence>,
}

/// Evidence entry for a single job in the queue.
#[derive(Debug, Clone)]
pub struct JobEvidence {
    /// Job id.
    pub job_id: u64,
    /// Optional name.
    pub name: Option<String>,
    /// Processing-time estimate (ms).
    pub estimate_ms: f64,
    /// Weight (importance).
    pub weight: f64,
    /// Base ratio (w/p).
    pub ratio: f64,
    /// Aging contribution (`aging_factor * age_ms`).
    pub aging_reward: f64,
    /// Starvation floor (`ratio * starve_boost_ratio`) when guard applies, else 0.
    pub starvation_floor: f64,
    /// Age in queue (ms).
    pub age_ms: f64,
    /// Effective priority (ratio + aging, with starvation guard).
    pub effective_priority: f64,
    /// Monotone loss proxy minimized by policy (lower is better).
    pub objective_loss_proxy: f64,
    /// Estimate source.
    pub estimate_source: EstimateSource,
    /// Weight source.
    pub weight_source: WeightSource,
}

/// Reason for job selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    /// No jobs in queue.
    QueueEmpty,
    /// Selected by SRPT (shortest remaining time).
    ShortestRemaining,
    /// Selected by Smith's rule (weight/time).
    HighestWeightedPriority,
    /// Selected by FIFO override.
    Fifo,
    /// Selected due to aging (waited too long).
    AgingBoost,
    /// Continued from preemption.
    Continuation,
}

impl SelectionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::QueueEmpty => "queue_empty",
            Self::ShortestRemaining => "shortest_remaining",
            Self::HighestWeightedPriority => "highest_weighted_priority",
            Self::Fifo => "fifo",
            Self::AgingBoost => "aging_boost",
            Self::Continuation => "continuation",
        }
    }
}

/// Source of a processing-time estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimateSource {
    /// Explicit estimate provided by caller.
    Explicit,
    /// Historical estimate derived from prior runs.
    Historical,
    /// Default estimate (fallback).
    Default,
    /// Unknown estimate (no data).
    Unknown,
}

impl EstimateSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Historical => "historical",
            Self::Default => "default",
            Self::Unknown => "unknown",
        }
    }
}

/// Source of a weight/importance value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeightSource {
    /// Explicit weight provided by caller.
    Explicit,
    /// Default weight (fallback).
    Default,
    /// Unknown weight (no data).
    Unknown,
}

impl WeightSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Default => "default",
            Self::Unknown => "unknown",
        }
    }
}

/// Tie-break reason for ordering decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TieBreakReason {
    /// Effective priority (base ratio + aging) decided.
    EffectivePriority,
    /// Base ratio (w/p) decided.
    BaseRatio,
    /// Weight decided.
    Weight,
    /// Remaining time decided.
    RemainingTime,
    /// Arrival sequence decided.
    ArrivalSeq,
    /// Job id decided.
    JobId,
    /// Continued current job without comparison.
    Continuation,
}

impl TieBreakReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::EffectivePriority => "effective_priority",
            Self::BaseRatio => "base_ratio",
            Self::Weight => "weight",
            Self::RemainingTime => "remaining_time",
            Self::ArrivalSeq => "arrival_seq",
            Self::JobId => "job_id",
            Self::Continuation => "continuation",
        }
    }
}

/// Effective scheduling discipline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulingMode {
    /// Smith's rule (weight / remaining time).
    Smith,
    /// SRPT (shortest remaining processing time).
    Srpt,
    /// FIFO (arrival order).
    Fifo,
}

impl SchedulingEvidence {
    /// Serialize scheduling evidence to JSONL with the supplied event tag.
    #[must_use]
    pub fn to_jsonl(&self, event: &str) -> String {
        let mut out = String::with_capacity(256 + (self.jobs.len() * 64));
        out.push_str("{\"event\":\"");
        out.push_str(&escape_json(event));
        out.push_str("\",\"current_time\":");
        let _ = write!(out, "{:.6}", self.current_time);
        out.push_str(",\"selected_job_id\":");
        match self.selected_job_id {
            Some(id) => {
                let _ = write!(out, "{id}");
            }
            None => out.push_str("null"),
        }
        out.push_str(",\"queue_length\":");
        let _ = write!(out, "{}", self.queue_length);
        out.push_str(",\"mean_wait_time\":");
        let _ = write!(out, "{:.6}", self.mean_wait_time);
        out.push_str(",\"max_wait_time\":");
        let _ = write!(out, "{:.6}", self.max_wait_time);
        out.push_str(",\"reason\":\"");
        out.push_str(self.reason.as_str());
        out.push('"');
        out.push_str(",\"tie_break_reason\":");
        match self.tie_break_reason {
            Some(reason) => {
                out.push('"');
                out.push_str(reason.as_str());
                out.push('"');
            }
            None => out.push_str("null"),
        }
        out.push_str(",\"jobs\":[");
        for (idx, job) in self.jobs.iter().enumerate() {
            if idx > 0 {
                out.push(',');
            }
            out.push_str(&job.to_json());
        }
        out.push_str("]}");
        out
    }
}

impl JobEvidence {
    fn to_json(&self) -> String {
        let mut out = String::with_capacity(128);
        out.push_str("{\"job_id\":");
        let _ = write!(out, "{}", self.job_id);
        out.push_str(",\"name\":");
        match &self.name {
            Some(name) => {
                out.push('"');
                out.push_str(&escape_json(name));
                out.push('"');
            }
            None => out.push_str("null"),
        }
        out.push_str(",\"estimate_ms\":");
        let _ = write!(out, "{:.6}", self.estimate_ms);
        out.push_str(",\"weight\":");
        let _ = write!(out, "{:.6}", self.weight);
        out.push_str(",\"ratio\":");
        let _ = write!(out, "{:.6}", self.ratio);
        out.push_str(",\"aging_reward\":");
        let _ = write!(out, "{:.6}", self.aging_reward);
        out.push_str(",\"starvation_floor\":");
        let _ = write!(out, "{:.6}", self.starvation_floor);
        out.push_str(",\"age_ms\":");
        let _ = write!(out, "{:.6}", self.age_ms);
        out.push_str(",\"effective_priority\":");
        let _ = write!(out, "{:.6}", self.effective_priority);
        out.push_str(",\"objective_loss_proxy\":");
        let _ = write!(out, "{:.6}", self.objective_loss_proxy);
        out.push_str(",\"estimate_source\":\"");
        out.push_str(self.estimate_source.as_str());
        out.push('"');
        out.push_str(",\"weight_source\":\"");
        out.push_str(self.weight_source.as_str());
        out.push('"');
        out.push('}');
        out
    }
}

fn escape_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if c < ' ' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Scheduler statistics.
#[derive(Debug, Clone, Default)]
pub struct SchedulerStats {
    /// Total jobs submitted.
    pub total_submitted: u64,

    /// Total jobs completed.
    pub total_completed: u64,

    /// Total jobs rejected (queue full).
    pub total_rejected: u64,

    /// Total preemptions.
    pub total_preemptions: u64,

    /// Total time processing.
    pub total_processing_time: f64,

    /// Sum of response times (for mean calculation).
    pub total_response_time: f64,

    /// Max response time observed.
    pub max_response_time: f64,

    /// Current queue length.
    pub queue_length: usize,
}

impl SchedulerStats {
    /// Mean response time.
    pub fn mean_response_time(&self) -> f64 {
        if self.total_completed > 0 {
            self.total_response_time / self.total_completed as f64
        } else {
            0.0
        }
    }

    /// Throughput (jobs per time unit).
    pub fn throughput(&self) -> f64 {
        if self.total_processing_time > 0.0 {
            self.total_completed as f64 / self.total_processing_time
        } else {
            0.0
        }
    }
}

/// Queueing theory scheduler with fair SRPT.
#[derive(Debug)]
pub struct QueueingScheduler {
    config: SchedulerConfig,

    /// Priority queue of jobs.
    queue: BinaryHeap<PriorityJob>,

    /// Currently running job (if preemptive and processing).
    current_job: Option<Job>,

    /// Current simulation time.
    current_time: f64,

    /// Next job ID.
    next_job_id: u64,

    /// Next arrival sequence number.
    next_arrival_seq: u64,

    /// Statistics.
    stats: SchedulerStats,
}

#[derive(Debug, Clone, Copy)]
struct PriorityTerms {
    aging_reward: f64,
    starvation_floor: f64,
    effective_priority: f64,
}

impl QueueingScheduler {
    /// Create a new scheduler with given configuration.
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            queue: BinaryHeap::new(),
            current_job: None,
            current_time: 0.0,
            next_job_id: 1,
            next_arrival_seq: 1,
            stats: SchedulerStats::default(),
        }
    }

    /// Submit a new job to the scheduler.
    ///
    /// Returns the job ID if accepted, None if rejected (queue full).
    pub fn submit(&mut self, weight: f64, estimated_time: f64) -> Option<u64> {
        self.submit_named(weight, estimated_time, None::<&str>)
    }

    /// Submit a named job.
    pub fn submit_named(
        &mut self,
        weight: f64,
        estimated_time: f64,
        name: Option<impl Into<String>>,
    ) -> Option<u64> {
        self.submit_with_sources(
            weight,
            estimated_time,
            WeightSource::Explicit,
            EstimateSource::Explicit,
            name,
        )
    }

    /// Submit a job with explicit estimate/weight sources for evidence logging.
    pub fn submit_with_sources(
        &mut self,
        weight: f64,
        estimated_time: f64,
        weight_source: WeightSource,
        estimate_source: EstimateSource,
        name: Option<impl Into<String>>,
    ) -> Option<u64> {
        if self.queue.len() >= self.config.max_queue_size {
            self.stats.total_rejected += 1;
            return None;
        }

        let id = self.next_job_id;
        self.next_job_id += 1;

        let mut job =
            Job::new(id, weight, estimated_time).with_sources(weight_source, estimate_source);
        job.weight = self.normalize_weight_with_source(job.weight, job.weight_source);
        job.remaining_time =
            self.normalize_time_with_source(job.remaining_time, job.estimate_source);
        job.total_time = job.remaining_time;
        job.arrival_time = self.current_time;
        job.arrival_seq = self.next_arrival_seq;
        self.next_arrival_seq += 1;
        if let Some(n) = name {
            job.name = Some(n.into());
        }

        let priority_job = self.make_priority_job(job);
        self.queue.push(priority_job);

        self.stats.total_submitted += 1;
        self.stats.queue_length = self.queue.len();

        // Check for preemption
        if self.config.preemptive {
            self.maybe_preempt();
        }

        Some(id)
    }

    /// Advance time by the given amount and process jobs.
    ///
    /// Returns a list of completed job IDs.
    pub fn tick(&mut self, delta_time: f64) -> Vec<u64> {
        let mut completed = Vec::new();
        if delta_time <= 0.0 {
            return completed;
        }

        let mut remaining_time = delta_time;
        let mut now = self.current_time;
        let mut processed_time = 0.0;

        while remaining_time > 0.0 {
            // Get or select next job
            let Some(mut job) = (if let Some(j) = self.current_job.take() {
                Some(j)
            } else {
                self.queue.pop().map(|pj| pj.job)
            }) else {
                now += remaining_time;
                break; // Queue empty
            };

            // Process job
            let process_time = remaining_time.min(job.remaining_time);
            job.remaining_time -= process_time;
            remaining_time -= process_time;
            now += process_time;
            processed_time += process_time;

            if job.is_complete() {
                // Job completed
                let response_time = now - job.arrival_time;
                self.stats.total_response_time += response_time;
                self.stats.max_response_time = self.stats.max_response_time.max(response_time);
                self.stats.total_completed += 1;
                completed.push(job.id);
            } else {
                // Job not complete, save for next tick
                self.current_job = Some(job);
            }
        }

        self.stats.total_processing_time += processed_time;
        self.current_time = now;
        // Recompute priorities for aged jobs
        self.refresh_priorities();

        self.stats.queue_length = self.queue.len();
        completed
    }

    /// Select the next job to run without advancing time.
    pub fn peek_next(&self) -> Option<&Job> {
        self.current_job
            .as_ref()
            .or_else(|| self.queue.peek().map(|pj| &pj.job))
    }

    /// Get scheduling evidence for the current state.
    pub fn evidence(&self) -> SchedulingEvidence {
        let (mean_wait, max_wait) = self.compute_wait_stats();

        let mut candidates: Vec<PriorityJob> = self
            .queue
            .iter()
            .map(|pj| self.make_priority_job(pj.job.clone()))
            .collect();

        if let Some(ref current) = self.current_job {
            candidates.push(self.make_priority_job(current.clone()));
        }

        candidates.sort_by(|a, b| b.cmp(a));

        let selected_job_id = if let Some(ref current) = self.current_job {
            Some(current.id)
        } else {
            candidates.first().map(|pj| pj.job.id)
        };

        let tie_break_reason = if self.current_job.is_some() {
            Some(TieBreakReason::Continuation)
        } else if candidates.len() > 1 {
            Some(self.tie_break_reason(&candidates[0], &candidates[1]))
        } else {
            None
        };

        let reason = if self.queue.is_empty() && self.current_job.is_none() {
            SelectionReason::QueueEmpty
        } else if self.current_job.is_some() {
            SelectionReason::Continuation
        } else if self.config.mode() == SchedulingMode::Fifo {
            SelectionReason::Fifo
        } else if let Some(pj) = candidates.first() {
            let wait_time = (self.current_time - pj.job.arrival_time).max(0.0);
            let aging_contribution = self.config.aging_factor * wait_time;
            let aging_boost = (self.config.wait_starve_ms > 0.0
                && wait_time >= self.config.wait_starve_ms)
                || aging_contribution > pj.base_ratio * 0.5;
            if aging_boost {
                SelectionReason::AgingBoost
            } else if self.config.smith_enabled && pj.job.weight > 1.0 {
                SelectionReason::HighestWeightedPriority
            } else {
                SelectionReason::ShortestRemaining
            }
        } else {
            SelectionReason::QueueEmpty
        };

        let jobs = candidates
            .iter()
            .map(|pj| {
                let age_ms = (self.current_time - pj.job.arrival_time).max(0.0);
                let terms = self.compute_priority_terms(&pj.job);
                JobEvidence {
                    job_id: pj.job.id,
                    name: pj.job.name.clone(),
                    estimate_ms: pj.job.remaining_time,
                    weight: pj.job.weight,
                    ratio: pj.base_ratio,
                    aging_reward: terms.aging_reward,
                    starvation_floor: terms.starvation_floor,
                    age_ms,
                    effective_priority: pj.priority,
                    objective_loss_proxy: 1.0 / pj.priority.max(self.config.w_min),
                    estimate_source: pj.job.estimate_source,
                    weight_source: pj.job.weight_source,
                }
            })
            .collect();

        SchedulingEvidence {
            current_time: self.current_time,
            selected_job_id,
            queue_length: self.queue.len() + if self.current_job.is_some() { 1 } else { 0 },
            mean_wait_time: mean_wait,
            max_wait_time: max_wait,
            reason,
            tie_break_reason,
            jobs,
        }
    }

    /// Get current statistics.
    pub fn stats(&self) -> SchedulerStats {
        let mut stats = self.stats.clone();
        stats.queue_length = self.queue.len() + if self.current_job.is_some() { 1 } else { 0 };
        stats
    }

    /// Cancel a job by ID.
    pub fn cancel(&mut self, job_id: u64) -> bool {
        // Check current job
        if let Some(ref j) = self.current_job
            && j.id == job_id
        {
            self.current_job = None;
            self.stats.queue_length = self.queue.len();
            return true;
        }

        // Remove from queue (rebuild without the job)
        let old_len = self.queue.len();
        let jobs: Vec<_> = self
            .queue
            .drain()
            .filter(|pj| pj.job.id != job_id)
            .collect();
        self.queue = jobs.into_iter().collect();

        self.stats.queue_length = self.queue.len();
        old_len != self.queue.len()
    }

    /// Clear all jobs.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.current_job = None;
        self.stats.queue_length = 0;
    }

    /// Reset scheduler state.
    pub fn reset(&mut self) {
        self.queue.clear();
        self.current_job = None;
        self.current_time = 0.0;
        self.next_job_id = 1;
        self.next_arrival_seq = 1;
        self.stats = SchedulerStats::default();
    }

    // --- Internal Methods ---

    /// Normalize a weight into the configured clamp range.
    fn normalize_weight(&self, weight: f64) -> f64 {
        if weight.is_nan() {
            return self.config.w_min;
        }
        if weight.is_infinite() {
            return if weight.is_sign_positive() {
                self.config.w_max
            } else {
                self.config.w_min
            };
        }
        weight.clamp(self.config.w_min, self.config.w_max)
    }

    /// Normalize a processing-time estimate into the configured clamp range.
    fn normalize_time(&self, estimate_ms: f64) -> f64 {
        if estimate_ms.is_nan() {
            return self.config.p_max_ms;
        }
        if estimate_ms.is_infinite() {
            return if estimate_ms.is_sign_positive() {
                self.config.p_max_ms
            } else {
                self.config.p_min_ms
            };
        }
        estimate_ms.clamp(self.config.p_min_ms, self.config.p_max_ms)
    }

    /// Resolve a weight based on its declared source, then clamp to config limits.
    fn normalize_weight_with_source(&self, weight: f64, source: WeightSource) -> f64 {
        let resolved = match source {
            WeightSource::Explicit => weight,
            WeightSource::Default => self.config.weight_default,
            WeightSource::Unknown => self.config.weight_unknown,
        };
        self.normalize_weight(resolved)
    }

    /// Resolve an estimate based on its declared source, then clamp to config limits.
    fn normalize_time_with_source(&self, estimate_ms: f64, source: EstimateSource) -> f64 {
        let resolved = match source {
            EstimateSource::Explicit | EstimateSource::Historical => estimate_ms,
            EstimateSource::Default => self.config.estimate_default_ms,
            EstimateSource::Unknown => self.config.estimate_unknown_ms,
        };
        self.normalize_time(resolved)
    }

    /// Compute base ratio (w/p) for Smith's rule.
    fn compute_base_ratio(&self, job: &Job) -> f64 {
        if self.config.mode() == SchedulingMode::Fifo {
            return 0.0;
        }
        let remaining = job.remaining_time.max(self.config.p_min_ms);
        let weight = match self.config.mode() {
            SchedulingMode::Smith => job.weight,
            SchedulingMode::Srpt => 1.0,
            SchedulingMode::Fifo => 0.0,
        };
        weight / remaining
    }

    /// Compute the scheduling objective terms.
    ///
    /// We maximize:
    /// `priority = base_ratio + aging_reward`, then apply starvation floor.
    ///
    /// The equivalent minimized quantity is:
    /// `loss_proxy = 1 / max(priority, w_min)`.
    fn compute_priority_terms(&self, job: &Job) -> PriorityTerms {
        if self.config.mode() == SchedulingMode::Fifo {
            return PriorityTerms {
                aging_reward: 0.0,
                starvation_floor: 0.0,
                effective_priority: 0.0,
            };
        }

        let base_ratio = self.compute_base_ratio(job);
        let wait_time = (self.current_time - job.arrival_time).max(0.0);
        let aging_reward = self.config.aging_factor * wait_time;
        let starvation_floor =
            if self.config.wait_starve_ms > 0.0 && wait_time >= self.config.wait_starve_ms {
                base_ratio * self.config.starve_boost_ratio
            } else {
                0.0
            };

        let effective_priority = (base_ratio + aging_reward).max(starvation_floor);

        PriorityTerms {
            aging_reward,
            starvation_floor,
            effective_priority,
        }
    }

    /// Compute effective priority (base ratio + aging, with starvation guard).
    fn compute_priority(&self, job: &Job) -> f64 {
        self.compute_priority_terms(job).effective_priority
    }

    /// Build a priority-queue entry for a job.
    fn make_priority_job(&self, job: Job) -> PriorityJob {
        let base_ratio = self.compute_base_ratio(&job);
        let priority = self.compute_priority(&job);
        PriorityJob {
            priority,
            base_ratio,
            job,
            mode: self.config.mode(),
        }
    }

    /// Determine the tie-break reason between two candidates.
    fn tie_break_reason(&self, a: &PriorityJob, b: &PriorityJob) -> TieBreakReason {
        if self.config.mode() == SchedulingMode::Fifo {
            if a.job.arrival_seq != b.job.arrival_seq {
                return TieBreakReason::ArrivalSeq;
            }
            return TieBreakReason::JobId;
        }
        if a.priority.total_cmp(&b.priority) != Ordering::Equal {
            TieBreakReason::EffectivePriority
        } else if a.base_ratio.total_cmp(&b.base_ratio) != Ordering::Equal {
            TieBreakReason::BaseRatio
        } else if a.job.weight.total_cmp(&b.job.weight) != Ordering::Equal {
            TieBreakReason::Weight
        } else if a.job.remaining_time.total_cmp(&b.job.remaining_time) != Ordering::Equal {
            TieBreakReason::RemainingTime
        } else if a.job.arrival_seq != b.job.arrival_seq {
            TieBreakReason::ArrivalSeq
        } else {
            TieBreakReason::JobId
        }
    }

    /// Check if current job should be preempted.
    fn maybe_preempt(&mut self) {
        if self.config.mode() == SchedulingMode::Fifo {
            return;
        }
        if let Some(ref current) = self.current_job
            && let Some(pj) = self.queue.peek()
        {
            let current_pj = self.make_priority_job(current.clone());
            if pj.cmp(&current_pj) == Ordering::Greater {
                // Preempt
                let old = self
                    .current_job
                    .take()
                    .expect("current_job guaranteed by if-let guard");
                let priority_job = self.make_priority_job(old);
                self.queue.push(priority_job);
                self.stats.total_preemptions += 1;
            }
        }
    }

    /// Refresh priorities for all queued jobs (aging effect).
    fn refresh_priorities(&mut self) {
        let jobs: Vec<_> = self.queue.drain().map(|pj| pj.job).collect();
        for job in jobs {
            let priority_job = self.make_priority_job(job);
            self.queue.push(priority_job);
        }
    }

    /// Compute wait time statistics.
    fn compute_wait_stats(&self) -> (f64, f64) {
        let mut total_wait = 0.0;
        let mut max_wait = 0.0f64;
        let mut count = 0;

        for pj in self.queue.iter() {
            let wait = (self.current_time - pj.job.arrival_time).max(0.0);
            total_wait += wait;
            max_wait = max_wait.max(wait);
            count += 1;
        }

        if let Some(ref j) = self.current_job {
            let wait = (self.current_time - j.arrival_time).max(0.0);
            total_wait += wait;
            max_wait = max_wait.max(wait);
            count += 1;
        }

        let mean = if count > 0 {
            total_wait / count as f64
        } else {
            0.0
        };
        (mean, max_wait)
    }
}

// =============================================================================
// Unit Tests (bd-13pq.7)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn test_config() -> SchedulerConfig {
        SchedulerConfig {
            aging_factor: 0.001,
            p_min_ms: DEFAULT_P_MIN_MS,
            p_max_ms: DEFAULT_P_MAX_MS,
            estimate_default_ms: DEFAULT_ESTIMATE_DEFAULT_MS,
            estimate_unknown_ms: DEFAULT_ESTIMATE_UNKNOWN_MS,
            w_min: DEFAULT_W_MIN,
            w_max: DEFAULT_W_MAX,
            weight_default: DEFAULT_WEIGHT_DEFAULT,
            weight_unknown: DEFAULT_WEIGHT_UNKNOWN,
            wait_starve_ms: DEFAULT_WAIT_STARVE_MS,
            starve_boost_ratio: DEFAULT_STARVE_BOOST_RATIO,
            smith_enabled: true,
            force_fifo: false,
            max_queue_size: 100,
            preemptive: true,
            time_quantum: 10.0,
            enable_logging: false,
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct WorkloadJob {
        arrival: u64,
        weight: f64,
        duration: f64,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum SimPolicy {
        Smith,
        Fifo,
    }

    #[derive(Debug)]
    struct SimulationMetrics {
        mean: f64,
        p95: f64,
        p99: f64,
        max: f64,
        job_count: usize,
        completion_order: Vec<u64>,
    }

    fn mixed_workload() -> Vec<WorkloadJob> {
        let mut jobs = Vec::new();
        jobs.push(WorkloadJob {
            arrival: 0,
            weight: 1.0,
            duration: 100.0,
        });
        for t in 1..=200u64 {
            jobs.push(WorkloadJob {
                arrival: t,
                weight: 1.0,
                duration: 1.0,
            });
        }
        jobs
    }

    fn percentile(sorted: &[f64], p: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let idx = ((sorted.len() as f64 - 1.0) * p).ceil() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn summary_json(policy: SimPolicy, metrics: &SimulationMetrics) -> String {
        let policy = match policy {
            SimPolicy::Smith => "Smith",
            SimPolicy::Fifo => "Fifo",
        };
        let head: Vec<String> = metrics
            .completion_order
            .iter()
            .take(8)
            .map(|id| id.to_string())
            .collect();
        let tail: Vec<String> = metrics
            .completion_order
            .iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|id| id.to_string())
            .collect();
        format!(
            "{{\"policy\":\"{policy}\",\"jobs\":{jobs},\"mean\":{mean:.3},\"p95\":{p95:.3},\"p99\":{p99:.3},\"max\":{max:.3},\"order_head\":[{head}],\"order_tail\":[{tail}]}}",
            policy = policy,
            jobs = metrics.job_count,
            mean = metrics.mean,
            p95 = metrics.p95,
            p99 = metrics.p99,
            max = metrics.max,
            head = head.join(","),
            tail = tail.join(",")
        )
    }

    fn workload_summary_json(workload: &[WorkloadJob]) -> String {
        if workload.is_empty() {
            return "{\"workload\":\"empty\"}".to_string();
        }
        let mut min_arrival = u64::MAX;
        let mut max_arrival = 0u64;
        let mut min_duration = f64::INFINITY;
        let mut max_duration: f64 = 0.0;
        let mut total_work: f64 = 0.0;
        let mut long_jobs = 0usize;
        let long_threshold = 10.0;

        for job in workload {
            min_arrival = min_arrival.min(job.arrival);
            max_arrival = max_arrival.max(job.arrival);
            min_duration = min_duration.min(job.duration);
            max_duration = max_duration.max(job.duration);
            total_work += job.duration;
            if job.duration >= long_threshold {
                long_jobs += 1;
            }
        }

        format!(
            "{{\"workload\":\"mixed\",\"jobs\":{jobs},\"arrival_min\":{arrival_min},\"arrival_max\":{arrival_max},\"duration_min\":{duration_min:.3},\"duration_max\":{duration_max:.3},\"total_work\":{total_work:.3},\"long_jobs\":{long_jobs},\"long_threshold\":{long_threshold:.1}}}",
            jobs = workload.len(),
            arrival_min = min_arrival,
            arrival_max = max_arrival,
            duration_min = min_duration,
            duration_max = max_duration,
            total_work = total_work,
            long_jobs = long_jobs,
            long_threshold = long_threshold
        )
    }

    fn simulate_policy(policy: SimPolicy, workload: &[WorkloadJob]) -> SimulationMetrics {
        let mut config = test_config();
        config.aging_factor = 0.0;
        config.wait_starve_ms = 0.0;
        config.starve_boost_ratio = 1.0;
        config.smith_enabled = policy == SimPolicy::Smith;
        config.force_fifo = policy == SimPolicy::Fifo;
        config.preemptive = true;

        let mut scheduler = QueueingScheduler::new(config);
        let mut arrivals = workload.to_vec();
        arrivals.sort_by_key(|job| job.arrival);

        let mut arrival_times: HashMap<u64, f64> = HashMap::new();
        let mut response_times = Vec::with_capacity(arrivals.len());
        let mut completion_order = Vec::with_capacity(arrivals.len());

        let mut idx = 0usize;
        let mut safety = 0usize;

        while (idx < arrivals.len() || scheduler.peek_next().is_some()) && safety < 10_000 {
            let now = scheduler.current_time;

            while idx < arrivals.len() && (arrivals[idx].arrival as f64) <= now + f64::EPSILON {
                let job = arrivals[idx];
                let id = scheduler
                    .submit(job.weight, job.duration)
                    .expect("queue capacity should not be exceeded");
                arrival_times.insert(id, scheduler.current_time);
                idx += 1;
            }

            if scheduler.peek_next().is_none() {
                if idx < arrivals.len() {
                    let next_time = arrivals[idx].arrival as f64;
                    let delta = (next_time - scheduler.current_time).max(0.0);
                    let completed = scheduler.tick(delta);
                    for id in completed {
                        let arrival = arrival_times.get(&id).copied().unwrap_or(0.0);
                        response_times.push(scheduler.current_time - arrival);
                        completion_order.push(id);
                    }
                }
                safety += 1;
                continue;
            }

            let completed = scheduler.tick(1.0);
            for id in completed {
                let arrival = arrival_times.get(&id).copied().unwrap_or(0.0);
                response_times.push(scheduler.current_time - arrival);
                completion_order.push(id);
            }
            safety += 1;
        }

        assert_eq!(
            response_times.len(),
            arrivals.len(),
            "simulation did not complete all jobs"
        );

        let mut sorted = response_times.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let mean = response_times.iter().sum::<f64>() / response_times.len() as f64;
        let p95 = percentile(&sorted, 0.95);
        let p99 = percentile(&sorted, 0.99);
        let max = *sorted.last().unwrap_or(&0.0);

        SimulationMetrics {
            mean,
            p95,
            p99,
            max,
            job_count: response_times.len(),
            completion_order,
        }
    }

    // =========================================================================
    // Initialization tests
    // =========================================================================

    #[test]
    fn new_creates_empty_scheduler() {
        let scheduler = QueueingScheduler::new(test_config());
        assert_eq!(scheduler.stats().queue_length, 0);
        assert!(scheduler.peek_next().is_none());
    }

    #[test]
    fn default_config_valid() {
        let config = SchedulerConfig::default();
        let scheduler = QueueingScheduler::new(config);
        assert_eq!(scheduler.stats().queue_length, 0);
    }

    // =========================================================================
    // Job submission tests
    // =========================================================================

    #[test]
    fn submit_returns_job_id() {
        let mut scheduler = QueueingScheduler::new(test_config());
        let id = scheduler.submit(1.0, 10.0);
        assert_eq!(id, Some(1));
    }

    #[test]
    fn submit_increments_job_id() {
        let mut scheduler = QueueingScheduler::new(test_config());
        let id1 = scheduler.submit(1.0, 10.0);
        let id2 = scheduler.submit(1.0, 10.0);
        assert_eq!(id1, Some(1));
        assert_eq!(id2, Some(2));
    }

    #[test]
    fn submit_rejects_when_queue_full() {
        let mut config = test_config();
        config.max_queue_size = 2;
        let mut scheduler = QueueingScheduler::new(config);

        assert!(scheduler.submit(1.0, 10.0).is_some());
        assert!(scheduler.submit(1.0, 10.0).is_some());
        assert!(scheduler.submit(1.0, 10.0).is_none()); // Rejected
        assert_eq!(scheduler.stats().total_rejected, 1);
    }

    #[test]
    fn submit_named_job() {
        let mut scheduler = QueueingScheduler::new(test_config());
        let id = scheduler.submit_named(1.0, 10.0, Some("test-job"));
        assert!(id.is_some());
    }

    // =========================================================================
    // SRPT ordering tests
    // =========================================================================

    #[test]
    fn srpt_prefers_shorter_jobs() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 100.0); // Long job
        scheduler.submit(1.0, 10.0); // Short job

        let next = scheduler.peek_next().unwrap();
        assert_eq!(next.remaining_time, 10.0); // Short job selected
    }

    #[test]
    fn smith_rule_prefers_high_weight() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 10.0); // Low weight
        scheduler.submit(10.0, 10.0); // High weight

        let next = scheduler.peek_next().unwrap();
        assert_eq!(next.weight, 10.0); // High weight selected
    }

    #[test]
    fn smith_rule_balances_weight_and_time() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(2.0, 20.0); // priority = 2/20 = 0.1
        scheduler.submit(1.0, 5.0); // priority = 1/5 = 0.2

        let next = scheduler.peek_next().unwrap();
        assert_eq!(next.remaining_time, 5.0); // Higher priority
    }

    // =========================================================================
    // Aging tests
    // =========================================================================

    #[test]
    fn aging_increases_priority_over_time() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 100.0); // Long job
        scheduler.tick(0.0); // Process nothing, just advance

        let before_aging = scheduler.compute_priority(scheduler.peek_next().unwrap());

        scheduler.current_time = 100.0; // Advance time significantly
        scheduler.refresh_priorities();

        let after_aging = scheduler.compute_priority(scheduler.peek_next().unwrap());
        assert!(
            after_aging > before_aging,
            "Priority should increase with wait time"
        );
    }

    #[test]
    fn aging_prevents_starvation() {
        let mut config = test_config();
        config.aging_factor = 1.0; // High aging
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(1.0, 1000.0); // Very long job
        scheduler.submit(1.0, 1.0); // Short job

        // Initially, short job should be preferred
        assert_eq!(scheduler.peek_next().unwrap().remaining_time, 1.0);

        // After the short job completes, long job should eventually run
        let completed = scheduler.tick(1.0);
        assert_eq!(completed.len(), 1);

        assert!(scheduler.peek_next().is_some());
    }

    // =========================================================================
    // Preemption tests
    // =========================================================================

    #[test]
    fn preemption_when_higher_priority_arrives() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 100.0); // Start processing long job
        scheduler.tick(10.0); // Process 10 units

        let before = scheduler.peek_next().unwrap().remaining_time;
        assert_eq!(before, 90.0);

        scheduler.submit(1.0, 5.0); // Higher priority arrives

        // Should now be processing the short job
        let next = scheduler.peek_next().unwrap();
        assert_eq!(next.remaining_time, 5.0);

        // Stats should show preemption
        assert_eq!(scheduler.stats().total_preemptions, 1);
    }

    #[test]
    fn no_preemption_when_disabled() {
        let mut config = test_config();
        config.preemptive = false;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(1.0, 100.0);
        scheduler.tick(10.0);

        scheduler.submit(1.0, 5.0); // Would preempt if enabled

        // Should still be processing the first job
        let next = scheduler.peek_next().unwrap();
        assert_eq!(next.remaining_time, 90.0);
    }

    // =========================================================================
    // Processing tests
    // =========================================================================

    #[test]
    fn tick_processes_jobs() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 10.0);
        let completed = scheduler.tick(5.0);

        assert!(completed.is_empty()); // Not complete yet
        assert_eq!(scheduler.peek_next().unwrap().remaining_time, 5.0);
    }

    #[test]
    fn tick_completes_jobs() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 10.0);
        let completed = scheduler.tick(10.0);

        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0], 1);
        assert!(scheduler.peek_next().is_none());
    }

    #[test]
    fn tick_completes_multiple_jobs() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 5.0);
        scheduler.submit(1.0, 5.0);
        let completed = scheduler.tick(10.0);

        assert_eq!(completed.len(), 2);
    }

    #[test]
    fn tick_handles_zero_delta() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 10.0);
        let completed = scheduler.tick(0.0);
        assert!(completed.is_empty());
    }

    // =========================================================================
    // Statistics tests
    // =========================================================================

    #[test]
    fn stats_track_submissions() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 10.0);
        scheduler.submit(1.0, 10.0);

        let stats = scheduler.stats();
        assert_eq!(stats.total_submitted, 2);
        assert_eq!(stats.queue_length, 2);
    }

    #[test]
    fn stats_track_completions() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 10.0);
        scheduler.tick(10.0);

        let stats = scheduler.stats();
        assert_eq!(stats.total_completed, 1);
    }

    #[test]
    fn stats_compute_mean_response_time() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 10.0);
        scheduler.submit(1.0, 10.0);
        scheduler.tick(20.0);

        let stats = scheduler.stats();
        // First job: 10 time units, Second job: 20 time units
        // Mean: (10 + 20) / 2 = 15
        assert_eq!(stats.total_completed, 2);
        assert!(stats.mean_response_time() > 0.0);
    }

    #[test]
    fn stats_compute_throughput() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 10.0);
        scheduler.tick(10.0);

        let stats = scheduler.stats();
        // 1 job in 10 time units
        assert!((stats.throughput() - 0.1).abs() < 0.01);
    }

    // =========================================================================
    // Evidence tests
    // =========================================================================

    #[test]
    fn evidence_reports_queue_empty() {
        let scheduler = QueueingScheduler::new(test_config());
        let evidence = scheduler.evidence();
        assert_eq!(evidence.reason, SelectionReason::QueueEmpty);
        assert!(evidence.selected_job_id.is_none());
        assert!(evidence.tie_break_reason.is_none());
        assert!(evidence.jobs.is_empty());
    }

    #[test]
    fn evidence_reports_selected_job() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 10.0);
        let evidence = scheduler.evidence();
        assert_eq!(evidence.selected_job_id, Some(1));
        assert_eq!(evidence.jobs.len(), 1);
    }

    #[test]
    fn evidence_reports_wait_stats() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 100.0);
        scheduler.submit(1.0, 100.0);
        scheduler.current_time = 50.0;
        scheduler.refresh_priorities();

        let evidence = scheduler.evidence();
        assert!(evidence.mean_wait_time > 0.0);
        assert!(evidence.max_wait_time > 0.0);
    }

    #[test]
    fn evidence_reports_priority_objective_terms() {
        let mut config = test_config();
        config.aging_factor = 0.5;
        config.wait_starve_ms = 10.0;
        config.starve_boost_ratio = 2.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(1.0, 20.0);
        scheduler.current_time = 20.0;
        scheduler.refresh_priorities();

        let evidence = scheduler.evidence();
        let job = evidence.jobs.first().expect("job evidence");
        assert!(job.aging_reward > 0.0);
        assert!(job.starvation_floor > 0.0);
        assert!(job.effective_priority >= job.ratio + job.aging_reward);
        assert!(
            (job.objective_loss_proxy - (1.0 / job.effective_priority.max(DEFAULT_W_MIN))).abs()
                < 1e-9
        );
    }

    // =========================================================================
    // Config override tests
    // =========================================================================

    #[test]
    fn force_fifo_overrides_priority() {
        let mut config = test_config();
        config.force_fifo = true;
        let mut scheduler = QueueingScheduler::new(config);

        let first = scheduler.submit(1.0, 100.0).unwrap();
        let second = scheduler.submit(10.0, 1.0).unwrap();

        let next = scheduler.peek_next().unwrap();
        assert_eq!(next.id, first);
        assert_ne!(next.id, second);
        assert_eq!(scheduler.evidence().reason, SelectionReason::Fifo);
    }

    #[test]
    fn default_sources_use_config_values() {
        let mut config = test_config();
        config.weight_default = 7.0;
        config.estimate_default_ms = 12.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit_with_sources(
            999.0,
            999.0,
            WeightSource::Default,
            EstimateSource::Default,
            None::<&str>,
        );

        let next = scheduler.peek_next().unwrap();
        assert!((next.weight - 7.0).abs() < f64::EPSILON);
        assert!((next.remaining_time - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_sources_use_config_values() {
        let mut config = test_config();
        config.weight_unknown = 2.5;
        config.estimate_unknown_ms = 250.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit_with_sources(
            0.0,
            0.0,
            WeightSource::Unknown,
            EstimateSource::Unknown,
            None::<&str>,
        );

        let next = scheduler.peek_next().unwrap();
        assert!((next.weight - 2.5).abs() < f64::EPSILON);
        assert!((next.remaining_time - 250.0).abs() < f64::EPSILON);
    }

    // =========================================================================
    // Tie-break tests
    // =========================================================================

    #[test]
    fn tie_break_prefers_base_ratio_when_effective_equal() {
        let mut config = test_config();
        config.aging_factor = 0.1;
        let mut scheduler = QueueingScheduler::new(config);

        // Job A: lower base ratio but older (aging brings it up).
        let id_a = scheduler.submit(1.0, 2.0).unwrap(); // ratio 0.5
        scheduler.current_time = 5.0;
        scheduler.refresh_priorities();

        // Job B: higher base ratio, newer.
        let id_b = scheduler.submit(1.0, 1.0).unwrap(); // ratio 1.0
        scheduler.refresh_priorities();

        let next = scheduler.peek_next().unwrap();
        assert_eq!(next.id, id_b);

        let evidence = scheduler.evidence();
        assert_eq!(evidence.selected_job_id, Some(id_b));
        assert_eq!(evidence.tie_break_reason, Some(TieBreakReason::BaseRatio));
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn tie_break_prefers_weight_over_arrival() {
        let mut scheduler = QueueingScheduler::new(test_config());

        let high_weight = scheduler.submit(2.0, 2.0).unwrap(); // ratio 1.0
        let _low_weight = scheduler.submit(1.0, 1.0).unwrap(); // ratio 1.0

        let evidence = scheduler.evidence();
        assert_eq!(evidence.selected_job_id, Some(high_weight));
        assert_eq!(evidence.tie_break_reason, Some(TieBreakReason::Weight));
    }

    #[test]
    fn tie_break_prefers_arrival_seq_when_all_equal() {
        let mut config = test_config();
        config.aging_factor = 0.0;
        let mut scheduler = QueueingScheduler::new(config);

        let first = scheduler.submit(1.0, 10.0).unwrap();
        let second = scheduler.submit(1.0, 10.0).unwrap();

        let evidence = scheduler.evidence();
        assert_eq!(evidence.selected_job_id, Some(first));
        assert_eq!(evidence.tie_break_reason, Some(TieBreakReason::ArrivalSeq));
        assert_ne!(first, second);
    }

    // =========================================================================
    // Ordering + safety edge cases (bd-3e1t.10.4)
    // =========================================================================

    #[test]
    fn srpt_mode_ignores_weights() {
        let mut config = test_config();
        config.smith_enabled = false;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(10.0, 100.0); // High weight, long
        scheduler.submit(1.0, 10.0); // Low weight, short

        let next = scheduler.peek_next().unwrap();
        assert_eq!(next.remaining_time, 10.0);
        assert_eq!(
            scheduler.evidence().reason,
            SelectionReason::ShortestRemaining
        );
    }

    #[test]
    fn fifo_mode_disables_preemption() {
        let mut config = test_config();
        config.force_fifo = true;
        config.preemptive = true;
        let mut scheduler = QueueingScheduler::new(config);

        let first = scheduler.submit(1.0, 100.0).unwrap();
        scheduler.tick(10.0);

        let _later = scheduler.submit(10.0, 1.0).unwrap();
        let next = scheduler.peek_next().unwrap();
        assert_eq!(next.id, first);
    }

    #[test]
    fn explicit_zero_weight_clamps_to_min() {
        let mut config = test_config();
        config.w_min = 0.5;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit_with_sources(
            0.0,
            1.0,
            WeightSource::Explicit,
            EstimateSource::Explicit,
            None::<&str>,
        );

        let next = scheduler.peek_next().unwrap();
        assert!((next.weight - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn explicit_zero_estimate_clamps_to_min() {
        let mut config = test_config();
        config.p_min_ms = 2.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit_with_sources(
            1.0,
            0.0,
            WeightSource::Explicit,
            EstimateSource::Explicit,
            None::<&str>,
        );

        let next = scheduler.peek_next().unwrap();
        assert!((next.remaining_time - 2.0).abs() < f64::EPSILON);
    }

    // =========================================================================
    // Cancel tests
    // =========================================================================

    #[test]
    fn cancel_removes_job() {
        let mut scheduler = QueueingScheduler::new(test_config());
        let id = scheduler.submit(1.0, 10.0).unwrap();

        assert!(scheduler.cancel(id));
        assert!(scheduler.peek_next().is_none());
    }

    #[test]
    fn cancel_returns_false_for_nonexistent() {
        let mut scheduler = QueueingScheduler::new(test_config());
        assert!(!scheduler.cancel(999));
    }

    // =========================================================================
    // Reset tests
    // =========================================================================

    #[test]
    fn reset_clears_all_state() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 10.0);
        scheduler.tick(5.0);

        scheduler.reset();

        assert!(scheduler.peek_next().is_none());
        assert_eq!(scheduler.stats().total_submitted, 0);
        assert_eq!(scheduler.stats().total_completed, 0);
    }

    #[test]
    fn clear_removes_jobs_but_keeps_stats() {
        let mut scheduler = QueueingScheduler::new(test_config());

        scheduler.submit(1.0, 10.0);
        scheduler.clear();

        assert!(scheduler.peek_next().is_none());
        assert_eq!(scheduler.stats().total_submitted, 1); // Stats preserved
    }

    // =========================================================================
    // Job tests
    // =========================================================================

    #[test]
    fn job_progress_increases() {
        let mut job = Job::new(1, 1.0, 100.0);
        assert_eq!(job.progress(), 0.0);

        job.remaining_time = 50.0;
        assert!((job.progress() - 0.5).abs() < 0.01);

        job.remaining_time = 0.0;
        assert_eq!(job.progress(), 1.0);
    }

    #[test]
    fn job_is_complete() {
        let mut job = Job::new(1, 1.0, 10.0);
        assert!(!job.is_complete());

        job.remaining_time = 0.0;
        assert!(job.is_complete());
    }

    // =========================================================================
    // Property tests
    // =========================================================================

    #[test]
    fn property_work_conserving() {
        let mut scheduler = QueueingScheduler::new(test_config());

        // Submit jobs
        for i in 0..10 {
            scheduler.submit(1.0, (i as f64) + 1.0);
        }

        // Process - should never be idle while jobs remain
        let mut total_processed = 0;
        while scheduler.peek_next().is_some() {
            let completed = scheduler.tick(1.0);
            total_processed += completed.len();
        }

        assert_eq!(total_processed, 10);
    }

    #[test]
    fn property_bounded_memory() {
        let mut config = test_config();
        config.max_queue_size = 100;
        let mut scheduler = QueueingScheduler::new(config);

        // Submit many jobs
        for _ in 0..1000 {
            scheduler.submit(1.0, 10.0);
        }

        assert!(scheduler.stats().queue_length <= 100);
    }

    #[test]
    fn property_deterministic() {
        let run = || {
            let mut scheduler = QueueingScheduler::new(test_config());
            let mut completions = Vec::new();

            for i in 0..20 {
                scheduler.submit(((i % 3) + 1) as f64, ((i % 5) + 1) as f64);
            }

            for _ in 0..50 {
                completions.extend(scheduler.tick(1.0));
            }

            completions
        };

        let run1 = run();
        let run2 = run();

        assert_eq!(run1, run2, "Scheduling should be deterministic");
    }

    #[test]
    fn smith_beats_fifo_on_mixed_workload() {
        let workload = mixed_workload();
        let smith = simulate_policy(SimPolicy::Smith, &workload);
        let fifo = simulate_policy(SimPolicy::Fifo, &workload);

        eprintln!("{}", workload_summary_json(&workload));
        eprintln!("{}", summary_json(SimPolicy::Smith, &smith));
        eprintln!("{}", summary_json(SimPolicy::Fifo, &fifo));

        assert!(
            smith.mean < fifo.mean,
            "mean should improve: smith={} fifo={}",
            summary_json(SimPolicy::Smith, &smith),
            summary_json(SimPolicy::Fifo, &fifo)
        );
        assert!(
            smith.p95 < fifo.p95,
            "p95 should improve: smith={} fifo={}",
            summary_json(SimPolicy::Smith, &smith),
            summary_json(SimPolicy::Fifo, &fifo)
        );
        assert!(
            smith.p99 < fifo.p99,
            "p99 should improve: smith={} fifo={}",
            summary_json(SimPolicy::Smith, &smith),
            summary_json(SimPolicy::Fifo, &fifo)
        );
    }

    #[test]
    fn simulation_is_deterministic_per_policy() {
        let workload = mixed_workload();
        let smith1 = simulate_policy(SimPolicy::Smith, &workload);
        let smith2 = simulate_policy(SimPolicy::Smith, &workload);
        let fifo1 = simulate_policy(SimPolicy::Fifo, &workload);
        let fifo2 = simulate_policy(SimPolicy::Fifo, &workload);

        assert_eq!(smith1.completion_order, smith2.completion_order);
        assert_eq!(fifo1.completion_order, fifo2.completion_order);
        assert!((smith1.mean - smith2.mean).abs() < 1e-9);
        assert!((fifo1.mean - fifo2.mean).abs() < 1e-9);
    }

    #[test]
    fn effect_queue_trace_is_deterministic() {
        let mut config = test_config();
        config.preemptive = false;
        config.aging_factor = 0.0;
        config.wait_starve_ms = 0.0;
        config.force_fifo = false;
        config.smith_enabled = true;

        let mut scheduler = QueueingScheduler::new(config);
        let id_alpha = scheduler
            .submit_with_sources(
                1.0,
                8.0,
                WeightSource::Explicit,
                EstimateSource::Explicit,
                Some("alpha"),
            )
            .expect("alpha accepted");
        let id_beta = scheduler
            .submit_with_sources(
                4.0,
                2.0,
                WeightSource::Explicit,
                EstimateSource::Explicit,
                Some("beta"),
            )
            .expect("beta accepted");
        let id_gamma = scheduler
            .submit_with_sources(
                2.0,
                10.0,
                WeightSource::Explicit,
                EstimateSource::Explicit,
                Some("gamma"),
            )
            .expect("gamma accepted");
        let id_delta = scheduler
            .submit_with_sources(
                3.0,
                3.0,
                WeightSource::Explicit,
                EstimateSource::Explicit,
                Some("delta"),
            )
            .expect("delta accepted");

        scheduler.refresh_priorities();

        let mut selected = Vec::new();
        while let Some(job) = scheduler.peek_next().cloned() {
            let evidence = scheduler.evidence();
            if let Some(id) = evidence.selected_job_id {
                selected.push(id);
            }
            println!("{}", evidence.to_jsonl("effect_queue_select"));

            let completed = scheduler.tick(job.remaining_time);
            assert!(
                !completed.is_empty(),
                "expected completion per tick in non-preemptive mode"
            );
        }

        assert_eq!(selected, vec![id_beta, id_delta, id_gamma, id_alpha]);
    }

    // =========================================================================
    // Edge case tests
    // =========================================================================

    #[test]
    fn zero_weight_handled() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(0.0, 10.0);
        assert!(scheduler.peek_next().is_some());
    }

    #[test]
    fn zero_time_completes_immediately() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 0.0);
        let completed = scheduler.tick(1.0);
        assert_eq!(completed.len(), 1);
    }

    #[test]
    fn negative_time_handled() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, -10.0);
        let completed = scheduler.tick(1.0);
        assert_eq!(completed.len(), 1);
    }

    // =========================================================================
    // Job builder edge cases (bd-2x2ys)
    // =========================================================================

    #[test]
    fn job_new_nan_weight_clamps_to_min() {
        let job = Job::new(1, f64::NAN, 10.0);
        assert_eq!(job.weight, DEFAULT_W_MIN);
    }

    #[test]
    fn job_new_pos_inf_weight_clamps_to_max() {
        let job = Job::new(1, f64::INFINITY, 10.0);
        assert_eq!(job.weight, DEFAULT_W_MAX);
    }

    #[test]
    fn job_new_neg_inf_weight_clamps_to_min() {
        let job = Job::new(1, f64::NEG_INFINITY, 10.0);
        assert_eq!(job.weight, DEFAULT_W_MIN);
    }

    #[test]
    fn job_new_nan_estimate_clamps_to_max() {
        let job = Job::new(1, 1.0, f64::NAN);
        assert_eq!(job.remaining_time, DEFAULT_P_MAX_MS);
        assert_eq!(job.total_time, DEFAULT_P_MAX_MS);
    }

    #[test]
    fn job_new_pos_inf_estimate_clamps_to_max() {
        let job = Job::new(1, 1.0, f64::INFINITY);
        assert_eq!(job.remaining_time, DEFAULT_P_MAX_MS);
    }

    #[test]
    fn job_new_neg_inf_estimate_clamps_to_min() {
        let job = Job::new(1, 1.0, f64::NEG_INFINITY);
        assert_eq!(job.remaining_time, DEFAULT_P_MIN_MS);
    }

    #[test]
    fn job_with_name_sets_name() {
        let job = Job::with_name(1, 1.0, 10.0, "alpha");
        assert_eq!(job.name.as_deref(), Some("alpha"));
        assert_eq!(job.id, 1);
    }

    #[test]
    fn job_with_sources_sets_both() {
        let job =
            Job::new(1, 1.0, 10.0).with_sources(WeightSource::Unknown, EstimateSource::Historical);
        assert_eq!(job.weight_source, WeightSource::Unknown);
        assert_eq!(job.estimate_source, EstimateSource::Historical);
    }

    #[test]
    fn job_progress_zero_total_time() {
        let mut job = Job::new(1, 1.0, 10.0);
        job.total_time = 0.0;
        assert_eq!(job.progress(), 1.0);
    }

    #[test]
    fn job_is_complete_negative_remaining() {
        let mut job = Job::new(1, 1.0, 10.0);
        job.remaining_time = -5.0;
        assert!(job.is_complete());
    }

    // =========================================================================
    // Scheduler normalization edge cases (bd-2x2ys)
    // =========================================================================

    #[test]
    fn submit_nan_weight_normalized() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(f64::NAN, 10.0);
        let next = scheduler.peek_next().unwrap();
        assert!(next.weight >= DEFAULT_W_MIN);
        assert!(next.weight.is_finite());
    }

    #[test]
    fn submit_inf_weight_normalized() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(f64::INFINITY, 10.0);
        let next = scheduler.peek_next().unwrap();
        assert!(next.weight <= DEFAULT_W_MAX);
        assert!(next.weight.is_finite());
    }

    #[test]
    fn submit_nan_estimate_normalized() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, f64::NAN);
        let next = scheduler.peek_next().unwrap();
        assert!(next.remaining_time <= DEFAULT_P_MAX_MS);
        assert!(next.remaining_time.is_finite());
    }

    #[test]
    fn submit_inf_estimate_normalized() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, f64::INFINITY);
        let next = scheduler.peek_next().unwrap();
        assert!(next.remaining_time <= DEFAULT_P_MAX_MS);
        assert!(next.remaining_time.is_finite());
    }

    // =========================================================================
    // Config mode tests (bd-2x2ys)
    // =========================================================================

    #[test]
    fn config_mode_smith() {
        let config = SchedulerConfig {
            smith_enabled: true,
            force_fifo: false,
            ..Default::default()
        };
        assert_eq!(config.mode(), SchedulingMode::Smith);
    }

    #[test]
    fn config_mode_srpt() {
        let config = SchedulerConfig {
            smith_enabled: false,
            force_fifo: false,
            ..Default::default()
        };
        assert_eq!(config.mode(), SchedulingMode::Srpt);
    }

    #[test]
    fn config_mode_fifo_overrides_smith() {
        let config = SchedulerConfig {
            smith_enabled: true,
            force_fifo: true,
            ..Default::default()
        };
        assert_eq!(config.mode(), SchedulingMode::Fifo);
    }

    // =========================================================================
    // Starvation guard (bd-2x2ys)
    // =========================================================================

    #[test]
    fn starvation_guard_triggers_after_threshold() {
        let mut config = test_config();
        config.aging_factor = 0.0;
        config.wait_starve_ms = 50.0;
        config.starve_boost_ratio = 5.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(1.0, 100.0); // ratio = 1/100 = 0.01
        scheduler.current_time = 60.0; // Beyond 50ms threshold
        scheduler.refresh_priorities();

        let evidence = scheduler.evidence();
        let job_ev = &evidence.jobs[0];
        // starvation_floor = 0.01 * 5.0 = 0.05
        assert!(
            job_ev.starvation_floor > 0.0,
            "starvation floor should be active: {}",
            job_ev.starvation_floor
        );
        assert!(
            job_ev.effective_priority >= job_ev.starvation_floor,
            "effective priority {} should be >= starvation floor {}",
            job_ev.effective_priority,
            job_ev.starvation_floor
        );
    }

    #[test]
    fn starvation_guard_disabled_when_zero() {
        let mut config = test_config();
        config.aging_factor = 0.0;
        config.wait_starve_ms = 0.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(1.0, 100.0);
        scheduler.current_time = 1000.0;
        scheduler.refresh_priorities();

        let evidence = scheduler.evidence();
        let job_ev = &evidence.jobs[0];
        assert!(
            (job_ev.starvation_floor - 0.0).abs() < f64::EPSILON,
            "starvation floor should be 0 when disabled"
        );
    }

    // =========================================================================
    // Cancel edge cases (bd-2x2ys)
    // =========================================================================

    #[test]
    fn cancel_current_job() {
        let mut scheduler = QueueingScheduler::new(test_config());
        let id = scheduler.submit(1.0, 100.0).unwrap();
        scheduler.tick(10.0); // Start processing (moves job to current_job)

        assert!(scheduler.cancel(id));
        assert!(scheduler.peek_next().is_none());
    }

    #[test]
    fn cancel_from_middle_of_queue() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 100.0); // id=1
        let id2 = scheduler.submit(1.0, 50.0).unwrap(); // id=2
        scheduler.submit(1.0, 200.0); // id=3

        assert!(scheduler.cancel(id2));
        assert_eq!(scheduler.stats().queue_length, 2);
    }

    // =========================================================================
    // Tick edge cases (bd-2x2ys)
    // =========================================================================

    #[test]
    fn tick_negative_delta_returns_empty() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 10.0);
        let completed = scheduler.tick(-5.0);
        assert!(completed.is_empty());
    }

    #[test]
    fn tick_empty_queue_advances_time() {
        let mut scheduler = QueueingScheduler::new(test_config());
        let completed = scheduler.tick(100.0);
        assert!(completed.is_empty());
    }

    #[test]
    fn tick_processes_across_multiple_jobs_in_single_delta() {
        let mut config = test_config();
        config.aging_factor = 0.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(1.0, 3.0);
        scheduler.submit(1.0, 3.0);
        scheduler.submit(1.0, 3.0);

        // 9 units of work should complete all 3
        let completed = scheduler.tick(9.0);
        assert_eq!(completed.len(), 3);
    }

    // =========================================================================
    // Stats edge cases (bd-2x2ys)
    // =========================================================================

    #[test]
    fn stats_default_values() {
        let stats = SchedulerStats::default();
        assert_eq!(stats.total_submitted, 0);
        assert_eq!(stats.total_completed, 0);
        assert_eq!(stats.total_rejected, 0);
        assert_eq!(stats.total_preemptions, 0);
        assert_eq!(stats.queue_length, 0);
    }

    #[test]
    fn stats_mean_response_time_zero_completions() {
        let stats = SchedulerStats::default();
        assert_eq!(stats.mean_response_time(), 0.0);
    }

    #[test]
    fn stats_throughput_zero_processing_time() {
        let stats = SchedulerStats::default();
        assert_eq!(stats.throughput(), 0.0);
    }

    #[test]
    fn stats_max_response_time_tracked() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 5.0);
        scheduler.submit(1.0, 10.0);
        scheduler.tick(15.0);

        let stats = scheduler.stats();
        assert!(
            stats.max_response_time >= 10.0,
            "max response time {} should be >= 10",
            stats.max_response_time
        );
    }

    // =========================================================================
    // Evidence / JSONL edge cases (bd-2x2ys)
    // =========================================================================

    #[test]
    fn evidence_continuation_reason() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 100.0);
        scheduler.tick(10.0); // Partially process, sets current_job

        let evidence = scheduler.evidence();
        assert_eq!(evidence.reason, SelectionReason::Continuation);
    }

    #[test]
    fn evidence_single_job_no_tie_break() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 10.0);

        let evidence = scheduler.evidence();
        assert!(
            evidence.tie_break_reason.is_none(),
            "single job should have no tie break"
        );
    }

    #[test]
    fn evidence_to_jsonl_contains_required_fields() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 10.0);
        scheduler.submit(2.0, 5.0);

        let evidence = scheduler.evidence();
        let json = evidence.to_jsonl("test_event");

        assert!(json.contains("\"event\":\"test_event\""));
        assert!(json.contains("\"current_time\":"));
        assert!(json.contains("\"selected_job_id\":"));
        assert!(json.contains("\"queue_length\":"));
        assert!(json.contains("\"mean_wait_time\":"));
        assert!(json.contains("\"max_wait_time\":"));
        assert!(json.contains("\"reason\":"));
        assert!(json.contains("\"tie_break_reason\":"));
        assert!(json.contains("\"jobs\":["));
    }

    #[test]
    fn evidence_to_jsonl_empty_queue() {
        let scheduler = QueueingScheduler::new(test_config());
        let evidence = scheduler.evidence();
        let json = evidence.to_jsonl("empty");

        assert!(json.contains("\"selected_job_id\":null"));
        assert!(json.contains("\"tie_break_reason\":null"));
        assert!(json.contains("\"jobs\":[]"));
    }

    #[test]
    fn job_evidence_to_json_contains_all_fields() {
        let mut config = test_config();
        config.aging_factor = 0.5;
        config.wait_starve_ms = 5.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit_with_sources(
            2.0,
            10.0,
            WeightSource::Explicit,
            EstimateSource::Default,
            Some("test-job"),
        );
        scheduler.current_time = 10.0;
        scheduler.refresh_priorities();

        let evidence = scheduler.evidence();
        let json = evidence.to_jsonl("detail");

        assert!(json.contains("\"job_id\":"));
        assert!(json.contains("\"name\":\"test-job\""));
        assert!(json.contains("\"estimate_ms\":"));
        assert!(json.contains("\"weight\":"));
        assert!(json.contains("\"ratio\":"));
        assert!(json.contains("\"aging_reward\":"));
        assert!(json.contains("\"starvation_floor\":"));
        assert!(json.contains("\"age_ms\":"));
        assert!(json.contains("\"effective_priority\":"));
        assert!(json.contains("\"objective_loss_proxy\":"));
        assert!(json.contains("\"estimate_source\":"));
        assert!(json.contains("\"weight_source\":"));
    }

    #[test]
    fn evidence_jsonl_escapes_special_chars_in_name() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit_named(1.0, 10.0, Some("job\"with\\special\nchars"));

        let evidence = scheduler.evidence();
        let json = evidence.to_jsonl("escape_test");

        assert!(json.contains("\\\""));
        assert!(json.contains("\\\\"));
        assert!(json.contains("\\n"));
    }

    // =========================================================================
    // as_str coverage (bd-2x2ys)
    // =========================================================================

    #[test]
    fn selection_reason_as_str_coverage() {
        assert_eq!(SelectionReason::QueueEmpty.as_str(), "queue_empty");
        assert_eq!(
            SelectionReason::ShortestRemaining.as_str(),
            "shortest_remaining"
        );
        assert_eq!(
            SelectionReason::HighestWeightedPriority.as_str(),
            "highest_weighted_priority"
        );
        assert_eq!(SelectionReason::Fifo.as_str(), "fifo");
        assert_eq!(SelectionReason::AgingBoost.as_str(), "aging_boost");
        assert_eq!(SelectionReason::Continuation.as_str(), "continuation");
    }

    #[test]
    fn estimate_source_as_str_coverage() {
        assert_eq!(EstimateSource::Explicit.as_str(), "explicit");
        assert_eq!(EstimateSource::Historical.as_str(), "historical");
        assert_eq!(EstimateSource::Default.as_str(), "default");
        assert_eq!(EstimateSource::Unknown.as_str(), "unknown");
    }

    #[test]
    fn weight_source_as_str_coverage() {
        assert_eq!(WeightSource::Explicit.as_str(), "explicit");
        assert_eq!(WeightSource::Default.as_str(), "default");
        assert_eq!(WeightSource::Unknown.as_str(), "unknown");
    }

    #[test]
    fn tie_break_reason_as_str_coverage() {
        assert_eq!(
            TieBreakReason::EffectivePriority.as_str(),
            "effective_priority"
        );
        assert_eq!(TieBreakReason::BaseRatio.as_str(), "base_ratio");
        assert_eq!(TieBreakReason::Weight.as_str(), "weight");
        assert_eq!(TieBreakReason::RemainingTime.as_str(), "remaining_time");
        assert_eq!(TieBreakReason::ArrivalSeq.as_str(), "arrival_seq");
        assert_eq!(TieBreakReason::JobId.as_str(), "job_id");
        assert_eq!(TieBreakReason::Continuation.as_str(), "continuation");
    }

    // =========================================================================
    // Debug formatting (bd-2x2ys)
    // =========================================================================

    #[test]
    fn debug_job() {
        let job = Job::with_name(1, 2.0, 50.0, "render");
        let dbg = format!("{job:?}");
        assert!(dbg.contains("Job"));
        assert!(dbg.contains("render"));
    }

    #[test]
    fn debug_scheduler_config() {
        let config = SchedulerConfig::default();
        let dbg = format!("{config:?}");
        assert!(dbg.contains("SchedulerConfig"));
        assert!(dbg.contains("aging_factor"));
    }

    #[test]
    fn debug_scheduler_stats() {
        let stats = SchedulerStats::default();
        let dbg = format!("{stats:?}");
        assert!(dbg.contains("SchedulerStats"));
    }

    #[test]
    fn debug_scheduling_evidence() {
        let scheduler = QueueingScheduler::new(test_config());
        let evidence = scheduler.evidence();
        let dbg = format!("{evidence:?}");
        assert!(dbg.contains("SchedulingEvidence"));
    }

    #[test]
    fn debug_scheduling_mode() {
        assert!(format!("{:?}", SchedulingMode::Smith).contains("Smith"));
        assert!(format!("{:?}", SchedulingMode::Srpt).contains("Srpt"));
        assert!(format!("{:?}", SchedulingMode::Fifo).contains("Fifo"));
    }

    // =========================================================================
    // Historical estimate source passthrough (bd-2x2ys)
    // =========================================================================

    #[test]
    fn historical_estimate_passes_through() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit_with_sources(
            1.0,
            42.0,
            WeightSource::Explicit,
            EstimateSource::Historical,
            None::<&str>,
        );

        let next = scheduler.peek_next().unwrap();
        assert!((next.remaining_time - 42.0).abs() < f64::EPSILON);
    }

    // =========================================================================
    // Preemption count tracking (bd-2x2ys)
    // =========================================================================

    #[test]
    fn multiple_preemptions_counted() {
        let mut config = test_config();
        config.aging_factor = 0.0; // Disable aging for deterministic preemption
        config.wait_starve_ms = 0.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(1.0, 100.0); // Long job (priority=0.01)
        scheduler.tick(1.0); // Start processing, remaining=99

        scheduler.submit(1.0, 50.0); // Preempt 1 (priority=0.02 > 0.01)
        scheduler.tick(1.0); // Start processing job2, remaining=49

        scheduler.submit(1.0, 10.0); // Preempt 2 (priority=0.1 > 0.02)

        assert!(
            scheduler.stats().total_preemptions >= 2,
            "expected >= 2 preemptions, got {}",
            scheduler.stats().total_preemptions
        );
    }

    // =========================================================================
    // Queue rejection count (bd-2x2ys)
    // =========================================================================

    #[test]
    fn multiple_rejections_counted() {
        let mut config = test_config();
        config.max_queue_size = 1;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(1.0, 10.0); // Accepted
        scheduler.submit(1.0, 10.0); // Rejected
        scheduler.submit(1.0, 10.0); // Rejected

        assert_eq!(scheduler.stats().total_rejected, 2);
    }

    // =========================================================================
    // Reset vs clear distinction (bd-2x2ys)
    // =========================================================================

    #[test]
    fn reset_resets_job_id_sequence() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 10.0); // id=1
        scheduler.submit(1.0, 10.0); // id=2

        scheduler.reset();

        let id = scheduler.submit(1.0, 10.0).unwrap();
        assert_eq!(id, 1, "job id should restart from 1 after reset");
    }

    #[test]
    fn clear_preserves_job_id_sequence() {
        let mut scheduler = QueueingScheduler::new(test_config());
        scheduler.submit(1.0, 10.0); // id=1
        scheduler.submit(1.0, 10.0); // id=2

        scheduler.clear();

        let id = scheduler.submit(1.0, 10.0).unwrap();
        assert_eq!(id, 3, "job id should continue after clear");
    }

    // =========================================================================
    // Evidence aging_boost selection reason (bd-2x2ys)
    // =========================================================================

    #[test]
    fn evidence_aging_boost_reason() {
        let mut config = test_config();
        config.aging_factor = 1.0;
        config.wait_starve_ms = 10.0;
        let mut scheduler = QueueingScheduler::new(config);

        scheduler.submit(1.0, 100.0);
        scheduler.current_time = 100.0;
        scheduler.refresh_priorities();

        let evidence = scheduler.evidence();
        assert_eq!(
            evidence.reason,
            SelectionReason::AgingBoost,
            "long-waiting job should show aging boost reason"
        );
    }
}
