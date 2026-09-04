#![cfg(feature = "experimental")]
#![forbid(unsafe_code)]
//! E2E integration test for SOS barrier certificates.
//!
//! Runs 6 frame-budget scenarios through the barrier evaluator and
//! emits structured JSONL evidence for each evaluation.

use std::io::Write;
use std::time::Instant;

use ftui_runtime::sos_barrier;

/// JSONL event for a barrier evaluation.
#[derive(serde::Serialize)]
struct BarrierEvent {
    event: &'static str,
    scenario: String,
    state_vector: [f64; 2],
    barrier_value: f64,
    decision: &'static str,
    polynomial_degree: u32,
    eval_time_ns: u64,
    transition_from: Option<String>,
    transition_to: Option<String>,
}

fn decision_label(result: &sos_barrier::BarrierResult) -> &'static str {
    if result.value > 0.1 {
        "admit"
    } else if result.value > 0.0 {
        "warn"
    } else {
        "reject"
    }
}

struct ScenarioRunner {
    events: Vec<BarrierEvent>,
    prev_decision: Option<String>,
}

impl ScenarioRunner {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            prev_decision: None,
        }
    }

    fn eval(
        &mut self,
        scenario: &str,
        budget: f64,
        change_rate: f64,
    ) -> sos_barrier::BarrierResult {
        let start = Instant::now();
        let result = sos_barrier::evaluate(budget, change_rate);
        let elapsed_ns = start.elapsed().as_nanos() as u64;

        let decision = decision_label(&result);
        let transition_from = self.prev_decision.clone();
        let transition_to = if transition_from.as_deref() != Some(decision) {
            Some(decision.to_string())
        } else {
            None
        };

        self.events.push(BarrierEvent {
            event: "sos_barrier_eval",
            scenario: scenario.into(),
            state_vector: [budget, change_rate],
            barrier_value: result.value,
            decision,
            polynomial_degree: 4,
            eval_time_ns: elapsed_ns,
            transition_from,
            transition_to,
        });

        self.prev_decision = Some(decision.to_string());
        result
    }

    fn write_jsonl(&self, path: &std::path::Path) {
        let mut file = std::fs::File::create(path).expect("create JSONL");
        for event in &self.events {
            let line = serde_json::to_string(event).expect("serialize event");
            writeln!(file, "{}", line).expect("write event");
        }
    }
}

// ── Scenario A: Normal 60fps steady state ────────────────────────────────

#[test]
fn scenario_a_normal_steady_state() {
    let mut runner = ScenarioRunner::new();

    // 10 frames at 60fps: budget fully available, low change rate.
    for frame in 0..10 {
        let budget = 0.85 - (frame as f64 * 0.01); // slight variation
        let change = 0.05 + (frame as f64 * 0.005);
        let r = runner.eval("normal_60fps", budget, change);
        assert!(
            r.is_safe,
            "frame {} should be admitted: B={:.4}",
            frame, r.value
        );
    }

    let jsonl_path = std::env::temp_dir().join("sos_barrier_scenario_a.jsonl");
    runner.write_jsonl(&jsonl_path);
    assert_eq!(runner.events.len(), 10);
}

// ── Scenario B: Single spike near boundary ───────────────────────────────

#[test]
fn scenario_b_spike_near_boundary() {
    let mut runner = ScenarioRunner::new();

    // Normal frames.
    for _ in 0..3 {
        let r = runner.eval("spike", 0.7, 0.1);
        assert!(r.is_safe);
    }

    // Spike: budget drops, change_rate rises (25ms frame).
    let r = runner.eval("spike", 0.15, 0.4);
    // Near boundary but should still be admitted (B > 0).
    assert!(
        r.is_safe,
        "spike should still be admitted: B={:.4}",
        r.value
    );

    // Recovery.
    for _ in 0..3 {
        let r = runner.eval("spike", 0.7, 0.1);
        assert!(r.is_safe);
    }
}

// ── Scenario C: Overload (3 consecutive heavy frames) ────────────────────

#[test]
fn scenario_c_overload() {
    let mut runner = ScenarioRunner::new();

    // Normal start.
    let r = runner.eval("overload", 0.8, 0.1);
    assert!(r.is_safe);

    // 3 consecutive overloaded frames: budget nearly exhausted, high change.
    for frame in 0..3 {
        let r = runner.eval("overload", 0.02, 0.9);
        assert!(
            !r.is_safe,
            "overload frame {} should be rejected: B={:.4}",
            frame, r.value
        );
    }
}

// ── Scenario D: Recovery after overload ──────────────────────────────────

#[test]
fn scenario_d_recovery() {
    let mut runner = ScenarioRunner::new();

    // Overload.
    let r = runner.eval("recovery", 0.01, 0.95);
    assert!(!r.is_safe);

    // Gradual recovery.
    let r = runner.eval("recovery", 0.3, 0.3);
    assert!(
        r.is_safe,
        "should re-admit after recovery: B={:.4}",
        r.value
    );

    // Full recovery.
    let r = runner.eval("recovery", 0.8, 0.1);
    assert!(r.is_safe);

    // Verify transition happened.
    let transitions: Vec<_> = runner
        .events
        .iter()
        .filter(|e| e.transition_to.is_some())
        .collect();
    assert!(
        transitions.len() >= 2,
        "should have at least 2 transitions (reject→admit)"
    );
}

// ── Scenario E: Resize boundary (coalesced events) ───────────────────────

#[test]
fn scenario_e_resize_boundary() {
    let mut runner = ScenarioRunner::new();

    // Rapid resize events: budget drops during layout recompute.
    let resize_frames = [
        (0.4, 0.5),   // first resize
        (0.35, 0.55), // second resize
        (0.3, 0.6),   // third resize
        (0.5, 0.3),   // coalesced recompute settles
        (0.7, 0.15),  // normal after resize
    ];

    for (i, &(budget, change)) in resize_frames.iter().enumerate() {
        let r = runner.eval("resize_boundary", budget, change);
        // All should remain safe — budget > 0 with moderate change.
        assert!(
            r.is_safe,
            "resize frame {} should be admitted: B({},{})={:.4}",
            i, budget, change, r.value
        );
    }
}

// ── Scenario F: Gradual degradation transition ───────────────────────────

#[test]
fn scenario_f_degradation_transition() {
    let mut runner = ScenarioRunner::new();

    // Gradual p99 increase: budget shrinks, change_rate grows.
    let mut found_reject = false;
    for step in 0..20 {
        let budget = 0.5 - (step as f64 * 0.025); // 0.5 → 0.0
        let change = 0.2 + (step as f64 * 0.04); // 0.2 → 1.0
        let budget = budget.max(0.0);
        let change = change.min(1.0);

        let r = runner.eval("degradation", budget, change);

        if !r.is_safe {
            found_reject = true;
        }
    }

    assert!(
        found_reject,
        "gradual degradation should eventually trigger rejection"
    );

    // Verify we have both admit and reject events.
    let admits = runner
        .events
        .iter()
        .filter(|e| e.decision == "admit")
        .count();
    let rejects = runner
        .events
        .iter()
        .filter(|e| e.decision == "reject")
        .count();
    assert!(admits > 0, "should have some admits");
    assert!(rejects > 0, "should have some rejects");
}

// ── JSONL Schema Compliance ──────────────────────────────────────────────

#[test]
fn jsonl_schema_compliance() {
    let mut runner = ScenarioRunner::new();
    runner.eval("schema_test", 0.5, 0.3);
    runner.eval("schema_test", 0.0, 0.9);

    let jsonl_path = std::env::temp_dir().join("sos_barrier_schema_test.jsonl");
    runner.write_jsonl(&jsonl_path);

    // Read back and validate schema.
    let content = std::fs::read_to_string(&jsonl_path).expect("read JSONL");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);

    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("parse JSON");
        assert_eq!(v["event"], "sos_barrier_eval");
        assert!(v["scenario"].is_string());
        assert!(v["state_vector"].is_array());
        assert_eq!(v["state_vector"].as_array().unwrap().len(), 2);
        assert!(v["barrier_value"].is_f64());
        assert!(v["decision"].is_string());
        assert_eq!(v["polynomial_degree"], 4);
        assert!(v["eval_time_ns"].is_u64());
    }

    std::fs::remove_file(&jsonl_path).ok();
}

// ── No Panics Under Edge Inputs ──────────────────────────────────────────

#[test]
fn no_panic_extreme_inputs() {
    let extremes = [
        (f64::MIN, f64::MIN),
        (f64::MAX, f64::MAX),
        (f64::NEG_INFINITY, 0.5),
        (0.5, f64::INFINITY),
        (f64::NAN, 0.5),
        (0.5, f64::NAN),
    ];

    for &(b, c) in &extremes {
        // Should not panic — inputs are clamped.
        let _ = sos_barrier::evaluate(b, c);
    }
}
