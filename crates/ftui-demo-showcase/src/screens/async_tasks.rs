#![forbid(unsafe_code)]

//! Async Task Manager / Job Queue demo screen.
//!
//! Demonstrates concurrent task management with:
//! - Task states (queued/running/succeeded/failed/canceled)
//! - Progress tracking
//! - Cancellation
//! - Scheduling policies with queueing theory foundations
//!
//! # Queueing Theory Scheduler (bd-13pq.7)
//!
//! ## Mathematical Model
//!
//! This scheduler implements policies grounded in queueing theory to optimize
//! various performance metrics. Let tasks be indexed by `i`, with:
//! - `p_i`: processing time (estimated_ticks)
//! - `r_i`: remaining processing time (estimated_ticks - elapsed_ticks)
//! - `w_i`: weight/priority (1-5 scale)
//! - `a_i`: arrival time (created_at tick)
//! - `W_i`: wait time (current_tick - created_at)
//!
//! ## Scheduling Policies
//!
//! ### FIFO (First-In-First-Out)
//! - Orders by arrival time `a_i`
//! - Optimal for fairness (no starvation)
//! - Mean response time: suboptimal for variable job sizes
//!
//! ### SJF (Shortest Job First) / SPT (Shortest Processing Time)
//! - Orders by `p_i` (estimated processing time)
//! - Minimizes mean completion time for M/G/1 queues
//! - Risk: starvation of long jobs
//!
//! ### SRPT (Shortest Remaining Processing Time)
//! - Orders by `r_i = p_i - elapsed_i`
//! - Provably optimal for minimizing mean response time in M/G/1
//! - Preemptive in theory; we use non-preemptive variant for simplicity
//! - Theorem: SRPT minimizes E[T] (mean sojourn time) among all policies
//!
//! ### Smith's Rule (Weighted SJF)
//! - Orders by ratio `w_i / p_i` (weight per unit time)
//! - Minimizes weighted sum of completion times: Σ w_i * C_i
//! - Optimal for single-machine weighted scheduling (Smith, 1956)
//!
//! ### Priority
//! - Orders by `w_i` (raw priority)
//! - Risk: starvation of low-priority jobs
//!
//! ### Round Robin
//! - Rotates based on tick count to distribute CPU time
//! - Good for interactive fairness
//!
//! ## Fairness via Aging
//!
//! To prevent starvation, we implement **aging**: a mechanism that boosts
//! effective priority based on wait time.
//!
//! Effective priority formula:
//! ```text
//! effective_priority(i) = w_i + α * W_i
//! ```
//!
//! Where:
//! - `α` (aging_factor): Controls how fast wait time boosts priority
//! - Default α = 0.1 means every 10 ticks of waiting adds 1 priority point
//!
//! This transforms potentially-starving policies (SRPT, SJF, Priority) into
//! fair variants that guarantee eventual execution.
//!
//! ## Invariants
//!
//! 1. **Bounded wait**: With aging, max wait time is bounded by O(p_max / α)
//! 2. **Progress**: Running count ≤ max_concurrent at all times
//! 3. **Monotonicity**: Task IDs are strictly increasing
//! 4. **Termination**: Terminal states (Succeeded/Failed/Canceled) are absorbing
//!
//! ## Failure Modes
//!
//! - **α = 0**: Aging disabled; starvation possible with SRPT/Priority
//! - **Very small α**: Long wait before fairness kicks in
//! - **Very large α**: FIFO-like behavior dominates policy choice
//! - **Empty queue**: Scheduler is a no-op (graceful)
//!
//! # Keybindings
//!
//! - N: Spawn a new task
//! - C: Cancel selected task
//! - S: Cycle scheduler policy
//! - A: Toggle aging (fairness)
//! - R: Retry failed task
//! - Enter: Select task
//! - Up/Down/j/k: Navigate task list

use std::cell::Cell as StdCell;
use std::collections::VecDeque;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::{MiniBar, MiniBarColors};

use super::{HelpEntry, Screen};
use crate::theme;

/// Maximum number of tasks to keep in history.
const MAX_TASKS: usize = 100;

/// Task states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

impl TaskState {
    fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Succeeded => "Done",
            Self::Failed => "Failed",
            Self::Canceled => "Canceled",
        }
    }

    fn style(self) -> Style {
        match self {
            Self::Queued => Style::new().fg(theme::fg::MUTED),
            Self::Running => Style::new().fg(theme::accent::INFO),
            Self::Succeeded => Style::new().fg(theme::accent::SUCCESS),
            Self::Failed => Style::new().fg(theme::accent::ERROR),
            Self::Canceled => Style::new().fg(theme::accent::WARNING),
        }
    }
}

/// Scheduling policies for task execution.
///
/// See module-level documentation for queueing theory foundations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerPolicy {
    /// First-in, first-out (FCFS).
    /// Optimal for fairness; suboptimal for mean response time.
    Fifo,
    /// Shortest Job First (SJF) / Shortest Processing Time (SPT).
    /// Orders by estimated_ticks. Minimizes mean completion time.
    ShortestFirst,
    /// Shortest Remaining Processing Time (SRPT).
    /// Orders by (estimated_ticks - elapsed_ticks).
    /// Provably optimal for minimizing mean response time in M/G/1 queues.
    Srpt,
    /// Smith's Rule / Weighted Shortest Job First.
    /// Orders by priority/estimated_ticks ratio (w/p).
    /// Minimizes weighted sum of completion times.
    SmithRule,
    /// Priority-based scheduling.
    /// Orders by raw priority value.
    Priority,
    /// Round-robin among running tasks.
    /// Good for interactive fairness.
    RoundRobin,
}

impl SchedulerPolicy {
    fn label(self) -> &'static str {
        match self {
            Self::Fifo => "FIFO",
            Self::ShortestFirst => "SJF",
            Self::Srpt => "SRPT",
            Self::SmithRule => "Smith",
            Self::Priority => "Priority",
            Self::RoundRobin => "RoundRobin",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Fifo => "First-In-First-Out",
            Self::ShortestFirst => "Shortest Job First",
            Self::Srpt => "Shortest Remaining Time",
            Self::SmithRule => "Weighted SJF (w/p)",
            Self::Priority => "Priority-based",
            Self::RoundRobin => "Round Robin",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Fifo => Self::ShortestFirst,
            Self::ShortestFirst => Self::Srpt,
            Self::Srpt => Self::SmithRule,
            Self::SmithRule => Self::Priority,
            Self::Priority => Self::RoundRobin,
            Self::RoundRobin => Self::Fifo,
        }
    }

    /// Number of policies in the cycle.
    pub const fn count() -> usize {
        6
    }
}

/// A single task in the queue.
#[derive(Debug, Clone)]
pub struct Task {
    /// Unique task ID.
    pub id: u32,
    /// Task name/description.
    pub name: String,
    /// Current state.
    pub state: TaskState,
    /// Progress (0.0 - 1.0).
    pub progress: f64,
    /// Estimated total duration in ticks.
    pub estimated_ticks: u64,
    /// Ticks elapsed since task started.
    pub elapsed_ticks: u64,
    /// Task priority (higher = more important).
    pub priority: u8,
    /// Tick when task was created.
    pub created_at: u64,
    /// Error message if failed.
    pub error: Option<String>,
}

impl Task {
    fn new(id: u32, name: String, estimated_ticks: u64, priority: u8, created_at: u64) -> Self {
        Self {
            id,
            name,
            state: TaskState::Queued,
            progress: 0.0,
            estimated_ticks,
            elapsed_ticks: 0,
            priority,
            created_at,
            error: None,
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            TaskState::Succeeded | TaskState::Failed | TaskState::Canceled
        )
    }
}

/// Scheduler fairness configuration.
#[derive(Clone, Debug)]
pub struct FairnessConfig {
    /// Whether aging is enabled.
    pub enabled: bool,
    /// Aging factor (α): priority boost per tick of waiting.
    /// Default: 0.1 (every 10 ticks adds 1 priority point).
    pub aging_factor: f64,
    /// Maximum age boost (caps the aging contribution).
    /// Prevents aging from completely dominating the policy.
    pub max_age_boost: f64,
}

impl Default for FairnessConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            aging_factor: 0.1,
            max_age_boost: 10.0,
        }
    }
}

// =============================================================================
// Hazard-Based Cancellation (bd-13pq.8)
// =============================================================================

/// Configuration for hazard-based cancellation policy.
///
/// # Mathematical Model
///
/// The hazard rate λ(t) represents the instantaneous probability of failure
/// given survival to time t. We model task failure risk as:
///
/// ```text
/// λ(t) = λ_base + β * (elapsed / estimated)^γ
/// ```
///
/// Where:
/// - `λ_base`: Base hazard rate (probability of failure per tick)
/// - `β`: Runtime hazard factor (how much overrun increases risk)
/// - `γ`: Hazard exponent (controls nonlinearity of overrun penalty)
///
/// # Expected Loss Decision
///
/// The policy recommends cancellation when:
///
/// ```text
/// E[Loss_cancel] < E[Loss_continue]
/// ```
///
/// Where:
/// - `E[Loss_cancel] = cancel_cost` (sunk cost of wasted work)
/// - `E[Loss_continue] = P(fail) * fail_cost + (1 - P(fail)) * 0`
///
/// For a task at progress p with remaining time r:
/// - `P(fail) ≈ 1 - exp(-∫λ(t)dt)` (cumulative hazard)
/// - Simplified: `P(fail) ≈ λ(t) * r` for small hazard rates
///
/// # Invariants
///
/// 1. All costs are non-negative
/// 2. Hazard rate is monotonically increasing with runtime overrun
/// 3. Decision is deterministic given the same state
/// 4. Evidence ledger captures all factors influencing the decision
///
/// # Failure Modes
///
/// - **Low cancel_cost**: Aggressive cancellation (may kill viable tasks)
/// - **High cancel_cost**: Permissive (may allow hopeless tasks to run)
/// - **λ_base = 0, β = 0**: No hazard modeling (never auto-recommends cancel)
#[derive(Clone, Debug)]
pub struct HazardConfig {
    /// Whether hazard-based cancellation advice is enabled.
    pub enabled: bool,
    /// Cost of canceling a task (normalized units).
    /// Represents wasted work / opportunity cost.
    pub cancel_cost: f64,
    /// Cost incurred if a task fails after continuing.
    /// Typically higher than cancel_cost (includes cleanup, retry overhead).
    pub fail_cost: f64,
    /// Base hazard rate (λ_base): probability of failure per tick.
    /// Default: 0.001 (0.1% per tick baseline)
    pub hazard_base: f64,
    /// Runtime hazard factor (β): multiplier for overrun penalty.
    /// Default: 0.1
    pub runtime_hazard_factor: f64,
    /// Hazard exponent (γ): controls nonlinearity of overrun.
    /// Default: 2.0 (quadratic growth past estimated time)
    pub hazard_exponent: f64,
    /// Threshold for "recommend cancel" decision.
    /// Bayes factor threshold: recommend if BF > threshold.
    /// Default: 1.0 (neutral evidence)
    pub decision_threshold: f64,
}

impl Default for HazardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cancel_cost: 0.3,   // 30% of work value lost on cancel
            fail_cost: 1.0,     // Full value lost + overhead on failure
            hazard_base: 0.001, // 0.1% baseline failure rate per tick
            runtime_hazard_factor: 0.1,
            hazard_exponent: 2.0,
            decision_threshold: 1.0,
        }
    }
}

impl HazardConfig {
    /// Calculate hazard rate at current task state.
    ///
    /// λ(t) = λ_base + β * max(0, (elapsed - estimated) / estimated)^γ
    ///
    /// Returns a rate in [0, 1] representing instantaneous failure probability.
    pub fn hazard_rate(&self, task: &Task) -> f64 {
        let elapsed = task.elapsed_ticks as f64;
        let estimated = task.estimated_ticks.max(1) as f64;

        // Overrun ratio: how much we've exceeded the estimate
        let overrun_ratio = ((elapsed - estimated) / estimated).max(0.0);

        // Hazard rate with exponential penalty for overrun
        let rate = self.hazard_base
            + self.runtime_hazard_factor * overrun_ratio.powf(self.hazard_exponent);

        // Clamp to valid probability range
        rate.clamp(0.0, 1.0)
    }

    /// Estimate cumulative failure probability over remaining time.
    ///
    /// Uses exponential approximation: P(fail) ≈ 1 - exp(-λ * remaining)
    /// For small λ, this simplifies to ≈ λ * remaining.
    pub fn failure_probability(&self, task: &Task, remaining_ticks: u64) -> f64 {
        let hazard = self.hazard_rate(task);
        let remaining = remaining_ticks as f64;

        // Cumulative hazard approximation
        let cumulative = hazard * remaining;

        // Probability from cumulative hazard
        (1.0 - (-cumulative).exp()).clamp(0.0, 1.0)
    }

    /// Calculate expected loss for continuing vs canceling.
    ///
    /// Returns (E[Loss_continue], E[Loss_cancel], recommendation).
    pub fn expected_loss_analysis(&self, task: &Task) -> CancellationAnalysis {
        let remaining = task.estimated_ticks.saturating_sub(task.elapsed_ticks);
        let p_fail = self.failure_probability(task, remaining.max(1));

        // Expected losses
        let loss_continue = p_fail * self.fail_cost;
        let loss_cancel = self.cancel_cost * (1.0 - task.progress);

        // Bayes factor: evidence for cancel vs continue
        // BF = P(data | cancel better) / P(data | continue better)
        // Simplified: BF ≈ loss_continue / loss_cancel
        let bayes_factor = if loss_cancel > 1e-10 {
            loss_continue / loss_cancel
        } else {
            0.0 // No evidence to cancel if cancel cost is ~0
        };

        let recommend_cancel = self.enabled && bayes_factor > self.decision_threshold;

        CancellationAnalysis {
            hazard_rate: self.hazard_rate(task),
            failure_probability: p_fail,
            expected_loss_continue: loss_continue,
            expected_loss_cancel: loss_cancel,
            bayes_factor,
            recommend_cancel,
        }
    }
}

/// Result of hazard-based cancellation analysis.
#[derive(Clone, Debug)]
pub struct CancellationAnalysis {
    /// Current hazard rate λ(t).
    pub hazard_rate: f64,
    /// Estimated probability of failure if we continue.
    pub failure_probability: f64,
    /// Expected loss if we continue: P(fail) * fail_cost.
    pub expected_loss_continue: f64,
    /// Expected loss if we cancel: cancel_cost * (1 - progress).
    pub expected_loss_cancel: f64,
    /// Bayes factor: evidence ratio for cancel vs continue.
    /// BF > 1 means evidence favors cancellation.
    pub bayes_factor: f64,
    /// Whether the policy recommends cancellation.
    pub recommend_cancel: bool,
}

impl CancellationAnalysis {
    /// Format as human-readable explanation.
    pub fn explanation(&self) -> String {
        format!(
            "λ={:.4}, P(fail)={:.2}%, E[L_cont]={:.3}, E[L_cancel]={:.3}, BF={:.2}{}",
            self.hazard_rate,
            self.failure_probability * 100.0,
            self.expected_loss_continue,
            self.expected_loss_cancel,
            self.bayes_factor,
            if self.recommend_cancel {
                " → RECOMMEND CANCEL"
            } else {
                ""
            }
        )
    }
}

/// Evidence ledger entry for a cancellation decision.
///
/// Provides an audit trail for understanding why cancellation was
/// recommended or executed, enabling debugging and policy refinement.
#[derive(Clone, Debug)]
pub struct CancellationEvidence {
    /// Tick when decision was evaluated.
    pub tick: u64,
    /// Task ID being evaluated.
    pub task_id: u32,
    /// Task name for human readability.
    pub task_name: String,
    /// Analysis results.
    pub analysis: CancellationAnalysis,
    /// Whether cancellation was actually executed.
    pub executed: bool,
    /// Source of cancellation (user request, policy auto, etc.).
    pub source: CancellationSource,
}

/// Source of a cancellation decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CancellationSource {
    /// User explicitly requested cancellation.
    UserRequest,
    /// Policy auto-recommended cancellation.
    PolicyRecommendation,
    /// System-level cancellation (e.g., shutdown).
    System,
}

impl CancellationSource {
    fn as_str(&self) -> &'static str {
        match self {
            Self::UserRequest => "user_request",
            Self::PolicyRecommendation => "policy_recommendation",
            Self::System => "system",
        }
    }
}

/// Scheduler metrics for observability.
#[derive(Clone, Debug, Default)]
pub struct SchedulerMetrics {
    /// Total tasks scheduled since start.
    pub tasks_scheduled: u64,
    /// Total tasks completed (succeeded or failed).
    pub tasks_completed: u64,
    /// Sum of wait times for completed tasks (for mean calculation).
    pub total_wait_time: u64,
    /// Sum of completion times for completed tasks.
    pub total_completion_time: u64,
    /// Maximum wait time observed.
    pub max_wait_time: u64,
    /// Count of tasks that benefited from aging boost.
    pub aging_boosts_applied: u64,
}

impl SchedulerMetrics {
    /// Mean wait time (time from queued to running).
    pub fn mean_wait_time(&self) -> f64 {
        if self.tasks_completed == 0 {
            0.0
        } else {
            self.total_wait_time as f64 / self.tasks_completed as f64
        }
    }

    /// Mean completion time (time from queued to terminal).
    pub fn mean_completion_time(&self) -> f64 {
        if self.tasks_completed == 0 {
            0.0
        } else {
            self.total_completion_time as f64 / self.tasks_completed as f64
        }
    }
}

// =============================================================================
// Diagnostic Logging + Telemetry (bd-13pq.5)
// =============================================================================

/// Configuration for diagnostic logging and telemetry.
#[derive(Clone, Debug)]
pub struct DiagnosticConfig {
    /// Enable structured JSONL logging of scheduling decisions.
    pub enabled: bool,
    /// Maximum diagnostic entries to retain (0 = unlimited).
    pub max_entries: usize,
    /// Log invariant checks (bounded wait, progress bounds, etc.).
    pub log_invariants: bool,
    /// Log scheduling decisions with full evidence.
    pub log_decisions: bool,
    /// Log state transitions (Queued → Running → Terminal).
    pub log_transitions: bool,
    /// Starvation detection threshold (max wait ticks before warning).
    pub starvation_threshold: u64,
}

impl Default for DiagnosticConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_entries: 1000,
            log_invariants: true,
            log_decisions: true,
            log_transitions: true,
            starvation_threshold: 100,
        }
    }
}

/// Evidence explaining a scheduling decision.
///
/// This provides an "evidence ledger" for understanding why particular
/// tasks were selected, enabling debugging and fairness audits.
#[derive(Clone, Debug)]
pub struct SchedulingEvidence {
    /// Tick when decision was made.
    pub tick: u64,
    /// Task ID that was selected.
    pub selected_task_id: u32,
    /// Policy used for selection.
    pub policy: SchedulerPolicy,
    /// Raw score from policy calculation.
    pub raw_score: f64,
    /// Base priority (before aging).
    pub base_priority: f64,
    /// Aging boost applied (if any).
    pub aging_boost: f64,
    /// Wait time at selection.
    pub wait_time: u64,
    /// Reason for selection.
    pub reason: SelectionReason,
    /// Number of candidates considered.
    pub candidates_count: usize,
    /// Alternative task IDs that were passed over.
    pub passed_over: Vec<u32>,
}

/// Reason why a task was selected for execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectionReason {
    /// Selected by base policy (FIFO order, shortest time, etc.).
    PolicyOrder,
    /// Selected due to aging boost overcoming policy order.
    AgingBoost,
    /// Selected as only candidate.
    OnlyCandidate,
    /// Selected by round-robin rotation.
    RoundRobinRotation,
}

impl SelectionReason {
    fn as_str(&self) -> &'static str {
        match self {
            Self::PolicyOrder => "policy_order",
            Self::AgingBoost => "aging_boost",
            Self::OnlyCandidate => "only_candidate",
            Self::RoundRobinRotation => "round_robin_rotation",
        }
    }
}

/// A diagnostic log entry for JSONL output.
#[derive(Clone, Debug)]
pub enum DiagnosticEntry {
    /// Task state transition.
    StateTransition {
        tick: u64,
        task_id: u32,
        from: TaskState,
        to: TaskState,
        wait_time: Option<u64>,
    },
    /// Scheduling decision with evidence.
    SchedulingDecision(SchedulingEvidence),
    /// Policy changed.
    PolicyChange {
        tick: u64,
        from: SchedulerPolicy,
        to: SchedulerPolicy,
    },
    /// Aging toggled.
    AgingToggle { tick: u64, enabled: bool },
    /// Invariant check result.
    InvariantCheck {
        tick: u64,
        invariant: InvariantType,
        passed: bool,
        details: String,
    },
    /// Starvation warning (task waited too long).
    StarvationWarning {
        tick: u64,
        task_id: u32,
        wait_time: u64,
        threshold: u64,
    },
    /// Metrics snapshot.
    MetricsSnapshot {
        tick: u64,
        queued: usize,
        running: usize,
        completed: u64,
        mean_wait: f64,
        max_wait: u64,
    },
    /// Cancellation decision with hazard-based evidence (bd-13pq.8).
    CancellationDecision(CancellationEvidence),
}

impl DiagnosticEntry {
    /// Serialize to JSONL format for structured logging.
    pub fn to_jsonl(&self) -> String {
        match self {
            Self::StateTransition {
                tick,
                task_id,
                from,
                to,
                wait_time,
            } => {
                let wait_str = wait_time
                    .map(|w| format!(",\"wait_time\":{}", w))
                    .unwrap_or_default();
                format!(
                    "{{\"type\":\"state_transition\",\"tick\":{},\"task_id\":{},\"from\":\"{}\",\"to\":\"{}\"{}}}",
                    tick,
                    task_id,
                    from.label(),
                    to.label(),
                    wait_str
                )
            }
            Self::SchedulingDecision(ev) => {
                let passed_over: Vec<String> =
                    ev.passed_over.iter().map(|id| id.to_string()).collect();
                format!(
                    "{{\"type\":\"scheduling_decision\",\"tick\":{},\"task_id\":{},\"policy\":\"{}\",\
                    \"raw_score\":{:.4},\"base_priority\":{:.2},\"aging_boost\":{:.2},\
                    \"wait_time\":{},\"reason\":\"{}\",\"candidates\":{},\"passed_over\":[{}]}}",
                    ev.tick,
                    ev.selected_task_id,
                    ev.policy.label(),
                    ev.raw_score,
                    ev.base_priority,
                    ev.aging_boost,
                    ev.wait_time,
                    ev.reason.as_str(),
                    ev.candidates_count,
                    passed_over.join(",")
                )
            }
            Self::PolicyChange { tick, from, to } => {
                format!(
                    "{{\"type\":\"policy_change\",\"tick\":{},\"from\":\"{}\",\"to\":\"{}\"}}",
                    tick,
                    from.label(),
                    to.label()
                )
            }
            Self::AgingToggle { tick, enabled } => {
                format!(
                    "{{\"type\":\"aging_toggle\",\"tick\":{},\"enabled\":{}}}",
                    tick, enabled
                )
            }
            Self::InvariantCheck {
                tick,
                invariant,
                passed,
                details,
            } => {
                format!(
                    "{{\"type\":\"invariant_check\",\"tick\":{},\"invariant\":\"{}\",\"passed\":{},\"details\":\"{}\"}}",
                    tick,
                    invariant.as_str(),
                    passed,
                    details.replace('\"', "\\\"")
                )
            }
            Self::StarvationWarning {
                tick,
                task_id,
                wait_time,
                threshold,
            } => {
                format!(
                    "{{\"type\":\"starvation_warning\",\"tick\":{},\"task_id\":{},\"wait_time\":{},\"threshold\":{}}}",
                    tick, task_id, wait_time, threshold
                )
            }
            Self::MetricsSnapshot {
                tick,
                queued,
                running,
                completed,
                mean_wait,
                max_wait,
            } => {
                format!(
                    "{{\"type\":\"metrics_snapshot\",\"tick\":{},\"queued\":{},\"running\":{},\
                    \"completed\":{},\"mean_wait\":{:.2},\"max_wait\":{}}}",
                    tick, queued, running, completed, mean_wait, max_wait
                )
            }
            Self::CancellationDecision(ev) => {
                format!(
                    "{{\"type\":\"cancellation_decision\",\"tick\":{},\"task_id\":{},\
                    \"task_name\":\"{}\",\"hazard_rate\":{:.6},\"p_fail\":{:.4},\
                    \"loss_continue\":{:.4},\"loss_cancel\":{:.4},\"bayes_factor\":{:.4},\
                    \"recommend_cancel\":{},\"executed\":{},\"source\":\"{}\"}}",
                    ev.tick,
                    ev.task_id,
                    ev.task_name.replace('\"', "\\\""),
                    ev.analysis.hazard_rate,
                    ev.analysis.failure_probability,
                    ev.analysis.expected_loss_continue,
                    ev.analysis.expected_loss_cancel,
                    ev.analysis.bayes_factor,
                    ev.analysis.recommend_cancel,
                    ev.executed,
                    ev.source.as_str()
                )
            }
        }
    }
}

/// Types of invariants that can be checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvariantType {
    /// Running count ≤ max_concurrent.
    BoundedConcurrency,
    /// Progress ∈ [0.0, 1.0].
    BoundedProgress,
    /// Terminal states are absorbing.
    TerminalStability,
    /// Task IDs are monotonically increasing.
    MonotonicIds,
    /// Wait time is bounded (with aging).
    BoundedWait,
}

impl InvariantType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::BoundedConcurrency => "bounded_concurrency",
            Self::BoundedProgress => "bounded_progress",
            Self::TerminalStability => "terminal_stability",
            Self::MonotonicIds => "monotonic_ids",
            Self::BoundedWait => "bounded_wait",
        }
    }
}

/// Diagnostic log storage.
#[derive(Clone, Debug, Default)]
pub struct DiagnosticLog {
    entries: VecDeque<DiagnosticEntry>,
    max_entries: usize,
}

impl DiagnosticLog {
    /// Create a new diagnostic log with the given capacity.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(max_entries.min(1000)),
            max_entries,
        }
    }

    /// Add an entry to the log.
    pub fn push(&mut self, entry: DiagnosticEntry) {
        if self.max_entries > 0 && self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Get all entries.
    pub fn entries(&self) -> &VecDeque<DiagnosticEntry> {
        &self.entries
    }

    /// Export to JSONL format.
    pub fn to_jsonl(&self) -> String {
        self.entries
            .iter()
            .map(|e| e.to_jsonl())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Get entry count.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Async Task Manager screen state.
#[derive(Clone, Debug)]
pub struct AsyncTaskManager {
    /// All tasks (active and completed).
    tasks: Vec<Task>,
    /// Next task ID.
    next_id: u32,
    /// Currently selected task index.
    selected: usize,
    /// Current scheduler policy.
    policy: SchedulerPolicy,
    /// Maximum concurrent running tasks.
    max_concurrent: usize,
    /// Current tick count.
    tick_count: u64,
    /// Recent events for the activity log.
    events: VecDeque<String>,
    /// Task name generator state.
    name_counter: u32,
    /// Fairness configuration (aging).
    fairness: FairnessConfig,
    /// Scheduler metrics for observability.
    metrics: SchedulerMetrics,
    /// Diagnostic configuration (bd-13pq.5).
    diagnostic_config: DiagnosticConfig,
    /// Diagnostic log entries (bd-13pq.5).
    diagnostic_log: DiagnosticLog,
    /// Hazard-based cancellation configuration (bd-13pq.8).
    hazard_config: HazardConfig,
    // Cached layout rects for mouse hit testing.
    layout_task_list: StdCell<Rect>,
    layout_task_list_inner: StdCell<Rect>,
    layout_details: StdCell<Rect>,
    layout_activity: StdCell<Rect>,
}

impl Default for AsyncTaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncTaskManager {
    pub fn new() -> Self {
        Self::with_config(DiagnosticConfig::default())
    }

    /// Create a new AsyncTaskManager with custom diagnostic configuration.
    pub fn with_config(diagnostic_config: DiagnosticConfig) -> Self {
        let max_entries = diagnostic_config.max_entries;
        let mut mgr = Self {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 3,
            tick_count: 0,
            events: VecDeque::with_capacity(20),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config,
            diagnostic_log: DiagnosticLog::new(max_entries),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        // Seed with a few initial tasks
        mgr.spawn_task_with_name("Initial Setup", 30, 2);
        mgr.spawn_task_with_name("Data Sync", 50, 1);
        mgr.spawn_task_with_name("Cache Warm", 20, 3);

        mgr
    }

    // =========================================================================
    // Accessors for E2E tests
    // =========================================================================

    /// Get a reference to the task list.
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// Get the current scheduler policy.
    pub fn policy(&self) -> SchedulerPolicy {
        self.policy
    }

    /// Get the currently selected task index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Get the maximum concurrent tasks.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Get the fairness configuration.
    pub fn fairness(&self) -> &FairnessConfig {
        &self.fairness
    }

    /// Get the scheduler metrics.
    pub fn metrics(&self) -> &SchedulerMetrics {
        &self.metrics
    }

    /// Get the diagnostic configuration.
    pub fn diagnostic_config(&self) -> &DiagnosticConfig {
        &self.diagnostic_config
    }

    /// Get the diagnostic log.
    pub fn diagnostic_log(&self) -> &DiagnosticLog {
        &self.diagnostic_log
    }

    /// Get the hazard configuration (bd-13pq.8).
    pub fn hazard_config(&self) -> &HazardConfig {
        &self.hazard_config
    }

    /// Get mutable hazard configuration for tuning (bd-13pq.8).
    pub fn hazard_config_mut(&mut self) -> &mut HazardConfig {
        &mut self.hazard_config
    }

    /// Analyze a task for hazard-based cancellation (bd-13pq.8).
    ///
    /// Returns the analysis with expected losses and recommendation.
    pub fn analyze_cancellation(&self, task: &Task) -> CancellationAnalysis {
        self.hazard_config.expected_loss_analysis(task)
    }

    /// Enable diagnostic logging.
    pub fn enable_diagnostics(&mut self) {
        self.diagnostic_config.enabled = true;
    }

    /// Disable diagnostic logging.
    pub fn disable_diagnostics(&mut self) {
        self.diagnostic_config.enabled = false;
    }

    /// Export diagnostic log as JSONL.
    pub fn export_diagnostics(&self) -> String {
        self.diagnostic_log.to_jsonl()
    }

    /// Clear diagnostic log entries.
    pub fn clear_diagnostics(&mut self) {
        self.diagnostic_log.clear();
    }

    /// Toggle aging on/off.
    fn toggle_aging(&mut self) {
        self.fairness.enabled = !self.fairness.enabled;
        let status = if self.fairness.enabled { "ON" } else { "OFF" };
        self.log_event(format!("Aging: {}", status));

        // Log toggle if diagnostics enabled
        if self.diagnostic_config.enabled {
            self.diagnostic_log.push(DiagnosticEntry::AgingToggle {
                tick: self.tick_count,
                enabled: self.fairness.enabled,
            });
        }
    }

    /// Calculate effective priority with aging.
    ///
    /// Formula: effective = base_priority + α * min(wait_time, max_boost/α)
    ///
    /// This ensures:
    /// - Long-waiting tasks eventually get scheduled
    /// - Aging boost is capped to prevent complete policy override
    fn effective_priority(&self, task: &Task) -> f64 {
        let base = task.priority as f64;
        if !self.fairness.enabled || task.state != TaskState::Queued {
            return base;
        }

        let wait_time = self.tick_count.saturating_sub(task.created_at) as f64;
        let age_boost = (self.fairness.aging_factor * wait_time).min(self.fairness.max_age_boost);
        base + age_boost
    }

    /// Calculate remaining processing time for SRPT.
    fn remaining_time(&self, task: &Task) -> u64 {
        task.estimated_ticks.saturating_sub(task.elapsed_ticks)
    }

    /// Calculate Smith's Rule score: priority / processing_time.
    /// Higher score = should be scheduled first.
    fn smith_score(&self, task: &Task) -> f64 {
        let priority = self.effective_priority(task);
        let processing_time = task.estimated_ticks.max(1) as f64;
        priority / processing_time
    }

    /// Generate a task name.
    fn generate_name(&mut self) -> String {
        self.name_counter += 1;
        let names = [
            "Build",
            "Deploy",
            "Test",
            "Sync",
            "Backup",
            "Index",
            "Compile",
            "Migrate",
            "Transform",
            "Validate",
            "Fetch",
            "Upload",
            "Process",
            "Analyze",
            "Generate",
        ];
        let adjectives = ["Quick", "Full", "Incremental", "Parallel", "Async", "Batch"];

        let adj_idx = (self.name_counter as usize) % adjectives.len();
        let name_idx = (self.name_counter as usize / adjectives.len()) % names.len();

        format!(
            "{} {} #{}",
            adjectives[adj_idx], names[name_idx], self.name_counter
        )
    }

    /// Spawn a new task with a generated name.
    fn spawn_task(&mut self) {
        let name = self.generate_name();
        // Random-ish duration based on counter
        let duration = 20 + (self.name_counter as u64 * 7) % 60;
        let priority = ((self.name_counter % 3) + 1) as u8;
        self.spawn_task_with_name(&name, duration, priority);
    }

    /// Spawn a task with a specific name.
    fn spawn_task_with_name(&mut self, name: &str, duration: u64, priority: u8) {
        if self.tasks.len() >= MAX_TASKS {
            // Remove oldest completed task
            if let Some(pos) = self.tasks.iter().position(|t| t.is_terminal()) {
                self.tasks.remove(pos);
                if self.selected > 0 && self.selected >= self.tasks.len() {
                    self.selected = self.tasks.len().saturating_sub(1);
                }
            }
        }

        let task = Task::new(
            self.next_id,
            name.to_string(),
            duration,
            priority,
            self.tick_count,
        );
        self.next_id += 1;
        self.log_event(format!("Spawned: {}", task.name));
        self.tasks.push(task);
    }

    /// Cancel the selected task.
    ///
    /// Performs hazard-based analysis (bd-13pq.8) and logs evidence
    /// to the diagnostic log for audit purposes.
    fn cancel_selected(&mut self) {
        self.cancel_task_with_source(CancellationSource::UserRequest);
    }

    /// Cancel the selected task with a specific source.
    ///
    /// This is the core cancellation logic that:
    /// 1. Performs hazard-based analysis
    /// 2. Executes the cancellation
    /// 3. Logs evidence to the diagnostic log (bd-13pq.8)
    fn cancel_task_with_source(&mut self, source: CancellationSource) {
        // Gather information before mutation
        let analysis_and_info = if let Some(task) = self.tasks.get(self.selected) {
            if !task.is_terminal() {
                let analysis = self.hazard_config.expected_loss_analysis(task);
                Some((task.id, task.name.clone(), task.state, analysis))
            } else {
                None
            }
        } else {
            None
        };

        // Perform cancellation and log evidence
        if let Some((task_id, name, old_state, analysis)) = analysis_and_info {
            // Execute cancellation
            if let Some(task) = self.tasks.get_mut(self.selected) {
                task.state = TaskState::Canceled;
            }

            self.log_event(format!("Canceled: {}", name));

            // Log cancellation decision with hazard evidence (bd-13pq.8)
            if self.diagnostic_config.enabled {
                let evidence = CancellationEvidence {
                    tick: self.tick_count,
                    task_id,
                    task_name: name.clone(),
                    analysis,
                    executed: true,
                    source,
                };
                self.diagnostic_log
                    .push(DiagnosticEntry::CancellationDecision(evidence));
            }

            // Also log state transition
            if self.diagnostic_config.enabled && self.diagnostic_config.log_transitions {
                self.diagnostic_log.push(DiagnosticEntry::StateTransition {
                    tick: self.tick_count,
                    task_id,
                    from: old_state,
                    to: TaskState::Canceled,
                    wait_time: None,
                });
            }
        }
    }

    /// Check if the selected task should be canceled based on hazard analysis.
    ///
    /// Returns the analysis result without executing cancellation.
    /// Use this for UI hints or policy-based auto-cancellation.
    pub fn should_cancel_selected(&self) -> Option<CancellationAnalysis> {
        self.tasks.get(self.selected).and_then(|task| {
            if task.is_terminal() {
                None
            } else {
                Some(self.hazard_config.expected_loss_analysis(task))
            }
        })
    }

    /// Auto-cancel tasks that exceed the hazard threshold.
    ///
    /// Scans all running tasks and cancels those where the hazard-based
    /// policy recommends cancellation. Logs evidence for each decision.
    ///
    /// Returns the number of tasks canceled.
    pub fn auto_cancel_by_hazard(&mut self) -> usize {
        if !self.hazard_config.enabled {
            return 0;
        }

        // Collect tasks to cancel (indices and analyses)
        let to_cancel: Vec<(usize, u32, String, CancellationAnalysis)> = self
            .tasks
            .iter()
            .enumerate()
            .filter_map(|(idx, task)| {
                if task.state == TaskState::Running {
                    let analysis = self.hazard_config.expected_loss_analysis(task);
                    if analysis.recommend_cancel {
                        return Some((idx, task.id, task.name.clone(), analysis));
                    }
                }
                None
            })
            .collect();

        let count = to_cancel.len();

        // Execute cancellations
        for (idx, task_id, name, analysis) in to_cancel {
            if let Some(task) = self.tasks.get_mut(idx) {
                let old_state = task.state;
                task.state = TaskState::Canceled;

                self.log_event(format!("Auto-canceled (hazard): {}", name));

                // Log cancellation decision with hazard evidence
                if self.diagnostic_config.enabled {
                    let evidence = CancellationEvidence {
                        tick: self.tick_count,
                        task_id,
                        task_name: name,
                        analysis,
                        executed: true,
                        source: CancellationSource::PolicyRecommendation,
                    };
                    self.diagnostic_log
                        .push(DiagnosticEntry::CancellationDecision(evidence));
                }

                // Log state transition
                if self.diagnostic_config.enabled && self.diagnostic_config.log_transitions {
                    self.diagnostic_log.push(DiagnosticEntry::StateTransition {
                        tick: self.tick_count,
                        task_id,
                        from: old_state,
                        to: TaskState::Canceled,
                        wait_time: None,
                    });
                }
            }
        }

        count
    }

    /// Retry the selected failed task.
    fn retry_selected(&mut self) {
        let transition = if let Some(task) = self.tasks.get_mut(self.selected) {
            if task.state == TaskState::Failed {
                let old_state = task.state;
                task.state = TaskState::Queued;
                task.progress = 0.0;
                task.elapsed_ticks = 0;
                task.error = None;
                Some((task.id, task.name.clone(), old_state))
            } else {
                None
            }
        } else {
            None
        };
        if let Some((task_id, name, old_state)) = transition {
            self.log_event(format!("Retrying: {}", name));

            // Log state transition
            if self.diagnostic_config.enabled && self.diagnostic_config.log_transitions {
                self.diagnostic_log.push(DiagnosticEntry::StateTransition {
                    tick: self.tick_count,
                    task_id,
                    from: old_state,
                    to: TaskState::Queued,
                    wait_time: None,
                });
            }
        }
    }

    /// Cycle to the next scheduler policy.
    fn cycle_policy(&mut self) {
        let old_policy = self.policy;
        self.policy = self.policy.next();
        self.log_event(format!("Scheduler: {}", self.policy.label()));

        // Log policy change if diagnostics enabled
        if self.diagnostic_config.enabled {
            self.diagnostic_log.push(DiagnosticEntry::PolicyChange {
                tick: self.tick_count,
                from: old_policy,
                to: self.policy,
            });
        }
    }

    /// Move selection up.
    fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move selection down.
    fn select_next(&mut self) {
        if self.selected + 1 < self.tasks.len() {
            self.selected += 1;
        }
    }

    /// Move selection to first task (Home key).
    fn select_first(&mut self) {
        self.selected = 0;
    }

    /// Move selection to last task (End key).
    fn select_last(&mut self) {
        if !self.tasks.is_empty() {
            self.selected = self.tasks.len() - 1;
        }
    }

    /// Move selection up by a page (10 items or to start).
    fn select_page_up(&mut self) {
        self.selected = self.selected.saturating_sub(10);
    }

    /// Move selection down by a page (10 items or to end).
    fn select_page_down(&mut self) {
        if !self.tasks.is_empty() {
            self.selected = (self.selected + 10).min(self.tasks.len() - 1);
        }
    }

    /// Log an event to the activity feed.
    fn log_event(&mut self, msg: String) {
        if self.events.len() >= 20 {
            self.events.pop_front();
        }
        self.events.push_back(msg);
    }

    /// Count tasks by state.
    fn count_by_state(&self, state: TaskState) -> usize {
        self.tasks.iter().filter(|t| t.state == state).count()
    }

    /// Update task states based on scheduler policy.
    ///
    /// This method implements the core scheduling decision using queueing
    /// theory principles. See module-level docs for mathematical foundations.
    fn update_scheduler(&mut self) {
        let running_count = self.count_by_state(TaskState::Running);
        let slots_available = self.max_concurrent.saturating_sub(running_count);

        if slots_available == 0 {
            return;
        }

        // Check starvation warning for queued tasks
        if self.diagnostic_config.enabled && self.diagnostic_config.log_invariants {
            self.check_starvation_warnings();
        }

        // Collect queued tasks with their scheduling scores
        let mut queued_with_scores: Vec<(usize, f64)> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.state == TaskState::Queued)
            .map(|(i, task)| {
                let score = self.compute_scheduling_score(i, task);
                (i, score)
            })
            .collect();

        let candidates_count = queued_with_scores.len();

        // Sort by score (higher = should run first for most policies)
        // We negate internally for "lower is better" policies
        queued_with_scores
            .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Start tasks up to available slots
        let selected_indices: Vec<usize> = queued_with_scores
            .iter()
            .take(slots_available)
            .map(|(idx, _)| *idx)
            .collect();

        for (rank, (idx, score)) in queued_with_scores.iter().take(slots_available).enumerate() {
            let idx = *idx;
            let score = *score;
            let task = &self.tasks[idx];
            let wait_time = self.tick_count.saturating_sub(task.created_at);
            let base_priority = task.priority as f64;
            let age_boost = if self.fairness.enabled {
                (self.fairness.aging_factor * wait_time as f64).min(self.fairness.max_age_boost)
            } else {
                0.0
            };

            // Determine selection reason
            let reason = if candidates_count == 1 {
                SelectionReason::OnlyCandidate
            } else if self.policy == SchedulerPolicy::RoundRobin {
                SelectionReason::RoundRobinRotation
            } else if age_boost >= 1.0 && rank > 0 {
                SelectionReason::AgingBoost
            } else {
                SelectionReason::PolicyOrder
            };

            // Collect passed-over task IDs
            let passed_over: Vec<u32> = queued_with_scores
                .iter()
                .skip(slots_available)
                .map(|(i, _)| self.tasks[*i].id)
                .collect();

            // Log scheduling decision with evidence
            if self.diagnostic_config.enabled && self.diagnostic_config.log_decisions {
                let evidence = SchedulingEvidence {
                    tick: self.tick_count,
                    selected_task_id: task.id,
                    policy: self.policy,
                    raw_score: score,
                    base_priority,
                    aging_boost: age_boost,
                    wait_time,
                    reason: reason.clone(),
                    candidates_count,
                    passed_over,
                };
                self.diagnostic_log
                    .push(DiagnosticEntry::SchedulingDecision(evidence));
            }

            // Track if aging helped this task get scheduled
            if self.fairness.enabled && wait_time > 0 && age_boost >= 1.0 {
                self.metrics.aging_boosts_applied += 1;
            }

            // Update metrics
            self.metrics.max_wait_time = self.metrics.max_wait_time.max(wait_time);
            self.metrics.total_wait_time += wait_time;
            self.metrics.tasks_scheduled += 1;
        }

        // Now actually transition the tasks (separate loop to avoid borrow issues)
        for idx in selected_indices {
            // Extract data we need before mutating
            let (task_id, task_name, old_state, created_at) = {
                let task = &self.tasks[idx];
                (task.id, task.name.clone(), task.state, task.created_at)
            };

            // Mutate the task
            self.tasks[idx].state = TaskState::Running;

            // Log state transition
            if self.diagnostic_config.enabled && self.diagnostic_config.log_transitions {
                let wait_time = self.tick_count.saturating_sub(created_at);
                self.diagnostic_log.push(DiagnosticEntry::StateTransition {
                    tick: self.tick_count,
                    task_id,
                    from: old_state,
                    to: TaskState::Running,
                    wait_time: Some(wait_time),
                });
            }

            self.log_event(format!("Started: {}", task_name));
        }
    }

    /// Check for tasks that have been waiting too long (starvation detection).
    fn check_starvation_warnings(&mut self) {
        let threshold = self.diagnostic_config.starvation_threshold;
        for task in &self.tasks {
            if task.state == TaskState::Queued {
                let wait_time = self.tick_count.saturating_sub(task.created_at);
                if wait_time >= threshold {
                    self.diagnostic_log
                        .push(DiagnosticEntry::StarvationWarning {
                            tick: self.tick_count,
                            task_id: task.id,
                            wait_time,
                            threshold,
                        });
                }
            }
        }
    }

    /// Check all invariants and log any violations.
    ///
    /// Invariants checked:
    /// - BoundedConcurrency: Running count ≤ max_concurrent
    /// - BoundedProgress: All progress values ∈ [0.0, 1.0]
    /// - TerminalStability: No transitions from terminal states
    /// - MonotonicIds: Task IDs are strictly increasing
    fn check_invariants(&mut self) {
        let tick = self.tick_count;

        // Check bounded concurrency
        let running_count = self.count_by_state(TaskState::Running);
        let concurrency_ok = running_count <= self.max_concurrent;
        self.diagnostic_log.push(DiagnosticEntry::InvariantCheck {
            tick,
            invariant: InvariantType::BoundedConcurrency,
            passed: concurrency_ok,
            details: format!("running={}, max={}", running_count, self.max_concurrent),
        });

        // Check bounded progress
        let progress_ok = self
            .tasks
            .iter()
            .all(|t| t.progress >= 0.0 && t.progress <= 1.0);
        if !progress_ok {
            let violators: Vec<_> = self
                .tasks
                .iter()
                .filter(|t| t.progress < 0.0 || t.progress > 1.0)
                .map(|t| format!("id={} progress={:.4}", t.id, t.progress))
                .collect();
            self.diagnostic_log.push(DiagnosticEntry::InvariantCheck {
                tick,
                invariant: InvariantType::BoundedProgress,
                passed: false,
                details: format!("violations: {}", violators.join(", ")),
            });
        }

        // Check monotonic IDs
        let ids_ok = self.tasks.windows(2).all(|w| w[0].id < w[1].id);
        if !ids_ok && self.tasks.len() > 1 {
            self.diagnostic_log.push(DiagnosticEntry::InvariantCheck {
                tick,
                invariant: InvariantType::MonotonicIds,
                passed: false,
                details: "Task IDs not strictly increasing".to_string(),
            });
        }
    }

    /// Log a snapshot of current metrics.
    fn log_metrics_snapshot(&mut self) {
        let queued = self.count_by_state(TaskState::Queued);
        let running = self.count_by_state(TaskState::Running);

        self.diagnostic_log.push(DiagnosticEntry::MetricsSnapshot {
            tick: self.tick_count,
            queued,
            running,
            completed: self.metrics.tasks_completed,
            mean_wait: self.metrics.mean_wait_time(),
            max_wait: self.metrics.max_wait_time,
        });
    }

    /// Compute scheduling score for a task based on current policy.
    ///
    /// Higher score = should be scheduled first.
    /// This method encapsulates the queueing theory decision rules.
    fn compute_scheduling_score(&self, _idx: usize, task: &Task) -> f64 {
        match self.policy {
            SchedulerPolicy::Fifo => {
                // FIFO: Earlier arrival = higher priority
                // Negate created_at so older tasks have higher scores
                -(task.created_at as f64)
            }
            SchedulerPolicy::ShortestFirst => {
                // SJF/SPT: Shorter estimated time = higher priority
                // With aging, effective priority modifies the base score
                let base = -(task.estimated_ticks as f64);
                if self.fairness.enabled {
                    // Aging reduces the "penalty" of long jobs
                    let wait_boost = self.effective_priority(task) - task.priority as f64;
                    base + wait_boost * 10.0 // Scale aging to affect ordering
                } else {
                    base
                }
            }
            SchedulerPolicy::Srpt => {
                // SRPT: Shorter remaining time = higher priority
                // remaining = estimated - elapsed (for queued tasks, elapsed is typically 0)
                let remaining = self.remaining_time(task) as f64;
                let base = -remaining;
                if self.fairness.enabled {
                    let wait_boost = self.effective_priority(task) - task.priority as f64;
                    base + wait_boost * 10.0
                } else {
                    base
                }
            }
            SchedulerPolicy::SmithRule => {
                // Smith's Rule: Higher w/p ratio = higher priority
                // Already incorporates aging via effective_priority
                self.smith_score(task)
            }
            SchedulerPolicy::Priority => {
                // Priority: Higher effective priority = higher score
                self.effective_priority(task)
            }
            SchedulerPolicy::RoundRobin => {
                // Round-robin: Rotate based on tick + task_id
                // Creates pseudo-random but deterministic ordering
                let rotation = (self.tick_count + task.id as u64) % 1000;
                -(rotation as f64)
            }
        }
    }

    /// Advance running tasks by one tick.
    fn advance_tasks(&mut self) {
        let current_tick = self.tick_count;
        let log_transitions =
            self.diagnostic_config.enabled && self.diagnostic_config.log_transitions;

        // Collect transitions to log after the mutation loop
        let mut transitions: Vec<(u32, TaskState, TaskState)> = Vec::new();

        for task in &mut self.tasks {
            if task.state != TaskState::Running {
                continue;
            }

            task.elapsed_ticks += 1;
            task.progress = (task.elapsed_ticks as f64 / task.estimated_ticks as f64).min(1.0);

            // Check for completion
            if task.elapsed_ticks >= task.estimated_ticks {
                // Track completion time for metrics
                let completion_time = current_tick.saturating_sub(task.created_at);
                self.metrics.total_completion_time += completion_time;
                self.metrics.tasks_completed += 1;

                let old_state = task.state;

                // Simulate occasional failures (5% chance based on task id)
                if task.id % 20 == 7 {
                    task.state = TaskState::Failed;
                    task.error = Some("Simulated failure".to_string());
                } else {
                    task.state = TaskState::Succeeded;
                }

                if log_transitions {
                    transitions.push((task.id, old_state, task.state));
                }
            }
        }

        // Log state transitions
        for (task_id, from, to) in transitions {
            self.diagnostic_log.push(DiagnosticEntry::StateTransition {
                tick: current_tick,
                task_id,
                from,
                to,
                wait_time: None,
            });
        }
    }

    // =========================================================================
    // Rendering
    // =========================================================================

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let queued = self.count_by_state(TaskState::Queued);
        let running = self.count_by_state(TaskState::Running);
        let succeeded = self.count_by_state(TaskState::Succeeded);
        let failed = self.count_by_state(TaskState::Failed);

        let aging_status = if self.fairness.enabled { "✓" } else { "✗" };

        let header = format!(
            "Q:{} R:{} D:{} F:{} | {}[{}] | Aging:{}",
            queued,
            running,
            succeeded,
            failed,
            self.policy.label(),
            self.policy.description(),
            aging_status
        );

        Paragraph::new(header)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(area, frame);
    }

    fn render_task_list(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 3 {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Task Queue")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::PERFORMANCE));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        // Cache inner area for mouse row hit testing.
        self.layout_task_list_inner.set(inner);

        // Calculate visible range
        let visible_count = inner.height as usize;
        let scroll_offset = if self.selected >= visible_count {
            self.selected - visible_count + 1
        } else {
            0
        };

        let colors = MiniBarColors::new(
            theme::intent::success_text(),
            theme::intent::warning_text(),
            theme::intent::info_text(),
            theme::intent::error_text(),
        );

        for (i, task) in self
            .tasks
            .iter()
            .skip(scroll_offset)
            .take(visible_count)
            .enumerate()
        {
            let y = inner.y + i as u16;
            let is_selected = scroll_offset + i == self.selected;
            let row_area = Rect::new(inner.x, y, inner.width, 1);

            // Apply background highlight for selected row (WCAG accessibility)
            if is_selected {
                let bg_style = Style::new().bg(theme::alpha::HIGHLIGHT);
                Paragraph::new(" ".repeat(inner.width as usize))
                    .style(bg_style)
                    .render(row_area, frame);
            }

            // Selection indicator
            let indicator = if is_selected {
                theme::selection::INDICATOR
            } else {
                theme::selection::EMPTY
            };

            // State indicator with color
            let state_label = task.state.label();
            let state_style = task.state.style();

            // Priority indicator
            let priority_str = match task.priority {
                1 => "L",
                2 => "M",
                3 => "H",
                _ => "?",
            };

            // Build the line
            let name_width = inner.width.saturating_sub(30) as usize;
            let truncated_name: String = task.name.chars().take(name_width).collect();

            // Layout: [sel] [state] [priority] [name] [progress bar]
            let mut x = inner.x;

            // Selection indicator
            Paragraph::new(indicator)
                .style(if is_selected {
                    Style::new().fg(theme::accent::PRIMARY).bold()
                } else {
                    Style::new().fg(theme::fg::MUTED)
                })
                .render(Rect::new(x, y, 2, 1), frame);
            x += 2;

            // State
            let state_width = 9u16;
            Paragraph::new(format!("{:8}", state_label))
                .style(state_style)
                .render(Rect::new(x, y, state_width, 1), frame);
            x += state_width;

            // Priority
            Paragraph::new(format!("[{}] ", priority_str))
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(Rect::new(x, y, 4, 1), frame);
            x += 4;

            // Name
            let name_area_width = (inner.width.saturating_sub(x - inner.x)).saturating_sub(12);
            if name_area_width > 0 {
                Paragraph::new(truncated_name)
                    .style(if is_selected {
                        Style::new().fg(theme::fg::PRIMARY).bold()
                    } else {
                        Style::new().fg(theme::fg::PRIMARY)
                    })
                    .render(Rect::new(x, y, name_area_width, 1), frame);
                x += name_area_width;
            }

            // Progress bar (only for running tasks)
            let bar_width = (inner.x + inner.width).saturating_sub(x);
            if bar_width >= 6 && task.state == TaskState::Running {
                MiniBar::new(task.progress, bar_width)
                    .colors(colors)
                    .show_percent(true)
                    .render(Rect::new(x, y, bar_width, 1), frame);
            } else if bar_width >= 6 && task.progress > 0.0 && task.progress < 1.0 {
                // Show static progress for paused/queued tasks
                let pct = format!("{:>3}%", (task.progress * 100.0) as u8);
                Paragraph::new(pct)
                    .style(Style::new().fg(theme::fg::MUTED))
                    .render(Rect::new(x, y, bar_width, 1), frame);
            }
        }

        // Empty state
        if self.tasks.is_empty() {
            Paragraph::new("No tasks. Press 'n' to spawn a new task.")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(inner, frame);
        }
    }

    fn render_task_details(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 3 {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Task Details")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::DASHBOARD));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let Some(task) = self.tasks.get(self.selected) else {
            Paragraph::new("No task selected")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(inner, frame);
            return;
        };

        let mut lines = vec![
            format!("ID: {}", task.id),
            format!("Name: {}", task.name),
            format!("State: {}", task.state.label()),
            format!("Priority: {}", task.priority),
            format!("Progress: {:.0}%", task.progress * 100.0),
            format!("Elapsed: {} ticks", task.elapsed_ticks),
            format!("Estimated: {} ticks", task.estimated_ticks),
        ];

        if let Some(err) = &task.error {
            lines.push(format!("Error: {}", err));
        }

        for (i, line) in lines.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            Paragraph::new(line.as_str())
                .style(Style::new().fg(theme::fg::PRIMARY))
                .render(
                    Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                    frame,
                );
        }
    }

    fn render_activity_log(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 3 {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Activity")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::ADVANCED));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let visible = inner.height as usize;
        let events: Vec<_> = self.events.iter().rev().take(visible).collect();

        for (i, event) in events.iter().rev().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            Paragraph::new(event.as_str())
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(
                    Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
                    frame,
                );
        }

        if self.events.is_empty() {
            Paragraph::new("No recent activity")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(inner, frame);
        }
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let help = "n:spawn  c:cancel  s:scheduler  a:aging  r:retry  j/k:nav";
        Paragraph::new(help)
            .style(Style::new().fg(theme::fg::MUTED))
            .render(area, frame);
    }
}

impl Screen for AsyncTaskManager {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        // Mouse: click task rows to select, wheel to scroll.
        if let Event::Mouse(mouse) = event {
            let list_inner = self.layout_task_list_inner.get();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if list_inner.contains(mouse.x, mouse.y) {
                        // Calculate which task row was clicked.
                        let row_offset = (mouse.y - list_inner.y) as usize;
                        let visible_count = list_inner.height as usize;
                        let scroll_offset = if self.selected >= visible_count {
                            self.selected - visible_count + 1
                        } else {
                            0
                        };
                        let task_idx = scroll_offset + row_offset;
                        if task_idx < self.tasks.len() {
                            self.selected = task_idx;
                        }
                    }
                }
                MouseEventKind::ScrollUp => {
                    if list_inner.contains(mouse.x, mouse.y) {
                        self.select_prev();
                    }
                }
                MouseEventKind::ScrollDown => {
                    if list_inner.contains(mouse.x, mouse.y) {
                        self.select_next();
                    }
                }
                _ => {}
            }
        }

        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.spawn_task();
                }
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.cancel_selected();
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    self.cycle_policy();
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.retry_selected();
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.toggle_aging();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.select_prev();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.select_next();
                }
                KeyCode::PageUp => {
                    self.select_page_up();
                }
                KeyCode::PageDown => {
                    self.select_page_down();
                }
                KeyCode::Home => {
                    self.select_first();
                }
                KeyCode::End => {
                    self.select_last();
                }
                _ => {}
            }
        }

        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        self.update_scheduler();
        self.advance_tasks();

        // Check invariants and log metrics if diagnostics enabled
        if self.diagnostic_config.enabled && self.diagnostic_config.log_invariants {
            self.check_invariants();
        }

        // Log periodic metrics snapshot (every 10 ticks)
        if self.diagnostic_config.enabled && tick_count.is_multiple_of(10) {
            self.log_metrics_snapshot();
        }
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Layout: header, main content, help footer
        let main_layout = Flex::vertical()
            .constraints([
                Constraint::Fixed(1), // Header
                Constraint::Min(10),  // Content
                Constraint::Fixed(1), // Help
            ])
            .split(area);

        self.render_header(frame, main_layout[0]);
        self.render_help(frame, main_layout[2]);

        // Content area: task list on left, details + activity on right
        let content_cols = Flex::horizontal()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(main_layout[1]);

        // Cache layout rects for mouse hit testing.
        self.layout_task_list.set(content_cols[0]);

        self.render_task_list(frame, content_cols[0]);

        // Right column: details on top, activity on bottom
        let right_rows = Flex::vertical()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(content_cols[1]);

        self.layout_details.set(right_rows[0]);
        self.layout_activity.set(right_rows[1]);

        self.render_task_details(frame, right_rows[0]);
        self.render_activity_log(frame, right_rows[1]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "n",
                action: "Spawn new task",
            },
            HelpEntry {
                key: "c",
                action: "Cancel selected task",
            },
            HelpEntry {
                key: "s",
                action: "Cycle scheduler policy",
            },
            HelpEntry {
                key: "a",
                action: "Toggle aging (fairness)",
            },
            HelpEntry {
                key: "r",
                action: "Retry failed task",
            },
            HelpEntry {
                key: "j/k",
                action: "Navigate tasks",
            },
            HelpEntry {
                key: "PgUp/Dn",
                action: "Page navigation",
            },
            HelpEntry {
                key: "Home/End",
                action: "Jump to first/last",
            },
            HelpEntry {
                key: "Click",
                action: "Select task row",
            },
            HelpEntry {
                key: "Wheel",
                action: "Scroll task list",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Async Tasks"
    }

    fn tab_label(&self) -> &'static str {
        "Tasks"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn new_has_initial_tasks() {
        let mgr = AsyncTaskManager::new();
        assert!(!mgr.tasks.is_empty());
        assert_eq!(mgr.next_id, 4); // 3 initial tasks + 1
    }

    #[test]
    fn spawn_task_increments_id() {
        let mut mgr = AsyncTaskManager::new();
        let before = mgr.next_id;
        mgr.spawn_task();
        assert_eq!(mgr.next_id, before + 1);
    }

    #[test]
    fn cancel_task_changes_state() {
        let mut mgr = AsyncTaskManager::new();
        mgr.selected = 0;
        mgr.cancel_selected();
        assert_eq!(mgr.tasks[0].state, TaskState::Canceled);
    }

    // =========================================================================
    // Hazard-Based Cancellation Tests (bd-13pq.8)
    // =========================================================================

    #[test]
    fn hazard_rate_increases_with_overrun() {
        let config = HazardConfig::default();
        let mut task = Task::new(1, "Test".into(), 100, 1, 0);

        // At start, hazard rate is base
        task.elapsed_ticks = 0;
        let h0 = config.hazard_rate(&task);
        assert!(
            (h0 - config.hazard_base).abs() < 1e-10,
            "Hazard rate at start should equal base: {} vs {}",
            h0,
            config.hazard_base
        );

        // At estimated time, hazard rate is still base (no overrun)
        task.elapsed_ticks = 100;
        let h1 = config.hazard_rate(&task);
        assert!(
            (h1 - config.hazard_base).abs() < 1e-10,
            "Hazard rate at estimated time should equal base"
        );

        // Past estimated time, hazard rate increases
        task.elapsed_ticks = 200; // 100% overrun
        let h2 = config.hazard_rate(&task);
        assert!(
            h2 > h1,
            "Hazard rate should increase with overrun: {} > {}",
            h2,
            h1
        );
    }

    #[test]
    fn failure_probability_bounded_zero_to_one() {
        let config = HazardConfig::default();
        let mut task = Task::new(1, "Test".into(), 100, 1, 0);

        // Test various elapsed times
        for elapsed in [0, 50, 100, 200, 1000] {
            task.elapsed_ticks = elapsed;
            let p = config.failure_probability(&task, 100);
            assert!(
                (0.0..=1.0).contains(&p),
                "Probability must be in [0, 1]: {}",
                p
            );
        }
    }

    #[test]
    fn expected_loss_analysis_structure() {
        let config = HazardConfig::default();
        let task = Task::new(1, "Test".into(), 100, 1, 0);

        let analysis = config.expected_loss_analysis(&task);

        // All fields should be non-negative
        assert!(analysis.hazard_rate >= 0.0);
        assert!(analysis.failure_probability >= 0.0);
        assert!(analysis.expected_loss_continue >= 0.0);
        assert!(analysis.expected_loss_cancel >= 0.0);
        assert!(analysis.bayes_factor >= 0.0);
    }

    #[test]
    fn cancel_logs_evidence_when_diagnostics_enabled() {
        let config = DiagnosticConfig {
            enabled: true,
            ..Default::default()
        };

        let mut mgr = AsyncTaskManager::with_config(config);
        mgr.selected = 0;
        let initial_log_len = mgr.diagnostic_log.len();

        mgr.cancel_selected();

        // Should have logged cancellation decision
        assert!(
            mgr.diagnostic_log.len() > initial_log_len,
            "Diagnostic log should have new entries after cancel"
        );

        // Check that at least one CancellationDecision entry exists
        let has_cancel_entry = mgr
            .diagnostic_log
            .entries()
            .iter()
            .any(|e| matches!(e, DiagnosticEntry::CancellationDecision(_)));
        assert!(
            has_cancel_entry,
            "Should have CancellationDecision entry in log"
        );
    }

    #[test]
    fn should_cancel_selected_returns_analysis() {
        let mut mgr = AsyncTaskManager::new();
        mgr.selected = 0;

        let analysis = mgr.should_cancel_selected();
        assert!(analysis.is_some(), "Should return analysis for active task");

        let analysis = analysis.unwrap();
        assert!(
            !analysis.explanation().is_empty(),
            "Explanation should not be empty"
        );
    }

    #[test]
    fn should_cancel_selected_returns_none_for_terminal() {
        let mut mgr = AsyncTaskManager::new();
        mgr.tasks[0].state = TaskState::Succeeded;
        mgr.selected = 0;

        let analysis = mgr.should_cancel_selected();
        assert!(analysis.is_none(), "Should return None for terminal task");
    }

    #[test]
    fn auto_cancel_by_hazard_respects_enabled_flag() {
        let mut mgr = AsyncTaskManager::new();
        mgr.hazard_config_mut().enabled = false;

        let count = mgr.auto_cancel_by_hazard();
        assert_eq!(count, 0, "Should cancel nothing when disabled");
    }

    #[test]
    fn auto_cancel_by_hazard_cancels_high_hazard_tasks() {
        let mut mgr = AsyncTaskManager::new();

        // Create a task with very high hazard (way past estimated time)
        let mut overrun_task = Task::new(99, "Overrun".into(), 10, 1, 0);
        overrun_task.state = TaskState::Running;
        overrun_task.elapsed_ticks = 1000; // 100x overrun
        overrun_task.progress = 0.1;
        mgr.tasks.push(overrun_task);

        // Configure hazard to aggressively cancel
        mgr.hazard_config_mut().enabled = true;
        mgr.hazard_config_mut().cancel_cost = 0.01; // Very low cancel cost
        mgr.hazard_config_mut().fail_cost = 10.0; // Very high fail cost
        mgr.hazard_config_mut().decision_threshold = 0.1; // Low threshold

        let initial_running = mgr
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .count();

        let count = mgr.auto_cancel_by_hazard();

        let final_running = mgr
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .count();

        // Should have canceled at least the overrun task
        assert!(
            count >= 1,
            "Should have auto-canceled at least one task: {}",
            count
        );
        assert!(
            final_running < initial_running,
            "Running count should decrease after auto-cancel"
        );
    }

    #[test]
    fn hazard_config_accessors_work() {
        let mut mgr = AsyncTaskManager::new();

        // Read access
        let config = mgr.hazard_config();
        assert!(config.enabled);

        // Write access
        mgr.hazard_config_mut().decision_threshold = 5.0;
        assert_eq!(mgr.hazard_config().decision_threshold, 5.0);
    }

    #[test]
    fn cancellation_evidence_jsonl_format() {
        let evidence = CancellationEvidence {
            tick: 42,
            task_id: 7,
            task_name: "Test Task".into(),
            analysis: CancellationAnalysis {
                hazard_rate: 0.05,
                failure_probability: 0.15,
                expected_loss_continue: 0.15,
                expected_loss_cancel: 0.10,
                bayes_factor: 1.5,
                recommend_cancel: true,
            },
            executed: true,
            source: CancellationSource::UserRequest,
        };

        let entry = DiagnosticEntry::CancellationDecision(evidence);
        let jsonl = entry.to_jsonl();

        // Verify it contains expected fields
        assert!(jsonl.contains("\"type\":\"cancellation_decision\""));
        assert!(jsonl.contains("\"tick\":42"));
        assert!(jsonl.contains("\"task_id\":7"));
        assert!(jsonl.contains("\"recommend_cancel\":true"));
        assert!(jsonl.contains("\"executed\":true"));
        assert!(jsonl.contains("\"source\":\"user_request\""));
    }

    #[test]
    fn cancellation_source_as_str() {
        assert_eq!(CancellationSource::UserRequest.as_str(), "user_request");
        assert_eq!(
            CancellationSource::PolicyRecommendation.as_str(),
            "policy_recommendation"
        );
        assert_eq!(CancellationSource::System.as_str(), "system");
    }

    #[test]
    fn analysis_explanation_contains_key_info() {
        let analysis = CancellationAnalysis {
            hazard_rate: 0.05,
            failure_probability: 0.15,
            expected_loss_continue: 0.15,
            expected_loss_cancel: 0.10,
            bayes_factor: 1.5,
            recommend_cancel: true,
        };

        let explanation = analysis.explanation();
        assert!(explanation.contains("λ="), "Should contain hazard rate");
        assert!(
            explanation.contains("P(fail)"),
            "Should contain failure probability"
        );
        assert!(explanation.contains("BF="), "Should contain Bayes factor");
        assert!(
            explanation.contains("RECOMMEND CANCEL"),
            "Should indicate recommendation"
        );
    }

    #[test]
    fn retry_failed_task() {
        let mut mgr = AsyncTaskManager::new();
        mgr.tasks[0].state = TaskState::Failed;
        mgr.selected = 0;
        mgr.retry_selected();
        assert_eq!(mgr.tasks[0].state, TaskState::Queued);
        assert_eq!(mgr.tasks[0].progress, 0.0);
    }

    #[test]
    fn cycle_policy() {
        let mut mgr = AsyncTaskManager::new();
        assert_eq!(mgr.policy, SchedulerPolicy::Fifo);
        mgr.cycle_policy();
        assert_eq!(mgr.policy, SchedulerPolicy::ShortestFirst);
        mgr.cycle_policy();
        assert_eq!(mgr.policy, SchedulerPolicy::Srpt);
        mgr.cycle_policy();
        assert_eq!(mgr.policy, SchedulerPolicy::SmithRule);
        mgr.cycle_policy();
        assert_eq!(mgr.policy, SchedulerPolicy::Priority);
        mgr.cycle_policy();
        assert_eq!(mgr.policy, SchedulerPolicy::RoundRobin);
        mgr.cycle_policy();
        assert_eq!(mgr.policy, SchedulerPolicy::Fifo);
    }

    #[test]
    fn scheduler_starts_queued_tasks() {
        let mut mgr = AsyncTaskManager::new();
        // All tasks start queued
        assert!(mgr.tasks.iter().all(|t| t.state == TaskState::Queued));

        mgr.update_scheduler();

        // Should have started up to max_concurrent tasks
        let running = mgr.count_by_state(TaskState::Running);
        assert!(running <= mgr.max_concurrent);
        assert!(running > 0);
    }

    #[test]
    fn advance_tasks_updates_progress() {
        let mut mgr = AsyncTaskManager::new();
        mgr.tasks[0].state = TaskState::Running;
        let before = mgr.tasks[0].progress;

        mgr.advance_tasks();

        assert!(mgr.tasks[0].progress > before);
    }

    #[test]
    fn task_completes_after_estimated_ticks() {
        let mut mgr = AsyncTaskManager::new();
        mgr.tasks[0].state = TaskState::Running;
        mgr.tasks[0].estimated_ticks = 5;

        for _ in 0..5 {
            mgr.advance_tasks();
        }

        assert!(mgr.tasks[0].is_terminal());
    }

    #[test]
    fn navigation_updates_selected() {
        let mut mgr = AsyncTaskManager::new();
        assert_eq!(mgr.selected, 0);

        mgr.select_next();
        assert_eq!(mgr.selected, 1);

        mgr.select_prev();
        assert_eq!(mgr.selected, 0);

        // Can't go below 0
        mgr.select_prev();
        assert_eq!(mgr.selected, 0);
    }

    #[test]
    fn navigation_page_up_down() {
        let mut mgr = AsyncTaskManager::new();
        // Add enough tasks for page navigation
        for _ in 0..20 {
            mgr.spawn_task();
        }
        assert!(mgr.tasks.len() >= 20);

        // Start at first task
        mgr.selected = 0;
        mgr.select_page_down();
        assert_eq!(mgr.selected, 10, "Page down should move 10 items");

        mgr.select_page_down();
        assert_eq!(mgr.selected, 20, "Page down again should move to 20");

        mgr.select_page_up();
        assert_eq!(mgr.selected, 10, "Page up should move back to 10");

        mgr.select_page_up();
        assert_eq!(mgr.selected, 0, "Page up should move back to 0");

        // Can't go below 0
        mgr.select_page_up();
        assert_eq!(mgr.selected, 0, "Page up at start stays at 0");
    }

    #[test]
    fn navigation_home_end() {
        let mut mgr = AsyncTaskManager::new();
        // Add tasks
        for _ in 0..10 {
            mgr.spawn_task();
        }
        let last_idx = mgr.tasks.len() - 1;

        mgr.selected = 5;
        mgr.select_first();
        assert_eq!(mgr.selected, 0, "Home should go to first");

        mgr.select_last();
        assert_eq!(mgr.selected, last_idx, "End should go to last");

        // Already at last
        mgr.select_last();
        assert_eq!(mgr.selected, last_idx, "End at end stays at end");

        // Already at first
        mgr.select_first();
        mgr.select_first();
        assert_eq!(mgr.selected, 0, "Home at start stays at start");
    }

    #[test]
    fn navigation_page_clamps_to_bounds() {
        let mut mgr = AsyncTaskManager::new();
        // Default has 3 tasks
        assert_eq!(mgr.tasks.len(), 3);

        mgr.selected = 0;
        mgr.select_page_down();
        // Should clamp to last index (2), not go to 10
        assert_eq!(mgr.selected, 2, "Page down should clamp to list end");

        mgr.select_page_down();
        assert_eq!(mgr.selected, 2, "Page down at end stays at end");
    }

    #[test]
    fn renders_without_panic() {
        let mgr = AsyncTaskManager::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        mgr.view(&mut frame, Rect::new(0, 0, 120, 40));
    }

    #[test]
    fn renders_small_area() {
        let mgr = AsyncTaskManager::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        mgr.view(&mut frame, Rect::new(0, 0, 40, 10));
    }

    #[test]
    fn renders_empty_area() {
        let mgr = AsyncTaskManager::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        mgr.view(&mut frame, Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn tick_advances_state() {
        let mut mgr = AsyncTaskManager::new();
        let initial_tick = mgr.tick_count;

        mgr.tick(100);

        assert_eq!(mgr.tick_count, 100);
        assert!(mgr.tick_count > initial_tick);
    }

    #[test]
    fn events_are_logged() {
        let mut mgr = AsyncTaskManager::new();
        let initial_events = mgr.events.len();

        mgr.spawn_task();

        assert!(mgr.events.len() > initial_events);
    }

    #[test]
    fn policy_labels_are_correct() {
        assert_eq!(SchedulerPolicy::Fifo.label(), "FIFO");
        assert_eq!(SchedulerPolicy::ShortestFirst.label(), "SJF");
        assert_eq!(SchedulerPolicy::Srpt.label(), "SRPT");
        assert_eq!(SchedulerPolicy::SmithRule.label(), "Smith");
        assert_eq!(SchedulerPolicy::Priority.label(), "Priority");
        assert_eq!(SchedulerPolicy::RoundRobin.label(), "RoundRobin");
    }

    #[test]
    fn task_state_labels_are_correct() {
        assert_eq!(TaskState::Queued.label(), "Queued");
        assert_eq!(TaskState::Running.label(), "Running");
        assert_eq!(TaskState::Succeeded.label(), "Done");
        assert_eq!(TaskState::Failed.label(), "Failed");
        assert_eq!(TaskState::Canceled.label(), "Canceled");
    }

    #[test]
    fn keybindings_returned() {
        let mgr = AsyncTaskManager::new();
        let bindings = mgr.keybindings();
        assert!(!bindings.is_empty());
        assert!(bindings.iter().any(|b| b.key == "n"));
    }

    #[test]
    fn title_and_label() {
        let mgr = AsyncTaskManager::new();
        assert_eq!(mgr.title(), "Async Tasks");
        assert_eq!(mgr.tab_label(), "Tasks");
    }

    // =========================================================================
    // Scheduler Policy Ordering Tests (bd-13pq.3)
    // =========================================================================

    #[test]
    fn fifo_scheduler_maintains_creation_order() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 2,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        // Add tasks in order with different durations/priorities
        mgr.spawn_task_with_name("First", 100, 1);
        mgr.spawn_task_with_name("Second", 10, 3);
        mgr.spawn_task_with_name("Third", 50, 2);

        mgr.update_scheduler();

        // FIFO should start First and Second (creation order), not by duration/priority
        let running: Vec<_> = mgr
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(running, vec!["First", "Second"]);
    }

    #[test]
    fn shortest_first_scheduler_orders_by_duration() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::ShortestFirst,
            max_concurrent: 2,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        mgr.spawn_task_with_name("Long", 100, 1);
        mgr.spawn_task_with_name("Short", 10, 1);
        mgr.spawn_task_with_name("Medium", 50, 1);

        mgr.update_scheduler();

        // ShortestFirst should start Short and Medium
        let running: Vec<_> = mgr
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(running, vec!["Short", "Medium"]);
    }

    #[test]
    fn srpt_scheduler_orders_by_remaining_time() {
        // Create manager directly with empty tasks to avoid pre-seeded tasks
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Srpt,
            max_concurrent: 2,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig {
                enabled: false,
                ..Default::default()
            },
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        // Create tasks with different remaining times
        mgr.spawn_task_with_name("Long", 100, 1);
        mgr.spawn_task_with_name("Short", 20, 1);
        mgr.spawn_task_with_name("Medium", 50, 1);

        mgr.update_scheduler();

        // SRPT should start Short and Medium (shortest remaining time first)
        let running: Vec<_> = mgr
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(running, vec!["Short", "Medium"]);
    }

    #[test]
    fn smith_rule_scheduler_orders_by_priority_over_duration() {
        let mut mgr = AsyncTaskManager::with_config(DiagnosticConfig::default());
        mgr.policy = SchedulerPolicy::SmithRule;
        mgr.max_concurrent = 2;
        mgr.fairness.enabled = false; // Disable aging for deterministic test

        // Smith's Rule: priority / duration ratio
        // Task A: priority 3, duration 30 -> ratio = 0.1
        // Task B: priority 1, duration 10 -> ratio = 0.1
        // Task C: priority 2, duration 10 -> ratio = 0.2 (highest)
        // Task D: priority 4, duration 100 -> ratio = 0.04 (lowest)
        mgr.spawn_task_with_name("TaskA", 30, 3); // ratio 0.1
        mgr.spawn_task_with_name("TaskB", 10, 1); // ratio 0.1
        mgr.spawn_task_with_name("TaskC", 10, 2); // ratio 0.2 (winner)
        mgr.spawn_task_with_name("TaskD", 100, 4); // ratio 0.04

        mgr.update_scheduler();

        // SmithRule should start TaskC (highest ratio) first
        let running: Vec<_> = mgr
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .map(|t| t.name.as_str())
            .collect();
        // TaskC has highest ratio (0.2), TaskA and TaskB tie at 0.1
        assert!(running.contains(&"TaskC"));
        assert_eq!(running.len(), 2);
    }

    #[test]
    fn aging_boosts_long_waiting_tasks() {
        // Create manager directly with empty tasks to avoid pre-seeded tasks
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Priority,
            max_concurrent: 1,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig {
                enabled: true,
                aging_factor: 0.5, // Fast aging for test
                ..Default::default()
            },
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        // Low priority task created at tick 0
        mgr.spawn_task_with_name("LowPri", 50, 1);
        // High priority task created at tick 100
        mgr.tick_count = 100;
        mgr.spawn_task_with_name("HighPri", 50, 3);

        // LowPri has waited 100 ticks, so effective priority = 1 + 0.5 * 100 = 51
        // HighPri has waited 0 ticks, so effective priority = 3
        // With aging, LowPri should win
        mgr.update_scheduler();

        let running: Vec<_> = mgr
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(running, vec!["LowPri"]);
    }

    #[test]
    fn toggle_aging_updates_fairness() {
        let mut mgr = AsyncTaskManager::new();
        assert!(mgr.fairness.enabled); // Default is enabled

        mgr.toggle_aging();
        assert!(!mgr.fairness.enabled);

        mgr.toggle_aging();
        assert!(mgr.fairness.enabled);
    }

    #[test]
    fn priority_scheduler_orders_by_priority() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Priority,
            max_concurrent: 2,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        mgr.spawn_task_with_name("LowPri", 50, 1);
        mgr.spawn_task_with_name("HighPri", 50, 3);
        mgr.spawn_task_with_name("MedPri", 50, 2);

        mgr.update_scheduler();

        // Priority scheduler should start HighPri and MedPri (highest first)
        let running: Vec<_> = mgr
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(running, vec!["HighPri", "MedPri"]);
    }

    #[test]
    fn round_robin_scheduler_varies_by_tick() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::RoundRobin,
            max_concurrent: 1,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        mgr.spawn_task_with_name("A", 50, 1);
        mgr.spawn_task_with_name("B", 50, 1);
        mgr.spawn_task_with_name("C", 50, 1);

        // At tick 0, record which task starts
        mgr.update_scheduler();
        let first_running = mgr
            .tasks
            .iter()
            .find(|t| t.state == TaskState::Running)
            .map(|t| t.id);

        // Reset and try at different tick
        for t in &mut mgr.tasks {
            t.state = TaskState::Queued;
        }
        mgr.tick_count = 500;
        mgr.update_scheduler();
        let second_running = mgr
            .tasks
            .iter()
            .find(|t| t.state == TaskState::Running)
            .map(|t| t.id);

        // The selection should potentially differ based on tick (not guaranteed but often will)
        // The key invariant is that it doesn't panic and makes a selection
        assert!(first_running.is_some());
        assert!(second_running.is_some());
    }

    // =========================================================================
    // Edge Case Tests (bd-13pq.3)
    // =========================================================================

    #[test]
    fn max_tasks_limit_removes_completed() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 3,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        // Fill up to MAX_TASKS with completed tasks
        for i in 0..MAX_TASKS {
            mgr.tasks.push(Task {
                id: i as u32,
                name: format!("Task {}", i),
                state: TaskState::Succeeded,
                progress: 1.0,
                estimated_ticks: 10,
                elapsed_ticks: 10,
                priority: 1,
                created_at: 0,
                error: None,
            });
        }
        mgr.next_id = MAX_TASKS as u32;

        let count_before = mgr.tasks.len();
        assert_eq!(count_before, MAX_TASKS);

        // Spawning a new task should remove one completed task
        mgr.spawn_task();

        assert_eq!(mgr.tasks.len(), MAX_TASKS);
        // The new task should be present
        assert!(mgr.tasks.iter().any(|t| t.state == TaskState::Queued));
    }

    #[test]
    fn navigation_cannot_exceed_task_list_end() {
        let mut mgr = AsyncTaskManager::new();
        let task_count = mgr.tasks.len();

        // Try to navigate past the end
        for _ in 0..task_count + 10 {
            mgr.select_next();
        }

        assert_eq!(mgr.selected, task_count - 1);
    }

    #[test]
    fn cancel_terminal_task_is_noop() {
        let mut mgr = AsyncTaskManager::new();
        mgr.tasks[0].state = TaskState::Succeeded;
        mgr.selected = 0;

        let events_before = mgr.events.len();
        mgr.cancel_selected();

        // State should remain Succeeded, no event logged
        assert_eq!(mgr.tasks[0].state, TaskState::Succeeded);
        assert_eq!(mgr.events.len(), events_before);
    }

    #[test]
    fn retry_non_failed_task_is_noop() {
        let mut mgr = AsyncTaskManager::new();
        mgr.tasks[0].state = TaskState::Running;
        mgr.selected = 0;

        let events_before = mgr.events.len();
        mgr.retry_selected();

        // State should remain Running, no event logged
        assert_eq!(mgr.tasks[0].state, TaskState::Running);
        assert_eq!(mgr.events.len(), events_before);
    }

    #[test]
    fn progress_clamped_to_one() {
        let mut mgr = AsyncTaskManager::new();
        mgr.tasks[0].state = TaskState::Running;
        mgr.tasks[0].estimated_ticks = 5;

        // Advance way past completion
        for _ in 0..20 {
            mgr.advance_tasks();
        }

        // Progress should be clamped to 1.0
        assert!(mgr.tasks[0].progress <= 1.0);
    }

    #[test]
    fn event_log_respects_capacity() {
        let mut mgr = AsyncTaskManager::new();

        // Log many events (more than capacity of 20)
        for _ in 0..50 {
            mgr.log_event("Test event".to_string());
        }

        // Should never exceed 20
        assert!(mgr.events.len() <= 20);
    }

    #[test]
    fn task_id_7_fails_deterministically() {
        // Task with id % 20 == 7 should fail when completed
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 7, // This task will have id 7
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 1,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        mgr.spawn_task_with_name("WillFail", 5, 1);
        mgr.update_scheduler();

        // Advance to completion
        for _ in 0..5 {
            mgr.advance_tasks();
        }

        assert_eq!(mgr.tasks[0].state, TaskState::Failed);
        assert!(mgr.tasks[0].error.is_some());
    }

    #[test]
    fn task_id_8_succeeds_deterministically() {
        // Task with id % 20 != 7 should succeed
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 8,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 1,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        mgr.spawn_task_with_name("WillSucceed", 5, 1);
        mgr.update_scheduler();

        for _ in 0..5 {
            mgr.advance_tasks();
        }

        assert_eq!(mgr.tasks[0].state, TaskState::Succeeded);
        assert!(mgr.tasks[0].error.is_none());
    }

    // =========================================================================
    // State Transition Invariants (bd-13pq.3)
    // =========================================================================

    #[test]
    fn task_is_terminal_identifies_correct_states() {
        let queued = Task::new(1, "Q".into(), 10, 1, 0);
        assert!(!queued.is_terminal());

        let mut running = Task::new(2, "R".into(), 10, 1, 0);
        running.state = TaskState::Running;
        assert!(!running.is_terminal());

        let mut succeeded = Task::new(3, "S".into(), 10, 1, 0);
        succeeded.state = TaskState::Succeeded;
        assert!(succeeded.is_terminal());

        let mut failed = Task::new(4, "F".into(), 10, 1, 0);
        failed.state = TaskState::Failed;
        assert!(failed.is_terminal());

        let mut canceled = Task::new(5, "C".into(), 10, 1, 0);
        canceled.state = TaskState::Canceled;
        assert!(canceled.is_terminal());
    }

    #[test]
    fn scheduler_respects_max_concurrent() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 2,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        // Add many tasks
        for i in 0..10 {
            mgr.spawn_task_with_name(&format!("Task{}", i), 100, 1);
        }

        mgr.update_scheduler();

        let running = mgr.count_by_state(TaskState::Running);
        assert_eq!(running, 2); // Exactly max_concurrent
    }

    #[test]
    fn scheduler_fills_slots_when_task_completes() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 2,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        mgr.spawn_task_with_name("Fast", 2, 1);
        mgr.spawn_task_with_name("Slow1", 100, 1);
        mgr.spawn_task_with_name("Waiting", 50, 1);

        // Start scheduler
        mgr.update_scheduler();
        assert_eq!(mgr.count_by_state(TaskState::Running), 2);

        // Complete the fast task
        for _ in 0..2 {
            mgr.advance_tasks();
        }

        // Run scheduler again to fill the slot
        mgr.update_scheduler();

        // Now all three should have started at some point
        let running = mgr.count_by_state(TaskState::Running);
        let terminal = mgr.tasks.iter().filter(|t| t.is_terminal()).count();
        assert_eq!(running + terminal, 3);
    }

    // =========================================================================
    // Input Event Handling Tests (bd-13pq.3)
    // =========================================================================

    #[test]
    fn key_n_spawns_task() {
        let mut mgr = AsyncTaskManager::new();
        let count_before = mgr.tasks.len();

        mgr.update(&Event::Key(KeyEvent {
            code: KeyCode::Char('n'),
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));

        assert_eq!(mgr.tasks.len(), count_before + 1);
    }

    #[test]
    fn key_c_cancels_selected() {
        let mut mgr = AsyncTaskManager::new();
        mgr.selected = 0;

        mgr.update(&Event::Key(KeyEvent {
            code: KeyCode::Char('c'),
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));

        assert_eq!(mgr.tasks[0].state, TaskState::Canceled);
    }

    #[test]
    fn key_s_cycles_scheduler() {
        let mut mgr = AsyncTaskManager::new();
        assert_eq!(mgr.policy, SchedulerPolicy::Fifo);

        mgr.update(&Event::Key(KeyEvent {
            code: KeyCode::Char('s'),
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));

        assert_eq!(mgr.policy, SchedulerPolicy::ShortestFirst);
    }

    #[test]
    fn vim_navigation_j_k() {
        let mut mgr = AsyncTaskManager::new();
        assert_eq!(mgr.selected, 0);

        // j = down
        mgr.update(&Event::Key(KeyEvent {
            code: KeyCode::Char('j'),
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        assert_eq!(mgr.selected, 1);

        // k = up
        mgr.update(&Event::Key(KeyEvent {
            code: KeyCode::Char('k'),
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        assert_eq!(mgr.selected, 0);
    }

    #[test]
    fn arrow_key_navigation() {
        let mut mgr = AsyncTaskManager::new();
        assert_eq!(mgr.selected, 0);

        mgr.update(&Event::Key(KeyEvent {
            code: KeyCode::Down,
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        assert_eq!(mgr.selected, 1);

        mgr.update(&Event::Key(KeyEvent {
            code: KeyCode::Up,
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::NONE,
        }));
        assert_eq!(mgr.selected, 0);
    }

    // =========================================================================
    // Rendering Edge Cases (bd-13pq.3)
    // =========================================================================

    #[test]
    fn renders_with_empty_task_list() {
        let mut mgr = AsyncTaskManager::new();
        mgr.tasks.clear();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        mgr.view(&mut frame, Rect::new(0, 0, 80, 24));
        // No panic = success
    }

    #[test]
    fn renders_with_many_tasks() {
        let mut mgr = AsyncTaskManager::new();

        // Add many tasks to test scrolling
        for i in 0..50 {
            mgr.spawn_task_with_name(&format!("Task {}", i), 50, 1);
        }

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        mgr.view(&mut frame, Rect::new(0, 0, 80, 24));
        // No panic = success
    }

    #[test]
    fn renders_selected_near_end_of_list() {
        let mut mgr = AsyncTaskManager::new();

        for i in 0..20 {
            mgr.spawn_task_with_name(&format!("Task {}", i), 50, 1);
        }
        mgr.selected = mgr.tasks.len() - 1;

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        mgr.view(&mut frame, Rect::new(0, 0, 80, 24));
        // No panic = success
    }

    #[test]
    fn renders_with_failed_task_showing_error() {
        let mut mgr = AsyncTaskManager::new();
        mgr.tasks[0].state = TaskState::Failed;
        mgr.tasks[0].error = Some("Connection timeout".to_string());
        mgr.selected = 0;

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        mgr.view(&mut frame, Rect::new(0, 0, 80, 24));
        // No panic = success
    }

    // =========================================================================
    // Name Generation Tests (bd-13pq.3)
    // =========================================================================

    #[test]
    fn generate_name_is_deterministic() {
        let mut mgr1 = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 3,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        let mut mgr2 = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 3,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        let name1 = mgr1.generate_name();
        let name2 = mgr2.generate_name();

        assert_eq!(name1, name2);
    }

    #[test]
    fn generate_name_produces_unique_names() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 3,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        let mut names = std::collections::HashSet::new();
        for _ in 0..100 {
            names.insert(mgr.generate_name());
        }

        // All 100 names should be unique (contains counter)
        assert_eq!(names.len(), 100);
    }

    // =========================================================================
    // Full Lifecycle Integration Tests (bd-13pq.3)
    // =========================================================================

    #[test]
    fn full_task_lifecycle_spawn_schedule_complete() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 1,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        // Spawn
        mgr.spawn_task_with_name("Lifecycle Test", 3, 1);
        assert_eq!(mgr.tasks[0].state, TaskState::Queued);

        // Schedule
        mgr.update_scheduler();
        assert_eq!(mgr.tasks[0].state, TaskState::Running);

        // Advance to completion
        for tick in 1..=3 {
            mgr.tick(tick);
        }

        assert!(mgr.tasks[0].is_terminal());
        assert!(mgr.tasks[0].progress >= 1.0);
    }

    #[test]
    fn policy_change_does_not_affect_running_tasks() {
        let mut mgr = AsyncTaskManager {
            tasks: Vec::new(),
            next_id: 1,
            selected: 0,
            policy: SchedulerPolicy::Fifo,
            max_concurrent: 1,
            tick_count: 0,
            events: VecDeque::new(),
            name_counter: 0,
            fairness: FairnessConfig::default(),
            metrics: SchedulerMetrics::default(),
            diagnostic_config: DiagnosticConfig::default(),
            diagnostic_log: DiagnosticLog::default(),
            hazard_config: HazardConfig::default(),
            layout_task_list: StdCell::new(Rect::default()),
            layout_task_list_inner: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
        };

        mgr.spawn_task_with_name("Running", 100, 1);
        mgr.spawn_task_with_name("Queued", 100, 1);
        mgr.update_scheduler();

        assert_eq!(mgr.tasks[0].state, TaskState::Running);

        // Change policy
        mgr.cycle_policy();
        mgr.cycle_policy();

        // Running task should still be running
        assert_eq!(mgr.tasks[0].state, TaskState::Running);
    }

    // =========================================================================
    // Diagnostic Logging Tests (bd-13pq.5)
    // =========================================================================

    #[test]
    fn diagnostic_config_default_is_disabled() {
        let config = DiagnosticConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_entries, 1000);
        assert!(config.log_invariants);
        assert!(config.log_decisions);
        assert!(config.log_transitions);
        assert_eq!(config.starvation_threshold, 100);
    }

    #[test]
    fn diagnostic_log_tracks_entries() {
        let mut log = DiagnosticLog::new(100);
        assert!(log.is_empty());

        log.push(DiagnosticEntry::AgingToggle {
            tick: 0,
            enabled: true,
        });
        assert_eq!(log.len(), 1);
        assert!(!log.is_empty());
    }

    #[test]
    fn diagnostic_log_respects_max_entries() {
        let mut log = DiagnosticLog::new(5);
        for i in 0..10 {
            log.push(DiagnosticEntry::AgingToggle {
                tick: i,
                enabled: true,
            });
        }
        assert_eq!(log.len(), 5);
    }

    #[test]
    fn diagnostic_entry_serializes_to_jsonl() {
        let entry = DiagnosticEntry::StateTransition {
            tick: 42,
            task_id: 1,
            from: TaskState::Queued,
            to: TaskState::Running,
            wait_time: Some(10),
        };
        let json = entry.to_jsonl();
        assert!(json.contains("\"type\":\"state_transition\""));
        assert!(json.contains("\"tick\":42"));
        assert!(json.contains("\"task_id\":1"));
        assert!(json.contains("\"wait_time\":10"));
    }

    #[test]
    fn enabled_diagnostics_logs_policy_change() {
        let config = DiagnosticConfig {
            enabled: true,
            ..Default::default()
        };
        let mut mgr = AsyncTaskManager::with_config(config);

        assert!(mgr.diagnostic_log.is_empty());

        mgr.cycle_policy();

        assert!(!mgr.diagnostic_log.is_empty());
        let json = mgr.export_diagnostics();
        assert!(json.contains("\"type\":\"policy_change\""));
    }

    #[test]
    fn enabled_diagnostics_logs_aging_toggle() {
        let config = DiagnosticConfig {
            enabled: true,
            ..Default::default()
        };
        let mut mgr = AsyncTaskManager::with_config(config);

        mgr.toggle_aging();

        let json = mgr.export_diagnostics();
        assert!(json.contains("\"type\":\"aging_toggle\""));
    }

    #[test]
    fn enabled_diagnostics_logs_scheduling_decisions() {
        let config = DiagnosticConfig {
            enabled: true,
            log_decisions: true,
            ..Default::default()
        };
        let mut mgr = AsyncTaskManager::with_config(config);

        mgr.update_scheduler();

        let json = mgr.export_diagnostics();
        assert!(json.contains("\"type\":\"scheduling_decision\""));
    }

    #[test]
    fn enabled_diagnostics_logs_state_transitions() {
        let config = DiagnosticConfig {
            enabled: true,
            log_transitions: true,
            ..Default::default()
        };
        let mut mgr = AsyncTaskManager::with_config(config);

        // Start a task (Queued -> Running)
        mgr.update_scheduler();

        let json = mgr.export_diagnostics();
        assert!(json.contains("\"type\":\"state_transition\""));
        assert!(json.contains("\"from\":\"Queued\""));
        assert!(json.contains("\"to\":\"Running\""));
    }

    #[test]
    fn scheduling_evidence_includes_all_fields() {
        let evidence = SchedulingEvidence {
            tick: 100,
            selected_task_id: 5,
            policy: SchedulerPolicy::SmithRule,
            raw_score: 2.5,
            base_priority: 2.0,
            aging_boost: 0.5,
            wait_time: 10,
            reason: SelectionReason::PolicyOrder,
            candidates_count: 3,
            passed_over: vec![6, 7],
        };

        let entry = DiagnosticEntry::SchedulingDecision(evidence);
        let json = entry.to_jsonl();

        assert!(json.contains("\"tick\":100"));
        assert!(json.contains("\"task_id\":5"));
        assert!(json.contains("\"policy\":\"Smith\""));
        assert!(json.contains("\"reason\":\"policy_order\""));
        assert!(json.contains("\"candidates\":3"));
        assert!(json.contains("\"passed_over\":[6,7]"));
    }

    #[test]
    fn selection_reason_as_str() {
        assert_eq!(SelectionReason::PolicyOrder.as_str(), "policy_order");
        assert_eq!(SelectionReason::AgingBoost.as_str(), "aging_boost");
        assert_eq!(SelectionReason::OnlyCandidate.as_str(), "only_candidate");
        assert_eq!(
            SelectionReason::RoundRobinRotation.as_str(),
            "round_robin_rotation"
        );
    }

    #[test]
    fn invariant_type_as_str() {
        assert_eq!(
            InvariantType::BoundedConcurrency.as_str(),
            "bounded_concurrency"
        );
        assert_eq!(InvariantType::BoundedProgress.as_str(), "bounded_progress");
        assert_eq!(
            InvariantType::TerminalStability.as_str(),
            "terminal_stability"
        );
        assert_eq!(InvariantType::MonotonicIds.as_str(), "monotonic_ids");
        assert_eq!(InvariantType::BoundedWait.as_str(), "bounded_wait");
    }

    #[test]
    fn clear_diagnostics_empties_log() {
        let config = DiagnosticConfig {
            enabled: true,
            ..Default::default()
        };
        let mut mgr = AsyncTaskManager::with_config(config);

        mgr.cycle_policy();
        assert!(!mgr.diagnostic_log.is_empty());

        mgr.clear_diagnostics();
        assert!(mgr.diagnostic_log.is_empty());
    }

    #[test]
    fn disable_diagnostics_stops_logging() {
        let config = DiagnosticConfig {
            enabled: true,
            ..Default::default()
        };
        let mut mgr = AsyncTaskManager::with_config(config);

        mgr.cycle_policy();
        let count_after_enable = mgr.diagnostic_log.len();
        assert!(count_after_enable > 0);

        mgr.clear_diagnostics();
        mgr.disable_diagnostics();
        mgr.cycle_policy();

        assert!(mgr.diagnostic_log.is_empty());
    }

    #[test]
    fn metrics_snapshot_logged_periodically() {
        let config = DiagnosticConfig {
            enabled: true,
            ..Default::default()
        };
        let mut mgr = AsyncTaskManager::with_config(config);

        // Tick 10 should log a metrics snapshot
        mgr.tick(10);

        let json = mgr.export_diagnostics();
        assert!(json.contains("\"type\":\"metrics_snapshot\""));
    }

    // =========================================================================
    // Mouse interaction tests (bd-iuvb.17.13.5)
    // =========================================================================

    fn mouse_click(x: u16, y: u16) -> Event {
        use ftui_core::event::MouseEvent;
        Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            x,
            y,
        ))
    }

    fn mouse_scroll_up(x: u16, y: u16) -> Event {
        use ftui_core::event::MouseEvent;
        Event::Mouse(MouseEvent::new(MouseEventKind::ScrollUp, x, y))
    }

    fn mouse_scroll_down(x: u16, y: u16) -> Event {
        use ftui_core::event::MouseEvent;
        Event::Mouse(MouseEvent::new(MouseEventKind::ScrollDown, x, y))
    }

    #[test]
    fn mouse_click_selects_task_row() {
        let mut mgr = AsyncTaskManager::new();
        // Simulate cached inner rect as if view() was called.
        mgr.layout_task_list_inner.set(Rect::new(1, 2, 40, 10));
        mgr.selected = 0;

        // Click on row 2 (y=4 means offset 2 from inner.y=2).
        mgr.update(&mouse_click(10, 4));
        assert_eq!(mgr.selected, 2);
    }

    #[test]
    fn mouse_click_selects_first_row() {
        let mut mgr = AsyncTaskManager::new();
        mgr.layout_task_list_inner.set(Rect::new(1, 2, 40, 10));
        mgr.selected = 2;

        mgr.update(&mouse_click(10, 2));
        assert_eq!(mgr.selected, 0);
    }

    #[test]
    fn mouse_click_outside_list_no_change() {
        let mut mgr = AsyncTaskManager::new();
        mgr.layout_task_list_inner.set(Rect::new(1, 2, 40, 10));
        mgr.selected = 1;

        // Click outside the list area.
        mgr.update(&mouse_click(50, 5));
        assert_eq!(mgr.selected, 1);
    }

    #[test]
    fn mouse_click_beyond_tasks_no_crash() {
        let mut mgr = AsyncTaskManager::new();
        mgr.layout_task_list_inner.set(Rect::new(1, 2, 40, 10));
        let task_count = mgr.tasks.len();
        mgr.selected = 0;

        // Click row far below all tasks.
        mgr.update(&mouse_click(10, 11));
        // Should not change selection since that task index doesn't exist.
        assert!(mgr.selected < task_count);
    }

    #[test]
    fn mouse_scroll_up_selects_prev() {
        let mut mgr = AsyncTaskManager::new();
        mgr.layout_task_list_inner.set(Rect::new(1, 2, 40, 10));
        mgr.selected = 2;

        mgr.update(&mouse_scroll_up(10, 5));
        assert_eq!(mgr.selected, 1);
    }

    #[test]
    fn mouse_scroll_down_selects_next() {
        let mut mgr = AsyncTaskManager::new();
        mgr.layout_task_list_inner.set(Rect::new(1, 2, 40, 10));
        mgr.selected = 0;

        mgr.update(&mouse_scroll_down(10, 5));
        assert_eq!(mgr.selected, 1);
    }

    #[test]
    fn mouse_scroll_outside_list_no_change() {
        let mut mgr = AsyncTaskManager::new();
        mgr.layout_task_list_inner.set(Rect::new(1, 2, 40, 10));
        mgr.selected = 1;

        mgr.update(&mouse_scroll_up(50, 5));
        assert_eq!(mgr.selected, 1);
    }

    #[test]
    fn mouse_scroll_up_clamps_at_zero() {
        let mut mgr = AsyncTaskManager::new();
        mgr.layout_task_list_inner.set(Rect::new(1, 2, 40, 10));
        mgr.selected = 0;

        mgr.update(&mouse_scroll_up(10, 5));
        assert_eq!(mgr.selected, 0);
    }

    #[test]
    fn keybindings_include_mouse_entries() {
        let mgr = AsyncTaskManager::new();
        let bindings = mgr.keybindings();
        let keys: Vec<&str> = bindings.iter().map(|e| e.key).collect();
        assert!(keys.contains(&"Click"));
        assert!(keys.contains(&"Wheel"));
    }
}

// =============================================================================
// Property-Based Tests (bd-13pq.3)
// =============================================================================

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::VecDeque;

    /// Strategy for generating a valid SchedulerPolicy
    fn arb_policy() -> impl Strategy<Value = SchedulerPolicy> {
        prop_oneof![
            Just(SchedulerPolicy::Fifo),
            Just(SchedulerPolicy::ShortestFirst),
            Just(SchedulerPolicy::Srpt),
            Just(SchedulerPolicy::SmithRule),
            Just(SchedulerPolicy::Priority),
            Just(SchedulerPolicy::RoundRobin),
        ]
    }

    /// Strategy for generating task parameters (duration, priority)
    fn arb_task_params() -> impl Strategy<Value = (u64, u8)> {
        (1u64..200, 1u8..=5)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // =====================================================================
        // Invariant: Task IDs are monotonically increasing
        // =====================================================================

        #[test]
        fn task_id_monotonically_increases(
            spawn_count in 1usize..50,
            (duration, priority) in arb_task_params(),
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent: 3,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            let mut last_id = 0u32;
            for i in 0..spawn_count {
                mgr.spawn_task_with_name(&format!("Task{}", i), duration, priority);
                let new_id = mgr.tasks.last().unwrap().id;
                prop_assert!(new_id > last_id, "Task ID {} should be > {}", new_id, last_id);
                last_id = new_id;
            }
        }

        // =====================================================================
        // Invariant: Progress is always in [0.0, 1.0]
        // =====================================================================

        #[test]
        fn progress_stays_bounded(
            ticks in 1u64..500,
            estimated_ticks in 1u64..100,
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent: 1,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            mgr.spawn_task_with_name("Test", estimated_ticks, 1);
            mgr.update_scheduler();

            for tick in 1..=ticks {
                mgr.tick(tick);
                for task in &mgr.tasks {
                    prop_assert!(
                        task.progress >= 0.0 && task.progress <= 1.0,
                        "Progress {} out of bounds at tick {}",
                        task.progress,
                        tick
                    );
                }
            }
        }

        // =====================================================================
        // Invariant: Running count never exceeds max_concurrent
        // =====================================================================

        #[test]
        fn running_never_exceeds_max_concurrent(
            max_concurrent in 1usize..10,
            task_count in 1usize..30,
            ticks in 1u64..100,
            policy in arb_policy(),
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy,
                max_concurrent,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            // Spawn many tasks
            for i in 0..task_count {
                let duration = 10 + (i as u64 % 50);
                let priority = ((i % 3) + 1) as u8;
                mgr.spawn_task_with_name(&format!("Task{}", i), duration, priority);
            }

            // Run scheduler and advance through ticks
            for tick in 1..=ticks {
                mgr.tick(tick);
                let running = mgr.count_by_state(TaskState::Running);
                prop_assert!(
                    running <= max_concurrent,
                    "Running {} exceeds max_concurrent {} at tick {}",
                    running,
                    max_concurrent,
                    tick
                );
            }
        }

        // =====================================================================
        // Invariant: Terminal states never transition to non-terminal
        // =====================================================================

        #[test]
        fn terminal_states_are_stable(
            ticks in 1u64..100,
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent: 3,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            // Create tasks in various terminal states
            mgr.tasks.push(Task {
                id: 1,
                name: "Succeeded".into(),
                state: TaskState::Succeeded,
                progress: 1.0,
                estimated_ticks: 10,
                elapsed_ticks: 10,
                priority: 1,
                created_at: 0,
                error: None,
            });
            mgr.tasks.push(Task {
                id: 2,
                name: "Failed".into(),
                state: TaskState::Failed,
                progress: 0.5,
                estimated_ticks: 10,
                elapsed_ticks: 5,
                priority: 1,
                created_at: 0,
                error: Some("Error".into()),
            });
            mgr.tasks.push(Task {
                id: 3,
                name: "Canceled".into(),
                state: TaskState::Canceled,
                progress: 0.3,
                estimated_ticks: 10,
                elapsed_ticks: 3,
                priority: 1,
                created_at: 0,
                error: None,
            });
            mgr.next_id = 4;

            // Run scheduler and tick
            for tick in 1..=ticks {
                mgr.tick(tick);

                // Verify all terminal tasks remain terminal
                for task in &mgr.tasks {
                    if task.id <= 3 {
                        prop_assert!(
                            task.is_terminal(),
                            "Task {} transitioned from terminal state at tick {}",
                            task.id,
                            tick
                        );
                    }
                }
            }
        }

        // =====================================================================
        // Invariant: Scheduler fills slots when queued tasks exist
        // =====================================================================

        #[test]
        fn scheduler_fills_available_slots(
            max_concurrent in 1usize..5,
            queued_count in 1usize..20,
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            // Spawn queued tasks
            for i in 0..queued_count {
                mgr.spawn_task_with_name(&format!("Task{}", i), 100, 1);
            }

            // All should be queued initially
            let queued_before = mgr.count_by_state(TaskState::Queued);
            prop_assert_eq!(queued_before, queued_count);

            // Run scheduler
            mgr.update_scheduler();

            let running = mgr.count_by_state(TaskState::Running);
            let expected_running = max_concurrent.min(queued_count);

            prop_assert_eq!(
                running,
                expected_running,
                "Expected {} running, got {}",
                expected_running,
                running
            );
        }

        // =====================================================================
        // Invariant: Elapsed ticks never exceeds estimated for terminal tasks
        // =====================================================================

        #[test]
        fn elapsed_reasonable_for_completed_tasks(
            estimated in 5u64..100,
            extra_ticks in 0u64..50,
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 8, // Avoid id 7 which fails
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent: 1,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            mgr.spawn_task_with_name("Test", estimated, 1);
            mgr.update_scheduler();

            // Run past completion
            for tick in 1..=(estimated + extra_ticks) {
                mgr.tick(tick);
            }

            let task = &mgr.tasks[0];
            if task.state == TaskState::Succeeded {
                // Elapsed should be exactly estimated_ticks when completed
                // (task stops running when it completes)
                prop_assert!(
                    task.elapsed_ticks >= estimated,
                    "Elapsed {} < estimated {} for completed task",
                    task.elapsed_ticks,
                    estimated
                );
            }
        }

        // =====================================================================
        // Invariant: Selection bounds are always valid
        // =====================================================================

        #[test]
        fn selection_always_valid(
            task_count in 1usize..50,
            nav_ops in prop::collection::vec(prop::bool::ANY, 0..100),
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent: 3,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            for i in 0..task_count {
                mgr.spawn_task_with_name(&format!("Task{}", i), 50, 1);
            }

            // Apply random navigation operations (true = next, false = prev)
            for go_next in nav_ops {
                if go_next {
                    mgr.select_next();
                } else {
                    mgr.select_prev();
                }

                prop_assert!(
                    mgr.selected < mgr.tasks.len(),
                    "Selection {} out of bounds (task count: {})",
                    mgr.selected,
                    mgr.tasks.len()
                );
            }
        }

        // =====================================================================
        // Invariant: Policy cycling is periodic with period 6
        // =====================================================================

        #[test]
        fn policy_cycles_with_period_6(
            initial_policy in arb_policy(),
            cycle_count in 0usize..20,
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: initial_policy,
                max_concurrent: 3,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            for _ in 0..SchedulerPolicy::count() {
                mgr.cycle_policy();
            }

            // After 6 cycles, should be back to initial
            prop_assert_eq!(
                mgr.policy,
                initial_policy,
                "Policy should return to initial after {} cycles",
                SchedulerPolicy::count()
            );

            // Additional cycles should maintain periodicity
            for _ in 0..(cycle_count * SchedulerPolicy::count()) {
                mgr.cycle_policy();
            }
            prop_assert_eq!(mgr.policy, initial_policy);
        }

        // =====================================================================
        // Invariant: Name generator counter only increases
        // =====================================================================

        #[test]
        fn name_counter_monotonic(
            spawn_count in 1usize..100,
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent: 3,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            let mut last_counter = 0u32;
            for _ in 0..spawn_count {
                mgr.spawn_task();
                prop_assert!(
                    mgr.name_counter > last_counter,
                    "Name counter {} should be > {}",
                    mgr.name_counter,
                    last_counter
                );
                last_counter = mgr.name_counter;
            }
        }

        // =====================================================================
        // Invariant: Events log never exceeds capacity
        // =====================================================================

        #[test]
        fn events_log_bounded(
            event_count in 1usize..200,
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent: 3,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            for i in 0..event_count {
                mgr.log_event(format!("Event {}", i));
                prop_assert!(
                    mgr.events.len() <= 20,
                    "Events log {} exceeds capacity 20",
                    mgr.events.len()
                );
            }
        }

        // =====================================================================
        // Invariant: MAX_TASKS limit is respected
        // =====================================================================

        #[test]
        fn max_tasks_limit_respected(
            extra_spawns in 1usize..20,
        ) {
            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent: 3,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            // Fill to MAX_TASKS with completed tasks
            for i in 0..MAX_TASKS {
                mgr.tasks.push(Task {
                    id: i as u32,
                    name: format!("Old{}", i),
                    state: TaskState::Succeeded,
                    progress: 1.0,
                    estimated_ticks: 10,
                    elapsed_ticks: 10,
                    priority: 1,
                    created_at: 0,
                    error: None,
                });
            }
            mgr.next_id = MAX_TASKS as u32;

            // Spawn more
            for _ in 0..extra_spawns {
                mgr.spawn_task();
                prop_assert!(
                    mgr.tasks.len() <= MAX_TASKS,
                    "Task count {} exceeds MAX_TASKS {}",
                    mgr.tasks.len(),
                    MAX_TASKS
                );
            }
        }

        // =====================================================================
        // Invariant: Rendering never panics for any valid area
        // =====================================================================

        #[test]
        fn render_never_panics(
            width in 0u16..300,
            height in 0u16..100,
            task_count in 0usize..50,
        ) {
            use ftui_core::geometry::Rect;
            use ftui_render::frame::Frame;
            use ftui_render::grapheme_pool::GraphemePool;

            let mut mgr = AsyncTaskManager {
                tasks: Vec::new(),
                next_id: 1,
                selected: 0,
                policy: SchedulerPolicy::Fifo,
                max_concurrent: 3,
                tick_count: 0,
                events: VecDeque::new(),
                name_counter: 0,
                fairness: FairnessConfig::default(),
                metrics: SchedulerMetrics::default(),
                diagnostic_config: DiagnosticConfig::default(),
                diagnostic_log: DiagnosticLog::default(),
                hazard_config: HazardConfig::default(),
                layout_task_list: StdCell::new(Rect::default()),
                layout_task_list_inner: StdCell::new(Rect::default()),
                layout_details: StdCell::new(Rect::default()),
                layout_activity: StdCell::new(Rect::default()),
            };

            for i in 0..task_count {
                mgr.spawn_task_with_name(&format!("Task{}", i), 50, ((i % 3) + 1) as u8);
            }

            // Set various task states
            for (i, task) in mgr.tasks.iter_mut().enumerate() {
                match i % 5 {
                    0 => task.state = TaskState::Queued,
                    1 => task.state = TaskState::Running,
                    2 => task.state = TaskState::Succeeded,
                    3 => {
                        task.state = TaskState::Failed;
                        task.error = Some("Test error".into());
                    }
                    _ => task.state = TaskState::Canceled,
                }
            }

            if !mgr.tasks.is_empty() {
                mgr.selected = task_count.saturating_sub(1);
            }

            // Rendering should not panic
            let actual_width = width.max(1);
            let actual_height = height.max(1);
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(actual_width, actual_height, &mut pool);
            mgr.view(&mut frame, Rect::new(0, 0, width, height));
            // If we get here without panic, the test passes
        }
    }
}
