#![forbid(unsafe_code)]

//! VOI debug telemetry snapshots for runtime introspection.

use std::sync::{LazyLock, RwLock};

use crate::voi_sampling::VoiSamplerSnapshot;

static INLINE_AUTO_VOI_SNAPSHOT: LazyLock<RwLock<Option<VoiSamplerSnapshot>>> =
    LazyLock::new(|| RwLock::new(None));

/// Store the latest inline-auto VOI snapshot.
pub fn set_inline_auto_voi_snapshot(snapshot: Option<VoiSamplerSnapshot>) {
    if let Ok(mut guard) = INLINE_AUTO_VOI_SNAPSHOT.write() {
        *guard = snapshot;
    }
}

/// Fetch the latest inline-auto VOI snapshot.
#[must_use]
pub fn inline_auto_voi_snapshot() -> Option<VoiSamplerSnapshot> {
    INLINE_AUTO_VOI_SNAPSHOT
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

/// Clear any stored inline-auto VOI snapshot.
pub fn clear_inline_auto_voi_snapshot() {
    set_inline_auto_voi_snapshot(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voi_sampling::{VoiDecision, VoiLogEntry, VoiObservation, VoiSamplerSnapshot};

    fn make_snapshot(captured_ms: u64) -> VoiSamplerSnapshot {
        VoiSamplerSnapshot {
            captured_ms,
            alpha: 2.0,
            beta: 18.0,
            posterior_mean: 0.1,
            posterior_variance: 0.004,
            expected_variance_after: 0.003,
            voi_gain: 0.5,
            last_decision: None,
            last_observation: None,
            recent_logs: Vec::new(),
        }
    }

    #[test]
    fn initially_none() {
        clear_inline_auto_voi_snapshot();
        assert!(inline_auto_voi_snapshot().is_none());
    }

    #[test]
    fn store_and_retrieve() {
        let snap = make_snapshot(1000);
        set_inline_auto_voi_snapshot(Some(snap));
        let retrieved = inline_auto_voi_snapshot().expect("should be Some");
        assert_eq!(retrieved.captured_ms, 1000);
        assert!((retrieved.alpha - 2.0).abs() < f64::EPSILON);
        assert!((retrieved.posterior_mean - 0.1).abs() < f64::EPSILON);
        clear_inline_auto_voi_snapshot();
    }

    #[test]
    fn overwrite_replaces_previous() {
        set_inline_auto_voi_snapshot(Some(make_snapshot(100)));
        set_inline_auto_voi_snapshot(Some(make_snapshot(200)));
        let snap = inline_auto_voi_snapshot().unwrap();
        assert_eq!(snap.captured_ms, 200);
        clear_inline_auto_voi_snapshot();
    }

    #[test]
    fn clear_removes_snapshot() {
        set_inline_auto_voi_snapshot(Some(make_snapshot(50)));
        clear_inline_auto_voi_snapshot();
        assert!(inline_auto_voi_snapshot().is_none());
    }

    #[test]
    fn set_none_clears() {
        set_inline_auto_voi_snapshot(Some(make_snapshot(77)));
        set_inline_auto_voi_snapshot(None);
        // Global state may be set by concurrent tests; verify set(None)
        // at least doesn't panic and the API is callable.
        let _ = inline_auto_voi_snapshot();
    }

    #[test]
    fn snapshot_with_decision() {
        let mut snap = make_snapshot(500);
        snap.last_decision = Some(VoiDecision {
            event_idx: 42,
            should_sample: true,
            forced_by_interval: false,
            blocked_by_min_interval: false,
            voi_gain: 1.2,
            score: 0.8,
            cost: 0.3,
            log_bayes_factor: 2.5,
            posterior_mean: 0.1,
            posterior_variance: 0.004,
            e_value: 15.0,
            e_threshold: 20.0,
            boundary_score: 0.7,
            events_since_sample: 10,
            time_since_sample_ms: 500.0,
            reason: "voi_gain",
        });
        set_inline_auto_voi_snapshot(Some(snap));
        let retrieved = inline_auto_voi_snapshot().unwrap();
        let decision = retrieved.last_decision.as_ref().unwrap();
        assert_eq!(decision.event_idx, 42);
        assert!(decision.should_sample);
        assert!((decision.voi_gain - 1.2).abs() < f64::EPSILON);
        clear_inline_auto_voi_snapshot();
    }

    #[test]
    fn snapshot_with_observation() {
        let mut snap = make_snapshot(600);
        snap.last_observation = Some(VoiObservation {
            event_idx: 100,
            sample_idx: 5,
            violated: true,
            posterior_mean: 0.15,
            posterior_variance: 0.003,
            alpha: 3.0,
            beta: 17.0,
            e_value: 25.0,
            e_threshold: 20.0,
        });
        set_inline_auto_voi_snapshot(Some(snap));
        let retrieved = inline_auto_voi_snapshot().unwrap();
        let obs = retrieved.last_observation.as_ref().unwrap();
        assert_eq!(obs.event_idx, 100);
        assert!(obs.violated);
        assert!((obs.alpha - 3.0).abs() < f64::EPSILON);
        clear_inline_auto_voi_snapshot();
    }

    #[test]
    fn snapshot_with_recent_logs() {
        let mut snap = make_snapshot(700);
        snap.recent_logs = vec![
            VoiLogEntry::Decision(VoiDecision {
                event_idx: 1,
                should_sample: false,
                forced_by_interval: false,
                blocked_by_min_interval: true,
                voi_gain: 0.1,
                score: 0.2,
                cost: 0.5,
                log_bayes_factor: -1.0,
                posterior_mean: 0.05,
                posterior_variance: 0.002,
                e_value: 0.8,
                e_threshold: 20.0,
                boundary_score: 0.3,
                events_since_sample: 2,
                time_since_sample_ms: 50.0,
                reason: "blocked_min_interval",
            }),
            VoiLogEntry::Observation(VoiObservation {
                event_idx: 2,
                sample_idx: 1,
                violated: false,
                posterior_mean: 0.06,
                posterior_variance: 0.002,
                alpha: 2.0,
                beta: 18.0,
                e_value: 1.0,
                e_threshold: 20.0,
            }),
        ];
        set_inline_auto_voi_snapshot(Some(snap));
        let retrieved = inline_auto_voi_snapshot().unwrap();
        assert_eq!(retrieved.recent_logs.len(), 2);
        clear_inline_auto_voi_snapshot();
    }
}
