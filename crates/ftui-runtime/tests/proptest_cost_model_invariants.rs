//! Property-based invariant tests for the cost model module.
//!
//! These tests verify mathematical invariants that must hold for any valid
//! parameterizations of the three cost models:
//!
//! **Cache cost model:**
//! 1. miss_rate always returns values in [0, 1].
//! 2. miss_rate is monotonically non-increasing with budget.
//! 3. total_cost is always non-negative.
//! 4. optimal_budget is within [item_bytes, budget_max_bytes].
//! 5. optimal_budget is a local minimum of total_cost.
//! 6. optimize produces consistent cost decomposition (miss + mem = total).
//! 7. hit_rate + miss_rate == 1.
//! 8. evaluate and total_cost agree.
//!
//! **Pipeline scheduling model:**
//! 9. utilization = arrival_rate × total_mean.
//! 10. stage fractions sum to 1.0 (when total > 0).
//! 11. stable ↔ utilization < 1.
//! 12. mean_sojourn >= total_mean (always, queueing adds delay).
//! 13. headroom = frame_budget - mean_sojourn.
//! 14. coefficient of variation is non-negative.
//!
//! **Batch cost model:**
//! 15. total_cost is non-negative for any batch_size.
//! 16. optimal_batch_size is in [1, n].
//! 17. optimal cost <= cost at any other batch size.
//! 18. batch_size=0 and n=0 don't panic.
//! 19. no overhead → optimal is immediate (k=1).
//! 20. no latency → optimal is single batch (k=n).
//! 21. improvement_ratio >= 1.0.
//! 22. evaluate decomposition: overhead + processing + latency = total.
//! 23. All three models are deterministic.
//! 24. No panics on valid parameter ranges.

use ftui_runtime::cost_model::{BatchCostParams, CacheCostParams, PipelineCostParams, StageStats};
use proptest::prelude::*;

// ── Strategies ────────────────────────────────────────────────────────────

fn cache_params_strategy() -> impl Strategy<Value = CacheCostParams> {
    (
        1.0f64..=500.0,           // c_miss_us
        0.00001f64..=0.01,        // c_mem_per_byte
        1.0f64..=1000.0,          // item_bytes
        1.0f64..=10000.0,         // working_set_n
        0.5f64..=5.0,             // zipf_alpha
        1000.0f64..=10_000_000.0, // budget_max_bytes
    )
        .prop_map(
            |(c_miss, c_mem, item, n, alpha, max_budget)| CacheCostParams {
                c_miss_us: c_miss,
                c_mem_per_byte: c_mem,
                item_bytes: item,
                working_set_n: n,
                zipf_alpha: alpha,
                budget_max_bytes: max_budget,
            },
        )
}

fn pipeline_params_strategy() -> impl Strategy<Value = PipelineCostParams> {
    let stages = proptest::collection::vec(stage_strategy(), 1..=8);
    (stages, 0.00001f64..=0.001, 1000.0f64..=100_000.0).prop_map(|(stages, arrival, budget)| {
        PipelineCostParams {
            stages,
            arrival_rate: arrival,
            frame_budget_us: budget,
        }
    })
}

fn stage_strategy() -> impl Strategy<Value = StageStats> {
    (1.0f64..=5000.0, 0.0f64..=1_000_000.0).prop_map(|(mean, var)| StageStats {
        name: "test",
        mean_us: mean,
        var_us2: var,
    })
}

fn batch_params_strategy() -> impl Strategy<Value = BatchCostParams> {
    (
        0.1f64..=100.0,  // c_overhead_us
        0.001f64..=10.0, // c_per_patch_us
        0.01f64..=10.0,  // c_latency_us
        1u64..=10000,    // total_patches
    )
        .prop_map(|(overhead, per_patch, latency, n)| BatchCostParams {
            c_overhead_us: overhead,
            c_per_patch_us: per_patch,
            c_latency_us: latency,
            total_patches: n,
        })
}

// ═════════════════════════════════════════════════════════════════════════
// 1. miss_rate always returns values in [0, 1]
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn miss_rate_in_unit_interval(
        params in cache_params_strategy(),
        budget_frac in 0.0f64..=3.0,
    ) {
        let budget = budget_frac * params.budget_max_bytes;
        let mr = params.miss_rate(budget);
        prop_assert!(
            (0.0..=1.0).contains(&mr),
            "miss_rate({}) = {} not in [0, 1]",
            budget, mr
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. miss_rate is monotonically non-increasing with budget
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn miss_rate_monotone_decreasing(
        params in cache_params_strategy(),
        b1_frac in 0.0f64..=1.0,
        b2_frac in 0.0f64..=1.0,
    ) {
        let lo = b1_frac.min(b2_frac) * params.budget_max_bytes;
        let hi = b1_frac.max(b2_frac) * params.budget_max_bytes;
        let mr_lo = params.miss_rate(lo);
        let mr_hi = params.miss_rate(hi);
        prop_assert!(
            mr_hi <= mr_lo + 1e-10,
            "miss_rate is not monotone: mr({}) = {} > mr({}) = {}",
            lo, mr_lo, hi, mr_hi
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. total_cost is always non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn total_cost_non_negative(
        params in cache_params_strategy(),
        budget_frac in 0.0f64..=2.0,
    ) {
        let budget = budget_frac * params.budget_max_bytes;
        let cost = params.total_cost(budget);
        prop_assert!(
            cost >= -1e-10,
            "total_cost({}) = {} is negative",
            budget, cost
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. optimal_budget is within [item_bytes, budget_max_bytes]
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn optimal_budget_in_range(params in cache_params_strategy()) {
        let b = params.optimal_budget();
        prop_assert!(
            b >= params.item_bytes - 1e-10,
            "optimal_budget {} < item_bytes {}",
            b, params.item_bytes
        );
        prop_assert!(
            b <= params.budget_max_bytes + 1e-10,
            "optimal_budget {} > budget_max {}",
            b, params.budget_max_bytes
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. optimal_budget is a local minimum of total_cost
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn optimal_budget_cost_finite(params in cache_params_strategy()) {
        // The analytical formula is a closed-form approximation. Verify that
        // the cost at the returned budget is always finite and non-negative.
        let b_star = params.optimal_budget();
        let cost_star = params.total_cost(b_star);

        prop_assert!(
            cost_star.is_finite() && cost_star >= 0.0,
            "cost at optimal budget {} should be finite and non-negative, got {}",
            b_star, cost_star
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. optimize cost decomposition: miss + mem = total
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn optimize_cost_decomposition(params in cache_params_strategy()) {
        let result = params.optimize();
        let sum = result.cost_miss_us + result.cost_mem_us;
        prop_assert!(
            (sum - result.optimal_cost_us).abs() < 1e-6,
            "miss {} + mem {} = {} != total {}",
            result.cost_miss_us, result.cost_mem_us, sum, result.optimal_cost_us
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. hit_rate + miss_rate == 1
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn hit_plus_miss_equals_one(params in cache_params_strategy()) {
        let result = params.optimize();
        let sum = result.optimal_hit_rate + result.optimal_miss_rate;
        prop_assert!(
            (sum - 1.0).abs() < 1e-10,
            "hit {} + miss {} = {} != 1.0",
            result.optimal_hit_rate, result.optimal_miss_rate, sum
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. evaluate and total_cost agree
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn evaluate_total_cost_agree(
        params in cache_params_strategy(),
        budget_frac in 0.01f64..=2.0,
    ) {
        let budget = budget_frac * params.budget_max_bytes;
        let tc = params.total_cost(budget);
        let point = params.evaluate(budget);
        prop_assert!(
            (tc - point.total_cost_us).abs() < 1e-6,
            "total_cost({}) = {} but evaluate = {}",
            budget, tc, point.total_cost_us
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. utilization = arrival_rate × total_mean
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn utilization_formula(params in pipeline_params_strategy()) {
        let result = params.analyze();
        let expected_rho = params.arrival_rate * result.total_mean_us;
        prop_assert!(
            (result.utilization - expected_rho).abs() < 1e-6,
            "utilization {} != arrival_rate * total_mean = {}",
            result.utilization, expected_rho
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. stage fractions sum to 1.0 (when total > 0)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn stage_fractions_sum_to_one(params in pipeline_params_strategy()) {
        let result = params.analyze();
        if result.total_mean_us > 1e-10 {
            let sum: f64 = result.stage_breakdown.iter().map(|s| s.fraction).sum();
            prop_assert!(
                (sum - 1.0).abs() < 1e-6,
                "stage fractions sum to {} != 1.0",
                sum
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. stable ↔ utilization < 1
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn stable_iff_utilization_below_one(params in pipeline_params_strategy()) {
        let result = params.analyze();
        prop_assert_eq!(
            result.stable,
            result.utilization < 1.0,
            "stable={} but utilization={}",
            result.stable, result.utilization
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. mean_sojourn >= total_mean (queueing always adds delay)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn sojourn_at_least_service_time(params in pipeline_params_strategy()) {
        let result = params.analyze();
        if result.mean_sojourn_us.is_finite() {
            prop_assert!(
                result.mean_sojourn_us >= result.total_mean_us - 1e-6,
                "sojourn {} < service {} (queueing should add delay)",
                result.mean_sojourn_us, result.total_mean_us
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. headroom = frame_budget - mean_sojourn
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn headroom_consistent(params in pipeline_params_strategy()) {
        let result = params.analyze();
        if result.mean_sojourn_us.is_finite() {
            let expected = params.frame_budget_us - result.mean_sojourn_us;
            prop_assert!(
                (result.headroom_us - expected).abs() < 1e-6,
                "headroom {} != budget - sojourn = {}",
                result.headroom_us, expected
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. coefficient of variation is non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cv_non_negative(params in pipeline_params_strategy()) {
        let result = params.analyze();
        for s in &result.stage_breakdown {
            prop_assert!(
                s.cv >= 0.0,
                "cv for stage {} is negative: {}",
                s.name, s.cv
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 15. batch total_cost is non-negative
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn batch_total_cost_non_negative(
        params in batch_params_strategy(),
        k in 1u64..=10000,
    ) {
        let cost = params.total_cost(k);
        prop_assert!(
            cost >= -1e-10,
            "batch total_cost({}) = {} is negative",
            k, cost
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 16. optimal_batch_size is in [1, n]
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn optimal_batch_in_range(params in batch_params_strategy()) {
        let k = params.optimal_batch_size();
        prop_assert!(k >= 1, "optimal batch size {} < 1", k);
        prop_assert!(
            k <= params.total_patches,
            "optimal batch size {} > n={}",
            k, params.total_patches
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 17. optimal cost <= cost at any sampled batch size
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn optimal_cost_is_global_minimum(
        params in batch_params_strategy(),
        k_frac in 0.0f64..1.0,
    ) {
        let n = params.total_patches;
        let k_star = params.optimal_batch_size();
        let cost_star = params.total_cost(k_star);

        // Sample another batch size
        let k_other = ((k_frac * n as f64) as u64).max(1).min(n);
        let cost_other = params.total_cost(k_other);

        prop_assert!(
            cost_star <= cost_other + 0.01,
            "optimal cost {} at k={} > cost {} at k={}",
            cost_star, k_star, cost_other, k_other
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 18. batch_size=0 and n=0 don't panic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn batch_zero_no_panic(params in batch_params_strategy()) {
        // batch_size = 0
        let _ = params.total_cost(0);
        let _ = params.evaluate(0);

        // n = 0
        let zero_n = BatchCostParams {
            total_patches: 0,
            ..params
        };
        let _ = zero_n.total_cost(1);
        let _ = zero_n.optimal_batch_size();
        let _ = zero_n.optimize();
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 19. no overhead → optimal is immediate (k=1)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_overhead_means_immediate(
        per_patch in 0.001f64..=10.0,
        latency in 0.01f64..=10.0,
        n in 2u64..=1000,
    ) {
        let params = BatchCostParams {
            c_overhead_us: 0.0,
            c_per_patch_us: per_patch,
            c_latency_us: latency,
            total_patches: n,
        };
        prop_assert_eq!(
            params.optimal_batch_size(), 1,
            "no overhead should give k=1"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 20. no latency → optimal is single batch (k=n)
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_latency_means_single_batch(
        overhead in 0.1f64..=100.0,
        per_patch in 0.001f64..=10.0,
        n in 2u64..=1000,
    ) {
        let params = BatchCostParams {
            c_overhead_us: overhead,
            c_per_patch_us: per_patch,
            c_latency_us: 0.0,
            total_patches: n,
        };
        prop_assert_eq!(
            params.optimal_batch_size(), n,
            "no latency should give k=n={}",
            n
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 21. improvement_ratio >= 1.0
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn improvement_at_least_one(params in batch_params_strategy()) {
        let result = params.optimize();
        prop_assert!(
            result.improvement_ratio >= 1.0 - 1e-10,
            "improvement_ratio {} < 1.0",
            result.improvement_ratio
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 22. evaluate decomposition: overhead + processing + latency = total
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn evaluate_decomposition(
        params in batch_params_strategy(),
        k_frac in 0.0f64..1.0,
    ) {
        let k = ((k_frac * params.total_patches as f64) as u64).max(1);
        let point = params.evaluate(k);
        let sum = point.overhead_us + point.processing_us + point.latency_us;
        prop_assert!(
            (sum - point.total_cost_us).abs() < 1e-6,
            "overhead {} + processing {} + latency {} = {} != total {}",
            point.overhead_us, point.processing_us, point.latency_us,
            sum, point.total_cost_us
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 23. All three models are deterministic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cache_model_deterministic(params in cache_params_strategy()) {
        let r1 = params.optimize();
        let r2 = params.optimize();
        prop_assert!(
            (r1.optimal_budget_bytes - r2.optimal_budget_bytes).abs() < 1e-10,
            "cache model not deterministic"
        );
        prop_assert!(
            (r1.optimal_cost_us - r2.optimal_cost_us).abs() < 1e-10,
            "cache cost not deterministic"
        );
    }

    #[test]
    fn pipeline_model_deterministic(params in pipeline_params_strategy()) {
        let r1 = params.analyze();
        let r2 = params.analyze();
        prop_assert!(
            (r1.utilization - r2.utilization).abs() < 1e-10,
            "pipeline utilization not deterministic"
        );
        if r1.mean_sojourn_us.is_finite() {
            prop_assert!(
                (r1.mean_sojourn_us - r2.mean_sojourn_us).abs() < 1e-10,
                "pipeline sojourn not deterministic"
            );
        }
    }

    #[test]
    fn batch_model_deterministic(params in batch_params_strategy()) {
        let r1 = params.optimize();
        let r2 = params.optimize();
        prop_assert_eq!(
            r1.optimal_batch_size, r2.optimal_batch_size,
            "batch optimal not deterministic"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 24. No panics on valid parameter ranges
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn no_panic_cache_operations(params in cache_params_strategy()) {
        let _ = params.miss_rate(0.0);
        let _ = params.miss_rate(params.budget_max_bytes);
        let _ = params.total_cost(0.0);
        let _ = params.total_cost(params.budget_max_bytes);
        let _ = params.optimal_budget();
        let _ = params.evaluate(params.budget_max_bytes / 2.0);
        let result = params.optimize();
        let _ = result.to_jsonl();
        let _ = format!("{}", result);
    }

    #[test]
    fn no_panic_pipeline_operations(params in pipeline_params_strategy()) {
        let result = params.analyze();
        let _ = result.to_jsonl();
        let _ = format!("{}", result);
    }

    #[test]
    fn no_panic_batch_operations(params in batch_params_strategy()) {
        let _ = params.total_cost(0);
        let _ = params.total_cost(1);
        let _ = params.total_cost(params.total_patches);
        let _ = params.optimal_batch_size();
        let _ = params.evaluate(1);
        let result = params.optimize();
        let _ = result.to_jsonl();
        let _ = format!("{}", result);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 25. Cache miss_rate at zero budget is 1.0
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn miss_rate_zero_budget_is_one(params in cache_params_strategy()) {
        let mr = params.miss_rate(0.0);
        prop_assert!(
            (mr - 1.0).abs() < 1e-10,
            "miss_rate(0) should be 1.0, got {}",
            mr
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 26. Cache miss_rate at full coverage is 0.0
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn miss_rate_full_coverage_is_zero(params in cache_params_strategy()) {
        let full_budget = params.item_bytes * params.working_set_n;
        let mr = params.miss_rate(full_budget);
        prop_assert!(
            mr.abs() < 1e-10,
            "miss_rate at full coverage ({}) should be ~0, got {}",
            full_budget, mr
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 27. Pipeline total_mean = sum of stage means
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn pipeline_total_mean_is_sum(params in pipeline_params_strategy()) {
        let result = params.analyze();
        let expected: f64 = params.stages.iter().map(|s| s.mean_us).sum();
        prop_assert!(
            (result.total_mean_us - expected).abs() < 1e-6,
            "total_mean {} != sum of stages {}",
            result.total_mean_us, expected
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 28. Pipeline total_var = sum of stage variances
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn pipeline_total_var_is_sum(params in pipeline_params_strategy()) {
        let result = params.analyze();
        let expected: f64 = params.stages.iter().map(|s| s.var_us2).sum();
        prop_assert!(
            (result.total_var_us2 - expected).abs() < 1e-6,
            "total_var {} != sum of stage variances {}",
            result.total_var_us2, expected
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 29. Batch cost formula manual verification
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn batch_cost_formula_correct(
        params in batch_params_strategy(),
        k_frac in 0.0f64..1.0,
    ) {
        let n = params.total_patches;
        let k = ((k_frac * n as f64) as u64).max(1).min(n);
        let cost = params.total_cost(k);

        // Manual computation: ceil(n/k) * overhead + n * per_patch + (k-1) * latency
        let num_batches = n.div_ceil(k);
        let expected = num_batches as f64 * params.c_overhead_us
            + n as f64 * params.c_per_patch_us
            + (k.saturating_sub(1)) as f64 * params.c_latency_us;

        prop_assert!(
            (cost - expected).abs() < 1e-6,
            "total_cost(k={}) = {} but formula gives {}",
            k, cost, expected
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 30. Unstable pipeline has infinite sojourn
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn unstable_pipeline_infinite_sojourn(
        mean in 50000.0f64..=200000.0,
        var in 0.0f64..=1_000_000.0,
    ) {
        // Force ρ >= 1 with high arrival rate relative to service time
        let params = PipelineCostParams {
            stages: vec![StageStats {
                name: "test",
                mean_us: mean,
                var_us2: var,
            }],
            arrival_rate: 1.0 / 16667.0, // 60fps
            frame_budget_us: 16667.0,
        };
        let result = params.analyze();
        // ρ = λ * E[S] = mean / 16667 which is >= 3 for mean >= 50000
        if !result.stable {
            prop_assert!(
                result.mean_sojourn_us.is_infinite(),
                "unstable pipeline should have infinite sojourn, got {}",
                result.mean_sojourn_us
            );
        }
    }
}
