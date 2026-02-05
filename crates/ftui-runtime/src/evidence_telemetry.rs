#![forbid(unsafe_code)]

//! Evidence telemetry snapshots for runtime explainability overlays.
//!
//! These snapshots provide a low-overhead, in-memory view of the most recent
//! diff, resize, and budget decisions so demo screens can render cockpit
//! views without parsing JSONL logs.

use std::sync::{LazyLock, RwLock};

use ftui_render::budget::{BudgetDecision, DegradationLevel};
use ftui_render::diff_strategy::{DiffStrategy, StrategyEvidence};

use crate::bocpd::BocpdEvidence;
use crate::resize_coalescer::Regime;

/// Snapshot of the most recent diff-strategy decision.
#[derive(Debug, Clone)]
pub struct DiffDecisionSnapshot {
    pub event_idx: u64,
    pub screen_mode: String,
    pub cols: u16,
    pub rows: u16,
    pub evidence: StrategyEvidence,
    pub span_count: usize,
    pub span_coverage_pct: f64,
    pub max_span_len: usize,
    pub scan_cost_estimate: usize,
    pub fallback_reason: String,
    pub tile_used: bool,
    pub tile_fallback: String,
    pub strategy_used: DiffStrategy,
}

/// Snapshot of the most recent resize/coalescer decision.
#[derive(Debug, Clone)]
pub struct ResizeDecisionSnapshot {
    pub event_idx: u64,
    pub action: &'static str,
    pub dt_ms: f64,
    pub event_rate: f64,
    pub regime: Regime,
    pub pending_size: Option<(u16, u16)>,
    pub applied_size: Option<(u16, u16)>,
    pub time_since_render_ms: f64,
    pub bocpd: Option<BocpdEvidence>,
}

/// Conformal evidence snapshot for budget decisions.
#[derive(Debug, Clone)]
pub struct ConformalSnapshot {
    pub bucket_key: String,
    pub sample_count: usize,
    pub upper_us: f64,
    pub risk: bool,
}

/// Snapshot of the most recent budget decision.
#[derive(Debug, Clone)]
pub struct BudgetDecisionSnapshot {
    pub frame_idx: u64,
    pub decision: BudgetDecision,
    pub controller_decision: BudgetDecision,
    pub degradation_before: DegradationLevel,
    pub degradation_after: DegradationLevel,
    pub frame_time_us: f64,
    pub budget_us: f64,
    pub pid_output: f64,
    pub e_value: f64,
    pub frames_observed: u32,
    pub frames_since_change: u32,
    pub in_warmup: bool,
    pub conformal: Option<ConformalSnapshot>,
}

static DIFF_SNAPSHOT: LazyLock<RwLock<Option<DiffDecisionSnapshot>>> =
    LazyLock::new(|| RwLock::new(None));
static RESIZE_SNAPSHOT: LazyLock<RwLock<Option<ResizeDecisionSnapshot>>> =
    LazyLock::new(|| RwLock::new(None));
static BUDGET_SNAPSHOT: LazyLock<RwLock<Option<BudgetDecisionSnapshot>>> =
    LazyLock::new(|| RwLock::new(None));

/// Store the latest diff decision snapshot.
pub fn set_diff_snapshot(snapshot: Option<DiffDecisionSnapshot>) {
    if let Ok(mut guard) = DIFF_SNAPSHOT.write() {
        *guard = snapshot;
    }
}

/// Fetch the latest diff decision snapshot.
#[must_use]
pub fn diff_snapshot() -> Option<DiffDecisionSnapshot> {
    DIFF_SNAPSHOT.read().ok().and_then(|guard| guard.clone())
}

/// Clear any stored diff snapshot.
pub fn clear_diff_snapshot() {
    set_diff_snapshot(None);
}

/// Store the latest resize decision snapshot.
pub fn set_resize_snapshot(snapshot: Option<ResizeDecisionSnapshot>) {
    if let Ok(mut guard) = RESIZE_SNAPSHOT.write() {
        *guard = snapshot;
    }
}

/// Fetch the latest resize decision snapshot.
#[must_use]
pub fn resize_snapshot() -> Option<ResizeDecisionSnapshot> {
    RESIZE_SNAPSHOT.read().ok().and_then(|guard| guard.clone())
}

/// Clear any stored resize snapshot.
pub fn clear_resize_snapshot() {
    set_resize_snapshot(None);
}

/// Store the latest budget decision snapshot.
pub fn set_budget_snapshot(snapshot: Option<BudgetDecisionSnapshot>) {
    if let Ok(mut guard) = BUDGET_SNAPSHOT.write() {
        *guard = snapshot;
    }
}

/// Fetch the latest budget decision snapshot.
#[must_use]
pub fn budget_snapshot() -> Option<BudgetDecisionSnapshot> {
    BUDGET_SNAPSHOT.read().ok().and_then(|guard| guard.clone())
}

/// Clear any stored budget snapshot.
pub fn clear_budget_snapshot() {
    set_budget_snapshot(None);
}
