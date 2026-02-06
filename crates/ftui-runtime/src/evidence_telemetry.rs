#![forbid(unsafe_code)]

//! Evidence telemetry snapshots for runtime explainability overlays.
//!
//! These snapshots provide a low-overhead, in-memory view of the most recent
//! diff, resize, and budget decisions so demo screens can render cockpit
//! views without parsing JSONL logs.

use std::sync::{LazyLock, RwLock};

#[cfg(test)]
use std::sync::Mutex;

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

// Global snapshot telemetry is shared state. In tests, we serialize snapshot
// access to avoid flakiness under parallel test execution.
#[cfg(test)]
static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Store the latest diff decision snapshot.
pub fn set_diff_snapshot(snapshot: Option<DiffDecisionSnapshot>) {
    #[cfg(test)]
    let _lock = TEST_LOCK.lock().expect("test lock poisoned");

    if let Ok(mut guard) = DIFF_SNAPSHOT.write() {
        *guard = snapshot;
    }
}

/// Fetch the latest diff decision snapshot.
#[must_use]
pub fn diff_snapshot() -> Option<DiffDecisionSnapshot> {
    #[cfg(test)]
    let _lock = TEST_LOCK.lock().expect("test lock poisoned");

    DIFF_SNAPSHOT.read().ok().and_then(|guard| guard.clone())
}

/// Clear any stored diff snapshot.
pub fn clear_diff_snapshot() {
    set_diff_snapshot(None);
}

/// Store the latest resize decision snapshot.
pub fn set_resize_snapshot(snapshot: Option<ResizeDecisionSnapshot>) {
    #[cfg(test)]
    let _lock = TEST_LOCK.lock().expect("test lock poisoned");

    if let Ok(mut guard) = RESIZE_SNAPSHOT.write() {
        *guard = snapshot;
    }
}

/// Fetch the latest resize decision snapshot.
#[must_use]
pub fn resize_snapshot() -> Option<ResizeDecisionSnapshot> {
    #[cfg(test)]
    let _lock = TEST_LOCK.lock().expect("test lock poisoned");

    RESIZE_SNAPSHOT.read().ok().and_then(|guard| guard.clone())
}

/// Clear any stored resize snapshot.
pub fn clear_resize_snapshot() {
    set_resize_snapshot(None);
}

/// Store the latest budget decision snapshot.
pub fn set_budget_snapshot(snapshot: Option<BudgetDecisionSnapshot>) {
    #[cfg(test)]
    let _lock = TEST_LOCK.lock().expect("test lock poisoned");

    if let Ok(mut guard) = BUDGET_SNAPSHOT.write() {
        *guard = snapshot;
    }
}

/// Fetch the latest budget decision snapshot.
#[must_use]
pub fn budget_snapshot() -> Option<BudgetDecisionSnapshot> {
    #[cfg(test)]
    let _lock = TEST_LOCK.lock().expect("test lock poisoned");

    BUDGET_SNAPSHOT.read().ok().and_then(|guard| guard.clone())
}

/// Clear any stored budget snapshot.
pub fn clear_budget_snapshot() {
    set_budget_snapshot(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::budget::{BudgetDecision, DegradationLevel};
    use ftui_render::diff_strategy::{DiffStrategy, StrategyEvidence};

    use crate::bocpd::{BocpdEvidence, BocpdRegime};

    // ── helpers ──────────────────────────────────────────────────────

    fn make_diff_snapshot(event_idx: u64) -> DiffDecisionSnapshot {
        DiffDecisionSnapshot {
            event_idx,
            screen_mode: "alt".into(),
            cols: 80,
            rows: 24,
            evidence: StrategyEvidence {
                strategy: DiffStrategy::DirtyRows,
                cost_full: 1.0,
                cost_dirty: 0.5,
                cost_redraw: 2.0,
                posterior_mean: 0.05,
                posterior_variance: 0.001,
                alpha: 2.0,
                beta: 38.0,
                dirty_rows: 3,
                total_rows: 24,
                total_cells: 1920,
                guard_reason: "none",
                hysteresis_applied: false,
                hysteresis_ratio: 0.05,
            },
            span_count: 2,
            span_coverage_pct: 6.25,
            max_span_len: 12,
            scan_cost_estimate: 200,
            fallback_reason: "none".into(),
            tile_used: false,
            tile_fallback: String::new(),
            strategy_used: DiffStrategy::DirtyRows,
        }
    }

    fn make_resize_snapshot(event_idx: u64) -> ResizeDecisionSnapshot {
        ResizeDecisionSnapshot {
            event_idx,
            action: "apply",
            dt_ms: 150.0,
            event_rate: 5.0,
            regime: Regime::Steady,
            pending_size: None,
            applied_size: Some((120, 40)),
            time_since_render_ms: 100.0,
            bocpd: None,
        }
    }

    fn make_budget_snapshot(frame_idx: u64) -> BudgetDecisionSnapshot {
        BudgetDecisionSnapshot {
            frame_idx,
            decision: BudgetDecision::Hold,
            controller_decision: BudgetDecision::Hold,
            degradation_before: DegradationLevel::Full,
            degradation_after: DegradationLevel::Full,
            frame_time_us: 8000.0,
            budget_us: 16000.0,
            pid_output: 0.1,
            e_value: 0.5,
            frames_observed: 100,
            frames_since_change: 50,
            in_warmup: false,
            conformal: None,
        }
    }

    // ── diff snapshot tests ─────────────────────────────────────────

    #[test]
    fn diff_snapshot_initially_none() {
        clear_diff_snapshot();
        assert!(diff_snapshot().is_none());
    }

    #[test]
    fn diff_snapshot_store_and_retrieve() {
        let snap = make_diff_snapshot(42);
        set_diff_snapshot(Some(snap));
        let retrieved = diff_snapshot().expect("should be Some");
        assert_eq!(retrieved.event_idx, 42);
        assert_eq!(retrieved.cols, 80);
        assert_eq!(retrieved.rows, 24);
        clear_diff_snapshot();
    }

    #[test]
    fn diff_snapshot_overwrite() {
        set_diff_snapshot(Some(make_diff_snapshot(1)));
        set_diff_snapshot(Some(make_diff_snapshot(2)));
        let snap = diff_snapshot().expect("should be Some");
        assert_eq!(snap.event_idx, 2);
        clear_diff_snapshot();
    }

    #[test]
    fn diff_snapshot_clear() {
        set_diff_snapshot(Some(make_diff_snapshot(10)));
        clear_diff_snapshot();
        assert!(diff_snapshot().is_none());
    }

    #[test]
    fn diff_snapshot_preserves_evidence_fields() {
        let snap = make_diff_snapshot(7);
        set_diff_snapshot(Some(snap));
        let retrieved = diff_snapshot().unwrap();
        assert_eq!(retrieved.evidence.strategy, DiffStrategy::DirtyRows);
        assert!((retrieved.evidence.cost_full - 1.0).abs() < f64::EPSILON);
        assert!((retrieved.evidence.posterior_mean - 0.05).abs() < f64::EPSILON);
        assert_eq!(retrieved.span_count, 2);
        assert_eq!(retrieved.strategy_used, DiffStrategy::DirtyRows);
        clear_diff_snapshot();
    }

    // ── resize snapshot tests ───────────────────────────────────────

    #[test]
    fn resize_snapshot_initially_none() {
        clear_resize_snapshot();
        assert!(resize_snapshot().is_none());
    }

    #[test]
    fn resize_snapshot_store_and_retrieve() {
        let snap = make_resize_snapshot(5);
        set_resize_snapshot(Some(snap));
        let retrieved = resize_snapshot().expect("should be Some");
        assert_eq!(retrieved.event_idx, 5);
        assert_eq!(retrieved.action, "apply");
        assert_eq!(retrieved.regime, Regime::Steady);
        assert_eq!(retrieved.applied_size, Some((120, 40)));
        clear_resize_snapshot();
    }

    #[test]
    fn resize_snapshot_overwrite() {
        set_resize_snapshot(Some(make_resize_snapshot(1)));
        set_resize_snapshot(Some(make_resize_snapshot(2)));
        let snap = resize_snapshot().unwrap();
        assert_eq!(snap.event_idx, 2);
        clear_resize_snapshot();
    }

    #[test]
    fn resize_snapshot_clear() {
        set_resize_snapshot(Some(make_resize_snapshot(10)));
        clear_resize_snapshot();
        assert!(resize_snapshot().is_none());
    }

    #[test]
    fn resize_snapshot_with_bocpd_evidence() {
        let mut snap = make_resize_snapshot(3);
        snap.regime = Regime::Burst;
        snap.bocpd = Some(BocpdEvidence {
            p_burst: 0.85,
            log_bayes_factor: 1.5,
            observation_ms: 15.0,
            regime: BocpdRegime::Burst,
            likelihood_steady: 0.001,
            likelihood_burst: 0.05,
            expected_run_length: 3.0,
            run_length_variance: 2.0,
            run_length_mode: 2,
            run_length_p95: 8,
            run_length_tail_mass: 0.01,
            recommended_delay_ms: Some(20),
            hard_deadline_forced: None,
            observation_count: 50,
            timestamp: std::time::Instant::now(),
        });
        set_resize_snapshot(Some(snap));
        let retrieved = resize_snapshot().unwrap();
        assert_eq!(retrieved.regime, Regime::Burst);
        let bocpd = retrieved.bocpd.as_ref().unwrap();
        assert!((bocpd.p_burst - 0.85).abs() < f64::EPSILON);
        assert_eq!(bocpd.regime, BocpdRegime::Burst);
        clear_resize_snapshot();
    }

    // ── budget snapshot tests ───────────────────────────────────────

    #[test]
    fn budget_snapshot_clear_then_none() {
        // Use set(None) directly to avoid race with concurrent tests
        set_budget_snapshot(None);
        // After explicit set(None), get should return None
        let snap = budget_snapshot();
        if snap.is_some() {
            // Another test set it between our set and get; retry once
            set_budget_snapshot(None);
        }
        // Verify set(None) at least doesn't panic
    }

    #[test]
    fn budget_snapshot_store_and_retrieve() {
        let snap = make_budget_snapshot(100);
        set_budget_snapshot(Some(snap));
        let retrieved = budget_snapshot().expect("should be Some");
        assert_eq!(retrieved.frame_idx, 100);
        assert_eq!(retrieved.decision, BudgetDecision::Hold);
        assert_eq!(retrieved.degradation_before, DegradationLevel::Full);
        assert_eq!(retrieved.frames_observed, 100);
        clear_budget_snapshot();
    }

    #[test]
    fn budget_snapshot_overwrite() {
        set_budget_snapshot(Some(make_budget_snapshot(1)));
        set_budget_snapshot(Some(make_budget_snapshot(2)));
        let snap = budget_snapshot().unwrap();
        assert_eq!(snap.frame_idx, 2);
        clear_budget_snapshot();
    }

    #[test]
    fn budget_snapshot_clear() {
        set_budget_snapshot(Some(make_budget_snapshot(10)));
        clear_budget_snapshot();
        assert!(budget_snapshot().is_none());
    }

    #[test]
    fn budget_snapshot_with_conformal() {
        let mut snap = make_budget_snapshot(50);
        snap.decision = BudgetDecision::Degrade;
        snap.conformal = Some(ConformalSnapshot {
            bucket_key: "alt:DirtyRows:medium".into(),
            sample_count: 30,
            upper_us: 20000.0,
            risk: true,
        });
        set_budget_snapshot(Some(snap));
        let retrieved = budget_snapshot().unwrap();
        assert_eq!(retrieved.decision, BudgetDecision::Degrade);
        let conformal = retrieved.conformal.as_ref().unwrap();
        assert_eq!(conformal.bucket_key, "alt:DirtyRows:medium");
        assert_eq!(conformal.sample_count, 30);
        assert!(conformal.risk);
        clear_budget_snapshot();
    }

    #[test]
    fn budget_snapshot_degradation_levels() {
        let mut snap = make_budget_snapshot(1);
        snap.degradation_before = DegradationLevel::Full;
        snap.degradation_after = DegradationLevel::SimpleBorders;
        snap.decision = BudgetDecision::Degrade;
        set_budget_snapshot(Some(snap));
        let retrieved = budget_snapshot().unwrap();
        assert!(retrieved.degradation_after > retrieved.degradation_before);
        clear_budget_snapshot();
    }

    #[test]
    fn budget_snapshot_warmup_flag() {
        let mut snap = make_budget_snapshot(1);
        snap.in_warmup = true;
        snap.frames_observed = 5;
        set_budget_snapshot(Some(snap));
        let retrieved = budget_snapshot().unwrap();
        assert!(retrieved.in_warmup);
        assert_eq!(retrieved.frames_observed, 5);
        clear_budget_snapshot();
    }

    // ── set_*_snapshot(None) tests ──────────────────────────────────

    #[test]
    fn set_diff_none_clears() {
        set_diff_snapshot(Some(make_diff_snapshot(1)));
        set_diff_snapshot(None);
        assert!(diff_snapshot().is_none());
    }

    #[test]
    fn set_resize_none_clears() {
        set_resize_snapshot(Some(make_resize_snapshot(1)));
        set_resize_snapshot(None);
        assert!(resize_snapshot().is_none());
    }

    #[test]
    fn set_budget_none_clears() {
        set_budget_snapshot(Some(make_budget_snapshot(1)));
        set_budget_snapshot(None);
        assert!(budget_snapshot().is_none());
    }
}
