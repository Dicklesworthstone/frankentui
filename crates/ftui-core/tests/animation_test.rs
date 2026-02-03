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
    assert!(
        seq.is_complete(),
        "200ms tick should complete 100ms+100ms sequence"
    );
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

// ===========================================================================
// E2E Choreography Tests (bd-1i67.6)
// ===========================================================================

mod choreo_timeline {
    use super::*;
    use ftui_core::animation::timeline::{LoopCount, PlaybackState, Timeline};

    #[test]
    fn sequential_animation_order() {
        // Add 3 labeled animations at sequential offsets.
        let mut tl = Timeline::new()
            .add_labeled("a", Duration::ZERO, Fade::new(MS_100))
            .add_labeled("b", MS_100, Fade::new(MS_100))
            .add_labeled("c", Duration::from_millis(200), Fade::new(MS_100))
            .set_duration(Duration::from_millis(300));
        tl.play();

        // At t=50ms: a is active, b and c haven't started.
        tl.tick(Duration::from_millis(50));
        assert!(tl.event_value("a").unwrap() > 0.0);
        assert!((tl.event_value("b").unwrap() - 0.0).abs() < f32::EPSILON);
        assert!((tl.event_value("c").unwrap() - 0.0).abs() < f32::EPSILON);

        // At t=150ms: a done, b active, c not started.
        tl.tick(MS_100);
        assert!((tl.event_value("a").unwrap() - 1.0).abs() < 0.01);
        assert!(tl.event_value("b").unwrap() > 0.0);
        assert!((tl.event_value("c").unwrap() - 0.0).abs() < f32::EPSILON);

        // At t=300ms: all done.
        tl.tick(Duration::from_millis(150));
        assert!(tl.state() == PlaybackState::Finished);
    }

    #[test]
    fn parallel_animations_sync() {
        // Two animations starting at the same time.
        let mut tl = Timeline::new()
            .add_labeled("x", Duration::ZERO, Fade::new(Duration::from_millis(200)))
            .add_labeled("y", Duration::ZERO, Fade::new(Duration::from_millis(200)))
            .set_duration(Duration::from_millis(200));
        tl.play();
        tl.tick(MS_100);

        let vx = tl.event_value("x").unwrap();
        let vy = tl.event_value("y").unwrap();
        assert!((vx - vy).abs() < 0.01, "parallel animations should be in sync: {vx} vs {vy}");
    }

    #[test]
    fn mixed_sequential_and_parallel() {
        let mut tl = Timeline::new()
            .add_labeled("par_a", Duration::ZERO, Fade::new(MS_100))
            .add_labeled("par_b", Duration::ZERO, Fade::new(MS_100))
            .add_labeled("after", MS_100, Fade::new(MS_100))
            .set_duration(Duration::from_millis(200));
        tl.play();

        // At t=50ms: par_a and par_b active, after hasn't started.
        tl.tick(Duration::from_millis(50));
        assert!(tl.event_value("par_a").unwrap() > 0.0);
        assert!(tl.event_value("par_b").unwrap() > 0.0);
        assert!((tl.event_value("after").unwrap() - 0.0).abs() < f32::EPSILON);

        // At t=150ms: par_a/b done, after is active.
        tl.tick(MS_100);
        assert!(tl.event_value("after").unwrap() > 0.0);
    }

    #[test]
    fn loop_and_repeat() {
        let mut tl = Timeline::new()
            .add_labeled("anim", Duration::ZERO, Fade::new(MS_100))
            .set_duration(MS_100)
            .set_loop_count(LoopCount::Times(2)); // plays 3 times total
        tl.play();

        // Complete first play.
        tl.tick(MS_100);
        assert_eq!(tl.state(), PlaybackState::Playing, "should loop, not finish");

        // Complete second play.
        tl.tick(MS_100);
        assert_eq!(tl.state(), PlaybackState::Playing, "should loop again");

        // Complete third play.
        tl.tick(MS_100);
        assert_eq!(tl.state(), PlaybackState::Finished, "should finish after 3 plays");
    }

    #[test]
    fn pause_and_resume() {
        let mut tl = Timeline::new()
            .add_labeled("a", Duration::ZERO, Fade::new(Duration::from_millis(200)))
            .set_duration(Duration::from_millis(200));
        tl.play();
        tl.tick(MS_100);
        let v_before = tl.event_value("a").unwrap();

        tl.pause();
        assert_eq!(tl.state(), PlaybackState::Paused);
        tl.tick(MS_100); // Should not progress.
        let v_after = tl.event_value("a").unwrap();
        assert!((v_before - v_after).abs() < 0.01, "paused timeline should not progress");

        tl.resume();
        assert_eq!(tl.state(), PlaybackState::Playing);
        tl.tick(MS_100);
        assert_eq!(tl.state(), PlaybackState::Finished);
    }

    #[test]
    fn seek_to_label() {
        let mut tl = Timeline::new()
            .add_labeled("start", Duration::ZERO, Fade::new(MS_100))
            .add_labeled("middle", MS_100, Fade::new(MS_100))
            .add_labeled("end", Duration::from_millis(200), Fade::new(MS_100))
            .set_duration(Duration::from_millis(300));
        tl.play();

        assert!(tl.seek_label("middle"));
        // After seeking, middle animation should have started.
        let v = tl.event_value("start").unwrap();
        assert!((v - 1.0).abs() < 0.01, "start should be complete after seeking past it");
    }

    #[test]
    fn infinite_loop_never_finishes() {
        let mut tl = Timeline::new()
            .add_labeled("pulse", Duration::ZERO, Fade::new(MS_100))
            .set_duration(MS_100)
            .set_loop_count(LoopCount::Infinite);
        tl.play();

        for _ in 0..100 {
            tl.tick(MS_100);
            assert_ne!(tl.state(), PlaybackState::Finished);
        }
    }
}

mod choreo_group {
    use super::*;
    use ftui_core::animation::group::AnimationGroup;

    #[test]
    fn group_creation_and_control() {
        let group = AnimationGroup::new()
            .add("fade_a", Fade::new(MS_100))
            .add("fade_b", Fade::new(Duration::from_millis(200)));
        assert_eq!(group.len(), 2);
        assert!(!group.all_complete());
    }

    #[test]
    fn shared_play_and_cancel() {
        let mut group = AnimationGroup::new()
            .add("a", Fade::new(MS_100))
            .add("b", Fade::new(MS_100));

        group.tick(Duration::from_millis(50));
        assert!(!group.all_complete());

        // cancel_all resets everything.
        group.cancel_all();
        assert!(!group.all_complete()); // Fades start at 0 and aren't "complete"
        let va = group.get("a").unwrap().value();
        assert!((va - 0.0).abs() < 0.01, "cancel should reset value");
    }

    #[test]
    fn progress_tracking() {
        let mut group = AnimationGroup::new()
            .add("a", Fade::new(MS_100))
            .add("b", Fade::new(Duration::from_millis(200)));

        // At t=100ms: a is complete (1.0), b is halfway (~0.5).
        group.tick(MS_100);
        let progress = group.overall_progress();
        assert!(progress > 0.5 && progress < 1.0, "overall progress should be between 0.5 and 1.0, got {progress}");
    }

    #[test]
    fn individual_access() {
        let mut group = AnimationGroup::new()
            .add("fast", Fade::new(MS_100))
            .add("slow", Fade::new(Duration::from_millis(500)));

        group.tick(MS_100);
        assert!(group.get("fast").unwrap().is_complete());
        assert!(!group.get("slow").unwrap().is_complete());
        assert!(group.get("nonexistent").is_none());
    }

    #[test]
    fn group_insert_replaces() {
        let mut group = AnimationGroup::new()
            .add("x", Fade::new(MS_100));
        group.tick(MS_100);
        assert!(group.get("x").unwrap().is_complete());

        // Insert replaces with new animation.
        group.insert("x", Box::new(Fade::new(SEC_1)));
        assert!(!group.get("x").unwrap().is_complete());
    }

    #[test]
    fn empty_group_is_complete() {
        let group = AnimationGroup::new();
        assert!(group.all_complete());
        assert!((group.overall_progress() - 0.0).abs() < f32::EPSILON);
    }
}

mod choreo_stagger {
    use super::*;
    use ftui_core::animation::stagger::{StaggerMode, stagger_offsets, stagger_offsets_with_jitter};

    #[test]
    fn linear_stagger_timing() {
        let offsets = stagger_offsets(5, Duration::from_millis(50), StaggerMode::Linear);
        assert_eq!(offsets.len(), 5);
        assert_eq!(offsets[0], Duration::ZERO);
        assert_eq!(offsets[4], Duration::from_millis(200));
        // Check monotonic.
        for w in offsets.windows(2) {
            assert!(w[1] >= w[0]);
        }
    }

    #[test]
    fn eased_stagger_curves() {
        for mode in [StaggerMode::EaseIn, StaggerMode::EaseOut, StaggerMode::EaseInOut] {
            let offsets = stagger_offsets(10, Duration::from_millis(20), mode);
            assert_eq!(offsets.len(), 10);
            assert_eq!(offsets[0], Duration::ZERO);
            // Monotonic.
            for w in offsets.windows(2) {
                assert!(w[1] >= w[0], "offsets should be monotonic for {mode:?}");
            }
            // Total span should be (count-1)*delay = 180ms.
            let total = Duration::from_millis(180);
            assert!(
                offsets[9].abs_diff(total) < Duration::from_millis(2),
                "last offset should be near {total:?}, got {:?} for {mode:?}",
                offsets[9]
            );
        }
    }

    #[test]
    fn jitter_deterministic() {
        let a = stagger_offsets_with_jitter(
            5, Duration::from_millis(50), StaggerMode::Linear,
            Duration::from_millis(10), 42,
        );
        let b = stagger_offsets_with_jitter(
            5, Duration::from_millis(50), StaggerMode::Linear,
            Duration::from_millis(10), 42,
        );
        assert_eq!(a, b, "same seed should produce identical offsets");
    }

    #[test]
    fn jitter_different_seeds_differ() {
        let a = stagger_offsets_with_jitter(
            5, Duration::from_millis(50), StaggerMode::Linear,
            Duration::from_millis(10), 42,
        );
        let b = stagger_offsets_with_jitter(
            5, Duration::from_millis(50), StaggerMode::Linear,
            Duration::from_millis(10), 99,
        );
        // Very unlikely to be identical with different seeds and jitter.
        assert_ne!(a, b, "different seeds should produce different offsets");
    }

    #[test]
    fn custom_easing_mode() {
        let offsets = stagger_offsets(
            5,
            Duration::from_millis(100),
            StaggerMode::Custom(ftui_core::animation::ease_out_cubic),
        );
        assert_eq!(offsets.len(), 5);
        assert_eq!(offsets[0], Duration::ZERO);
        for w in offsets.windows(2) {
            assert!(w[1] >= w[0]);
        }
    }
}

mod choreo_callbacks {
    use super::*;
    use ftui_core::animation::callbacks::{AnimationEvent, Callbacks};

    #[test]
    fn on_start_fires_once() {
        let mut anim = Callbacks::new(Fade::new(Duration::from_millis(200)))
            .on_start();

        anim.tick(Duration::from_millis(10));
        let events: Vec<_> = anim.drain_events();
        assert!(events.contains(&AnimationEvent::Started));

        anim.tick(Duration::from_millis(10));
        let events: Vec<_> = anim.drain_events();
        assert!(!events.contains(&AnimationEvent::Started), "Started should fire only once");
    }

    #[test]
    fn on_complete_fires_once() {
        let mut anim = Callbacks::new(Fade::new(MS_100))
            .on_complete();

        anim.tick(MS_100);
        let events: Vec<_> = anim.drain_events();
        assert!(events.contains(&AnimationEvent::Completed));

        anim.tick(MS_100);
        let events: Vec<_> = anim.drain_events();
        assert!(!events.contains(&AnimationEvent::Completed), "Completed should fire only once");
    }

    #[test]
    fn progress_threshold_fires() {
        let mut anim = Callbacks::new(Fade::new(Duration::from_millis(200)))
            .at_progress(0.5);

        // Tick to 40% — threshold not yet crossed.
        anim.tick(Duration::from_millis(80));
        let events: Vec<_> = anim.drain_events();
        assert!(
            !events.iter().any(|e| matches!(e, AnimationEvent::Progress(_))),
            "50% threshold should not fire at 40%"
        );

        // Tick to 60% — threshold crossed.
        anim.tick(Duration::from_millis(40));
        let events: Vec<_> = anim.drain_events();
        assert!(
            events.iter().any(|e| matches!(e, AnimationEvent::Progress(_))),
            "50% threshold should fire by 60%"
        );
    }

    #[test]
    fn multiple_thresholds_in_order() {
        let mut anim = Callbacks::new(Fade::new(SEC_1))
            .at_progress(0.25)
            .at_progress(0.5)
            .at_progress(0.75);

        // Tick past all thresholds at once.
        anim.tick(SEC_1);
        let events: Vec<_> = anim.drain_events();

        let progress_events: Vec<f32> = events
            .iter()
            .filter_map(|e| match e {
                &AnimationEvent::Progress(p) => Some(p),
                _ => None,
            })
            .collect();

        assert_eq!(progress_events.len(), 3, "should fire 3 progress events");
        // Should be in ascending order.
        for w in progress_events.windows(2) {
            assert!(w[1] >= w[0], "progress events should be in order: {progress_events:?}");
        }
    }

    #[test]
    fn reset_allows_events_to_fire_again() {
        let mut anim = Callbacks::new(Fade::new(MS_100))
            .on_start()
            .on_complete();

        anim.tick(MS_100);
        let _ = anim.drain_events();

        anim.reset();
        anim.tick(MS_100);
        let events: Vec<_> = anim.drain_events();
        assert!(events.contains(&AnimationEvent::Started), "Started should fire again after reset");
        assert!(events.contains(&AnimationEvent::Completed), "Completed should fire again after reset");
    }

    #[test]
    fn drain_clears_queue() {
        let mut anim = Callbacks::new(Fade::new(MS_100))
            .on_start()
            .on_complete();
        anim.tick(MS_100);

        let first: Vec<_> = anim.drain_events();
        assert!(!first.is_empty());

        let second: Vec<_> = anim.drain_events();
        assert!(second.is_empty(), "drain should clear the queue");
    }
}

mod choreo_presets {
    use super::*;
    use ftui_core::animation::presets::*;

    #[test]
    fn cascade_in_e2e() {
        let mut group = cascade_in(
            5,
            Duration::from_millis(200),
            Duration::from_millis(50),
            StaggerMode::Linear,
        );

        // Key frames: 0%, 25%, 50%, 75%, 100%.
        let total = Duration::from_millis(400); // 4*50ms stagger + 200ms item
        let steps = [0.0, 0.25, 0.5, 0.75, 1.0];
        let mut prev_progress = 0.0;
        for &pct in &steps {
            let target = total.mul_f64(pct as f64);
            let tick_amount = target.saturating_sub(Duration::from_millis(
                (prev_progress * total.as_millis() as f64) as u64,
            ));
            if !tick_amount.is_zero() {
                group.tick(tick_amount);
            }
            prev_progress = pct;
        }
        assert!(group.all_complete(), "cascade_in should complete");
    }

    #[test]
    fn fan_out_e2e() {
        let mut group = fan_out(7, Duration::from_millis(200), Duration::from_millis(200));
        // Tick through 5 steps to completion.
        for _ in 0..20 {
            group.tick(Duration::from_millis(25));
        }
        assert!(group.all_complete(), "fan_out should complete");
    }

    #[test]
    fn typewriter_e2e() {
        let text = "Hello, World!";
        let mut tw = typewriter(text.len(), Duration::from_millis(500));

        let mut prev_chars = 0;
        for step in 0..25 {
            tw.tick(Duration::from_millis(20));
            let chars = tw.visible_chars();
            assert!(chars >= prev_chars, "visible chars should not decrease at step {step}");
            prev_chars = chars;
        }
        assert!(tw.is_complete());
        assert_eq!(tw.visible_chars(), text.len());
    }

    #[test]
    fn pulse_sequence_e2e() {
        let mut group = pulse_sequence(3, Duration::from_millis(200), Duration::from_millis(200));

        // Track that pulses peak sequentially.
        let mut pulse_peaks = vec![0.0f32; 3];
        for _ in 0..40 {
            group.tick(Duration::from_millis(20));
            for i in 0..3 {
                let v = group.get(&format!("pulse_{i}")).unwrap().value();
                if v > pulse_peaks[i] {
                    pulse_peaks[i] = v;
                }
            }
        }
        // Each pulse should have peaked near 1.0.
        for (i, peak) in pulse_peaks.iter().enumerate() {
            assert!(*peak > 0.9, "pulse_{i} should peak near 1.0, got {peak}");
        }
    }

    #[test]
    fn slide_presets_e2e() {
        let mut left = slide_in_left(30, Duration::from_millis(300));
        let mut right = slide_in_right(30, Duration::from_millis(300));

        assert_eq!(left.position(), -30);
        assert_eq!(right.position(), 30);

        left.tick(Duration::from_millis(300));
        right.tick(Duration::from_millis(300));

        assert_eq!(left.position(), 0);
        assert_eq!(right.position(), 0);
    }

    #[test]
    fn fade_through_e2e() {
        let mut ft = fade_through(Duration::from_millis(200));

        // Start near 1.0.
        assert!((ft.value() - 1.0).abs() < 0.01);

        // Midpoint near 0.0.
        ft.tick(Duration::from_millis(200));
        assert!(ft.value() < 0.1);

        // End near 1.0.
        ft.tick(Duration::from_millis(200));
        assert!((ft.value() - 1.0).abs() < 0.01);
        assert!(ft.is_complete());
    }

    #[test]
    fn cascade_combined_with_callbacks() {
        use ftui_core::animation::callbacks::{AnimationEvent, Callbacks};

        let inner = cascade_in(3, MS_100, Duration::from_millis(50), StaggerMode::Linear);
        let mut anim = Callbacks::new(inner)
            .on_start()
            .on_complete()
            .at_progress(0.5);

        anim.tick(Duration::from_millis(10));
        let events: Vec<_> = anim.drain_events();
        assert!(events.contains(&AnimationEvent::Started));

        // Run to completion.
        anim.tick(SEC_1);
        let events: Vec<_> = anim.drain_events();
        assert!(events.contains(&AnimationEvent::Completed));
    }
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
