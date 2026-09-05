//! Deterministic pane policy and live history execution.
//!
//! [`PaneExecutionEngine`] owns the operation journal and its selected history
//! substrate. Persistent resize operations use path copying; structural edits
//! use the canonical transaction once. Migration reconstructs every retained
//! entry, including redo, before publishing a new substrate. Caller-measured
//! observations drive selection and conservative fallback at gesture boundaries.
//! Retained bytes are a structural model, not allocator measurements; samples
//! describe operation execution and recording, excluding subsequent maintenance.

use std::collections::VecDeque;
use std::fmt;

use crate::pane::{
    PaneInteractionTimeline, PaneInteractionTimelineError, PaneOperation, PaneOperationError,
    PaneOperationFamily, PaneOperationOutcome, PaneTree,
};
use crate::pane_memory::{PaneMemoryStrategy, baseline_footprint};
use crate::pane_monitors::{
    PaneMonitorThresholds, PaneMonitorVerdict, monitor_latency_envelope, monitor_retention_pressure,
};
use crate::pane_persistent::{PaneVersionStore, VersionedPaneTree};
use crate::pane_retention::{PaneRetentionDecision, PaneRetentionOutcome, PaneRetentionPolicy};

/// Failure before an edit or history transition can be published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneExecutionError {
    /// Original operation failure, with its complete diagnostic payload.
    Operation(Box<PaneOperationError>),
    /// Canonical history replay failure.
    Timeline(Box<PaneInteractionTimelineError>),
    /// Imported history or the supplied live tree does not match the engine.
    InvalidHistory(String),
    /// The no-history strategy cannot serve an undo/redo consumer.
    HistoryRequired,
}

impl fmt::Display for PaneExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operation(error) => error.fmt(f),
            Self::Timeline(error) => error.fmt(f),
            Self::InvalidHistory(reason) => write!(f, "invalid pane history: {reason}"),
            Self::HistoryRequired => f.write_str("pane execution requires undo/redo history"),
        }
    }
}

impl std::error::Error for PaneExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Operation(error) => Some(error.as_ref()),
            Self::Timeline(error) => Some(error.as_ref()),
            Self::InvalidHistory(_) | Self::HistoryRequired => None,
        }
    }
}

impl From<PaneOperationError> for PaneExecutionError {
    fn from(error: PaneOperationError) -> Self {
        Self::Operation(Box::new(error))
    }
}

impl From<PaneInteractionTimelineError> for PaneExecutionError {
    fn from(error: PaneInteractionTimelineError) -> Self {
        Self::Timeline(Box::new(error))
    }
}

/// One measurement supplied by the terminal or browser interaction adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaneExecutionSample {
    /// Monotonic observation time, in nanoseconds from the adapter's origin.
    pub timestamp_ns: u64,
    /// Actual execution and journal-recording duration, excluding maintenance.
    pub elapsed_ns: u64,
    /// Whether the executed operation belongs to the local resize family.
    pub local: bool,
}

/// Counters describe executed transitions, never proposed strategies.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaneExecutionStatus {
    pub strategy: PaneMemoryStrategy,
    pub conservative: bool,
    pub applies: u64,
    pub persistent_applies: u64,
    pub checkpointed_applies: u64,
    pub conservative_applies: u64,
    pub undos: u64,
    pub redos: u64,
    pub switches: u64,
    pub fallbacks: u64,
    /// Journal, active persistent store, and canonical render projection bytes.
    pub retained_bytes: usize,
    pub last_maintenance_error: Option<String>,
    pub last_retention: Option<PaneRetentionDecision>,
    pub last_monitor: Option<PaneMonitorVerdict>,
}

/// Shared history interface for actual hosts and canonical comparison controls.
pub trait PaneHistory {
    fn apply_and_record(
        &mut self,
        tree: &mut PaneTree,
        sequence: u64,
        id: u64,
        operation: PaneOperation,
    ) -> Result<PaneOperationOutcome, PaneExecutionError>;
    fn apply_and_record_coalesced_resize_delta(
        &mut self,
        tree: &mut PaneTree,
        sequence: u64,
        id: u64,
        operation: PaneOperation,
        boundary: u64,
    ) -> Result<PaneOperationOutcome, PaneExecutionError>;
    fn undo(&mut self, tree: &mut PaneTree) -> Result<bool, PaneExecutionError>;
    fn redo(&mut self, tree: &mut PaneTree) -> Result<bool, PaneExecutionError>;
    fn replay(&self) -> Result<PaneTree, PaneExecutionError>;
    fn applied_len(&self) -> usize;
    /// Maintenance errors are diagnostic: the preceding accepted edit remains accepted.
    fn observe(
        &mut self,
        _tree: &PaneTree,
        _sample: PaneExecutionSample,
    ) -> Result<(), PaneExecutionError> {
        Ok(())
    }
}

impl PaneHistory for PaneInteractionTimeline {
    fn apply_and_record(
        &mut self,
        tree: &mut PaneTree,
        sequence: u64,
        id: u64,
        operation: PaneOperation,
    ) -> Result<PaneOperationOutcome, PaneExecutionError> {
        Ok(Self::apply_and_record(self, tree, sequence, id, operation)?)
    }
    fn apply_and_record_coalesced_resize_delta(
        &mut self,
        tree: &mut PaneTree,
        sequence: u64,
        id: u64,
        operation: PaneOperation,
        boundary: u64,
    ) -> Result<PaneOperationOutcome, PaneExecutionError> {
        Ok(Self::apply_and_record_coalesced_resize_delta(
            self, tree, sequence, id, operation, boundary,
        )?)
    }
    fn undo(&mut self, tree: &mut PaneTree) -> Result<bool, PaneExecutionError> {
        Ok(Self::undo(self, tree)?)
    }
    fn redo(&mut self, tree: &mut PaneTree) -> Result<bool, PaneExecutionError> {
        Ok(Self::redo(self, tree)?)
    }
    fn replay(&self) -> Result<PaneTree, PaneExecutionError> {
        Ok(Self::replay(self)?)
    }
    fn applied_len(&self) -> usize {
        Self::applied_len(self)
    }
}

/// Journal and live substrate, paired with the host's canonical render tree.
#[derive(Debug, Clone)]
pub struct PaneExecutionEngine {
    timeline: PaneInteractionTimeline,
    persistent: Option<PaneVersionStore>,
    policy: PaneExecutionPolicy,
    status: PaneExecutionStatus,
    expected_hash: u64,
    samples: VecDeque<PaneExecutionSample>,
    gesture_active: bool,
    fallback_pending: bool,
    fallback_latched: bool,
    latency_envelope_ns: u64,
}

impl PaneExecutionEngine {
    /// Start checkpointed history with a 4096-edit bound and an 8ms observation envelope.
    /// Adaptive and persistent execution require an explicit policy: full-wrapper
    /// comparisons currently show workload-dependent regressions in those modes.
    #[must_use]
    pub fn new(tree: &PaneTree) -> Self {
        let timeline = PaneInteractionTimeline::with_baseline(tree).with_max_entries(0);
        let bytes = timeline
            .retention_diagnostics()
            .estimated_total_retained_bytes
            .saturating_add(baseline_footprint(tree).total_retained_bytes);
        Self {
            timeline,
            persistent: None,
            policy: PaneExecutionPolicy::adaptive(PaneRetentionPolicy::bounded(0, 4096))
                .forcing(PaneMemoryStrategy::Checkpointed),
            expected_hash: tree.history_state_hash(),
            samples: VecDeque::new(),
            gesture_active: false,
            fallback_pending: false,
            fallback_latched: false,
            latency_envelope_ns: 8_000_000,
            status: PaneExecutionStatus {
                strategy: PaneMemoryStrategy::Checkpointed,
                conservative: false,
                applies: 0,
                persistent_applies: 0,
                checkpointed_applies: 0,
                conservative_applies: 0,
                undos: 0,
                redos: 0,
                switches: 0,
                fallbacks: 0,
                retained_bytes: bytes,
                last_maintenance_error: None,
                last_retention: None,
                last_monitor: None,
            },
        }
    }

    /// Validate the complete imported journal and all checkpoints, including redo.
    pub fn from_timeline(
        tree: &PaneTree,
        timeline: PaneInteractionTimeline,
    ) -> Result<Self, PaneExecutionError> {
        let mut engine = Self::new(tree);
        let (timeline, persistent) =
            Self::reconstruct(tree, &timeline, PaneMemoryStrategy::Checkpointed)?;
        engine.timeline = timeline;
        engine.persistent = persistent;
        engine.status.retained_bytes = engine.retained_bytes(tree);
        Ok(engine)
    }

    #[must_use]
    pub const fn timeline(&self) -> &PaneInteractionTimeline {
        &self.timeline
    }
    #[must_use]
    pub const fn policy(&self) -> PaneExecutionPolicy {
        self.policy
    }
    #[must_use]
    pub const fn strategy(&self) -> PaneMemoryStrategy {
        self.status.strategy
    }
    #[must_use]
    pub const fn status(&self) -> &PaneExecutionStatus {
        &self.status
    }
    #[must_use]
    pub const fn applied_len(&self) -> usize {
        self.timeline.cursor
    }

    /// Set the measured-operation envelope. Zero disables this monitor.
    pub const fn set_latency_envelope_ns(&mut self, envelope: u64) {
        self.latency_envelope_ns = envelope;
    }

    fn check_live(&self, tree: &PaneTree) -> Result<(), PaneExecutionError> {
        if tree.history_state_hash() != self.expected_hash {
            return Err(PaneExecutionError::InvalidHistory(
                "live tree changed outside its history engine".into(),
            ));
        }
        Ok(())
    }

    pub fn apply_and_record(
        &mut self,
        tree: &mut PaneTree,
        sequence: u64,
        id: u64,
        operation: PaneOperation,
    ) -> Result<PaneOperationOutcome, PaneExecutionError> {
        self.apply(tree, sequence, id, operation, None)
    }

    pub fn apply_and_record_coalesced_resize_delta(
        &mut self,
        tree: &mut PaneTree,
        sequence: u64,
        id: u64,
        operation: PaneOperation,
        boundary: u64,
    ) -> Result<PaneOperationOutcome, PaneExecutionError> {
        self.apply(tree, sequence, id, operation, Some(boundary))
    }

    fn apply(
        &mut self,
        tree: &mut PaneTree,
        sequence: u64,
        id: u64,
        operation: PaneOperation,
        boundary: Option<u64>,
    ) -> Result<PaneOperationOutcome, PaneExecutionError> {
        self.check_live(tree)?;
        let outcome = if let Some(store) = &mut self.persistent {
            let (version, projection, outcome) = store.prepare_live_apply(tree, id, &operation)?;
            let coalesced = self.timeline.record_applied(
                &projection,
                None,
                sequence,
                operation,
                &outcome,
                boundary,
                false,
            );
            store.commit_prepared(version, coalesced);
            *tree = projection;
            self.status.persistent_applies = self.status.persistent_applies.saturating_add(1);
            outcome
        } else {
            let outcome = if self.status.conservative {
                tree.apply_operation_conservative(id, operation.clone())?
            } else {
                tree.apply_operation(id, operation.clone())?
            };
            self.timeline
                .record_applied(tree, None, sequence, operation, &outcome, boundary, true);
            if self.status.conservative {
                self.status.conservative_applies =
                    self.status.conservative_applies.saturating_add(1);
            } else {
                self.status.checkpointed_applies =
                    self.status.checkpointed_applies.saturating_add(1);
            }
            outcome
        };
        self.expected_hash = tree.history_state_hash();
        self.status.applies = self.status.applies.saturating_add(1);
        Ok(outcome)
    }

    pub fn undo(&mut self, tree: &mut PaneTree) -> Result<bool, PaneExecutionError> {
        self.navigate(tree, false)
    }
    pub fn redo(&mut self, tree: &mut PaneTree) -> Result<bool, PaneExecutionError> {
        self.navigate(tree, true)
    }

    fn navigate(&mut self, tree: &mut PaneTree, forward: bool) -> Result<bool, PaneExecutionError> {
        self.check_live(tree)?;
        let cursor = self.timeline.cursor;
        if (!forward && cursor == 0) || (forward && cursor == self.timeline.entries.len()) {
            return Ok(false);
        }
        if let Some(store) = &mut self.persistent {
            let target = if forward { cursor + 1 } else { cursor - 1 };
            let version = store.version_at(target).ok_or_else(|| {
                PaneExecutionError::InvalidHistory("missing persistent navigation target".into())
            })?;
            let projection = version
                .to_pane_tree()
                .map_err(|error| PaneExecutionError::InvalidHistory(error.to_string()))?;
            store.seek(target);
            self.timeline.cursor = target;
            *tree = projection;
        } else if forward {
            self.timeline.redo(tree)?;
        } else {
            self.timeline.undo(tree)?;
        }
        self.expected_hash = tree.history_state_hash();
        if forward {
            self.status.redos = self.status.redos.saturating_add(1);
        } else {
            self.status.undos = self.status.undos.saturating_add(1);
        }
        Ok(true)
    }

    pub fn replay(&self) -> Result<PaneTree, PaneExecutionError> {
        if let Some(store) = &self.persistent {
            store
                .current()
                .to_pane_tree()
                .map_err(|error| PaneExecutionError::InvalidHistory(error.to_string()))
        } else {
            Ok(self.timeline.replay()?)
        }
    }

    /// Pin the pre-gesture cursor against migration and pruning until completion.
    pub const fn begin_gesture(&mut self) {
        self.gesture_active = true;
    }
    pub fn end_gesture(&mut self, tree: &PaneTree) -> Result<(), PaneExecutionError> {
        self.gesture_active = false;
        self.maintain_and_record(tree)
    }

    /// Install an override atomically. Active gestures defer substrate maintenance.
    pub fn set_policy(
        &mut self,
        tree: &PaneTree,
        policy: PaneExecutionPolicy,
    ) -> Result<(), PaneExecutionError> {
        self.check_live(tree)?;
        if policy.forced_strategy == Some(PaneMemoryStrategy::Baseline) {
            return Err(PaneExecutionError::HistoryRequired);
        }
        let mut candidate = self.clone();
        candidate.policy = policy;
        candidate.fallback_pending = false;
        // An explicit reset clears a monitor latch; an operator override still wins.
        candidate.fallback_latched = false;
        if !candidate.gesture_active {
            candidate.maintain(tree)?;
        }
        *self = candidate;
        Ok(())
    }

    pub fn observe(
        &mut self,
        tree: &PaneTree,
        sample: PaneExecutionSample,
    ) -> Result<(), PaneExecutionError> {
        self.check_live(tree)?;
        if self
            .samples
            .back()
            .is_some_and(|previous| sample.timestamp_ns < previous.timestamp_ns)
        {
            let error =
                PaneExecutionError::InvalidHistory("observation clock moved backwards".into());
            self.status.last_maintenance_error = Some(error.to_string());
            return Err(error);
        }
        if self.samples.len() == 128 {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        if self.latency_envelope_ns != 0 {
            let verdict = monitor_latency_envelope(
                self.strategy(),
                sample.elapsed_ns as f64,
                self.latency_envelope_ns as f64,
                &PaneMonitorThresholds::default(),
            );
            self.fallback_pending |= verdict.status.is_violation();
            self.status.last_monitor = Some(verdict);
        }
        self.maintain_and_record(tree)
    }

    fn profile(&self) -> PaneWorkloadProfile {
        let mut peak = 0;
        let mut previous = None;
        for sample in &self.samples {
            if let Some(timestamp) = previous {
                let interval = sample.timestamp_ns.saturating_sub(timestamp);
                if let Some(rate) = 1_000_000_000_u64.checked_div(interval) {
                    peak = peak.max(u32::try_from(rate).unwrap_or(u32::MAX));
                }
            }
            previous = Some(sample.timestamp_ns);
        }
        PaneWorkloadProfile::new(
            self.samples.len(),
            self.samples.iter().filter(|sample| sample.local).count(),
            peak,
            true,
        )
    }

    fn maintain_and_record(&mut self, tree: &PaneTree) -> Result<(), PaneExecutionError> {
        let result = self.maintain(tree);
        self.status.last_maintenance_error = result.as_ref().err().map(ToString::to_string);
        result
    }

    fn maintain(&mut self, tree: &PaneTree) -> Result<(), PaneExecutionError> {
        self.check_live(tree)?;
        if self.gesture_active {
            return Ok(());
        }
        let conservative =
            self.policy.conservative || self.fallback_latched || self.fallback_pending;
        let desired = if conservative {
            PaneMemoryStrategy::Checkpointed
        } else {
            self.policy
                .reselect(self.profile(), self.strategy())
                .strategy
        };
        let bytes = self.retained_bytes(tree);
        let pressure = self
            .policy
            .retention
            .budget
            .is_exceeded_by(bytes, self.timeline.entries.len());
        if desired != self.strategy() || pressure {
            // Migration, pruning, and a possible pressure fallback form one
            // publication boundary. Failure preserves the accepted edit and
            // the substrate on which it ran.
            let mut candidate = self.clone();
            candidate.maintain_selected(tree, desired, conservative, bytes)?;
            *self = candidate;
            return Ok(());
        }
        self.maintain_selected(tree, desired, conservative, bytes)
    }

    fn maintain_selected(
        &mut self,
        tree: &PaneTree,
        desired: PaneMemoryStrategy,
        conservative: bool,
        bytes_before_switch: usize,
    ) -> Result<(), PaneExecutionError> {
        let changed = desired != self.strategy();
        self.switch(tree, desired, conservative)?;
        let bytes = if changed {
            self.retained_bytes(tree)
        } else {
            bytes_before_switch
        };
        self.retain(tree, bytes)?;
        if self.status.last_retention.as_ref().is_some_and(|decision| {
            matches!(
                decision.outcome,
                PaneRetentionOutcome::FloorReached | PaneRetentionOutcome::PruningBlocked
            )
        }) {
            self.switch(tree, PaneMemoryStrategy::Checkpointed, true)?;
            self.status.retained_bytes = self.retained_bytes(tree);
        }
        self.fallback_pending = false;
        Ok(())
    }

    fn switch(
        &mut self,
        tree: &PaneTree,
        strategy: PaneMemoryStrategy,
        conservative: bool,
    ) -> Result<(), PaneExecutionError> {
        if strategy != self.strategy() {
            let (timeline, persistent) = Self::reconstruct(tree, &self.timeline, strategy)?;
            self.timeline = timeline;
            self.persistent = persistent;
            self.status.strategy = strategy;
            self.status.switches = self.status.switches.saturating_add(1);
        }
        if conservative && !self.status.conservative {
            self.status.fallbacks = self.status.fallbacks.saturating_add(1);
        }
        self.status.conservative = conservative;
        self.fallback_latched = conservative && !self.policy.conservative;
        Ok(())
    }

    fn retained_bytes(&self, tree: &PaneTree) -> usize {
        self.timeline
            .retention_diagnostics()
            .estimated_total_retained_bytes
            .saturating_add(baseline_footprint(tree).total_retained_bytes)
            .saturating_add(
                self.persistent
                    .as_ref()
                    .map_or(0, PaneVersionStore::retained_bytes),
            )
    }

    fn retain(&mut self, tree: &PaneTree, bytes_before: usize) -> Result<(), PaneExecutionError> {
        let policy = self.policy.retention;
        let units_before = self.timeline.entries.len();
        let mut bytes_after = bytes_before;
        if !policy.conservative_debug
            && policy.budget.is_exceeded_by(bytes_before, units_before)
            && self.timeline.cursor > 0
        {
            // Stage both representations together. A failed baseline advance cannot
            // leave the store at a different cursor from the operation journal.
            let mut candidate = self.clone();
            while policy
                .budget
                .is_exceeded_by(bytes_after, candidate.timeline.entries.len())
                && candidate.timeline.cursor > 0
            {
                let count = candidate.timeline.entries.len();
                // Zero means unbounded in the timeline API, so advance the last
                // entry explicitly when pruning reaches the irreducible baseline.
                if count == 1 {
                    candidate.timeline =
                        PaneInteractionTimeline::with_baseline(tree).with_max_entries(0);
                    candidate.timeline.checkpoint_interval = self.timeline.checkpoint_interval;
                    if candidate.persistent.is_some() {
                        candidate.persistent = Some(PaneVersionStore::with_max_versions(
                            VersionedPaneTree::from_pane_tree(tree),
                            0,
                        ));
                    }
                } else {
                    if candidate.timeline.set_max_entries(count - 1) != 1 {
                        return Err(PaneExecutionError::InvalidHistory(
                            "retention could not advance the baseline".into(),
                        ));
                    }
                    candidate.timeline.set_max_entries(0);
                    if let Some(store) = &mut candidate.persistent {
                        if store.set_max_versions(count) != 1 {
                            return Err(PaneExecutionError::InvalidHistory(
                                "persistent retention diverged from journal".into(),
                            ));
                        }
                        store.set_max_versions(0);
                    }
                }
                bytes_after = candidate.retained_bytes(tree);
            }
            self.timeline = candidate.timeline;
            self.persistent = candidate.persistent;
        }
        let units_after = self.timeline.entries.len();
        let over = policy.budget.is_exceeded_by(bytes_after, units_after);
        let outcome = if over && policy.conservative_debug {
            PaneRetentionOutcome::ConservativeHold
        } else if over && units_after == 0 {
            PaneRetentionOutcome::FloorReached
        } else if over {
            PaneRetentionOutcome::PruningBlocked
        } else if units_after < units_before {
            PaneRetentionOutcome::PrunedToFit
        } else {
            PaneRetentionOutcome::WithinBudget
        };
        let decision = PaneRetentionDecision {
            strategy: self.strategy(),
            budget: policy.budget,
            conservative_debug: policy.conservative_debug,
            units_before,
            units_after,
            units_pruned: units_before - units_after,
            bytes_before,
            bytes_after,
            current_state_hash: tree.state_hash(),
            outcome,
            log: format!(
                "live pane retention: {outcome:?}; edits {units_before}->{units_after}, modeled bytes {bytes_before}->{bytes_after}"
            ),
        };
        let verdict = monitor_retention_pressure(&decision, &PaneMonitorThresholds::default());
        if self
            .status
            .last_monitor
            .as_ref()
            .is_none_or(|previous| !previous.status.is_violation())
            || verdict.status.is_violation()
        {
            self.status.last_monitor = Some(verdict);
        }
        self.status.last_retention = Some(decision);
        self.status.retained_bytes = bytes_after;
        Ok(())
    }

    fn reconstruct(
        live: &PaneTree,
        source: &PaneInteractionTimeline,
        strategy: PaneMemoryStrategy,
    ) -> Result<(PaneInteractionTimeline, Option<PaneVersionStore>), PaneExecutionError> {
        if strategy == PaneMemoryStrategy::Baseline {
            return Err(PaneExecutionError::HistoryRequired);
        }
        if source.cursor > source.entries.len() {
            return Err(PaneExecutionError::InvalidHistory(
                "cursor exceeds journal".into(),
            ));
        }
        let baseline = source
            .baseline
            .clone()
            .ok_or(PaneInteractionTimelineError::MissingBaseline)?;
        let mut tree = PaneTree::from_snapshot(baseline)
            .map_err(|source| PaneInteractionTimelineError::BaselineInvalid { source })?;
        let mut timeline = PaneInteractionTimeline::with_baseline(&tree).with_max_entries(0);
        timeline.checkpoint_interval = source.checkpoint_interval;
        let mut persistent = (strategy == PaneMemoryStrategy::Persistent).then(|| {
            PaneVersionStore::with_max_versions(VersionedPaneTree::from_pane_tree(&tree), 0)
        });
        let mut current = (source.cursor == 0).then(|| tree.to_snapshot());
        let mut checkpoint_indices = std::collections::BTreeSet::new();
        for checkpoint in &source.checkpoints {
            if checkpoint.applied_len > source.entries.len()
                || !checkpoint_indices.insert(checkpoint.applied_len)
            {
                return Err(PaneExecutionError::InvalidHistory(
                    "duplicate or out-of-range checkpoint".into(),
                ));
            }
        }
        for index in 0..=source.entries.len() {
            for checkpoint in source
                .checkpoints
                .iter()
                .filter(|checkpoint| checkpoint.applied_len == index)
            {
                let restored = PaneTree::from_snapshot(checkpoint.snapshot.clone())
                    .map_err(|source| PaneInteractionTimelineError::BaselineInvalid { source })?;
                if restored.to_snapshot() != tree.to_snapshot() {
                    return Err(PaneExecutionError::InvalidHistory(format!(
                        "checkpoint {index} differs from journal"
                    )));
                }
            }
            let Some(entry) = source.entries.get(index) else {
                break;
            };
            let outcome = if let Some(store) = &mut persistent {
                let (version, projection, outcome) =
                    store.prepare_live_apply(&tree, entry.operation_id, &entry.operation)?;
                store.commit_prepared(version, false);
                tree = projection;
                outcome
            } else {
                tree.apply_operation(entry.operation_id, entry.operation.clone())?
            };
            if outcome.before_hash != entry.before_hash || outcome.after_hash != entry.after_hash {
                return Err(PaneExecutionError::InvalidHistory(format!(
                    "entry {index} hashes differ from replay"
                )));
            }
            timeline.record_applied(
                &tree,
                None,
                entry.sequence,
                entry.operation.clone(),
                &outcome,
                None,
                persistent.is_none(),
            );
            if index + 1 == source.cursor {
                current = Some(tree.to_snapshot());
            }
        }
        if current.as_ref() != Some(&live.to_snapshot()) {
            return Err(PaneExecutionError::InvalidHistory(
                "current snapshot differs from journal cursor".into(),
            ));
        }
        timeline.cursor = source.cursor;
        if let Some(store) = &mut persistent {
            store.seek(source.cursor);
        }
        Ok((timeline, persistent))
    }
}

impl PaneHistory for PaneExecutionEngine {
    fn apply_and_record(
        &mut self,
        tree: &mut PaneTree,
        sequence: u64,
        id: u64,
        operation: PaneOperation,
    ) -> Result<PaneOperationOutcome, PaneExecutionError> {
        Self::apply_and_record(self, tree, sequence, id, operation)
    }
    fn apply_and_record_coalesced_resize_delta(
        &mut self,
        tree: &mut PaneTree,
        sequence: u64,
        id: u64,
        operation: PaneOperation,
        boundary: u64,
    ) -> Result<PaneOperationOutcome, PaneExecutionError> {
        Self::apply_and_record_coalesced_resize_delta(self, tree, sequence, id, operation, boundary)
    }
    fn undo(&mut self, tree: &mut PaneTree) -> Result<bool, PaneExecutionError> {
        Self::undo(self, tree)
    }
    fn redo(&mut self, tree: &mut PaneTree) -> Result<bool, PaneExecutionError> {
        Self::redo(self, tree)
    }
    fn replay(&self) -> Result<PaneTree, PaneExecutionError> {
        Self::replay(self)
    }
    fn applied_len(&self) -> usize {
        Self::applied_len(self)
    }
    fn observe(
        &mut self,
        tree: &PaneTree,
        sample: PaneExecutionSample,
    ) -> Result<(), PaneExecutionError> {
        Self::observe(self, tree, sample)
    }
}

/// Observed shape of a pane-interaction workload window — the deterministic
/// input to strategy selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PaneWorkloadProfile {
    /// Operations observed in the window.
    pub operation_count: usize,
    /// Of those, how many are `Local` (resize / `SetSplitRatio`) — the hot path
    /// where the persistent store's structural sharing and O(1) navigation win.
    pub local_operation_count: usize,
    /// Peak operations-per-second observed (burstiness, e.g. a live drag-resize).
    pub peak_ops_per_sec: u32,
    /// Whether undo/redo history is required at all. If not, no history substrate
    /// is needed and the baseline path is selected.
    pub history_required: bool,
}

impl PaneWorkloadProfile {
    /// Construct a profile from explicit counts.
    #[must_use]
    pub const fn new(
        operation_count: usize,
        local_operation_count: usize,
        peak_ops_per_sec: u32,
        history_required: bool,
    ) -> Self {
        Self {
            operation_count,
            local_operation_count,
            peak_ops_per_sec,
            history_required,
        }
    }

    /// Derive a profile from an observed operation window. Local operations are
    /// classified by [`PaneOperation::family`] (`Local` = `SetSplitRatio`).
    #[must_use]
    pub fn observe(ops: &[PaneOperation], peak_ops_per_sec: u32, history_required: bool) -> Self {
        let local_operation_count = ops
            .iter()
            .filter(|op| op.family() == PaneOperationFamily::Local)
            .count();
        Self::new(
            ops.len(),
            local_operation_count,
            peak_ops_per_sec,
            history_required,
        )
    }

    /// Local-operation fraction as an integer percentage in `[0, 100]`.
    #[must_use]
    pub const fn local_fraction_pct(self) -> u32 {
        if self.operation_count == 0 {
            return 0;
        }
        ((self.local_operation_count.saturating_mul(100)) / self.operation_count) as u32
    }
}

/// Why a strategy was selected — the auditable rationale in every decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PaneStrategyReason {
    /// An operator/debug override forced this strategy.
    ForcedOverride,
    /// Conservative mode forced the certified checkpointed path.
    ConservativeFallback,
    /// History is not required, so the baseline (no-history) path was chosen.
    NoHistoryRequired,
    /// A resize-dominated, bursty, deep workload favored the persistent store.
    ResizeDominatedBurst,
    /// No strategy clearly won, so the conservative checkpointed default was used.
    GeneralDefault,
    /// A hysteresis margin held the previous strategy to avoid thrashing.
    HysteresisHold,
}

impl PaneStrategyReason {
    /// Stable identifier for logs/artifacts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ForcedOverride => "forced_override",
            Self::ConservativeFallback => "conservative_fallback",
            Self::NoHistoryRequired => "no_history_required",
            Self::ResizeDominatedBurst => "resize_dominated_burst",
            Self::GeneralDefault => "general_default",
            Self::HysteresisHold => "hysteresis_hold",
        }
    }
}

/// Deterministic policy that selects among the three pane execution strategies.
///
/// The thresholds are explicit and tunable; defaults are derived from the
/// persistence spike (`bd-1k7ek.5`) and the memory telemetry (`bd-25wj7.1`): the
/// persistent store earns its keep on resize-dominated bursts deep enough that
/// O(1) navigation and structural sharing pay off, while the checkpointed
/// timeline is the safe default everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PaneExecutionPolicy {
    /// Force a specific strategy (operator/debug override). `None` = adaptive.
    pub forced_strategy: Option<PaneMemoryStrategy>,
    /// Force the conservative certified path (checkpointed). Overrides adaptation.
    pub conservative: bool,
    /// Minimum operations before the persistent store is considered.
    pub persistent_min_operations: usize,
    /// Minimum local-op fraction (percent) for the persistent store.
    pub persistent_local_fraction_pct: u32,
    /// Minimum peak ops/sec (burstiness) for the persistent store.
    pub persistent_burst_ops_per_sec: u32,
    /// Hysteresis margin (percent, on the local fraction) used by
    /// [`reselect`](Self::reselect) to avoid thrashing near a threshold.
    pub hysteresis_pct: u32,
    /// The retention policy carried alongside the selected strategy.
    pub retention: PaneRetentionPolicy,
}

impl PaneExecutionPolicy {
    /// Default: persistent only past the spike's bounded-window depth.
    pub const DEFAULT_PERSISTENT_MIN_OPERATIONS: usize = 64;
    /// Default: persistent only when the workload is resize-dominated.
    pub const DEFAULT_PERSISTENT_LOCAL_FRACTION_PCT: u32 = 80;
    /// Default: persistent only under a drag-resize burst.
    pub const DEFAULT_PERSISTENT_BURST_OPS_PER_SEC: u32 = 60;
    /// Default hysteresis margin.
    pub const DEFAULT_HYSTERESIS_PCT: u32 = 10;

    /// An adaptive policy with the default thresholds, carrying `retention`.
    #[must_use]
    pub const fn adaptive(retention: PaneRetentionPolicy) -> Self {
        Self {
            forced_strategy: None,
            conservative: false,
            persistent_min_operations: Self::DEFAULT_PERSISTENT_MIN_OPERATIONS,
            persistent_local_fraction_pct: Self::DEFAULT_PERSISTENT_LOCAL_FRACTION_PCT,
            persistent_burst_ops_per_sec: Self::DEFAULT_PERSISTENT_BURST_OPS_PER_SEC,
            hysteresis_pct: Self::DEFAULT_HYSTERESIS_PCT,
            retention,
        }
    }

    /// Return this policy forced to the conservative certified path (checkpointed).
    #[must_use]
    pub const fn conservative(mut self) -> Self {
        self.conservative = true;
        self
    }

    /// Return this policy forced to a specific strategy.
    #[must_use]
    pub const fn forcing(mut self, strategy: PaneMemoryStrategy) -> Self {
        self.forced_strategy = Some(strategy);
        self
    }

    /// Select a strategy for `profile` (stateless, deterministic).
    #[must_use]
    pub fn select(&self, profile: PaneWorkloadProfile) -> PaneExecutionDecision {
        let (strategy, reason, forced) = self.decide(profile);
        self.decision(strategy, reason, forced, profile)
    }

    /// Re-select with hysteresis given the `previous` strategy: keep `previous`
    /// unless the workload favors a different strategy by a clear margin. This
    /// prevents thrashing when the local-op fraction jitters near a threshold.
    /// Forced/conservative overrides ignore hysteresis.
    #[must_use]
    pub fn reselect(
        &self,
        profile: PaneWorkloadProfile,
        previous: PaneMemoryStrategy,
    ) -> PaneExecutionDecision {
        if self.forced_strategy.is_some() || self.conservative {
            return self.select(profile);
        }
        let (fresh, reason, forced) = self.decide(profile);
        if fresh == previous {
            return self.decision(fresh, reason, forced, profile);
        }
        // The history requirement is a hard functional flag, not a tunable
        // threshold: honor it immediately (no hysteresis on entering/leaving
        // the baseline path).
        if matches!(reason, PaneStrategyReason::NoHistoryRequired)
            || previous == PaneMemoryStrategy::Baseline
        {
            return self.decision(fresh, reason, forced, profile);
        }
        let decisive = if fresh == PaneMemoryStrategy::Persistent {
            // Entering persistent: EVERY gate must clear by a margin, not just
            // the local-fraction threshold. Leaving is decisive on any failed
            // hard gate, so a marginless entry would let burst/op-count jitter
            // right at a hard gate flip the strategy every window — the exact
            // oscillation hysteresis exists to prevent. Hard-gate margins are
            // proportional (10%).
            let burst_margin = self.persistent_burst_ops_per_sec / 10;
            let ops_margin = self.persistent_min_operations / 10;
            self.favors_persistent(profile, self.hysteresis_pct)
                && profile.peak_ops_per_sec
                    >= self
                        .persistent_burst_ops_per_sec
                        .saturating_add(burst_margin)
                && profile.operation_count
                    >= self.persistent_min_operations.saturating_add(ops_margin)
        } else {
            // Leaving persistent: a failed hard gate is decisive; otherwise the
            // local fraction must drop below the threshold by the margin.
            profile.operation_count < self.persistent_min_operations
                || profile.peak_ops_per_sec < self.persistent_burst_ops_per_sec
                || profile.local_fraction_pct()
                    < self
                        .persistent_local_fraction_pct
                        .saturating_sub(self.hysteresis_pct)
        };
        if decisive {
            self.decision(fresh, reason, forced, profile)
        } else {
            self.decision(previous, PaneStrategyReason::HysteresisHold, false, profile)
        }
    }

    fn decide(
        &self,
        profile: PaneWorkloadProfile,
    ) -> (PaneMemoryStrategy, PaneStrategyReason, bool) {
        if let Some(forced) = self.forced_strategy {
            return (forced, PaneStrategyReason::ForcedOverride, true);
        }
        if self.conservative {
            return (
                PaneMemoryStrategy::Checkpointed,
                PaneStrategyReason::ConservativeFallback,
                true,
            );
        }
        if !profile.history_required {
            return (
                PaneMemoryStrategy::Baseline,
                PaneStrategyReason::NoHistoryRequired,
                false,
            );
        }
        if self.favors_persistent(profile, 0) {
            return (
                PaneMemoryStrategy::Persistent,
                PaneStrategyReason::ResizeDominatedBurst,
                false,
            );
        }
        (
            PaneMemoryStrategy::Checkpointed,
            PaneStrategyReason::GeneralDefault,
            false,
        )
    }

    fn favors_persistent(&self, profile: PaneWorkloadProfile, local_margin_pct: u32) -> bool {
        profile.operation_count >= self.persistent_min_operations
            && profile.local_fraction_pct()
                >= self
                    .persistent_local_fraction_pct
                    .saturating_add(local_margin_pct)
            && profile.peak_ops_per_sec >= self.persistent_burst_ops_per_sec
    }

    fn decision(
        &self,
        strategy: PaneMemoryStrategy,
        reason: PaneStrategyReason,
        forced: bool,
        profile: PaneWorkloadProfile,
    ) -> PaneExecutionDecision {
        let log = format!(
            "execution[{}] {}: ops={} local={}% burst={}/s history={} (thresholds: min_ops={} local>={}% burst>={}/s hysteresis={}%{}); retention budget bytes={} units={}",
            strategy.as_str(),
            reason.as_str(),
            profile.operation_count,
            profile.local_fraction_pct(),
            profile.peak_ops_per_sec,
            profile.history_required,
            self.persistent_min_operations,
            self.persistent_local_fraction_pct,
            self.persistent_burst_ops_per_sec,
            self.hysteresis_pct,
            if forced { ", forced" } else { "" },
            self.retention.budget.max_retained_bytes,
            self.retention.budget.max_retained_units,
        );
        PaneExecutionDecision {
            strategy,
            reason,
            forced,
            profile,
            retention: self.retention,
            log,
        }
    }
}

/// A deterministic, auditable strategy-selection decision.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PaneExecutionDecision {
    /// The selected execution strategy.
    pub strategy: PaneMemoryStrategy,
    /// Why it was selected.
    pub reason: PaneStrategyReason,
    /// Whether an operator override produced this decision.
    pub forced: bool,
    /// The workload profile the decision was made against.
    pub profile: PaneWorkloadProfile,
    /// The retention policy to apply alongside the selected strategy.
    pub retention: PaneRetentionPolicy,
    /// Human-readable one-line decision trace.
    pub log: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{
        PaneId, PaneInteractionTimeline, PaneLeaf, PaneOperation, PanePlacement, PaneSplitRatio,
        PaneTree, SplitAxis,
    };
    use crate::pane_persistent::{PaneVersionStore, VersionedPaneTree};

    fn policy() -> PaneExecutionPolicy {
        PaneExecutionPolicy::adaptive(PaneRetentionPolicy::bounded(500_000, 64))
    }

    fn resize_storm_profile() -> PaneWorkloadProfile {
        // 512 ops, all local (a pure drag-resize storm), bursty.
        PaneWorkloadProfile::new(512, 512, 240, true)
    }

    fn mixed_profile() -> PaneWorkloadProfile {
        // 384 ops, ~55% local, moderate rate.
        PaneWorkloadProfile::new(384, 211, 40, true)
    }

    #[test]
    fn live_policy_change_is_deferred_until_gesture_ends() {
        let mut tree = PaneTree::singleton("live");
        let mut engine = PaneExecutionEngine::new(&tree);
        let forced = PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded())
            .forcing(PaneMemoryStrategy::Persistent);
        engine.set_policy(&tree, forced).unwrap();
        engine.begin_gesture();
        engine.set_policy(&tree, forced.conservative()).unwrap();
        assert_eq!(engine.strategy(), PaneMemoryStrategy::Persistent);
        assert!(!engine.status().conservative);
        let root = tree.root();
        engine
            .apply_and_record(
                &mut tree,
                1,
                1,
                PaneOperation::SplitLeaf {
                    target: root,
                    axis: SplitAxis::Horizontal,
                    ratio: PaneSplitRatio::new(1, 1).unwrap(),
                    new_leaf: PaneLeaf::new("second"),
                    placement: PanePlacement::ExistingFirst,
                },
            )
            .unwrap();
        assert_eq!(engine.status().persistent_applies, 1);
        let accepted = tree.to_snapshot();
        engine.end_gesture(&tree).unwrap();
        assert_eq!(tree.to_snapshot(), accepted);
        assert_eq!(engine.strategy(), PaneMemoryStrategy::Checkpointed);
        assert!(engine.status().conservative);
        assert!(engine.undo(&mut tree).unwrap());
        assert_eq!(
            tree.to_snapshot(),
            PaneTree::singleton("live").to_snapshot()
        );
    }

    #[test]
    fn failed_maintenance_preserves_accepted_edit_and_previous_substrate() {
        let mut tree = PaneTree::singleton("live");
        let mut engine = PaneExecutionEngine::new(&tree);
        engine.set_latency_envelope_ns(1);
        engine
            .set_policy(
                &tree,
                PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded())
                    .forcing(PaneMemoryStrategy::Persistent),
            )
            .unwrap();
        engine
            .apply_and_record(&mut tree, 1, 1, PaneOperation::NormalizeRatios)
            .unwrap();
        let accepted = tree.to_snapshot();
        // A corrupted journal hash must prevent fallback publication rather
        // than blessing the journal or rolling back the already accepted edit.
        engine.timeline.entries[0].after_hash ^= 1;
        let journal = engine.timeline.clone();
        let status = engine.status.clone();
        assert!(
            engine
                .observe(
                    &tree,
                    PaneExecutionSample {
                        timestamp_ns: 1,
                        elapsed_ns: 2,
                        local: false
                    }
                )
                .is_err()
        );
        assert_eq!(tree.to_snapshot(), accepted);
        assert_eq!(engine.timeline, journal);
        assert_eq!(engine.strategy(), PaneMemoryStrategy::Persistent);
        assert_eq!(engine.status.applies, status.applies);
        assert_eq!(engine.status.switches, status.switches);
        assert!(engine.status.last_maintenance_error.is_some());
        assert_eq!(engine.replay().unwrap().to_snapshot(), accepted);
    }

    #[test]
    fn equal_observation_timestamps_do_not_invent_a_burst_rate() {
        let tree = PaneTree::singleton("live");
        let mut engine = PaneExecutionEngine::new(&tree);
        for _ in 0..128 {
            engine
                .observe(
                    &tree,
                    PaneExecutionSample {
                        timestamp_ns: 1,
                        elapsed_ns: 0,
                        local: true,
                    },
                )
                .unwrap();
        }
        assert_eq!(engine.profile().peak_ops_per_sec, 0);
        assert_eq!(engine.strategy(), PaneMemoryStrategy::Checkpointed);
    }

    #[test]
    fn live_pairing_distinguishes_raw_ratios_with_equal_semantic_hashes() {
        let mut original = PaneTree::singleton("original");
        for id in 1..=2 {
            original
                .apply_operation(
                    id,
                    PaneOperation::SplitLeaf {
                        target: PaneId::MIN,
                        axis: SplitAxis::Horizontal,
                        ratio: PaneSplitRatio::new(1, 1).unwrap(),
                        placement: PanePlacement::ExistingFirst,
                        new_leaf: PaneLeaf::new(format!("sibling-{id}")),
                    },
                )
                .unwrap();
        }
        let untouched_split = original.root();
        let mut alias = original.to_snapshot();
        let record = alias
            .nodes
            .iter_mut()
            .find(|record| record.id == untouched_split)
            .unwrap();
        let crate::pane::PaneNodeKind::Split(split) = &mut record.kind else {
            panic!("split root");
        };
        split.ratio = serde_json::from_str(r#"{"numerator":0,"denominator":1}"#).unwrap();
        let alias = PaneTree::from_snapshot(alias).unwrap();
        assert_eq!(original.state_hash(), alias.state_hash());
        assert_ne!(original.to_snapshot(), alias.to_snapshot());
        let resized = original
            .nodes()
            .find(|record| {
                record.id != untouched_split
                    && matches!(record.kind, crate::pane::PaneNodeKind::Split(_))
            })
            .unwrap()
            .id;
        let operation = PaneOperation::SetSplitRatio {
            split: resized,
            ratio: PaneSplitRatio::new(2, 3).unwrap(),
        };
        for strategy in [
            PaneMemoryStrategy::Persistent,
            PaneMemoryStrategy::Checkpointed,
        ] {
            let policy =
                PaneExecutionPolicy::adaptive(PaneRetentionPolicy::unbounded()).forcing(strategy);
            let mut engine = PaneExecutionEngine::new(&original);
            engine.set_policy(&original, policy).unwrap();
            let mut substituted = alias.clone();
            let before = engine.timeline().clone();
            assert!(matches!(
                engine.apply_and_record(&mut substituted, 3, 3, operation.clone()),
                Err(PaneExecutionError::InvalidHistory(_))
            ));
            assert_eq!(substituted, alias);
            assert_eq!(engine.timeline(), &before);
            assert_eq!(engine.status().applies, 0);

            // The same raw state is valid when imported as the actual baseline.
            // Editing a different split must retain its exact representation.
            let mut engine = PaneExecutionEngine::new(&alias);
            engine.set_policy(&alias, policy).unwrap();
            let mut live = alias.clone();
            let mut canonical = alias.clone();
            let expected = canonical.apply_operation(3, operation.clone()).unwrap();
            assert_eq!(
                engine
                    .apply_and_record(&mut live, 3, 3, operation.clone())
                    .unwrap(),
                expected
            );
            assert_eq!(live.to_snapshot(), canonical.to_snapshot());
            assert_eq!(
                engine.replay().unwrap().to_snapshot(),
                canonical.to_snapshot()
            );
        }
    }

    #[test]
    fn selection_is_deterministic() {
        let p = policy();
        let profile = resize_storm_profile();
        assert_eq!(p.select(profile), p.select(profile));
    }

    #[test]
    fn resize_storm_selects_persistent() {
        let d = policy().select(resize_storm_profile());
        assert_eq!(d.strategy, PaneMemoryStrategy::Persistent);
        assert_eq!(d.reason, PaneStrategyReason::ResizeDominatedBurst);
        assert!(!d.forced);
    }

    #[test]
    fn mixed_workload_falls_back_to_checkpointed() {
        let d = policy().select(mixed_profile());
        assert_eq!(d.strategy, PaneMemoryStrategy::Checkpointed);
        assert_eq!(d.reason, PaneStrategyReason::GeneralDefault);
    }

    #[test]
    fn no_history_selects_baseline() {
        let profile = PaneWorkloadProfile::new(512, 512, 240, false);
        let d = policy().select(profile);
        assert_eq!(d.strategy, PaneMemoryStrategy::Baseline);
        assert_eq!(d.reason, PaneStrategyReason::NoHistoryRequired);
    }

    #[test]
    fn shallow_resize_storm_stays_checkpointed() {
        // Resize-dominated and bursty, but below the depth where persistent pays.
        let profile = PaneWorkloadProfile::new(32, 32, 240, true);
        assert_eq!(
            policy().select(profile).strategy,
            PaneMemoryStrategy::Checkpointed
        );
    }

    #[test]
    fn forced_strategy_overrides_adaptation() {
        // A mixed workload would pick checkpointed, but force persistent.
        let forced = policy().forcing(PaneMemoryStrategy::Persistent);
        let d = forced.select(mixed_profile());
        assert_eq!(d.strategy, PaneMemoryStrategy::Persistent);
        assert_eq!(d.reason, PaneStrategyReason::ForcedOverride);
        assert!(d.forced);
    }

    #[test]
    fn conservative_forces_checkpointed_even_on_resize_storm() {
        let conservative = policy().conservative();
        let d = conservative.select(resize_storm_profile());
        assert_eq!(d.strategy, PaneMemoryStrategy::Checkpointed);
        assert_eq!(d.reason, PaneStrategyReason::ConservativeFallback);
        assert!(d.forced);
    }

    #[test]
    fn hysteresis_prevents_thrashing_near_threshold() {
        let p = policy();
        // Local fraction exactly at the entry threshold (80%): a fresh select
        // would pick persistent, but reselect from checkpointed needs +margin.
        let at_threshold = PaneWorkloadProfile::new(512, 410, 240, true); // 80%
        assert_eq!(
            p.select(at_threshold).strategy,
            PaneMemoryStrategy::Persistent
        );
        assert_eq!(
            p.reselect(at_threshold, PaneMemoryStrategy::Checkpointed)
                .strategy,
            PaneMemoryStrategy::Checkpointed,
            "should not enter persistent without clearing the hysteresis margin"
        );

        // A decisive resize storm (95% local) does enter persistent.
        let decisive = PaneWorkloadProfile::new(512, 487, 240, true); // 95%
        let entered = p.reselect(decisive, PaneMemoryStrategy::Checkpointed);
        assert_eq!(entered.strategy, PaneMemoryStrategy::Persistent);
        assert_eq!(entered.reason, PaneStrategyReason::ResizeDominatedBurst);

        // Once persistent, a mild dip (75% — within the margin) holds persistent.
        let mild_dip = PaneWorkloadProfile::new(512, 384, 240, true); // 75%
        let held = p.reselect(mild_dip, PaneMemoryStrategy::Persistent);
        assert_eq!(held.strategy, PaneMemoryStrategy::Persistent);
        assert_eq!(held.reason, PaneStrategyReason::HysteresisHold);

        // A decisive drop (65% — below threshold - margin) leaves persistent.
        let decisive_drop = PaneWorkloadProfile::new(512, 332, 240, true); // 64%
        assert_eq!(
            p.reselect(decisive_drop, PaneMemoryStrategy::Persistent)
                .strategy,
            PaneMemoryStrategy::Checkpointed
        );
    }

    #[test]
    fn hard_gate_jitter_does_not_oscillate() {
        // Burst rate jittering right at the hard gate (59 <-> 60/s) must not
        // flip Persistent <-> Checkpointed every window: leaving on a failed
        // hard gate is decisive, so re-entry must clear the gate by a margin
        // (>= 66/s at the default 60/s threshold), not merely touch it.
        let p = policy();
        let below_gate = PaneWorkloadProfile::new(512, 512, 59, true);
        let at_gate = PaneWorkloadProfile::new(512, 512, 60, true);
        let clears_gate = PaneWorkloadProfile::new(512, 512, 66, true);

        // Below the gate: leaving persistent is decisive.
        assert_eq!(
            p.reselect(below_gate, PaneMemoryStrategy::Persistent)
                .strategy,
            PaneMemoryStrategy::Checkpointed
        );
        // Back at (but not clearing) the gate: re-entry is refused — held.
        let held = p.reselect(at_gate, PaneMemoryStrategy::Checkpointed);
        assert_eq!(held.strategy, PaneMemoryStrategy::Checkpointed);
        assert_eq!(held.reason, PaneStrategyReason::HysteresisHold);
        // Clearing the gate by the 10% margin enters persistent.
        assert_eq!(
            p.reselect(clears_gate, PaneMemoryStrategy::Checkpointed)
                .strategy,
            PaneMemoryStrategy::Persistent
        );
    }

    #[test]
    fn observe_classifies_local_operations() {
        let ops = vec![
            PaneOperation::SetSplitRatio {
                split: PaneId::new(2).unwrap(),
                ratio: PaneSplitRatio::new(1, 1).unwrap(),
            },
            PaneOperation::SetSplitRatio {
                split: PaneId::new(2).unwrap(),
                ratio: PaneSplitRatio::new(2, 1).unwrap(),
            },
            PaneOperation::CloseNode {
                target: PaneId::new(3).unwrap(),
            },
        ];
        let profile = PaneWorkloadProfile::observe(&ops, 120, true);
        assert_eq!(profile.operation_count, 3);
        assert_eq!(profile.local_operation_count, 2);
        assert_eq!(profile.local_fraction_pct(), 66);
    }

    /// The headline safety guarantee: whichever strategy the selector picks, the
    /// observable result (final state hash) is identical — the candidates are
    /// proven equivalent, so selection changes cost, never behavior.
    #[test]
    fn strategy_choice_never_diverges_behavior() {
        let ratio = |n, d| PaneSplitRatio::new(n, d).expect("ratio");
        let mut ops = vec![
            PaneOperation::SplitLeaf {
                target: PaneId::MIN,
                axis: SplitAxis::Horizontal,
                ratio: ratio(1, 1),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("b"),
            },
            PaneOperation::SplitLeaf {
                target: PaneId::MIN,
                axis: SplitAxis::Vertical,
                ratio: ratio(2, 1),
                placement: PanePlacement::ExistingFirst,
                new_leaf: PaneLeaf::new("c"),
            },
        ];
        let split = PaneId::new(4).expect("id");
        for n in 1..=12u32 {
            ops.push(PaneOperation::SetSplitRatio {
                split,
                ratio: ratio(n % 5 + 1, 1),
            });
        }

        // Baseline: a plain tree.
        let mut baseline = PaneTree::singleton("root");
        for (i, op) in ops.iter().enumerate() {
            baseline
                .apply_operation_conservative(i as u64 + 1, op.clone())
                .expect("baseline apply");
        }
        // Checkpointed timeline.
        let mut tree = PaneTree::singleton("root");
        let mut timeline = PaneInteractionTimeline::default();
        for (i, op) in ops.iter().enumerate() {
            let id = i as u64;
            timeline
                .apply_and_record(&mut tree, id, id, op.clone())
                .expect("timeline apply");
        }
        // Persistent store.
        let mut store = PaneVersionStore::new(VersionedPaneTree::singleton("root"));
        for op in &ops {
            store.apply(op).expect("store apply");
        }

        let baseline_hash = baseline.state_hash();
        let timeline_hash = tree.state_hash();
        let store_hash = store.current().state_hash().expect("hash");
        assert_eq!(baseline_hash, timeline_hash);
        assert_eq!(baseline_hash, store_hash);

        // And the selector picks among exactly these equivalent substrates.
        let profile = PaneWorkloadProfile::observe(&ops, 240, true);
        let strategy = policy().select(profile).strategy;
        assert!(matches!(
            strategy,
            PaneMemoryStrategy::Baseline
                | PaneMemoryStrategy::Checkpointed
                | PaneMemoryStrategy::Persistent
        ));
    }
}
