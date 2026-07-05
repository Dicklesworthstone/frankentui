//! Supervised orchestration substrate (bd-1dccp / bd-11ngr).
//!
//! A reusable, deterministic, evidence-emitting supervision primitive for the
//! `doctor_frankentui` orchestration lanes (seed networking, subprocess capture,
//! suite fan-out). Today each lane re-implements its own retry / deadline /
//! cancellation logic (seed.rs has a private `Deadline` + `RetryPolicy`; capture
//! / doctor / suite use ad-hoc `wait-timeout` polling and `AtomicBool` flags) and
//! none of them emit a structured, replayable supervision record. This module
//! generalises that pattern into one composable primitive that:
//!
//! * honours an explicit [`SupervisionBudget`] (deadline + bounded attempts),
//! * applies a deterministic [`Backoff`] schedule,
//! * is cancellation-aware via [`ftui_runtime::cancellation::CancellationToken`],
//! * preserves a four-valued outcome severity lattice
//!   ([`StepOutcome`]: `Succeeded < Failed < TimedOut < Cancelled`), mirroring
//!   the Asupersync `Outcome`/`CancelReason` discipline, and
//! * emits a deterministic [`SupervisionRecord`] whose [`SupervisionRecord::evidence_terms`]
//!   feed the existing [`crate::runmeta::DecisionRecord`] JSONL ledger so deadline,
//!   retry, timeout, and cancellation events become triage-able rather than
//!   ad-hoc stderr lines.
//!
//! # Determinism
//!
//! The generic [`supervise`] decision core is pure: wall-clock time is injected
//! via the [`SupervisionClock`] trait, so retry / deadline / cancellation
//! behaviour is unit-tested with a [`MockClock`] and never depends on real
//! timing. Wall-clock durations are deliberately excluded from the record's
//! deterministic fingerprint ([`SupervisionRecord::evidence_terms`]); only
//! configured budgets and attempt-outcome *classes* appear there.

use std::path::PathBuf;
use std::time::Duration;

use ftui_runtime::cancellation::CancellationToken;
use serde::{Deserialize, Serialize};

use crate::runmeta::DecisionRecord;

// ============================================================================
// Cancellation + outcome vocabulary (Asupersync severity discipline)
// ============================================================================

/// Why a supervised step stopped early. Mirrors the Asupersync `CancelReason`
/// vocabulary, narrowed to what doctor lanes actually distinguish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    /// The caller's [`CancellationToken`] fired (operator abort / parent cancel).
    Caller,
    /// The overall budget deadline elapsed before completion.
    Deadline,
    /// Process-wide shutdown requested.
    Shutdown,
}

impl CancelReason {
    /// Stable lower-case label for evidence terms.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            CancelReason::Caller => "caller",
            CancelReason::Deadline => "deadline",
            CancelReason::Shutdown => "shutdown",
        }
    }
}

/// Final outcome of a supervised step, ordered by severity:
/// `Succeeded < Failed < TimedOut < Cancelled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StepOutcome {
    /// The step completed successfully.
    Succeeded,
    /// The step failed permanently (no retries left, or a fatal error).
    Failed {
        /// Redaction-safe failure detail (caller is responsible for redaction).
        reason: String,
    },
    /// The step did not complete within the budget deadline.
    TimedOut,
    /// The step was cancelled.
    Cancelled {
        /// Why the step was cancelled.
        reason: CancelReason,
    },
}

impl StepOutcome {
    /// Severity rank used for the lattice ordering (lower = better).
    #[must_use]
    pub fn severity(&self) -> u8 {
        match self {
            StepOutcome::Succeeded => 0,
            StepOutcome::Failed { .. } => 1,
            StepOutcome::TimedOut => 2,
            StepOutcome::Cancelled { .. } => 3,
        }
    }

    /// Whether the step ultimately succeeded.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, StepOutcome::Succeeded)
    }

    /// Stable lower-case label for evidence terms.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            StepOutcome::Succeeded => "succeeded",
            StepOutcome::Failed { .. } => "failed",
            StepOutcome::TimedOut => "timed_out",
            StepOutcome::Cancelled { .. } => "cancelled",
        }
    }
}

impl PartialOrd for StepOutcome {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StepOutcome {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.severity().cmp(&other.severity())
    }
}

/// What a single attempt produced. The supervisor maps these into retries,
/// stops, and the final [`StepOutcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptResult {
    /// The attempt succeeded; stop with [`StepOutcome::Succeeded`].
    Ok,
    /// A transient failure; retry if the budget allows.
    Retryable(String),
    /// A permanent failure; stop immediately with [`StepOutcome::Failed`].
    Fatal(String),
}

/// The class of a single recorded attempt (deterministic; no timing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcomeKind {
    /// Attempt succeeded.
    Ok,
    /// Attempt failed transiently (eligible for retry).
    Retryable,
    /// Attempt failed permanently.
    Fatal,
    /// Attempt slot was skipped because the deadline elapsed.
    TimedOut,
    /// Attempt slot was skipped because cancellation fired.
    Cancelled,
}

impl AttemptOutcomeKind {
    /// Stable lower-case label for evidence terms.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            AttemptOutcomeKind::Ok => "ok",
            AttemptOutcomeKind::Retryable => "retryable",
            AttemptOutcomeKind::Fatal => "fatal",
            AttemptOutcomeKind::TimedOut => "timed_out",
            AttemptOutcomeKind::Cancelled => "cancelled",
        }
    }
}

/// Deterministic evidence for a single attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptEvidence {
    /// 1-based attempt index.
    pub index: u32,
    /// What class the attempt resolved to.
    pub outcome: AttemptOutcomeKind,
    /// Optional redaction-safe detail (caller redacts before passing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ============================================================================
// Budget + backoff (configured, deterministic)
// ============================================================================

/// How much work a supervised step may consume. Mirrors the Asupersync `Budget`
/// idea (deadline + bounded effort); `meet` takes the tighter constraint so a
/// child step inherits a budget no looser than its caller's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisionBudget {
    /// Total wall-clock budget across all attempts, in milliseconds.
    pub deadline_ms: u64,
    /// Maximum number of attempts (always >= 1).
    pub max_attempts: u32,
}

impl SupervisionBudget {
    /// Build a budget from a [`Duration`] deadline and an attempt cap.
    #[must_use]
    pub fn new(deadline: Duration, max_attempts: u32) -> Self {
        Self {
            deadline_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
            max_attempts: max_attempts.max(1),
        }
    }

    /// The deadline as a [`Duration`].
    #[must_use]
    pub fn deadline(&self) -> Duration {
        Duration::from_millis(self.deadline_ms)
    }

    /// Combine two budgets by taking the tighter constraint on each field
    /// (shorter deadline, fewer attempts). Mirrors Asupersync `Budget::meet`.
    #[must_use]
    pub fn meet(self, other: Self) -> Self {
        Self {
            deadline_ms: self.deadline_ms.min(other.deadline_ms),
            max_attempts: self.max_attempts.min(other.max_attempts).max(1),
        }
    }
}

/// Backoff growth shape between attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackoffKind {
    /// Constant delay each retry.
    Fixed,
    /// Delay doubles each retry (base, 2·base, 4·base, ...), capped at `max_ms`.
    Exponential,
}

/// A deterministic backoff schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backoff {
    /// Base delay in milliseconds.
    pub base_ms: u64,
    /// Growth shape.
    pub kind: BackoffKind,
    /// Maximum delay in milliseconds (clamp).
    pub max_ms: u64,
}

impl Backoff {
    /// A no-delay backoff (every attempt runs immediately).
    #[must_use]
    pub fn none() -> Self {
        Self {
            base_ms: 0,
            kind: BackoffKind::Fixed,
            max_ms: 0,
        }
    }

    /// A fixed-delay backoff.
    #[must_use]
    pub fn fixed(base_ms: u64) -> Self {
        Self {
            base_ms,
            kind: BackoffKind::Fixed,
            max_ms: base_ms,
        }
    }

    /// An exponential backoff clamped at `max_ms`.
    #[must_use]
    pub fn exponential(base_ms: u64, max_ms: u64) -> Self {
        Self {
            base_ms,
            kind: BackoffKind::Exponential,
            max_ms,
        }
    }

    /// Deterministic delay to wait *before* the given 1-based attempt.
    ///
    /// `delay_before(1) == 0` (the first attempt never waits). For attempt
    /// `n > 1` the delay reflects `n - 1` prior failures: `base` after the
    /// first failure, then doubling for [`BackoffKind::Exponential`].
    #[must_use]
    pub fn delay_before(self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return Duration::ZERO;
        }
        let prior_failures = attempt - 1;
        let ms = match self.kind {
            BackoffKind::Fixed => self.base_ms,
            BackoffKind::Exponential => {
                let shift = (prior_failures - 1).min(20);
                self.base_ms.saturating_mul(1_u64 << shift)
            }
        };
        Duration::from_millis(ms.min(self.max_ms.max(self.base_ms)))
    }

    /// Stable label for evidence terms.
    #[must_use]
    pub fn label(self) -> String {
        match self.kind {
            BackoffKind::Fixed => format!("fixed:{}ms", self.base_ms),
            BackoffKind::Exponential => format!("exp:{}..{}ms", self.base_ms, self.max_ms),
        }
    }
}

// ============================================================================
// Injected clock (purity boundary)
// ============================================================================

/// Time + cancellable-sleep injection so the supervisor's decision logic is
/// pure and unit-testable without wall-clock dependence.
pub trait SupervisionClock {
    /// Elapsed time since the supervised run started.
    fn elapsed(&self) -> Duration;

    /// Sleep up to `dur`, returning early if `token` is cancelled. Returns
    /// `true` if cancellation was observed during the wait.
    fn sleep_cancellable(&mut self, dur: Duration, token: &CancellationToken) -> bool;
}

/// Real-time clock backed by [`std::time::Instant`] and the cancellation
/// token's `wait_timeout`.
pub struct RealClock {
    start: std::time::Instant,
}

impl RealClock {
    /// Start a real clock now.
    #[must_use]
    pub fn start() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::start()
    }
}

impl SupervisionClock for RealClock {
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    fn sleep_cancellable(&mut self, dur: Duration, token: &CancellationToken) -> bool {
        if dur.is_zero() {
            return token.is_cancelled();
        }
        token.wait_timeout(dur)
    }
}

// ============================================================================
// Supervision record (deterministic evidence)
// ============================================================================

/// The deterministic, replayable record of a supervised step. Its
/// [`evidence_terms`](Self::evidence_terms) feed the [`DecisionRecord`] ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisionRecord {
    /// Orchestration lane (e.g. `seed`, `capture`, `suite`).
    pub lane: String,
    /// Step name within the lane (e.g. `wait_for_server`).
    pub step: String,
    /// The budget the step ran under.
    pub budget: SupervisionBudget,
    /// The backoff schedule used.
    pub backoff: Backoff,
    /// Per-attempt evidence in execution order.
    pub attempts: Vec<AttemptEvidence>,
    /// Number of attempt slots recorded.
    pub attempts_used: u32,
    /// Final outcome of the step.
    pub final_outcome: StepOutcome,
    /// Cancellation reason, if the step stopped early due to cancel/deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<CancelReason>,
}

impl SupervisionRecord {
    /// Whether the supervised step ultimately succeeded.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.final_outcome.is_success()
    }

    /// Deterministic, timing-free `key=value` evidence terms suitable for the
    /// [`DecisionRecord`] JSONL ledger. Wall-clock durations are excluded; only
    /// configured budgets and attempt-outcome classes appear.
    #[must_use]
    pub fn evidence_terms(&self) -> Vec<String> {
        let mut terms = vec![
            format!("lane={}", self.lane),
            format!("step={}", self.step),
            format!("final_outcome={}", self.final_outcome.label()),
            format!("attempts_used={}", self.attempts_used),
            format!("max_attempts={}", self.budget.max_attempts),
            format!("deadline_ms={}", self.budget.deadline_ms),
            format!("backoff={}", self.backoff.label()),
        ];
        if let Some(reason) = self.cancel_reason {
            terms.push(format!("cancel_reason={}", reason.label()));
        }
        let sequence: Vec<&str> = self
            .attempts
            .iter()
            .map(|attempt| attempt.outcome.label())
            .collect();
        terms.push(format!("attempt_outcomes={}", sequence.join(",")));
        terms
    }

    /// Build a [`DecisionRecord`] from this supervision record for the evidence
    /// ledger. `timestamp`, `trace_id`, and `policy_id` are provided by the
    /// caller so the record stays deterministic when those are deterministic.
    #[must_use]
    pub fn to_decision_record(
        &self,
        timestamp: impl Into<String>,
        trace_id: impl Into<String>,
        policy_id: impl Into<String>,
    ) -> DecisionRecord {
        let fallback_active = !self.succeeded();
        let fallback_reason = if fallback_active {
            Some(match &self.final_outcome {
                StepOutcome::Failed { reason } => reason.clone(),
                other => other.label().to_string(),
            })
        } else {
            None
        };
        DecisionRecord {
            timestamp: timestamp.into(),
            trace_id: trace_id.into(),
            decision_id: format!("supervise:{}:{}", self.lane, self.step),
            action: format!("supervise_{}", self.lane),
            evidence_terms: self.evidence_terms(),
            fallback_active,
            fallback_reason,
            policy_id: policy_id.into(),
        }
    }
}

// ============================================================================
// Generic supervisor (synchronous fallible attempts, e.g. seed RPC calls)
// ============================================================================

/// Run `attempt` under supervision: bounded retries within `budget`,
/// deterministic `backoff`, cancellation-aware via `token`, producing a
/// [`SupervisionRecord`]. The closure receives the 1-based attempt index and
/// returns an [`AttemptResult`].
///
/// Timing is injected via `clock`, so the control flow is fully deterministic
/// under a [`MockClock`]. This is the right supervisor for *synchronous* work
/// that returns promptly (e.g. a blocking HTTP probe); long-running child
/// processes use [`supervise_subprocess`] instead.
pub fn supervise<C, F>(
    lane: &str,
    step: &str,
    budget: SupervisionBudget,
    backoff: Backoff,
    token: &CancellationToken,
    clock: &mut C,
    mut attempt: F,
) -> SupervisionRecord
where
    C: SupervisionClock,
    F: FnMut(u32) -> AttemptResult,
{
    let deadline = budget.deadline();
    let mut attempts: Vec<AttemptEvidence> = Vec::new();
    let mut cancel_reason: Option<CancelReason> = None;
    let mut final_outcome = StepOutcome::Failed {
        reason: "no attempts executed".to_string(),
    };

    for index in 1..=budget.max_attempts {
        // Pre-attempt cancellation check.
        if token.is_cancelled() {
            cancel_reason = Some(CancelReason::Caller);
            attempts.push(AttemptEvidence {
                index,
                outcome: AttemptOutcomeKind::Cancelled,
                detail: None,
            });
            final_outcome = StepOutcome::Cancelled {
                reason: CancelReason::Caller,
            };
            break;
        }
        // Pre-attempt deadline check.
        if clock.elapsed() >= deadline {
            cancel_reason = Some(CancelReason::Deadline);
            attempts.push(AttemptEvidence {
                index,
                outcome: AttemptOutcomeKind::TimedOut,
                detail: None,
            });
            final_outcome = StepOutcome::TimedOut;
            break;
        }

        // Backoff before this attempt (zero for the first), clamped to the
        // remaining budget so a long backoff never overruns the deadline.
        let delay = backoff.delay_before(index);
        if !delay.is_zero() {
            let remaining = deadline.saturating_sub(clock.elapsed());
            let wait = delay.min(remaining);
            if clock.sleep_cancellable(wait, token) {
                cancel_reason = Some(CancelReason::Caller);
                attempts.push(AttemptEvidence {
                    index,
                    outcome: AttemptOutcomeKind::Cancelled,
                    detail: None,
                });
                final_outcome = StepOutcome::Cancelled {
                    reason: CancelReason::Caller,
                };
                break;
            }
            if clock.elapsed() >= deadline {
                cancel_reason = Some(CancelReason::Deadline);
                attempts.push(AttemptEvidence {
                    index,
                    outcome: AttemptOutcomeKind::TimedOut,
                    detail: None,
                });
                final_outcome = StepOutcome::TimedOut;
                break;
            }
        }

        match attempt(index) {
            AttemptResult::Ok => {
                attempts.push(AttemptEvidence {
                    index,
                    outcome: AttemptOutcomeKind::Ok,
                    detail: None,
                });
                final_outcome = StepOutcome::Succeeded;
                break;
            }
            AttemptResult::Fatal(message) => {
                attempts.push(AttemptEvidence {
                    index,
                    outcome: AttemptOutcomeKind::Fatal,
                    detail: Some(message.clone()),
                });
                final_outcome = StepOutcome::Failed { reason: message };
                break;
            }
            AttemptResult::Retryable(message) => {
                attempts.push(AttemptEvidence {
                    index,
                    outcome: AttemptOutcomeKind::Retryable,
                    detail: Some(message.clone()),
                });
                // If this was the last attempt the Failed outcome stands.
                final_outcome = StepOutcome::Failed { reason: message };
            }
        }
    }

    let attempts_used = u32::try_from(attempts.len()).unwrap_or(u32::MAX);
    SupervisionRecord {
        lane: lane.to_string(),
        step: step.to_string(),
        budget,
        backoff,
        attempts,
        attempts_used,
        final_outcome,
        cancel_reason,
    }
}

// ============================================================================
// Subprocess supervisor (real children with wait-timeout polling)
// ============================================================================

/// How a supervised child's stdout/stderr are wired.
///
/// The substrate default (and the original behaviour) is [`SubprocessStdio::Capture`],
/// which pipes both streams and drains them into the [`SupervisedSubprocess`]
/// `stdout`/`stderr` fields. Lanes that stream a child's combined output to an
/// on-disk log (the capture / suite lanes) use [`SubprocessStdio::ToFile`], which
/// directs *both* streams, interleaved, into a single log file truncated fresh on
/// each attempt — matching the existing `run_capture_subprocess_to_log` contract.
/// Lanes that keep the two streams in *separate* on-disk logs (the capture lane's
/// seed-demo thread) use [`SubprocessStdio::ToFiles`], which redirects stdout and
/// stderr into their own files, each truncated fresh on every attempt. Lanes that
/// deliberately discard a child's output (the doctor capture-smoke probes, which
/// rely on the child's own artifacts rather than its stdout/stderr) use
/// [`SubprocessStdio::Null`]. In the `ToFile`/`ToFiles`/`Null` modes the in-memory
/// `stdout`/`stderr` fields stay empty; because no stream is piped into the
/// supervisor there is no in-process pipe to drain, so a chatty child can never
/// deadlock on a full pipe.
#[derive(Debug, Clone)]
pub enum SubprocessStdio {
    /// Pipe stdout and stderr and capture them into the returned String fields.
    Capture,
    /// Stream stdout and stderr (interleaved) into the file at this path,
    /// truncated fresh on every attempt.
    ToFile(PathBuf),
    /// Stream stdout and stderr into their own separate files, each truncated
    /// fresh on every attempt.
    ToFiles {
        /// Destination for the child's stdout.
        stdout: PathBuf,
        /// Destination for the child's stderr.
        stderr: PathBuf,
    },
    /// Discard stdout and stderr (both wired to the null device).
    Null,
}

/// Outcome of a supervised subprocess execution.
#[derive(Debug)]
pub struct SupervisedSubprocess {
    /// The deterministic supervision record.
    pub record: SupervisionRecord,
    /// Exit code of the last attempt, if the child exited. Signal-terminated
    /// children report the shell convention `128 + signal` (e.g. SIGTERM → 143)
    /// rather than `None`, matching [`crate::util::exit_status_code`].
    pub exit_code: Option<i32>,
    /// Captured stdout of the last attempt (empty unless [`SubprocessStdio::Capture`]).
    pub stdout: String,
    /// Captured stderr of the last attempt (empty unless [`SubprocessStdio::Capture`]).
    pub stderr: String,
}

/// How long to poll between `wait_timeout` checks while supervising a child.
const SUBPROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Open `path` for writing, creating it and truncating any existing contents —
/// the per-attempt fresh-log contract shared by the capture / suite lanes.
fn open_truncated_log(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
}

/// Open `path` (create + truncate) and clone the handle so a child's stdout and
/// stderr can both be redirected, interleaved, into the same log file. Mirrors
/// the capture / suite lane's existing log-redirection contract.
fn open_interleaved_log(path: &std::path::Path) -> std::io::Result<(std::fs::File, std::fs::File)> {
    let out = open_truncated_log(path)?;
    let err = out.try_clone()?;
    Ok((out, err))
}

/// Open two independent (create + truncate) log files so a child's stdout and
/// stderr can be redirected into separate on-disk logs.
fn open_separate_logs(
    stdout: &std::path::Path,
    stderr: &std::path::Path,
) -> std::io::Result<(std::fs::File, std::fs::File)> {
    let out = open_truncated_log(stdout)?;
    let err = open_truncated_log(stderr)?;
    Ok((out, err))
}

/// Spawn and supervise a child process to completion under `budget`, killing it
/// on deadline or cancellation. The whole process is retried per the budget when
/// it exits non-zero and `retry_on_nonzero` is set.
///
/// Classification: exit code `0` → [`StepOutcome::Succeeded`]; non-zero (or
/// signal termination, reported as `128 + signal`) → [`StepOutcome::Failed`]
/// (retryable when `retry_on_nonzero`); deadline → [`StepOutcome::TimedOut`];
/// token → [`StepOutcome::Cancelled`].
///
/// `stdio` selects how the child's output is wired (see [`SubprocessStdio`]):
/// captured into the returned String fields, or streamed to an on-disk log for
/// the capture / suite lanes.
///
/// `make_command` is called once per attempt so each retry gets a fresh
/// [`std::process::Command`].
#[allow(clippy::too_many_arguments)]
pub fn supervise_subprocess(
    lane: &str,
    step: &str,
    budget: SupervisionBudget,
    backoff: Backoff,
    token: &CancellationToken,
    retry_on_nonzero: bool,
    stdio: &SubprocessStdio,
    mut make_command: impl FnMut() -> std::process::Command,
) -> SupervisedSubprocess {
    use std::io::Read;
    use std::process::Stdio;
    use wait_timeout::ChildExt;

    /// Join a pipe-reader thread and return whatever it drained. The child has
    /// already exited (or been killed) when this is called, so the pipe is at
    /// EOF and the join is prompt.
    fn drain_reader(handle: Option<std::thread::JoinHandle<String>>) -> String {
        handle.and_then(|h| h.join().ok()).unwrap_or_default()
    }

    let clock = RealClock::start();
    let deadline = budget.deadline();
    let mut attempts: Vec<AttemptEvidence> = Vec::new();
    let mut cancel_reason: Option<CancelReason> = None;
    let mut final_outcome = StepOutcome::Failed {
        reason: "no attempts executed".to_string(),
    };
    let mut last_exit: Option<i32> = None;
    let mut last_stdout = String::new();
    let mut last_stderr = String::new();

    'attempts: for index in 1..=budget.max_attempts {
        // Evidence must reflect THIS attempt: without the reset, a timed-out
        // or cancelled retry would report the previous attempt's exit code
        // and captured output as its own.
        last_exit = None;
        last_stdout.clear();
        last_stderr.clear();

        if token.is_cancelled() {
            cancel_reason = Some(CancelReason::Caller);
            attempts.push(AttemptEvidence {
                index,
                outcome: AttemptOutcomeKind::Cancelled,
                detail: None,
            });
            final_outcome = StepOutcome::Cancelled {
                reason: CancelReason::Caller,
            };
            break;
        }
        if clock.elapsed() >= deadline {
            cancel_reason = Some(CancelReason::Deadline);
            attempts.push(AttemptEvidence {
                index,
                outcome: AttemptOutcomeKind::TimedOut,
                detail: None,
            });
            final_outcome = StepOutcome::TimedOut;
            break;
        }

        // Inter-attempt backoff, clamped to the remaining budget.
        let delay = backoff.delay_before(index);
        if !delay.is_zero() {
            let remaining = deadline.saturating_sub(clock.elapsed());
            if token.wait_timeout(delay.min(remaining)) {
                cancel_reason = Some(CancelReason::Caller);
                attempts.push(AttemptEvidence {
                    index,
                    outcome: AttemptOutcomeKind::Cancelled,
                    detail: None,
                });
                final_outcome = StepOutcome::Cancelled {
                    reason: CancelReason::Caller,
                };
                break;
            }
        }

        let mut command = make_command();
        // Wire the child's stdio. `Capture` pipes both streams into the in-process
        // buffers and `Null` discards them (both wired inline here, yielding no log
        // handles); `ToFile`/`ToFiles` redirect them straight to disk (interleaved
        // into one file, or split across two) and yield handles drained below.
        let log_handles: Option<std::io::Result<(std::fs::File, std::fs::File)>> = match stdio {
            SubprocessStdio::Capture => {
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
                None
            }
            SubprocessStdio::Null => {
                command.stdout(Stdio::null()).stderr(Stdio::null());
                None
            }
            SubprocessStdio::ToFile(path) => Some(open_interleaved_log(path)),
            SubprocessStdio::ToFiles { stdout, stderr } => Some(open_separate_logs(stdout, stderr)),
        };
        if let Some(handles) = log_handles {
            match handles {
                Ok((out, err)) => {
                    command.stdout(Stdio::from(out)).stderr(Stdio::from(err));
                }
                Err(error) => {
                    let message = format!("log open failed: {error}");
                    attempts.push(AttemptEvidence {
                        index,
                        outcome: AttemptOutcomeKind::Fatal,
                        detail: Some(message.clone()),
                    });
                    final_outcome = StepOutcome::Failed { reason: message };
                    break;
                }
            }
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let message = format!("spawn failed: {error}");
                attempts.push(AttemptEvidence {
                    index,
                    outcome: AttemptOutcomeKind::Fatal,
                    detail: Some(message.clone()),
                });
                final_outcome = StepOutcome::Failed { reason: message };
                break;
            }
        };

        // Capture mode: drain both pipes CONCURRENTLY while the child runs.
        // Reading only after exit deadlocks any child that writes more than
        // the OS pipe buffer (~64KB): it blocks on write(), never exits, and
        // would be misreported as TimedOut with its output discarded.
        let mut stdout_reader: Option<std::thread::JoinHandle<String>> = None;
        let mut stderr_reader: Option<std::thread::JoinHandle<String>> = None;
        if matches!(stdio, SubprocessStdio::Capture) {
            if let Some(mut out) = child.stdout.take() {
                stdout_reader = Some(std::thread::spawn(move || {
                    let mut buf = String::new();
                    let _ = out.read_to_string(&mut buf);
                    buf
                }));
            }
            if let Some(mut err) = child.stderr.take() {
                stderr_reader = Some(std::thread::spawn(move || {
                    let mut buf = String::new();
                    let _ = err.read_to_string(&mut buf);
                    buf
                }));
            }
        }

        // Poll the child, honouring the global deadline and the cancel token.
        let exit_status = loop {
            let remaining = deadline.saturating_sub(clock.elapsed());
            if remaining.is_zero() {
                let _ = child.kill();
                let _ = child.wait();
                last_stdout = drain_reader(stdout_reader.take());
                last_stderr = drain_reader(stderr_reader.take());
                cancel_reason = Some(CancelReason::Deadline);
                attempts.push(AttemptEvidence {
                    index,
                    outcome: AttemptOutcomeKind::TimedOut,
                    detail: None,
                });
                final_outcome = StepOutcome::TimedOut;
                break 'attempts;
            }
            if token.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                last_stdout = drain_reader(stdout_reader.take());
                last_stderr = drain_reader(stderr_reader.take());
                cancel_reason = Some(CancelReason::Caller);
                attempts.push(AttemptEvidence {
                    index,
                    outcome: AttemptOutcomeKind::Cancelled,
                    detail: None,
                });
                final_outcome = StepOutcome::Cancelled {
                    reason: CancelReason::Caller,
                };
                break 'attempts;
            }
            let poll = SUBPROCESS_POLL_INTERVAL.min(remaining);
            match child.wait_timeout(poll) {
                Ok(Some(status)) => break status,
                Ok(None) => continue,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    last_stdout = drain_reader(stdout_reader.take());
                    last_stderr = drain_reader(stderr_reader.take());
                    let message = format!("wait failed: {error}");
                    attempts.push(AttemptEvidence {
                        index,
                        outcome: AttemptOutcomeKind::Fatal,
                        detail: Some(message.clone()),
                    });
                    final_outcome = StepOutcome::Failed { reason: message };
                    break 'attempts;
                }
            }
        };

        // Collect the concurrently-drained output. Only the capture mode
        // spawns readers; `ToFile`/`ToFiles`/`Null` leave these `None`, so the
        // in-memory buffers stay empty.
        last_stdout = drain_reader(stdout_reader.take());
        last_stderr = drain_reader(stderr_reader.take());
        // Signal-aware: a child killed by a signal reports `128 + signal` rather
        // than `None`, matching the capture / suite lane's `exit_status_code`.
        last_exit = Some(crate::util::exit_status_code(exit_status));

        if exit_status.success() {
            attempts.push(AttemptEvidence {
                index,
                outcome: AttemptOutcomeKind::Ok,
                detail: None,
            });
            final_outcome = StepOutcome::Succeeded;
            break;
        }

        let message = format!("exit_code={}", last_exit.unwrap_or(-1));
        if retry_on_nonzero {
            attempts.push(AttemptEvidence {
                index,
                outcome: AttemptOutcomeKind::Retryable,
                detail: Some(message.clone()),
            });
            final_outcome = StepOutcome::Failed { reason: message };
        } else {
            attempts.push(AttemptEvidence {
                index,
                outcome: AttemptOutcomeKind::Fatal,
                detail: Some(message.clone()),
            });
            final_outcome = StepOutcome::Failed { reason: message };
            break;
        }
    }

    let attempts_used = u32::try_from(attempts.len()).unwrap_or(u32::MAX);
    SupervisedSubprocess {
        record: SupervisionRecord {
            lane: lane.to_string(),
            step: step.to_string(),
            budget,
            backoff,
            attempts,
            attempts_used,
            final_outcome,
            cancel_reason,
        },
        exit_code: last_exit,
        stdout: last_stdout,
        stderr: last_stderr,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_runtime::cancellation::CancellationSource;

    /// Deterministic clock: virtual time advances by exactly the requested sleep
    /// duration, with an optional scripted cancellation after N sleeps.
    struct MockClock {
        elapsed: Duration,
        sleeps: u32,
        cancel_after_sleeps: Option<u32>,
    }

    impl MockClock {
        fn new() -> Self {
            Self {
                elapsed: Duration::ZERO,
                sleeps: 0,
                cancel_after_sleeps: None,
            }
        }

        fn starting_at(elapsed: Duration) -> Self {
            Self {
                elapsed,
                sleeps: 0,
                cancel_after_sleeps: None,
            }
        }
    }

    impl SupervisionClock for MockClock {
        fn elapsed(&self) -> Duration {
            self.elapsed
        }

        fn sleep_cancellable(&mut self, dur: Duration, token: &CancellationToken) -> bool {
            self.elapsed += dur;
            self.sleeps += 1;
            if Some(self.sleeps) == self.cancel_after_sleeps {
                return true;
            }
            token.is_cancelled()
        }
    }

    fn budget(deadline_ms: u64, max_attempts: u32) -> SupervisionBudget {
        SupervisionBudget {
            deadline_ms,
            max_attempts,
        }
    }

    #[test]
    fn first_attempt_success_records_single_ok() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        let record = supervise(
            "seed",
            "probe",
            budget(10_000, 3),
            Backoff::none(),
            &source.token(),
            &mut clock,
            |_| AttemptResult::Ok,
        );
        assert_eq!(record.final_outcome, StepOutcome::Succeeded);
        assert_eq!(record.attempts_used, 1);
        assert!(record.succeeded());
        assert_eq!(record.attempts[0].outcome, AttemptOutcomeKind::Ok);
        assert!(record.cancel_reason.is_none());
    }

    #[test]
    fn retry_then_success_records_each_attempt() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        let mut calls = 0u32;
        let record = supervise(
            "seed",
            "probe",
            budget(10_000, 4),
            Backoff::exponential(50, 1_000),
            &source.token(),
            &mut clock,
            |_| {
                calls += 1;
                if calls < 3 {
                    AttemptResult::Retryable(format!("transient {calls}"))
                } else {
                    AttemptResult::Ok
                }
            },
        );
        assert_eq!(record.final_outcome, StepOutcome::Succeeded);
        assert_eq!(record.attempts_used, 3);
        assert_eq!(record.attempts[0].outcome, AttemptOutcomeKind::Retryable);
        assert_eq!(record.attempts[1].outcome, AttemptOutcomeKind::Retryable);
        assert_eq!(record.attempts[2].outcome, AttemptOutcomeKind::Ok);
    }

    #[test]
    fn exhausted_retries_end_in_failed() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        let record = supervise(
            "seed",
            "probe",
            budget(100_000, 3),
            Backoff::fixed(10),
            &source.token(),
            &mut clock,
            |index| AttemptResult::Retryable(format!("attempt {index} failed")),
        );
        assert!(matches!(record.final_outcome, StepOutcome::Failed { .. }));
        assert_eq!(record.attempts_used, 3);
        assert!(
            record
                .attempts
                .iter()
                .all(|a| a.outcome == AttemptOutcomeKind::Retryable)
        );
        assert!(record.cancel_reason.is_none());
    }

    #[test]
    fn fatal_error_stops_immediately() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        let mut calls = 0u32;
        let record = supervise(
            "seed",
            "probe",
            budget(100_000, 5),
            Backoff::none(),
            &source.token(),
            &mut clock,
            |_| {
                calls += 1;
                AttemptResult::Fatal("permanent".to_string())
            },
        );
        assert!(matches!(record.final_outcome, StepOutcome::Failed { .. }));
        assert_eq!(record.attempts_used, 1);
        assert_eq!(calls, 1, "fatal must not retry");
        assert_eq!(record.attempts[0].outcome, AttemptOutcomeKind::Fatal);
    }

    #[test]
    fn deadline_exceeded_records_timed_out() {
        let source = CancellationSource::new();
        // Start already past the deadline so the very first slot times out.
        let mut clock = MockClock::starting_at(Duration::from_millis(5_000));
        let record = supervise(
            "seed",
            "probe",
            budget(1_000, 3),
            Backoff::none(),
            &source.token(),
            &mut clock,
            |_| AttemptResult::Ok,
        );
        assert_eq!(record.final_outcome, StepOutcome::TimedOut);
        assert_eq!(record.cancel_reason, Some(CancelReason::Deadline));
        assert_eq!(record.attempts[0].outcome, AttemptOutcomeKind::TimedOut);
    }

    #[test]
    fn backoff_accumulates_until_deadline_times_out() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        // 3 retries with fixed 500ms backoff = 1000ms after two backoffs, which
        // exceeds the 800ms deadline before the 3rd attempt runs.
        let record = supervise(
            "seed",
            "probe",
            budget(800, 5),
            Backoff::fixed(500),
            &source.token(),
            &mut clock,
            |_| AttemptResult::Retryable("again".to_string()),
        );
        assert_eq!(record.final_outcome, StepOutcome::TimedOut);
        assert_eq!(record.cancel_reason, Some(CancelReason::Deadline));
    }

    #[test]
    fn pre_cancelled_token_records_cancelled() {
        let source = CancellationSource::new();
        source.cancel();
        let mut clock = MockClock::new();
        let record = supervise(
            "seed",
            "probe",
            budget(10_000, 3),
            Backoff::none(),
            &source.token(),
            &mut clock,
            |_| AttemptResult::Ok,
        );
        assert_eq!(
            record.final_outcome,
            StepOutcome::Cancelled {
                reason: CancelReason::Caller
            }
        );
        assert_eq!(record.cancel_reason, Some(CancelReason::Caller));
        assert_eq!(record.attempts[0].outcome, AttemptOutcomeKind::Cancelled);
    }

    #[test]
    fn cancellation_during_backoff_records_cancelled() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        // Cancel during the first backoff (before attempt 2).
        clock.cancel_after_sleeps = Some(1);
        let record = supervise(
            "seed",
            "probe",
            budget(100_000, 5),
            Backoff::fixed(50),
            &source.token(),
            &mut clock,
            |_| AttemptResult::Retryable("again".to_string()),
        );
        assert_eq!(
            record.final_outcome,
            StepOutcome::Cancelled {
                reason: CancelReason::Caller
            }
        );
        // Attempt 1 retryable, then cancelled slot for attempt 2.
        assert_eq!(
            record.attempts.last().unwrap().outcome,
            AttemptOutcomeKind::Cancelled
        );
    }

    #[test]
    fn outcome_severity_lattice_orders_correctly() {
        assert!(StepOutcome::Succeeded < StepOutcome::Failed { reason: "x".into() });
        assert!(StepOutcome::Failed { reason: "x".into() } < StepOutcome::TimedOut);
        assert!(
            StepOutcome::TimedOut
                < StepOutcome::Cancelled {
                    reason: CancelReason::Caller
                }
        );
    }

    #[test]
    fn budget_meet_takes_tighter_constraint() {
        let outer = budget(10_000, 5);
        let inner = budget(2_000, 3);
        let met = outer.meet(inner);
        assert_eq!(met.deadline_ms, 2_000);
        assert_eq!(met.max_attempts, 3);
        // meet is symmetric.
        assert_eq!(inner.meet(outer), met);
        // never drops below one attempt.
        assert_eq!(budget(1, 0).meet(budget(1, 0)).max_attempts, 1);
    }

    #[test]
    fn exponential_backoff_schedule_is_deterministic() {
        let backoff = Backoff::exponential(100, 1_000);
        assert_eq!(backoff.delay_before(1), Duration::ZERO);
        assert_eq!(backoff.delay_before(2), Duration::from_millis(100));
        assert_eq!(backoff.delay_before(3), Duration::from_millis(200));
        assert_eq!(backoff.delay_before(4), Duration::from_millis(400));
        assert_eq!(backoff.delay_before(5), Duration::from_millis(800));
        // clamped at max_ms.
        assert_eq!(backoff.delay_before(6), Duration::from_millis(1_000));
        assert_eq!(backoff.delay_before(20), Duration::from_millis(1_000));
    }

    #[test]
    fn fixed_backoff_is_constant() {
        let backoff = Backoff::fixed(250);
        assert_eq!(backoff.delay_before(1), Duration::ZERO);
        assert_eq!(backoff.delay_before(2), Duration::from_millis(250));
        assert_eq!(backoff.delay_before(7), Duration::from_millis(250));
    }

    #[test]
    fn evidence_terms_are_deterministic_and_timing_free() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        let mut calls = 0u32;
        let record = supervise(
            "seed",
            "wait_for_server",
            budget(30_000, 3),
            Backoff::exponential(100, 1_000),
            &source.token(),
            &mut clock,
            |_| {
                calls += 1;
                if calls < 2 {
                    AttemptResult::Retryable("conn refused".to_string())
                } else {
                    AttemptResult::Ok
                }
            },
        );
        let terms = record.evidence_terms();
        assert!(terms.contains(&"lane=seed".to_string()));
        assert!(terms.contains(&"step=wait_for_server".to_string()));
        assert!(terms.contains(&"final_outcome=succeeded".to_string()));
        assert!(terms.contains(&"attempts_used=2".to_string()));
        assert!(terms.contains(&"attempt_outcomes=retryable,ok".to_string()));
        // No wall-clock duration ever leaks into the fingerprint.
        assert!(
            terms.iter().all(|t| !t.contains("elapsed")),
            "evidence terms must be timing-free: {terms:?}"
        );
    }

    #[test]
    fn record_serde_round_trips() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        let record = supervise(
            "capture",
            "vhs",
            budget(5_000, 2),
            Backoff::fixed(100),
            &source.token(),
            &mut clock,
            |_| AttemptResult::Ok,
        );
        let json = serde_json::to_string(&record).expect("serialize");
        let decoded: SupervisionRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, record);
    }

    #[test]
    fn to_decision_record_maps_failure_to_fallback() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        let record = supervise(
            "seed",
            "probe",
            budget(1_000, 2),
            Backoff::none(),
            &source.token(),
            &mut clock,
            |_| AttemptResult::Fatal("boom".to_string()),
        );
        let decision = record.to_decision_record("2026-06-22T00:00:00Z", "trace-1", "policy-1");
        assert!(decision.fallback_active);
        assert_eq!(decision.fallback_reason.as_deref(), Some("boom"));
        assert_eq!(decision.action, "supervise_seed");
        assert!(
            decision
                .evidence_terms
                .contains(&"final_outcome=failed".to_string())
        );
    }

    #[test]
    fn to_decision_record_success_has_no_fallback() {
        let source = CancellationSource::new();
        let mut clock = MockClock::new();
        let record = supervise(
            "seed",
            "probe",
            budget(1_000, 2),
            Backoff::none(),
            &source.token(),
            &mut clock,
            |_| AttemptResult::Ok,
        );
        let decision = record.to_decision_record("2026-06-22T00:00:00Z", "trace-1", "policy-1");
        assert!(!decision.fallback_active);
        assert!(decision.fallback_reason.is_none());
    }

    #[test]
    fn capture_mode_drains_large_output_without_deadlock() {
        // Far more stdout than the ~64KB OS pipe buffer: without concurrent
        // readers the child blocks on write(), never exits, and the step
        // wedges until the deadline. It must succeed promptly with the full
        // output captured.
        let source = CancellationSource::new();
        let supervised = supervise_subprocess(
            "test",
            "large-output",
            budget(30_000, 1),
            Backoff::none(),
            &source.token(),
            false,
            &SubprocessStdio::Capture,
            || {
                let mut cmd = std::process::Command::new("sh");
                cmd.arg("-c").arg("seq 1 100000");
                cmd
            },
        );
        assert_eq!(supervised.record.final_outcome, StepOutcome::Succeeded);
        assert_eq!(supervised.exit_code, Some(0));
        assert!(
            supervised.stdout.len() > 500_000,
            "captured only {} bytes",
            supervised.stdout.len()
        );
        assert!(supervised.stdout.starts_with("1\n"));
        assert!(supervised.stdout.ends_with("100000\n"));
    }

    #[test]
    fn timed_out_retry_does_not_report_previous_attempts_evidence() {
        // Attempt 1 exits nonzero fast (with output); attempt 2 sleeps past
        // the global deadline. The final evidence must reflect the timed-out
        // attempt (no exit code, no stale stdout), not attempt 1's.
        let source = CancellationSource::new();
        let mut calls = 0u32;
        let supervised = supervise_subprocess(
            "test",
            "stale-evidence",
            budget(1_500, 2),
            Backoff::none(),
            &source.token(),
            true,
            &SubprocessStdio::Capture,
            move || {
                calls += 1;
                let mut cmd = std::process::Command::new("sh");
                if calls == 1 {
                    cmd.arg("-c").arg("echo attempt-one; exit 3");
                } else {
                    cmd.arg("-c").arg("exec sleep 30");
                }
                cmd
            },
        );
        assert_eq!(supervised.record.final_outcome, StepOutcome::TimedOut);
        assert_eq!(
            supervised.exit_code, None,
            "timed-out attempt reported a stale exit code"
        );
        assert!(
            !supervised.stdout.contains("attempt-one"),
            "timed-out attempt reported the previous attempt's stdout"
        );
    }
}
