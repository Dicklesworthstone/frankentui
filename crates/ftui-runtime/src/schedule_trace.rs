//! Schedule Trace Module (bd-gyi5).
//!
//! Provides deterministic golden trace infrastructure for async task manager testing.
//! Records scheduler events (task start/stop, wakeups, yields, cancellations) and
//! generates stable checksums for regression detection.
//!
//! # Core Algorithm
//!
//! - Events are recorded with monotonic sequence numbers (not wall-clock)
//! - Traces are hashed using FNV-1a for stability across platforms
//! - Isomorphism proofs validate that behavioral changes preserve invariants
//!
//! # Example
//!
//! ```rust,ignore
//! use ftui_runtime::schedule_trace::{ScheduleTrace, TaskEvent};
//!
//! let mut trace = ScheduleTrace::new();
//!
//! // Record events
//! trace.record(TaskEvent::Spawn { task_id: 1, priority: 0 });
//! trace.record(TaskEvent::Start { task_id: 1 });
//! trace.record(TaskEvent::Complete { task_id: 1 });
//!
//! // Generate checksum
//! let checksum = trace.checksum();
//!
//! // Export for golden comparison
//! let json = trace.to_jsonl();
//! ```

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::fmt;
use std::time::Instant;

use crate::voi_sampling::{VoiConfig, VoiSampler, VoiSummary};

// =============================================================================
// Event Types
// =============================================================================

/// A scheduler event with deterministic ordering.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskEvent {
    /// Task spawned into the queue.
    Spawn {
        task_id: u64,
        priority: u8,
        name: Option<String>,
    },
    /// Task started execution.
    Start { task_id: u64 },
    /// Task yielded voluntarily.
    Yield { task_id: u64 },
    /// Task woken up (external trigger).
    Wakeup { task_id: u64, reason: WakeupReason },
    /// Task completed successfully.
    Complete { task_id: u64 },
    /// Task failed with error.
    Failed { task_id: u64, error: String },
    /// Task cancelled.
    Cancelled { task_id: u64, reason: CancelReason },
    /// Scheduler policy changed.
    PolicyChange {
        from: SchedulerPolicy,
        to: SchedulerPolicy,
    },
    /// Queue state snapshot (for debugging).
    QueueSnapshot { queued: usize, running: usize },
    /// Custom event for extensibility.
    Custom { tag: String, data: String },
}

/// Reason for task wakeup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeupReason {
    /// Timer expired.
    Timer,
    /// I/O ready.
    IoReady,
    /// Dependency completed.
    Dependency { task_id: u64 },
    /// User action.
    UserAction,
    /// Explicit wake call.
    Explicit,
    /// Unknown/other.
    Other(String),
}

/// Reason for task cancellation.
#[derive(Debug, Clone, PartialEq)]
pub enum CancelReason {
    /// User requested cancellation.
    UserRequest,
    /// Timeout exceeded.
    Timeout,
    /// Hazard-based policy decision.
    HazardPolicy { expected_loss: f64 },
    /// System shutdown.
    Shutdown,
    /// Other reason.
    Other(String),
}

/// Scheduler policy identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerPolicy {
    /// First-in, first-out.
    Fifo,
    /// Priority-based (highest first).
    Priority,
    /// Shortest remaining time first.
    ShortestFirst,
    /// Round-robin with time slices.
    RoundRobin,
    /// Weighted fair queuing.
    WeightedFair,
}

impl fmt::Display for SchedulerPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fifo => write!(f, "fifo"),
            Self::Priority => write!(f, "priority"),
            Self::ShortestFirst => write!(f, "shortest_first"),
            Self::RoundRobin => write!(f, "round_robin"),
            Self::WeightedFair => write!(f, "weighted_fair"),
        }
    }
}

// =============================================================================
// Trace Entry
// =============================================================================

/// A timestamped trace entry.
#[derive(Debug, Clone)]
pub struct TraceEntry {
    /// Monotonic sequence number (not wall-clock).
    pub seq: u64,
    /// Logical tick when event occurred.
    pub tick: u64,
    /// The event itself.
    pub event: TaskEvent,
}

impl TraceEntry {
    /// Serialize to JSONL format.
    pub fn to_jsonl(&self) -> String {
        let event_type = match &self.event {
            TaskEvent::Spawn { .. } => "spawn",
            TaskEvent::Start { .. } => "start",
            TaskEvent::Yield { .. } => "yield",
            TaskEvent::Wakeup { .. } => "wakeup",
            TaskEvent::Complete { .. } => "complete",
            TaskEvent::Failed { .. } => "failed",
            TaskEvent::Cancelled { .. } => "cancelled",
            TaskEvent::PolicyChange { .. } => "policy_change",
            TaskEvent::QueueSnapshot { .. } => "queue_snapshot",
            TaskEvent::Custom { .. } => "custom",
        };

        let details = match &self.event {
            TaskEvent::Spawn {
                task_id,
                priority,
                name,
            } => {
                format!(
                    "\"task_id\":{},\"priority\":{},\"name\":{}",
                    task_id,
                    priority,
                    name.as_ref()
                        .map(|n| format!("\"{}\"", n))
                        .unwrap_or_else(|| "null".to_string())
                )
            }
            TaskEvent::Start { task_id } => format!("\"task_id\":{}", task_id),
            TaskEvent::Yield { task_id } => format!("\"task_id\":{}", task_id),
            TaskEvent::Wakeup { task_id, reason } => {
                let reason_str = match reason {
                    WakeupReason::Timer => "timer".to_string(),
                    WakeupReason::IoReady => "io_ready".to_string(),
                    WakeupReason::Dependency { task_id } => format!("dependency:{}", task_id),
                    WakeupReason::UserAction => "user_action".to_string(),
                    WakeupReason::Explicit => "explicit".to_string(),
                    WakeupReason::Other(s) => format!("other:{}", s),
                };
                format!("\"task_id\":{},\"reason\":\"{}\"", task_id, reason_str)
            }
            TaskEvent::Complete { task_id } => format!("\"task_id\":{}", task_id),
            TaskEvent::Failed { task_id, error } => {
                format!("\"task_id\":{},\"error\":\"{}\"", task_id, error)
            }
            TaskEvent::Cancelled { task_id, reason } => {
                let reason_str = match reason {
                    CancelReason::UserRequest => "user_request".to_string(),
                    CancelReason::Timeout => "timeout".to_string(),
                    CancelReason::HazardPolicy { expected_loss } => {
                        format!("hazard_policy:{:.4}", expected_loss)
                    }
                    CancelReason::Shutdown => "shutdown".to_string(),
                    CancelReason::Other(s) => format!("other:{}", s),
                };
                format!("\"task_id\":{},\"reason\":\"{}\"", task_id, reason_str)
            }
            TaskEvent::PolicyChange { from, to } => {
                format!("\"from\":\"{}\",\"to\":\"{}\"", from, to)
            }
            TaskEvent::QueueSnapshot { queued, running } => {
                format!("\"queued\":{},\"running\":{}", queued, running)
            }
            TaskEvent::Custom { tag, data } => {
                format!("\"tag\":\"{}\",\"data\":\"{}\"", tag, data)
            }
        };

        format!(
            "{{\"seq\":{},\"tick\":{},\"event\":\"{}\",{}}}",
            self.seq, self.tick, event_type, details
        )
    }
}

// =============================================================================
// Schedule Trace
// =============================================================================

/// Configuration for the schedule trace.
#[derive(Debug, Clone)]
pub struct TraceConfig {
    /// Maximum entries to retain (0 = unlimited).
    pub max_entries: usize,
    /// Include queue snapshots after each event.
    ///
    /// If `snapshot_sampling` is set, snapshots are sampled via VOI.
    pub auto_snapshot: bool,
    /// Optional VOI sampling policy for queue snapshots.
    pub snapshot_sampling: Option<VoiConfig>,
    /// Minimum absolute queue delta to mark a snapshot as "violated".
    pub snapshot_change_threshold: usize,
    /// Seed for deterministic tie-breaking.
    pub seed: u64,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            auto_snapshot: false,
            snapshot_sampling: None,
            snapshot_change_threshold: 1,
            seed: 0,
        }
    }
}

/// The main schedule trace recorder.
#[derive(Debug, Clone)]
pub struct ScheduleTrace {
    /// Configuration.
    config: TraceConfig,
    /// Recorded entries.
    entries: VecDeque<TraceEntry>,
    /// Monotonic sequence counter.
    seq: u64,
    /// Current logical tick.
    tick: u64,
    /// Optional VOI sampler for queue snapshots.
    snapshot_sampler: Option<VoiSampler>,
    /// Last recorded queue snapshot (queued, running).
    last_snapshot: Option<(usize, usize)>,
}

impl ScheduleTrace {
    /// Create a new trace recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(TraceConfig::default())
    }

    /// Create with custom configuration.
    #[must_use]
    pub fn with_config(config: TraceConfig) -> Self {
        let capacity = if config.max_entries > 0 {
            config.max_entries
        } else {
            1024
        };
        let snapshot_sampler = config.snapshot_sampling.clone().map(VoiSampler::new);
        Self {
            config,
            entries: VecDeque::with_capacity(capacity),
            seq: 0,
            tick: 0,
            snapshot_sampler,
            last_snapshot: None,
        }
    }

    /// Advance the logical tick.
    pub fn advance_tick(&mut self) {
        self.tick += 1;
    }

    /// Set the logical tick explicitly.
    pub fn set_tick(&mut self, tick: u64) {
        self.tick = tick;
    }

    /// Get current tick.
    #[must_use]
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// Record an event.
    pub fn record(&mut self, event: TaskEvent) {
        let entry = TraceEntry {
            seq: self.seq,
            tick: self.tick,
            event,
        };
        self.seq += 1;

        // Enforce max entries
        if self.config.max_entries > 0 && self.entries.len() >= self.config.max_entries {
            self.entries.pop_front();
        }

        self.entries.push_back(entry);
    }

    /// Record an event with queue state and optional auto-snapshot.
    pub fn record_with_queue_state(&mut self, event: TaskEvent, queued: usize, running: usize) {
        self.record_with_queue_state_at(event, queued, running, Instant::now());
    }

    /// Record an event with queue state at a specific time (deterministic tests).
    pub fn record_with_queue_state_at(
        &mut self,
        event: TaskEvent,
        queued: usize,
        running: usize,
        now: Instant,
    ) {
        self.record(event);
        if self.config.auto_snapshot {
            self.maybe_snapshot(queued, running, now);
        }
    }

    /// Decide whether to record a queue snapshot and update VOI evidence.
    fn maybe_snapshot(&mut self, queued: usize, running: usize, now: Instant) {
        let should_sample = if let Some(ref mut sampler) = self.snapshot_sampler {
            let decision = sampler.decide(now);
            if !decision.should_sample {
                return;
            }
            let violated = self
                .last_snapshot
                .map(|(prev_q, prev_r)| {
                    let delta = prev_q.abs_diff(queued) + prev_r.abs_diff(running);
                    delta >= self.config.snapshot_change_threshold
                })
                .unwrap_or(false);
            sampler.observe_at(violated, now);
            true
        } else {
            true
        };

        if should_sample {
            self.record(TaskEvent::QueueSnapshot { queued, running });
            self.last_snapshot = Some((queued, running));
        }
    }

    /// Record a spawn event.
    pub fn spawn(&mut self, task_id: u64, priority: u8, name: Option<String>) {
        self.record(TaskEvent::Spawn {
            task_id,
            priority,
            name,
        });
    }

    /// Record a start event.
    pub fn start(&mut self, task_id: u64) {
        self.record(TaskEvent::Start { task_id });
    }

    /// Record a complete event.
    pub fn complete(&mut self, task_id: u64) {
        self.record(TaskEvent::Complete { task_id });
    }

    /// Record a cancelled event.
    pub fn cancel(&mut self, task_id: u64, reason: CancelReason) {
        self.record(TaskEvent::Cancelled { task_id, reason });
    }

    /// Get all entries.
    #[must_use]
    pub fn entries(&self) -> &VecDeque<TraceEntry> {
        &self.entries
    }

    /// Get entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.seq = 0;
        self.last_snapshot = None;
        if let Some(ref mut sampler) = self.snapshot_sampler {
            let config = sampler.config().clone();
            *sampler = VoiSampler::new(config);
        }
    }

    /// Snapshot sampling summary, if enabled.
    #[must_use]
    pub fn snapshot_sampling_summary(&self) -> Option<VoiSummary> {
        self.snapshot_sampler.as_ref().map(VoiSampler::summary)
    }

    /// Snapshot sampling logs rendered as JSONL, if enabled.
    #[must_use]
    pub fn snapshot_sampling_logs_jsonl(&self) -> Option<String> {
        self.snapshot_sampler
            .as_ref()
            .map(VoiSampler::logs_to_jsonl)
    }

    /// Export to JSONL format.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        self.entries
            .iter()
            .map(|e| e.to_jsonl())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Compute FNV-1a checksum of the trace.
    ///
    /// This checksum is stable across platforms and can be used for golden comparisons.
    #[must_use]
    pub fn checksum(&self) -> u64 {
        // FNV-1a 64-bit
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;

        let mut hash = FNV_OFFSET;

        for entry in &self.entries {
            // Hash seq
            for byte in entry.seq.to_le_bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(FNV_PRIME);
            }

            // Hash tick
            for byte in entry.tick.to_le_bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(FNV_PRIME);
            }

            // Hash event type discriminant + key data
            let event_bytes = self.event_to_bytes(&entry.event);
            for byte in event_bytes {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        }

        hash
    }

    /// Compute checksum as hex string.
    #[must_use]
    pub fn checksum_hex(&self) -> String {
        format!("{:016x}", self.checksum())
    }

    /// Convert event to bytes for hashing.
    fn event_to_bytes(&self, event: &TaskEvent) -> Vec<u8> {
        let mut bytes = Vec::new();

        match event {
            TaskEvent::Spawn {
                task_id, priority, ..
            } => {
                bytes.push(0x01);
                bytes.extend_from_slice(&task_id.to_le_bytes());
                bytes.push(*priority);
            }
            TaskEvent::Start { task_id } => {
                bytes.push(0x02);
                bytes.extend_from_slice(&task_id.to_le_bytes());
            }
            TaskEvent::Yield { task_id } => {
                bytes.push(0x03);
                bytes.extend_from_slice(&task_id.to_le_bytes());
            }
            TaskEvent::Wakeup { task_id, .. } => {
                bytes.push(0x04);
                bytes.extend_from_slice(&task_id.to_le_bytes());
            }
            TaskEvent::Complete { task_id } => {
                bytes.push(0x05);
                bytes.extend_from_slice(&task_id.to_le_bytes());
            }
            TaskEvent::Failed { task_id, .. } => {
                bytes.push(0x06);
                bytes.extend_from_slice(&task_id.to_le_bytes());
            }
            TaskEvent::Cancelled { task_id, .. } => {
                bytes.push(0x07);
                bytes.extend_from_slice(&task_id.to_le_bytes());
            }
            TaskEvent::PolicyChange { from, to } => {
                bytes.push(0x08);
                bytes.push(*from as u8);
                bytes.push(*to as u8);
            }
            TaskEvent::QueueSnapshot { queued, running } => {
                bytes.push(0x09);
                bytes.extend_from_slice(&(*queued as u64).to_le_bytes());
                bytes.extend_from_slice(&(*running as u64).to_le_bytes());
            }
            TaskEvent::Custom { tag, data } => {
                bytes.push(0x0A);
                bytes.extend_from_slice(tag.as_bytes());
                bytes.push(0x00); // separator
                bytes.extend_from_slice(data.as_bytes());
            }
        }

        bytes
    }
}

impl Default for ScheduleTrace {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Golden Comparison
// =============================================================================

/// Result of comparing a trace against a golden checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoldenCompareResult {
    /// Checksums match.
    Match,
    /// Checksums differ.
    Mismatch { expected: u64, actual: u64 },
    /// Golden file not found.
    MissingGolden,
}

impl GoldenCompareResult {
    /// Check if the comparison passed.
    #[must_use]
    pub fn is_match(&self) -> bool {
        matches!(self, Self::Match)
    }
}

/// Compare trace against expected golden checksum.
#[must_use]
pub fn compare_golden(trace: &ScheduleTrace, expected: u64) -> GoldenCompareResult {
    let actual = trace.checksum();
    if actual == expected {
        GoldenCompareResult::Match
    } else {
        GoldenCompareResult::Mismatch { expected, actual }
    }
}

// =============================================================================
// Isomorphism Proof
// =============================================================================

/// Evidence for an isomorphism proof.
///
/// When scheduler behavior changes, this documents why the change preserves
/// correctness despite producing a different trace.
#[derive(Debug, Clone)]
pub struct IsomorphismProof {
    /// Description of the change.
    pub change_description: String,
    /// Old checksum before the change.
    pub old_checksum: u64,
    /// New checksum after the change.
    pub new_checksum: u64,
    /// Invariants that are preserved.
    pub preserved_invariants: Vec<String>,
    /// Justification for why the traces are equivalent.
    pub justification: String,
    /// Who approved this change.
    pub approved_by: Option<String>,
    /// Timestamp of approval.
    pub approved_at: Option<String>,
}

impl IsomorphismProof {
    /// Create a new proof.
    pub fn new(
        change_description: impl Into<String>,
        old_checksum: u64,
        new_checksum: u64,
    ) -> Self {
        Self {
            change_description: change_description.into(),
            old_checksum,
            new_checksum,
            preserved_invariants: Vec::new(),
            justification: String::new(),
            approved_by: None,
            approved_at: None,
        }
    }

    /// Add a preserved invariant.
    #[must_use]
    pub fn with_invariant(mut self, invariant: impl Into<String>) -> Self {
        self.preserved_invariants.push(invariant.into());
        self
    }

    /// Add justification.
    #[must_use]
    pub fn with_justification(mut self, justification: impl Into<String>) -> Self {
        self.justification = justification.into();
        self
    }

    /// Export to JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let invariants = self
            .preserved_invariants
            .iter()
            .map(|i| format!("\"{}\"", i))
            .collect::<Vec<_>>()
            .join(",");

        let old_checksum = format!("{:016x}", self.old_checksum);
        let new_checksum = format!("{:016x}", self.new_checksum);
        let approved_by = self
            .approved_by
            .as_ref()
            .map(|s| format!("\"{}\"", s))
            .unwrap_or_else(|| "null".to_string());
        let approved_at = self
            .approved_at
            .as_ref()
            .map(|s| format!("\"{}\"", s))
            .unwrap_or_else(|| "null".to_string());

        format!(
            r#"{{"change":"{}","old_checksum":"{}","new_checksum":"{}","invariants":[{}],"justification":"{}","approved_by":{},"approved_at":{}}}"#,
            self.change_description,
            old_checksum,
            new_checksum,
            invariants,
            self.justification,
            approved_by,
            approved_at,
        )
    }
}

// =============================================================================
// Trace Summary
// =============================================================================

/// Summary statistics for a trace.
#[derive(Debug, Clone, Default)]
pub struct TraceSummary {
    /// Total events.
    pub total_events: usize,
    /// Spawn events.
    pub spawns: usize,
    /// Complete events.
    pub completes: usize,
    /// Failed events.
    pub failures: usize,
    /// Cancelled events.
    pub cancellations: usize,
    /// Yield events.
    pub yields: usize,
    /// Wakeup events.
    pub wakeups: usize,
    /// First tick.
    pub first_tick: u64,
    /// Last tick.
    pub last_tick: u64,
    /// Checksum.
    pub checksum: u64,
}

impl ScheduleTrace {
    /// Generate summary statistics.
    #[must_use]
    pub fn summary(&self) -> TraceSummary {
        let mut summary = TraceSummary {
            total_events: self.entries.len(),
            checksum: self.checksum(),
            ..Default::default()
        };

        if let Some(first) = self.entries.front() {
            summary.first_tick = first.tick;
        }
        if let Some(last) = self.entries.back() {
            summary.last_tick = last.tick;
        }

        for entry in &self.entries {
            match &entry.event {
                TaskEvent::Spawn { .. } => summary.spawns += 1,
                TaskEvent::Complete { .. } => summary.completes += 1,
                TaskEvent::Failed { .. } => summary.failures += 1,
                TaskEvent::Cancelled { .. } => summary.cancellations += 1,
                TaskEvent::Yield { .. } => summary.yields += 1,
                TaskEvent::Wakeup { .. } => summary.wakeups += 1,
                _ => {}
            }
        }

        summary
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_trace_ordering() {
        let mut trace = ScheduleTrace::new();

        trace.spawn(1, 0, Some("task_a".to_string()));
        trace.advance_tick();
        trace.start(1);
        trace.advance_tick();
        trace.complete(1);

        assert_eq!(trace.len(), 3);

        // Verify ordering
        let entries: Vec<_> = trace.entries().iter().collect();
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[2].seq, 2);
        assert_eq!(entries[0].tick, 0);
        assert_eq!(entries[1].tick, 1);
        assert_eq!(entries[2].tick, 2);
    }

    #[test]
    fn unit_trace_hash_stable() {
        // Create identical traces and verify they produce the same hash
        let mut trace1 = ScheduleTrace::new();
        let mut trace2 = ScheduleTrace::new();

        for trace in [&mut trace1, &mut trace2] {
            trace.spawn(1, 0, None);
            trace.advance_tick();
            trace.start(1);
            trace.advance_tick();
            trace.spawn(2, 1, None);
            trace.advance_tick();
            trace.complete(1);
            trace.start(2);
            trace.advance_tick();
            trace.cancel(2, CancelReason::UserRequest);
        }

        assert_eq!(trace1.checksum(), trace2.checksum());
        assert_eq!(trace1.checksum_hex(), trace2.checksum_hex());
    }

    #[test]
    fn unit_hash_differs_on_order_change() {
        let mut trace1 = ScheduleTrace::new();
        trace1.spawn(1, 0, None);
        trace1.spawn(2, 0, None);

        let mut trace2 = ScheduleTrace::new();
        trace2.spawn(2, 0, None);
        trace2.spawn(1, 0, None);

        assert_ne!(trace1.checksum(), trace2.checksum());
    }

    #[test]
    fn unit_jsonl_format() {
        let mut trace = ScheduleTrace::new();
        trace.spawn(1, 0, Some("test".to_string()));

        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"event\":\"spawn\""));
        assert!(jsonl.contains("\"task_id\":1"));
        assert!(jsonl.contains("\"name\":\"test\""));
    }

    #[test]
    fn unit_summary_counts() {
        let mut trace = ScheduleTrace::new();

        trace.spawn(1, 0, None);
        trace.spawn(2, 0, None);
        trace.start(1);
        trace.complete(1);
        trace.start(2);
        trace.cancel(2, CancelReason::Timeout);

        let summary = trace.summary();
        assert_eq!(summary.total_events, 6);
        assert_eq!(summary.spawns, 2);
        assert_eq!(summary.completes, 1);
        assert_eq!(summary.cancellations, 1);
    }

    #[test]
    fn unit_golden_compare_match() {
        let mut trace = ScheduleTrace::new();
        trace.spawn(1, 0, None);
        trace.complete(1);

        let expected = trace.checksum();
        let result = compare_golden(&trace, expected);
        assert!(result.is_match());
    }

    #[test]
    fn unit_golden_compare_mismatch() {
        let mut trace = ScheduleTrace::new();
        trace.spawn(1, 0, None);

        let result = compare_golden(&trace, 0xDEADBEEF);
        assert!(!result.is_match());

        match result {
            GoldenCompareResult::Mismatch { expected, actual } => {
                assert_eq!(expected, 0xDEADBEEF);
                assert_ne!(actual, 0xDEADBEEF);
            }
            _ => unreachable!("Expected mismatch"),
        }
    }

    #[test]
    fn unit_isomorphism_proof_json() {
        let proof = IsomorphismProof::new("Optimized scheduler loop", 0x1234, 0x5678)
            .with_invariant("All tasks complete in same order")
            .with_invariant("No task starves")
            .with_justification("Loop unrolling only affects timing, not ordering");

        let json = proof.to_json();
        assert!(json.contains("Optimized scheduler loop"));
        assert!(json.contains("0000000000001234"));
        assert!(json.contains("0000000000005678"));
    }

    #[test]
    fn unit_max_entries_enforced() {
        let config = TraceConfig {
            max_entries: 3,
            ..Default::default()
        };
        let mut trace = ScheduleTrace::with_config(config);

        for i in 0..10 {
            trace.spawn(i, 0, None);
        }

        assert_eq!(trace.len(), 3);

        // Should have the last 3 entries (task_id 7, 8, 9)
        let entries: Vec<_> = trace.entries().iter().collect();
        if let TaskEvent::Spawn { task_id, .. } = &entries[0].event {
            assert_eq!(*task_id, 7);
        }
    }

    #[test]
    fn unit_clear_resets_state() {
        let mut trace = ScheduleTrace::new();
        trace.spawn(1, 0, None);
        trace.spawn(2, 0, None);

        trace.clear();

        assert!(trace.is_empty());
        assert_eq!(trace.len(), 0);
    }

    #[test]
    fn unit_wakeup_reasons() {
        let mut trace = ScheduleTrace::new();

        trace.record(TaskEvent::Wakeup {
            task_id: 1,
            reason: WakeupReason::Timer,
        });
        trace.record(TaskEvent::Wakeup {
            task_id: 2,
            reason: WakeupReason::Dependency { task_id: 1 },
        });
        trace.record(TaskEvent::Wakeup {
            task_id: 3,
            reason: WakeupReason::IoReady,
        });

        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"reason\":\"timer\""));
        assert!(jsonl.contains("\"reason\":\"dependency:1\""));
        assert!(jsonl.contains("\"reason\":\"io_ready\""));
    }

    #[test]
    fn unit_auto_snapshot_with_sampling_records_queue() {
        let config = TraceConfig {
            auto_snapshot: true,
            snapshot_sampling: Some(VoiConfig {
                max_interval_events: 1,
                sample_cost: 1.0,
                ..Default::default()
            }),
            snapshot_change_threshold: 1,
            ..Default::default()
        };
        let mut trace = ScheduleTrace::with_config(config);
        let now = Instant::now();

        trace.record_with_queue_state_at(
            TaskEvent::Spawn {
                task_id: 1,
                priority: 0,
                name: None,
            },
            3,
            1,
            now,
        );

        assert!(
            trace
                .entries()
                .iter()
                .any(|entry| matches!(entry.event, TaskEvent::QueueSnapshot { .. }))
        );
        let summary = trace.snapshot_sampling_summary().expect("sampling enabled");
        assert_eq!(summary.total_samples, 1);
    }

    #[test]
    fn unit_cancel_reasons() {
        let mut trace = ScheduleTrace::new();

        trace.cancel(1, CancelReason::UserRequest);
        trace.cancel(2, CancelReason::Timeout);
        trace.cancel(
            3,
            CancelReason::HazardPolicy {
                expected_loss: 0.75,
            },
        );

        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"reason\":\"user_request\""));
        assert!(jsonl.contains("\"reason\":\"timeout\""));
        assert!(jsonl.contains("\"reason\":\"hazard_policy:0.7500\""));
    }

    #[test]
    fn unit_policy_change() {
        let mut trace = ScheduleTrace::new();

        trace.record(TaskEvent::PolicyChange {
            from: SchedulerPolicy::Fifo,
            to: SchedulerPolicy::Priority,
        });

        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"from\":\"fifo\""));
        assert!(jsonl.contains("\"to\":\"priority\""));
    }

    // ── TraceConfig defaults ────────────────────────────────────────────

    #[test]
    fn trace_config_default_values() {
        let config = TraceConfig::default();
        assert_eq!(config.max_entries, 10_000);
        assert!(!config.auto_snapshot);
        assert!(config.snapshot_sampling.is_none());
        assert_eq!(config.snapshot_change_threshold, 1);
        assert_eq!(config.seed, 0);
    }

    // ── ScheduleTrace constructors ──────────────────────────────────────

    #[test]
    fn schedule_trace_default_impl() {
        let trace = ScheduleTrace::default();
        assert!(trace.is_empty());
        assert_eq!(trace.len(), 0);
        assert_eq!(trace.tick(), 0);
    }

    #[test]
    fn with_config_unlimited_entries() {
        let config = TraceConfig {
            max_entries: 0,
            ..Default::default()
        };
        let mut trace = ScheduleTrace::with_config(config);
        for i in 0..50 {
            trace.spawn(i, 0, None);
        }
        assert_eq!(trace.len(), 50);
    }

    // ── Tick management ─────────────────────────────────────────────────

    #[test]
    fn set_tick_explicit() {
        let mut trace = ScheduleTrace::new();
        assert_eq!(trace.tick(), 0);
        trace.set_tick(42);
        assert_eq!(trace.tick(), 42);
        trace.advance_tick();
        assert_eq!(trace.tick(), 43);
    }

    #[test]
    fn advance_tick_increments() {
        let mut trace = ScheduleTrace::new();
        trace.advance_tick();
        trace.advance_tick();
        trace.advance_tick();
        assert_eq!(trace.tick(), 3);
    }

    // ── record_with_queue_state_at without auto_snapshot ─────────────────

    #[test]
    fn record_with_queue_state_no_auto_snapshot() {
        let config = TraceConfig {
            auto_snapshot: false,
            ..Default::default()
        };
        let mut trace = ScheduleTrace::with_config(config);
        let now = Instant::now();
        trace.record_with_queue_state_at(TaskEvent::Start { task_id: 1 }, 5, 2, now);
        // Only the Start event, no snapshot
        assert_eq!(trace.len(), 1);
        assert!(matches!(
            trace.entries().front().unwrap().event,
            TaskEvent::Start { task_id: 1 }
        ));
    }

    // ── snapshot_sampling_summary / logs when no sampler ─────────────────

    #[test]
    fn snapshot_sampling_summary_none_without_sampler() {
        let trace = ScheduleTrace::new();
        assert!(trace.snapshot_sampling_summary().is_none());
    }

    #[test]
    fn snapshot_sampling_logs_none_without_sampler() {
        let trace = ScheduleTrace::new();
        assert!(trace.snapshot_sampling_logs_jsonl().is_none());
    }

    #[test]
    fn snapshot_sampling_logs_some_with_sampler() {
        let config = TraceConfig {
            auto_snapshot: true,
            snapshot_sampling: Some(VoiConfig::default()),
            ..Default::default()
        };
        let trace = ScheduleTrace::with_config(config);
        assert!(trace.snapshot_sampling_logs_jsonl().is_some());
    }

    // ── clear resets sequence counter ────────────────────────────────────

    #[test]
    fn clear_resets_seq_counter() {
        let mut trace = ScheduleTrace::new();
        trace.spawn(1, 0, None);
        trace.spawn(2, 0, None);
        assert_eq!(trace.entries().back().unwrap().seq, 1);

        trace.clear();
        trace.spawn(3, 0, None);
        // After clear, seq restarts from 0
        assert_eq!(trace.entries().front().unwrap().seq, 0);
    }

    #[test]
    fn clear_resets_sampler() {
        let config = TraceConfig {
            auto_snapshot: true,
            snapshot_sampling: Some(VoiConfig {
                max_interval_events: 1,
                sample_cost: 1.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut trace = ScheduleTrace::with_config(config);
        let now = Instant::now();
        trace.record_with_queue_state_at(
            TaskEvent::Spawn {
                task_id: 1,
                priority: 0,
                name: None,
            },
            3,
            1,
            now,
        );
        trace.clear();
        assert!(trace.is_empty());
        // Sampler still exists but is fresh
        let summary = trace.snapshot_sampling_summary().unwrap();
        assert_eq!(summary.total_samples, 0);
    }

    // ── Checksum edge cases ─────────────────────────────────────────────

    #[test]
    fn checksum_empty_trace() {
        let trace = ScheduleTrace::new();
        // FNV-1a offset basis for empty input
        assert_eq!(trace.checksum(), 0xcbf29ce484222325);
    }

    #[test]
    fn checksum_hex_format() {
        let trace = ScheduleTrace::new();
        let hex = trace.checksum_hex();
        assert_eq!(hex.len(), 16);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn checksum_differs_for_different_events() {
        let mut t1 = ScheduleTrace::new();
        t1.spawn(1, 0, None);

        let mut t2 = ScheduleTrace::new();
        t2.start(1);

        assert_ne!(t1.checksum(), t2.checksum());
    }

    // ── GoldenCompareResult ─────────────────────────────────────────────

    #[test]
    fn golden_missing_golden_variant() {
        let result = GoldenCompareResult::MissingGolden;
        assert!(!result.is_match());
    }

    #[test]
    fn golden_match_variant() {
        assert!(GoldenCompareResult::Match.is_match());
    }

    // ── SchedulerPolicy Display ─────────────────────────────────────────

    #[test]
    fn scheduler_policy_display_all_variants() {
        assert_eq!(format!("{}", SchedulerPolicy::Fifo), "fifo");
        assert_eq!(format!("{}", SchedulerPolicy::Priority), "priority");
        assert_eq!(
            format!("{}", SchedulerPolicy::ShortestFirst),
            "shortest_first"
        );
        assert_eq!(format!("{}", SchedulerPolicy::RoundRobin), "round_robin");
        assert_eq!(
            format!("{}", SchedulerPolicy::WeightedFair),
            "weighted_fair"
        );
    }

    // ── TraceSummary coverage ───────────────────────────────────────────

    #[test]
    fn summary_yields_wakeups_failures() {
        let mut trace = ScheduleTrace::new();
        trace.spawn(1, 0, None);
        trace.start(1);
        trace.record(TaskEvent::Yield { task_id: 1 });
        trace.record(TaskEvent::Wakeup {
            task_id: 1,
            reason: WakeupReason::Timer,
        });
        trace.record(TaskEvent::Failed {
            task_id: 1,
            error: "oops".to_string(),
        });

        let summary = trace.summary();
        assert_eq!(summary.yields, 1);
        assert_eq!(summary.wakeups, 1);
        assert_eq!(summary.failures, 1);
        assert_eq!(summary.spawns, 1);
        assert_eq!(summary.completes, 0);
        assert_eq!(summary.cancellations, 0);
    }

    #[test]
    fn summary_tick_range() {
        let mut trace = ScheduleTrace::new();
        trace.set_tick(10);
        trace.spawn(1, 0, None);
        trace.set_tick(50);
        trace.complete(1);

        let summary = trace.summary();
        assert_eq!(summary.first_tick, 10);
        assert_eq!(summary.last_tick, 50);
    }

    #[test]
    fn summary_empty_trace() {
        let trace = ScheduleTrace::new();
        let summary = trace.summary();
        assert_eq!(summary.total_events, 0);
        assert_eq!(summary.first_tick, 0);
        assert_eq!(summary.last_tick, 0);
    }

    #[test]
    fn trace_summary_default() {
        let summary = TraceSummary::default();
        assert_eq!(summary.total_events, 0);
        assert_eq!(summary.spawns, 0);
        assert_eq!(summary.checksum, 0);
    }

    // ── JSONL format for uncovered event types ──────────────────────────

    #[test]
    fn jsonl_yield_event() {
        let mut trace = ScheduleTrace::new();
        trace.record(TaskEvent::Yield { task_id: 7 });
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"event\":\"yield\""));
        assert!(jsonl.contains("\"task_id\":7"));
    }

    #[test]
    fn jsonl_failed_event() {
        let mut trace = ScheduleTrace::new();
        trace.record(TaskEvent::Failed {
            task_id: 3,
            error: "timeout".to_string(),
        });
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"event\":\"failed\""));
        assert!(jsonl.contains("\"error\":\"timeout\""));
    }

    #[test]
    fn jsonl_custom_event() {
        let mut trace = ScheduleTrace::new();
        trace.record(TaskEvent::Custom {
            tag: "metric".to_string(),
            data: "cpu=42".to_string(),
        });
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"event\":\"custom\""));
        assert!(jsonl.contains("\"tag\":\"metric\""));
        assert!(jsonl.contains("\"data\":\"cpu=42\""));
    }

    #[test]
    fn jsonl_queue_snapshot_event() {
        let mut trace = ScheduleTrace::new();
        trace.record(TaskEvent::QueueSnapshot {
            queued: 5,
            running: 2,
        });
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"event\":\"queue_snapshot\""));
        assert!(jsonl.contains("\"queued\":5"));
        assert!(jsonl.contains("\"running\":2"));
    }

    #[test]
    fn jsonl_cancelled_event() {
        let mut trace = ScheduleTrace::new();
        trace.cancel(4, CancelReason::Shutdown);
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"event\":\"cancelled\""));
        assert!(jsonl.contains("\"reason\":\"shutdown\""));
    }

    #[test]
    fn jsonl_cancel_other_reason() {
        let mut trace = ScheduleTrace::new();
        trace.cancel(5, CancelReason::Other("oom".to_string()));
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"reason\":\"other:oom\""));
    }

    #[test]
    fn jsonl_spawn_without_name() {
        let mut trace = ScheduleTrace::new();
        trace.spawn(1, 3, None);
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"name\":null"));
        assert!(jsonl.contains("\"priority\":3"));
    }

    #[test]
    fn jsonl_complete_event() {
        let mut trace = ScheduleTrace::new();
        trace.complete(99);
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"event\":\"complete\""));
        assert!(jsonl.contains("\"task_id\":99"));
    }

    #[test]
    fn jsonl_start_event() {
        let mut trace = ScheduleTrace::new();
        trace.start(42);
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"event\":\"start\""));
        assert!(jsonl.contains("\"task_id\":42"));
    }

    #[test]
    fn jsonl_empty_trace() {
        let trace = ScheduleTrace::new();
        assert_eq!(trace.to_jsonl(), "");
    }

    // ── Wakeup reasons JSONL ────────────────────────────────────────────

    #[test]
    fn jsonl_wakeup_user_action() {
        let mut trace = ScheduleTrace::new();
        trace.record(TaskEvent::Wakeup {
            task_id: 1,
            reason: WakeupReason::UserAction,
        });
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"reason\":\"user_action\""));
    }

    #[test]
    fn jsonl_wakeup_explicit() {
        let mut trace = ScheduleTrace::new();
        trace.record(TaskEvent::Wakeup {
            task_id: 1,
            reason: WakeupReason::Explicit,
        });
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"reason\":\"explicit\""));
    }

    #[test]
    fn jsonl_wakeup_other() {
        let mut trace = ScheduleTrace::new();
        trace.record(TaskEvent::Wakeup {
            task_id: 1,
            reason: WakeupReason::Other("custom".to_string()),
        });
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"reason\":\"other:custom\""));
    }

    // ── IsomorphismProof ────────────────────────────────────────────────

    #[test]
    fn isomorphism_proof_with_approval() {
        let mut proof = IsomorphismProof::new("test change", 0xAA, 0xBB);
        proof.approved_by = Some("reviewer".to_string());
        proof.approved_at = Some("2026-01-01".to_string());

        let json = proof.to_json();
        assert!(json.contains("\"approved_by\":\"reviewer\""));
        assert!(json.contains("\"approved_at\":\"2026-01-01\""));
    }

    #[test]
    fn isomorphism_proof_without_approval() {
        let proof = IsomorphismProof::new("refactor", 0x11, 0x22);
        let json = proof.to_json();
        assert!(json.contains("\"approved_by\":null"));
        assert!(json.contains("\"approved_at\":null"));
    }

    #[test]
    fn isomorphism_proof_builder_chain() {
        let proof = IsomorphismProof::new("change", 1, 2)
            .with_invariant("ordering preserved")
            .with_invariant("no data loss")
            .with_justification("pure refactor");

        assert_eq!(proof.preserved_invariants.len(), 2);
        assert_eq!(proof.justification, "pure refactor");
        let json = proof.to_json();
        assert!(json.contains("ordering preserved"));
        assert!(json.contains("no data loss"));
        assert!(json.contains("pure refactor"));
    }

    // ── TraceEntry to_jsonl seq/tick ────────────────────────────────────

    #[test]
    fn trace_entry_jsonl_includes_seq_tick() {
        let mut trace = ScheduleTrace::new();
        trace.set_tick(7);
        trace.spawn(1, 0, None);
        let jsonl = trace.to_jsonl();
        assert!(jsonl.contains("\"seq\":0"));
        assert!(jsonl.contains("\"tick\":7"));
    }

    // ── Multiple events JSONL produces one line per entry ────────────────

    #[test]
    fn jsonl_multiple_entries_newline_separated() {
        let mut trace = ScheduleTrace::new();
        trace.spawn(1, 0, None);
        trace.start(1);
        trace.complete(1);

        let jsonl = trace.to_jsonl();
        let lines: Vec<_> = jsonl.lines().collect();
        assert_eq!(lines.len(), 3);
    }

    // ── snapshot_change_threshold ────────────────────────────────────────

    #[test]
    fn auto_snapshot_no_violation_below_threshold() {
        let config = TraceConfig {
            auto_snapshot: true,
            snapshot_sampling: Some(VoiConfig {
                max_interval_events: 1,
                sample_cost: 1.0,
                ..Default::default()
            }),
            snapshot_change_threshold: 10,
            ..Default::default()
        };
        let mut trace = ScheduleTrace::with_config(config);
        let now = Instant::now();

        // First event sets last_snapshot
        trace.record_with_queue_state_at(
            TaskEvent::Spawn {
                task_id: 1,
                priority: 0,
                name: None,
            },
            5,
            1,
            now,
        );
        // Second event: delta = |5-6| + |1-1| = 1, below threshold of 10
        trace.record_with_queue_state_at(TaskEvent::Start { task_id: 1 }, 6, 1, now);

        // Should have recorded snapshots (sampling says yes) but observe with violated=false
        let summary = trace.snapshot_sampling_summary().unwrap();
        assert_eq!(summary.total_samples, 2);
    }

    // ── event_to_bytes covers PolicyChange discriminant ──────────────────

    #[test]
    fn checksum_includes_policy_change() {
        let mut t1 = ScheduleTrace::new();
        t1.record(TaskEvent::PolicyChange {
            from: SchedulerPolicy::Fifo,
            to: SchedulerPolicy::Priority,
        });

        let mut t2 = ScheduleTrace::new();
        t2.record(TaskEvent::PolicyChange {
            from: SchedulerPolicy::Priority,
            to: SchedulerPolicy::Fifo,
        });

        assert_ne!(t1.checksum(), t2.checksum());
    }

    #[test]
    fn checksum_includes_custom_event_data() {
        let mut t1 = ScheduleTrace::new();
        t1.record(TaskEvent::Custom {
            tag: "a".to_string(),
            data: "1".to_string(),
        });

        let mut t2 = ScheduleTrace::new();
        t2.record(TaskEvent::Custom {
            tag: "b".to_string(),
            data: "1".to_string(),
        });

        assert_ne!(t1.checksum(), t2.checksum());
    }
}
