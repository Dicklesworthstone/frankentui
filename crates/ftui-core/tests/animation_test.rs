//! Integration tests for the animation module.

use ftui_core::animation::*;
use proptest::prelude::*;
use std::time::Duration;

const MS_100: Duration = Duration::from_millis(100);
const SEC_1: Duration = Duration::from_secs(1);

#[test]
fn fade_duration_tracking() {
    let mut fade = Fade::new(SEC_1);
    for _ in 0..1000 {
        fade.tick(Duration::from_millis(1));
    }
    assert!(fade.is_complete(), "1000x1ms should complete 1s fade");
}

#[test]
fn sequence_forwards_overshoot() {
    let mut seq = sequence(Fade::new(MS_100), Fade::new(MS_100));
    // One tick of 200ms should complete both
    seq.tick(Duration::from_millis(200));
    assert!(seq.is_complete(), "200ms tick should complete 100ms+100ms sequence");
}

#[test]
fn nested_sequence_completes() {
    let inner = sequence(Fade::new(MS_100), Fade::new(MS_100));
    let mut outer = sequence(inner, Fade::new(MS_100));
    outer.tick(Duration::from_millis(300));
    assert!(
        outer.is_complete(),
        "300ms tick should complete nested 100+100+100 sequence"
    );
}

#[test]
fn parallel_of_sequences_completes() {
    let s1 = sequence(Fade::new(MS_100), Fade::new(MS_100));
    let s2 = sequence(Fade::new(MS_100), Fade::new(MS_100));
    let mut par = parallel(s1, s2);
    par.tick(Duration::from_millis(200));
    assert!(
        par.is_complete(),
        "200ms tick should complete parallel 200ms sequences"
    );
}

#[test]
fn pulse_phase_wraps_properly() {
    let mut pulse = Pulse::new(1.0);
    pulse.tick(Duration::from_secs(10)); // 10 full cycles
    assert!(
        pulse.phase() < std::f32::consts::TAU,
        "phase should be bounded: {}",
        pulse.phase()
    );
}

#[test]
fn easing_functions_are_monotonic() {
    for easing in [
        linear,
        ease_in,
        ease_out,
        ease_in_out,
        ease_in_cubic,
        ease_out_cubic,
    ] {
        let mut prev = 0.0f32;
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            let v = easing(t);
            assert!(v >= prev - 0.001, "easing should be monotonic at t={}", t);
            prev = v;
        }
    }
}

#[test]
fn delay_waits_before_starting() {
    let mut delayed = delay(
        Duration::from_millis(200),
        Fade::new(Duration::from_millis(100)),
    );
    delayed.tick(Duration::from_millis(100));
    assert!(!delayed.has_started());
    assert!((delayed.value() - 0.0).abs() < f32::EPSILON);

    delayed.tick(Duration::from_millis(100));
    assert!(delayed.has_started());
    assert!((delayed.value() - 0.0).abs() < f32::EPSILON);
}

#[test]
fn sequence_chains_with_overshoot() {
    let mut seq = sequence(Fade::new(MS_100), Fade::new(MS_100));
    seq.tick(Duration::from_millis(150));
    assert!(!seq.is_complete());
    assert!(
        seq.value() > 0.0,
        "second animation should have started after overshoot"
    );
}

#[test]
fn parallel_ticks_both_animations() {
    let mut par = parallel(Fade::new(MS_100), Fade::new(MS_100));
    par.tick(Duration::from_millis(50));
    assert!(!par.is_complete());
    assert!(par.value() > 0.0);
}

proptest! {
    #[test]
    fn easing_outputs_bounded(t in -10.0f32..10.0f32) {
        for easing in [
            linear,
            ease_in,
            ease_out,
            ease_in_out,
            ease_in_cubic,
            ease_out_cubic,
        ] {
            let v = easing(t);
            prop_assert!(
                (0.0..=1.0).contains(&v),
                "easing output out of range: t={t} v={v}"
            );
        }
    }

    #[test]
    fn fade_completes_when_tick_ge_duration(duration_ms in 1u64..5000, extra_ms in 0u64..5000) {
        let duration = Duration::from_millis(duration_ms);
        let mut fade = Fade::new(duration);
        fade.tick(Duration::from_millis(duration_ms + extra_ms));
        prop_assert!(fade.is_complete());
        prop_assert!(fade.value() <= 1.0 + f32::EPSILON);
    }

    #[test]
    fn sequence_duration_sums(a_ms in 1u64..2000, b_ms in 1u64..2000) {
        let mut seq = sequence(
            Fade::new(Duration::from_millis(a_ms)),
            Fade::new(Duration::from_millis(b_ms)),
        );
        seq.tick(Duration::from_millis(a_ms + b_ms));
        prop_assert!(seq.is_complete());
    }

    #[test]
    fn parallel_duration_is_max(a_ms in 1u64..2000, b_ms in 1u64..2000) {
        let mut par = parallel(
            Fade::new(Duration::from_millis(a_ms)),
            Fade::new(Duration::from_millis(b_ms)),
        );
        let max_ms = a_ms.max(b_ms);
        par.tick(Duration::from_millis(max_ms));
        prop_assert!(par.is_complete());
    }
}
