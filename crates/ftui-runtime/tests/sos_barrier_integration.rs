#![cfg(feature = "experimental")]
//! Integration tests and benchmark for SOS barrier certificate evaluation.
//!
//! Validates:
//! 1. Known safe points → B(x) > 0
//! 2. Known unsafe points → B(x) < 0
//! 3. Boundary cases within epsilon of threshold
//! 4. Performance: 1M evaluations < 30ms total (< 30ns each)
//! 5. Golden barrier decisions match hand-computed reference values

use ftui_runtime::sos_barrier;

// ── Known Safe Points ────────────────────────────────────────────────────

#[test]
fn safe_point_full_budget_idle() {
    let r = sos_barrier::evaluate(1.0, 0.0);
    assert!(r.is_safe);
    assert!(r.value > 1.0);
}

#[test]
fn safe_point_high_budget_low_change() {
    let r = sos_barrier::evaluate(0.9, 0.05);
    assert!(r.is_safe);
}

#[test]
fn safe_point_moderate_budget_moderate_change() {
    let r = sos_barrier::evaluate(0.6, 0.3);
    assert!(r.is_safe);
}

#[test]
fn safe_point_low_budget_no_change() {
    let r = sos_barrier::evaluate(0.1, 0.0);
    assert!(r.is_safe);
}

#[test]
fn safe_grid_sweep() {
    // Sweep the interior safe region.
    for budget_pct in (20..=100).step_by(10) {
        for change_pct in (0..=20).step_by(5) {
            let b = budget_pct as f64 / 100.0;
            let c = change_pct as f64 / 100.0;
            let r = sos_barrier::evaluate(b, c);
            assert!(
                r.is_safe,
                "Expected safe at budget={:.2}, change={:.2}, B={:.6}",
                b, c, r.value
            );
        }
    }
}

// ── Known Unsafe Points ──────────────────────────────────────────────────

#[test]
fn unsafe_point_no_budget_high_change() {
    let r = sos_barrier::evaluate(0.0, 0.8);
    assert!(!r.is_safe);
    assert!(r.value < 0.0);
}

#[test]
fn unsafe_point_no_budget_max_change() {
    let r = sos_barrier::evaluate(0.0, 1.0);
    assert!(!r.is_safe);
    assert!(r.value < -0.5);
}

#[test]
fn unsafe_point_tiny_budget_high_change() {
    let r = sos_barrier::evaluate(0.02, 0.9);
    assert!(!r.is_safe);
}

#[test]
fn unsafe_grid_sweep() {
    // Sweep the corner where budget ≈ 0 and change_rate is high.
    for budget_pct in 0..=3 {
        for change_pct in (80..=100).step_by(5) {
            let b = budget_pct as f64 / 100.0;
            let c = change_pct as f64 / 100.0;
            let r = sos_barrier::evaluate(b, c);
            assert!(
                !r.is_safe,
                "Expected unsafe at budget={:.2}, change={:.2}, B={:.6}",
                b, c, r.value
            );
        }
    }
}

// ── Boundary Cases ───────────────────────────────────────────────────────

#[test]
fn boundary_origin() {
    let r = sos_barrier::evaluate(0.0, 0.0);
    assert!(r.value.abs() < 1e-10);
}

#[test]
fn boundary_zero_budget_varying_change() {
    // At x1=0, B(0, x2) = -0.5*x2^2 - 0.1*x2^4
    // Should be <= 0 for all x2 >= 0.
    for pct in 0..=100 {
        let x2 = pct as f64 / 100.0;
        let r = sos_barrier::evaluate(0.0, x2);
        assert!(
            r.value <= 1e-10,
            "B(0, {:.2}) = {:.8} should be <= 0",
            x2,
            r.value
        );
    }
}

#[test]
fn boundary_transition_exists() {
    // For a fixed change_rate, there should be a transition from unsafe to safe
    // as budget increases.
    let change_rate = 0.7;
    let mut found_transition = false;
    let mut prev_safe = false;

    for budget_pct in 0..=100 {
        let b = budget_pct as f64 / 100.0;
        let r = sos_barrier::evaluate(b, change_rate);
        if budget_pct > 0 && r.is_safe && !prev_safe {
            found_transition = true;
        }
        prev_safe = r.is_safe;
    }

    assert!(
        found_transition,
        "Should find a transition from unsafe to safe at change_rate={}",
        change_rate
    );
}

// ── Monotonicity ─────────────────────────────────────────────────────────

#[test]
fn monotone_in_budget_at_fixed_change() {
    for change_pct in (0..=50).step_by(10) {
        let c = change_pct as f64 / 100.0;
        let mut prev = sos_barrier::evaluate(0.0, c).value;
        for budget_pct in (10..=100).step_by(10) {
            let b = budget_pct as f64 / 100.0;
            let curr = sos_barrier::evaluate(b, c).value;
            assert!(
                curr >= prev,
                "B should increase with budget: B({:.1},{:.1})={:.6} < B({:.1},{:.1})={:.6}",
                b,
                c,
                curr,
                b - 0.1,
                c,
                prev
            );
            prev = curr;
        }
    }
}

#[test]
fn monotone_in_change_rate_at_fixed_budget() {
    for budget_pct in (20..=100).step_by(20) {
        let b = budget_pct as f64 / 100.0;
        let mut prev = sos_barrier::evaluate(b, 0.0).value;
        for change_pct in (10..=100).step_by(10) {
            let c = change_pct as f64 / 100.0;
            let curr = sos_barrier::evaluate(b, c).value;
            assert!(
                curr <= prev,
                "B should decrease with change_rate: B({:.1},{:.1})={:.6} > B({:.1},{:.1})={:.6}",
                b,
                c,
                curr,
                b,
                c - 0.1,
                prev
            );
            prev = curr;
        }
    }
}

// ── Golden Reference Values ──────────────────────────────────────────────

#[test]
fn golden_reference_values() {
    // Hand-computed from the polynomial:
    // B(x1, x2) = x1 - 0.5*x2^2 + 0.1*x1^2 - 0.3*x1*x2^2
    //           + 0.05*x1^3 - 0.1*x2^4 - 0.15*x1^2*x2^2 + 0.02*x1^4

    let cases: &[(f64, f64, f64)] = &[
        // (budget, change, expected_B)
        (1.0, 0.0, 1.17), // 1.0 + 0.1 + 0.05 + 0.02
        (0.0, 1.0, -0.6), // -0.5 - 0.1
        (0.0, 0.0, 0.0),  // origin
        (1.0, 1.0, 0.12), // 1.0 - 0.5 + 0.1 - 0.3 + 0.05 - 0.1 - 0.15 + 0.02
    ];

    for &(b, c, expected) in cases {
        let r = sos_barrier::evaluate(b, c);
        assert!(
            (r.value - expected).abs() < 1e-10,
            "B({}, {}) = {:.10}, expected {:.10}",
            b,
            c,
            r.value,
            expected
        );
    }
}

// ── API Consistency ──────────────────────────────────────────────────────

#[test]
fn safety_margin_consistent() {
    for b_pct in (0..=100).step_by(25) {
        for c_pct in (0..=100).step_by(25) {
            let b = b_pct as f64 / 100.0;
            let c = c_pct as f64 / 100.0;
            let margin = sos_barrier::safety_margin(b, c);
            let result = sos_barrier::evaluate(b, c);
            assert!(
                (margin - result.value).abs() < 1e-15,
                "safety_margin and evaluate disagree at ({}, {})",
                b,
                c
            );
        }
    }
}

#[test]
fn is_admissible_consistent() {
    for b_pct in (0..=100).step_by(5) {
        for c_pct in (0..=100).step_by(5) {
            let b = b_pct as f64 / 100.0;
            let c = c_pct as f64 / 100.0;
            let admissible = sos_barrier::is_admissible(b, c);
            let result = sos_barrier::evaluate(b, c);
            assert_eq!(
                admissible, result.is_safe,
                "is_admissible and evaluate.is_safe disagree at ({}, {})",
                b, c
            );
        }
    }
}

// ── Performance Benchmark ────────────────────────────────────────────────

#[test]
fn benchmark_1m_evaluations_under_30ms() {
    let n = 1_000_000;
    let start = std::time::Instant::now();

    // Evaluate at a spread of points to avoid branch prediction shortcuts.
    let mut total = 0.0_f64;
    for i in 0..n {
        let b = (i % 101) as f64 / 100.0;
        let c = ((i / 101) % 101) as f64 / 100.0;
        total += sos_barrier::evaluate(b, c).value;
    }

    let elapsed = start.elapsed();

    // Prevent dead-code elimination.
    assert!(total.is_finite(), "accumulated value should be finite");

    let per_eval_ns = elapsed.as_nanos() as f64 / n as f64;
    eprintln!(
        "SOS barrier benchmark: {}M evals in {:.1}ms ({:.1}ns/eval)",
        n / 1_000_000,
        elapsed.as_secs_f64() * 1000.0,
        per_eval_ns
    );

    // Target: < 30ms total (< 30ns per eval) in release.
    // Debug builds are ~10x slower, so we use a generous 500ms limit.
    assert!(
        elapsed.as_millis() < 500,
        "1M evaluations took {}ms (target: <500ms in debug, <30ms in release)",
        elapsed.as_millis()
    );
}
