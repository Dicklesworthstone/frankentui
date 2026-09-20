#![forbid(unsafe_code)]

//! Differential replay: does BOCPD actually beat the rate heuristic?
//!
//! `enable_bocpd` was flipped to `true` by default (G12). The reality check's
//! condition for that flip was not "BOCPD is more principled" but a measured
//! one: over recorded resize storms, BOCPD must render **no more frames during
//! a drag** than the heuristic and must apply the **final size within 40 ms** of
//! the last event. bd-g00-root-epic-ewths.16.4 is the measurement, and the
//! plan's rule is explicit -- if BOCPD loses on any pattern, the default goes
//! back rather than the bar coming down.
//!
//! # Why this is a replay and not a benchmark
//!
//! Both detectors are driven over *identical* event schedules on a virtual
//! clock: `ResizeStorm` generates deterministic (size, delay) sequences from a
//! seed, and the coalescer's time-injected API (`handle_resize_at`, `tick_at`)
//! takes timestamps rather than reading a clock. So the comparison measures the
//! detectors, never the machine, and reruns on a loaded box give the same
//! numbers. Nothing here sleeps.
//!
//! # What it counts
//!
//! - `frames_during_drag`: `ApplyResize` actions strictly before the last
//!   event. That is the cost the coalescer exists to reduce -- each one is a
//!   re-layout and repaint the user did not ask for.
//! - `final_apply_latency_ms`: from the last event to the `ApplyResize` that
//!   settles on its size. That is the cost of coalescing -- how long the UI
//!   stays laid out for a window that no longer exists.
//!
//! The two trade against each other, which is the whole reason to measure
//! rather than argue.
//!
//! # Running it
//!
//! `#[ignore]` by default, because it writes trace files and is a measurement
//! rather than a contract. `scripts/resize_storm_differential_e2e.sh` runs it
//! with `--ignored` and aggregates the traces into `docs/perf/`.
//!
//! ```bash
//! cargo test -p ftui-harness --test resize_storm_detector_differential -- --ignored --nocapture
//! ```

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ftui_harness::resize_storm::{StormConfig, StormPattern, ResizeStorm};
use ftui_runtime::resize_coalescer::{CoalesceAction, CoalescerConfig, ResizeCoalescer};

/// Frame cadence the runtime ticks at; the coalescer's delays are in these units.
const TICK_MS: u64 = 16;

/// The plan's acceptance bar for settling on the final size.
const FINAL_APPLY_BUDGET_MS: u64 = 40;

/// Seed for every pattern. Fixed rather than from the clock: this test's output
/// is committed evidence, and evidence you cannot reproduce is an anecdote.
const SEED: u64 = 0x5EED_0016_0004;

struct Outcome {
    frames_during_drag: usize,
    total_applies: usize,
    final_apply_latency_ms: u64,
    final_size: (u16, u16),
    expected_final_size: (u16, u16),
    regime_transitions: u64,
    bocpd_decisions: u64,
    heuristic_decisions: u64,
    trace: String,
}

/// Drive one detector over one storm on a virtual clock.
fn replay(pattern: &StormPattern, case: &str, detector: &str, config: CoalescerConfig) -> Outcome {
    let storm = ResizeStorm::new(StormConfig {
        seed: SEED,
        pattern: pattern.clone(),
        case_name: case.to_string(),
        ..StormConfig::default()
    });
    let events = storm.events().to_vec();
    assert!(!events.is_empty(), "{case}: storm generated no events");

    let initial = storm.config().initial_size;
    let mut coalescer = ResizeCoalescer::new(config, initial);

    let base = Instant::now();
    let mut trace = String::new();
    let mut now_ms = 0u64;
    let mut applies: Vec<(u64, u16, u16)> = Vec::new();

    // The instant the last event arrives: everything before it is "during the
    // drag", everything after is the tail we are measuring the latency of.
    let last_event_ms: u64 = events.iter().map(|e| e.delay_ms).sum();
    let expected_final_size = events
        .last()
        .map(|e| (e.width, e.height))
        .expect("events is non-empty");

    // Interleave events and 16 ms ticks on one timeline, so a tick that falls
    // between two events is delivered in the right order.
    let mut next_tick_ms = TICK_MS;
    for event in &events {
        let event_ms = now_ms + event.delay_ms;
        while next_tick_ms <= event_ms {
            let action = coalescer.tick_at(base + Duration::from_millis(next_tick_ms));
            record_apply(&mut trace, &mut applies, next_tick_ms, &action);
            next_tick_ms += TICK_MS;
        }
        now_ms = event_ms;
        trace.push_str(&format!(
            r#"{{"event":"resize_event","t_ms":{now_ms},"width":{},"height":{},"index":{}}}"#,
            event.width, event.height, event.index
        ));
        trace.push('\n');
        let action =
            coalescer.handle_resize_at(event.width, event.height, base + Duration::from_millis(now_ms));
        record_apply(&mut trace, &mut applies, now_ms, &action);
    }

    // Drain: keep ticking past the hard deadline so anything still pending is
    // applied. 400 ms is 4x the 100 ms deadline, so a pending resize that never
    // lands is a real failure rather than an impatient test.
    let drain_until = now_ms + 400;
    while next_tick_ms <= drain_until {
        let action = coalescer.tick_at(base + Duration::from_millis(next_tick_ms));
        record_apply(&mut trace, &mut applies, next_tick_ms, &action);
        next_tick_ms += TICK_MS;
    }

    let frames_during_drag = applies.iter().filter(|(t, _, _)| *t < last_event_ms).count();
    let final_apply = applies
        .iter()
        .rev()
        .find(|(_, w, h)| (*w, *h) == expected_final_size);
    let final_apply_latency_ms = final_apply
        .map(|(t, _, _)| t.saturating_sub(last_event_ms))
        .unwrap_or(u64::MAX);
    let final_size = coalescer.last_applied();
    let regime_transitions = coalescer.regime_transition_count();
    let decisions = coalescer.detector_decisions();

    trace.push_str(&format!(
        r#"{{"event":"summary","detector":"{detector}","case":"{case}","frames_during_drag":{frames_during_drag},"total_applies":{},"final_apply_latency_ms":{final_apply_latency_ms},"final_width":{},"final_height":{},"regime_transitions":{},"bocpd_decisions":{},"heuristic_decisions":{},"seed":{SEED}}}"#,
        applies.len(),
        final_size.0,
        final_size.1,
        regime_transitions,
        decisions.bocpd,
        decisions.heuristic,
    ));
    trace.push('\n');

    Outcome {
        frames_during_drag,
        total_applies: applies.len(),
        final_apply_latency_ms,
        final_size,
        expected_final_size,
        regime_transitions,
        bocpd_decisions: decisions.bocpd,
        heuristic_decisions: decisions.heuristic,
        trace,
    }
}

fn out_dir() -> PathBuf {
    let dir = std::env::var("RESIZE_DIFFERENTIAL_DIR")
        .map_or_else(|_| PathBuf::from("target/resize_differential"), PathBuf::from);
    fs::create_dir_all(&dir).expect("create the differential output directory");
    dir
}

#[test]
#[ignore = "writes trace files; run via scripts/resize_storm_differential_e2e.sh"]
fn bocpd_is_not_worse_than_the_heuristic_on_resize_storms() {
    let patterns: Vec<(&str, StormPattern)> = vec![
        ("burst_50", StormPattern::Burst { count: 50 }),
        ("burst_200", StormPattern::Burst { count: 200 }),
        (
            "sweep_80x24_to_200x60",
            StormPattern::Sweep {
                start_width: 80,
                start_height: 24,
                end_width: 200,
                end_height: 60,
                steps: 40,
            },
        ),
        (
            "oscillate_10",
            StormPattern::Oscillate {
                size_a: (80, 24),
                size_b: (160, 48),
                cycles: 10,
            },
        ),
        ("mixed_100", StormPattern::Mixed { count: 100 }),
        ("pathological_50", StormPattern::Pathological { count: 50 }),
    ];

    let dir = out_dir();
    let mut rows = Vec::new();
    let mut failures = Vec::new();

    for (case, pattern) in &patterns {
        let heuristic = replay(
            pattern,
            case,
            "heuristic",
            CoalescerConfig::default().without_bocpd().with_logging(true),
        );
        let bocpd = replay(
            pattern,
            case,
            "bocpd",
            CoalescerConfig::default().with_logging(true),
        );

        for (detector, outcome) in [("heuristic", &heuristic), ("bocpd", &bocpd)] {
            fs::write(dir.join(format!("{case}.{detector}.jsonl")), &outcome.trace)
                .expect("write trace");
        }

        // Correctness first: a detector that settles on the wrong size has not
        // won anything, however few frames it drew.
        for (detector, outcome) in [("heuristic", &heuristic), ("bocpd", &bocpd)] {
            if outcome.final_size != outcome.expected_final_size {
                failures.push(format!(
                    "{case}/{detector}: settled on {:?}, last event asked for {:?}",
                    outcome.final_size, outcome.expected_final_size
                ));
            }
        }

        let verdict = if bocpd.frames_during_drag > heuristic.frames_during_drag {
            failures.push(format!(
                "{case}: bocpd drew {} frames during the drag, heuristic drew {}",
                bocpd.frames_during_drag, heuristic.frames_during_drag
            ));
            "REGRESSION"
        } else if bocpd.final_apply_latency_ms > FINAL_APPLY_BUDGET_MS {
            failures.push(format!(
                "{case}: bocpd settled {} ms after the last event, budget is {FINAL_APPLY_BUDGET_MS} ms",
                bocpd.final_apply_latency_ms
            ));
            "SLOW"
        } else if bocpd.frames_during_drag < heuristic.frames_during_drag {
            "BETTER"
        } else {
            "EQUAL"
        };

        rows.push(format!(
            r#"{{"case":"{case}","verdict":"{verdict}","frames_during_drag":{{"heuristic":{},"bocpd":{}}},"total_applies":{{"heuristic":{},"bocpd":{}}},"final_apply_latency_ms":{{"heuristic":{},"bocpd":{}}},"regime_transitions":{{"heuristic":{},"bocpd":{}}},"detector_decisions_bocpd":{{"heuristic":{},"bocpd":{}}},"detector_decisions_heuristic":{{"heuristic":{},"bocpd":{}}}}}"#,
            heuristic.frames_during_drag,
            bocpd.frames_during_drag,
            heuristic.total_applies,
            bocpd.total_applies,
            heuristic.final_apply_latency_ms,
            bocpd.final_apply_latency_ms,
            heuristic.regime_transitions,
            bocpd.regime_transitions,
            heuristic.bocpd_decisions,
            bocpd.bocpd_decisions,
            heuristic.heuristic_decisions,
            bocpd.heuristic_decisions,
        ));

        // The decision counts are printed, not just written to the trace,
        // because they are what makes the comparison meaningful: if the
        // bocpd-configured run shows `bocpd_decided=0` then the posterior never
        // decided anything and the two columns are the same detector twice.
        // A verdict published without that number is not evidence.
        println!(
            "{case:24} heuristic frames={:<4} latency={:<5} | bocpd frames={:<4} latency={:<5} | \
             bocpd_decided={:<5} heur_fallback={:<5} transitions={}→{} | {verdict}",
            heuristic.frames_during_drag,
            heuristic.final_apply_latency_ms,
            bocpd.frames_during_drag,
            bocpd.final_apply_latency_ms,
            bocpd.bocpd_decisions,
            bocpd.heuristic_decisions,
            heuristic.regime_transitions,
            bocpd.regime_transitions,
        );
    }

    let summary = format!("{{\"seed\":{SEED},\"budget_ms\":{FINAL_APPLY_BUDGET_MS},\"cases\":[{}]}}\n", rows.join(","));
    fs::write(dir.join("summary.json"), &summary).expect("write summary");

    assert!(
        failures.is_empty(),
        "BOCPD lost on {} check(s). The plan says flip the default back rather than lower the bar:\n  {}\nTraces in {}",
        failures.len(),
        failures.join("\n  "),
        dir.display()
    );
}
