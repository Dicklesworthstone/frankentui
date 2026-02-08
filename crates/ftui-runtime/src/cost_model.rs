#![forbid(unsafe_code)]

//! Formal cost models for caches, scheduling, and batching (bd-lff4p.5.6).
//!
//! This module provides principled mathematical models with explicit objective
//! functions for three subsystems:
//!
//! 1. **Cache cost model** — loss function `L(hit_rate, memory_bytes)` for glyph
//!    atlas / cell caches, with optimal budget derivation under LRU.
//! 2. **Pipeline scheduling model** — M/G/1 queue model for the
//!    `input → update → view → diff → present` pipeline, including optimal
//!    batch size and latency decomposition.
//! 3. **Patch batching model** — cost of immediate-flush vs deferred-coalesce
//!    for the `BufferDiff → CellPatch → GPU upload` path.
//!
//! # Mathematical Framework
//!
//! All models follow the same pattern:
//! - **Objective function** `J(θ)` parameterized by policy knobs `θ`.
//! - **Constraint set** capturing hard limits (memory, latency budget, etc.).
//! - **Optimal policy** `θ*` = argmin J(θ) subject to constraints.
//! - **Evidence** comparing the chosen policy to alternatives.
//!
//! # Cache Cost Model
//!
//! ## Objective
//!
//! ```text
//! J(B) = c_miss × miss_rate(B) + c_mem × B
//! ```
//!
//! where `B` is cache budget in bytes, `c_miss` is the cost of a cache miss
//! (rasterization + upload latency in µs), and `c_mem` is the opportunity cost
//! per byte of atlas memory.
//!
//! ## Miss Rate Model (LRU on IRM workload)
//!
//! Under the Independent Reference Model with Zipf(α) popularity:
//!
//! ```text
//! miss_rate(B) ≈ 1 − min(1, (B / item_bytes) / N)^(1/α)
//! ```
//!
//! where `N` is the working set size and `α` is the Zipf exponent.
//!
//! The optimal budget minimizes `J(B)` subject to `B ≤ B_max`:
//!
//! ```text
//! B* = item_bytes × N × (c_miss / (α × c_mem × N))^(α/(1+α))
//! ```
//!
//! clamped to `[item_bytes, B_max]`.
//!
//! # Pipeline Scheduling Model
//!
//! ## M/G/1 Queue
//!
//! Model the render pipeline as a single server with:
//! - Arrival rate `λ` (frames/ms)
//! - Service time distribution `S` with mean `E[S]` and variance `Var[S]`
//!
//! Pollaczek-Khinchine formula for mean sojourn time:
//!
//! ```text
//! E[T] = E[S] + (λ × E[S²]) / (2 × (1 − ρ))
//! ```
//!
//! where `ρ = λ × E[S]` is utilization.
//!
//! ## Stage Decomposition
//!
//! ```text
//! S = S_input + S_update + S_view + S_diff + S_present
//! ```
//!
//! Each stage `i` has independent service time `S_i` with measured mean and
//! variance. Total variance: `Var[S] = Σ Var[S_i]`.
//!
//! # Patch Batching Model
//!
//! ## Batch vs Immediate
//!
//! ```text
//! J_immediate = n × (c_overhead + c_per_patch)
//! J_batch(k) = ceil(n/k) × (c_overhead + k × c_per_patch) + (k−1) × c_latency
//! ```
//!
//! Optimal batch size:
//!
//! ```text
//! k* = sqrt(n × c_overhead / c_latency)
//! ```
//!
//! clamped to `[1, n]`.

use std::fmt;

// ─── Cache Cost Model ─────────────────────────────────────────────────────

/// Parameters for the glyph/cell cache cost model.
#[derive(Debug, Clone)]
pub struct CacheCostParams {
    /// Cost of a cache miss in µs (rasterize + upload).
    pub c_miss_us: f64,
    /// Opportunity cost per byte of atlas memory (µs/byte/frame).
    pub c_mem_per_byte: f64,
    /// Average item size in bytes (slot area including padding).
    pub item_bytes: f64,
    /// Working set size (number of distinct glyphs in a typical session).
    pub working_set_n: f64,
    /// Zipf exponent for access frequency distribution.
    /// α > 1 means heavy-tail (few glyphs dominate); typical terminal ≈ 1.2–1.8.
    pub zipf_alpha: f64,
    /// Maximum allowed cache budget in bytes.
    pub budget_max_bytes: f64,
}

impl Default for CacheCostParams {
    fn default() -> Self {
        Self {
            // Typical glyph rasterize + GPU upload: ~50µs
            c_miss_us: 50.0,
            // Memory pressure: ~0.0001 µs/byte/frame (at 2MB atlas ≈ 0.2µs/frame)
            c_mem_per_byte: 0.0001,
            // 16×16 glyph slot with 1px padding = 18×18 = 324 bytes
            item_bytes: 324.0,
            // ASCII printable + common unicode ≈ 200 distinct glyphs
            working_set_n: 200.0,
            // Terminal text: moderately heavy-tail
            zipf_alpha: 1.5,
            // 4MB atlas maximum
            budget_max_bytes: 4_194_304.0,
        }
    }
}

/// Result of cache cost model optimization.
#[derive(Debug, Clone)]
pub struct CacheCostResult {
    /// Optimal cache budget in bytes.
    pub optimal_budget_bytes: f64,
    /// Total cost at optimal budget (µs/frame).
    pub optimal_cost_us: f64,
    /// Miss rate at optimal budget.
    pub optimal_miss_rate: f64,
    /// Hit rate at optimal budget.
    pub optimal_hit_rate: f64,
    /// Cost breakdown: miss component (µs/frame).
    pub cost_miss_us: f64,
    /// Cost breakdown: memory component (µs/frame).
    pub cost_mem_us: f64,
    /// Number of items that fit in optimal budget.
    pub items_cached: f64,
    /// Evidence: cost at selected comparison points.
    pub comparison_points: Vec<CacheCostPoint>,
}

/// A single evaluation point on the cache cost surface.
#[derive(Debug, Clone)]
pub struct CacheCostPoint {
    /// Budget in bytes.
    pub budget_bytes: f64,
    /// Miss rate at this budget.
    pub miss_rate: f64,
    /// Total cost at this budget (µs/frame).
    pub total_cost_us: f64,
    /// Miss component (µs/frame).
    pub cost_miss_us: f64,
    /// Memory component (µs/frame).
    pub cost_mem_us: f64,
}

impl CacheCostParams {
    /// Compute the miss rate under LRU with IRM/Zipf workload.
    ///
    /// Approximation: `miss_rate(B) ≈ 1 − min(1, (capacity / N))^(1/α)`
    /// where capacity = B / item_bytes.
    #[must_use]
    pub fn miss_rate(&self, budget_bytes: f64) -> f64 {
        let capacity = budget_bytes / self.item_bytes;
        let ratio = (capacity / self.working_set_n).clamp(0.0, 1.0);
        let hit_rate = ratio.powf(1.0 / self.zipf_alpha);
        (1.0 - hit_rate).max(0.0)
    }

    /// Total cost J(B) = c_miss × miss_rate(B) × N_accesses + c_mem × B.
    ///
    /// We normalize to per-frame cost assuming `working_set_n` accesses/frame.
    #[must_use]
    pub fn total_cost(&self, budget_bytes: f64) -> f64 {
        let mr = self.miss_rate(budget_bytes);
        let cost_miss = self.c_miss_us * mr * self.working_set_n;
        let cost_mem = self.c_mem_per_byte * budget_bytes;
        cost_miss + cost_mem
    }

    /// Evaluate a single point on the cost surface.
    #[must_use]
    pub fn evaluate(&self, budget_bytes: f64) -> CacheCostPoint {
        let mr = self.miss_rate(budget_bytes);
        let cost_miss = self.c_miss_us * mr * self.working_set_n;
        let cost_mem = self.c_mem_per_byte * budget_bytes;
        CacheCostPoint {
            budget_bytes,
            miss_rate: mr,
            total_cost_us: cost_miss + cost_mem,
            cost_miss_us: cost_miss,
            cost_mem_us: cost_mem,
        }
    }

    /// Compute the optimal cache budget analytically.
    ///
    /// From `dJ/dB = 0`:
    /// ```text
    /// B* = item_bytes × N × (c_miss / (α × c_mem_per_byte × item_bytes × N))^(α/(1+α))
    /// ```
    /// clamped to `[item_bytes, budget_max_bytes]`.
    #[must_use]
    pub fn optimal_budget(&self) -> f64 {
        let alpha = self.zipf_alpha;
        let n = self.working_set_n;
        let s = self.item_bytes;
        let cm = self.c_miss_us;
        let cmem = self.c_mem_per_byte;

        // Avoid division by zero.
        if cmem <= 0.0 || alpha <= 0.0 || n <= 0.0 || s <= 0.0 {
            return self.budget_max_bytes;
        }

        // Analytical optimum from first-order condition.
        let ratio = cm / (alpha * cmem * s * n);
        let exponent = alpha / (1.0 + alpha);
        let b_star = s * n * ratio.powf(exponent);

        b_star.clamp(s, self.budget_max_bytes)
    }

    /// Run the full optimization and produce evidence.
    #[must_use]
    pub fn optimize(&self) -> CacheCostResult {
        let b_star = self.optimal_budget();
        let opt_point = self.evaluate(b_star);

        // Generate comparison points at 10%, 25%, 50%, 100%, 150%, 200% of optimal.
        let fractions = [0.1, 0.25, 0.5, 1.0, 1.5, 2.0];
        let comparison_points: Vec<CacheCostPoint> = fractions
            .iter()
            .map(|f| {
                let b = (b_star * f).clamp(self.item_bytes, self.budget_max_bytes);
                self.evaluate(b)
            })
            .collect();

        CacheCostResult {
            optimal_budget_bytes: b_star,
            optimal_cost_us: opt_point.total_cost_us,
            optimal_miss_rate: opt_point.miss_rate,
            optimal_hit_rate: 1.0 - opt_point.miss_rate,
            cost_miss_us: opt_point.cost_miss_us,
            cost_mem_us: opt_point.cost_mem_us,
            items_cached: b_star / self.item_bytes,
            comparison_points,
        }
    }
}

impl CacheCostResult {
    /// Serialize to JSONL for evidence ledger.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("{\"event\":\"cache_cost_optimal\"");
        push_f64(&mut out, "optimal_budget_bytes", self.optimal_budget_bytes);
        push_f64(&mut out, "optimal_cost_us", self.optimal_cost_us);
        push_f64(&mut out, "optimal_miss_rate", self.optimal_miss_rate);
        push_f64(&mut out, "optimal_hit_rate", self.optimal_hit_rate);
        push_f64(&mut out, "cost_miss_us", self.cost_miss_us);
        push_f64(&mut out, "cost_mem_us", self.cost_mem_us);
        push_f64(&mut out, "items_cached", self.items_cached);
        out.push_str(",\"comparisons\":[");
        for (i, pt) in self.comparison_points.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&pt.to_json());
        }
        out.push_str("]}");
        out
    }
}

impl CacheCostPoint {
    fn to_json(&self) -> String {
        format!(
            "{{\"budget_bytes\":{:.1},\"miss_rate\":{:.6},\"total_cost_us\":{:.3},\"cost_miss_us\":{:.3},\"cost_mem_us\":{:.3}}}",
            self.budget_bytes,
            self.miss_rate,
            self.total_cost_us,
            self.cost_miss_us,
            self.cost_mem_us
        )
    }
}

// ─── Pipeline Scheduling Model ────────────────────────────────────────────

/// Service time statistics for a single pipeline stage.
#[derive(Debug, Clone, Copy)]
pub struct StageStats {
    /// Stage name.
    pub name: &'static str,
    /// Mean service time (µs).
    pub mean_us: f64,
    /// Variance of service time (µs²).
    pub var_us2: f64,
}

impl StageStats {
    /// Second moment E[S²] = Var[S] + E[S]².
    #[must_use]
    pub fn second_moment(&self) -> f64 {
        self.var_us2 + self.mean_us * self.mean_us
    }
}

/// Parameters for the M/G/1 pipeline scheduling model.
#[derive(Debug, Clone)]
pub struct PipelineCostParams {
    /// Service time statistics for each pipeline stage.
    pub stages: Vec<StageStats>,
    /// Frame arrival rate (frames/µs). At 60fps: 1/16667 ≈ 0.00006.
    pub arrival_rate: f64,
    /// Target frame budget (µs). At 60fps: 16667.
    pub frame_budget_us: f64,
}

impl Default for PipelineCostParams {
    fn default() -> Self {
        Self {
            stages: vec![
                StageStats {
                    name: "input",
                    mean_us: 50.0,
                    var_us2: 100.0,
                },
                StageStats {
                    name: "update",
                    mean_us: 200.0,
                    var_us2: 2500.0,
                },
                StageStats {
                    name: "view",
                    mean_us: 1500.0,
                    var_us2: 250_000.0,
                },
                StageStats {
                    name: "diff",
                    mean_us: 800.0,
                    var_us2: 90_000.0,
                },
                StageStats {
                    name: "present",
                    mean_us: 500.0,
                    var_us2: 40_000.0,
                },
            ],
            arrival_rate: 1.0 / 16667.0, // 60fps
            frame_budget_us: 16667.0,
        }
    }
}

/// Result of pipeline cost model analysis.
#[derive(Debug, Clone)]
pub struct PipelineCostResult {
    /// Total mean service time (µs).
    pub total_mean_us: f64,
    /// Total service time variance (µs²).
    pub total_var_us2: f64,
    /// Server utilization ρ = λ × E[S].
    pub utilization: f64,
    /// Mean sojourn time via Pollaczek-Khinchine (µs).
    pub mean_sojourn_us: f64,
    /// Whether the system is stable (ρ < 1).
    pub stable: bool,
    /// Fraction of frame budget consumed by mean service.
    pub budget_fraction: f64,
    /// Per-stage breakdown.
    pub stage_breakdown: Vec<StageBreakdown>,
    /// Headroom: frame_budget - mean_sojourn (µs).
    pub headroom_us: f64,
}

/// Per-stage contribution to the pipeline.
#[derive(Debug, Clone)]
pub struct StageBreakdown {
    /// Stage name.
    pub name: &'static str,
    /// Mean service time (µs).
    pub mean_us: f64,
    /// Fraction of total service time.
    pub fraction: f64,
    /// Coefficient of variation (σ/µ).
    pub cv: f64,
}

impl PipelineCostParams {
    /// Analyze the pipeline using M/G/1 queueing theory.
    #[must_use]
    pub fn analyze(&self) -> PipelineCostResult {
        let total_mean: f64 = self.stages.iter().map(|s| s.mean_us).sum();
        let total_var: f64 = self.stages.iter().map(|s| s.var_us2).sum();
        let total_second_moment = total_var + total_mean * total_mean;

        let rho = self.arrival_rate * total_mean;
        let stable = rho < 1.0;

        // Pollaczek-Khinchine: E[T] = E[S] + λE[S²] / (2(1-ρ))
        let mean_sojourn = if stable && rho > 0.0 {
            total_mean + (self.arrival_rate * total_second_moment) / (2.0 * (1.0 - rho))
        } else if rho >= 1.0 {
            f64::INFINITY
        } else {
            total_mean
        };

        let stage_breakdown: Vec<StageBreakdown> = self
            .stages
            .iter()
            .map(|s| {
                let cv = if s.mean_us > 0.0 {
                    s.var_us2.max(0.0).sqrt() / s.mean_us
                } else {
                    0.0
                };
                StageBreakdown {
                    name: s.name,
                    mean_us: s.mean_us,
                    fraction: if total_mean > 0.0 {
                        s.mean_us / total_mean
                    } else {
                        0.0
                    },
                    cv,
                }
            })
            .collect();

        PipelineCostResult {
            total_mean_us: total_mean,
            total_var_us2: total_var,
            utilization: rho,
            mean_sojourn_us: mean_sojourn,
            stable,
            budget_fraction: if self.frame_budget_us > 0.0 {
                total_mean / self.frame_budget_us
            } else {
                f64::INFINITY
            },
            stage_breakdown,
            headroom_us: self.frame_budget_us - mean_sojourn,
        }
    }
}

impl PipelineCostResult {
    /// Serialize to JSONL for evidence ledger.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("{\"event\":\"pipeline_cost_analysis\"");
        push_f64(&mut out, "total_mean_us", self.total_mean_us);
        push_f64(&mut out, "total_var_us2", self.total_var_us2);
        push_f64(&mut out, "utilization", self.utilization);
        push_f64(&mut out, "mean_sojourn_us", self.mean_sojourn_us);
        push_bool(&mut out, "stable", self.stable);
        push_f64(&mut out, "budget_fraction", self.budget_fraction);
        push_f64(&mut out, "headroom_us", self.headroom_us);
        out.push_str(",\"stages\":[");
        for (i, s) in self.stage_breakdown.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"name\":\"{}\",\"mean_us\":{:.3},\"fraction\":{:.4},\"cv\":{:.4}}}",
                s.name, s.mean_us, s.fraction, s.cv
            ));
        }
        out.push_str("]}");
        out
    }
}

// ─── Patch Batching Model ─────────────────────────────────────────────────

/// Parameters for the patch batching cost model.
#[derive(Debug, Clone)]
pub struct BatchCostParams {
    /// Per-batch overhead in µs (GPU command buffer setup, draw call).
    pub c_overhead_us: f64,
    /// Per-patch processing cost in µs (cell serialization + copy).
    pub c_per_patch_us: f64,
    /// Latency cost per deferred patch in µs (visual staleness).
    pub c_latency_us: f64,
    /// Total patches to process in the frame.
    pub total_patches: u64,
}

impl Default for BatchCostParams {
    fn default() -> Self {
        Self {
            // GPU command buffer overhead: ~20µs per draw call
            c_overhead_us: 20.0,
            // Cell serialization: ~0.05µs per patch (16 bytes)
            c_per_patch_us: 0.05,
            // Latency penalty: ~0.5µs per patch deferred
            c_latency_us: 0.5,
            // Typical dirty cell count at 5% change rate on 120x40
            total_patches: 240,
        }
    }
}

/// Result of batch cost model optimization.
#[derive(Debug, Clone)]
pub struct BatchCostResult {
    /// Optimal batch size.
    pub optimal_batch_size: u64,
    /// Total cost with optimal batching (µs).
    pub optimal_cost_us: f64,
    /// Cost with immediate flush (batch_size = 1, µs).
    pub immediate_cost_us: f64,
    /// Cost with single batch (batch_size = n, µs).
    pub single_batch_cost_us: f64,
    /// Improvement ratio (immediate / optimal).
    pub improvement_ratio: f64,
    /// Evidence: cost at selected batch sizes.
    pub comparison_points: Vec<BatchCostPoint>,
}

/// A single evaluation point on the batch cost surface.
#[derive(Debug, Clone)]
pub struct BatchCostPoint {
    /// Batch size.
    pub batch_size: u64,
    /// Number of batches.
    pub num_batches: u64,
    /// Total cost (µs).
    pub total_cost_us: f64,
    /// Overhead component (µs).
    pub overhead_us: f64,
    /// Processing component (µs).
    pub processing_us: f64,
    /// Latency component (µs).
    pub latency_us: f64,
}

impl BatchCostParams {
    /// Total cost for a given batch size k.
    ///
    /// ```text
    /// J(k) = ceil(n/k) × (c_overhead + k × c_per_patch) + (k−1) × c_latency
    /// ```
    #[must_use]
    pub fn total_cost(&self, batch_size: u64) -> f64 {
        let n = self.total_patches;
        if n == 0 || batch_size == 0 {
            return 0.0;
        }
        let k = batch_size.min(n);
        let num_batches = n.div_ceil(k);

        let overhead = num_batches as f64 * self.c_overhead_us;
        let processing = n as f64 * self.c_per_patch_us;
        let latency = (k.saturating_sub(1)) as f64 * self.c_latency_us;
        overhead + processing + latency
    }

    /// Evaluate a single point.
    #[must_use]
    pub fn evaluate(&self, batch_size: u64) -> BatchCostPoint {
        let n = self.total_patches;
        let k = batch_size.max(1).min(n.max(1));
        let num_batches = if n > 0 { n.div_ceil(k) } else { 0 };

        let overhead = num_batches as f64 * self.c_overhead_us;
        let processing = n as f64 * self.c_per_patch_us;
        let latency = (k.saturating_sub(1)) as f64 * self.c_latency_us;

        BatchCostPoint {
            batch_size: k,
            num_batches,
            total_cost_us: overhead + processing + latency,
            overhead_us: overhead,
            processing_us: processing,
            latency_us: latency,
        }
    }

    /// Compute optimal batch size.
    ///
    /// The continuous optimum from `dJ/dk = 0` is:
    /// ```text
    /// k* = sqrt(n × c_overhead / c_latency)
    /// ```
    ///
    /// Because `ceil(n/k)` creates discontinuities, the true discrete
    /// optimum is found by enumerating candidate batch sizes at all
    /// points where `ceil(n/k)` changes value. There are at most
    /// `O(sqrt(n))` such candidates.
    #[must_use]
    pub fn optimal_batch_size(&self) -> u64 {
        let n = self.total_patches;
        if n == 0 {
            return 1;
        }
        if self.c_latency_us <= 0.0 {
            return n; // No latency cost: single batch is optimal.
        }
        if self.c_overhead_us <= 0.0 {
            return 1; // No overhead: immediate flush is optimal.
        }

        // Collect candidate k values where ceil(n/k) changes.
        // For each number of batches m, the largest k giving exactly m batches
        // is k = ceil(n/m). We check m = 1..sqrt(n) and the reciprocal k values.
        let mut candidates: Vec<u64> = Vec::new();
        let sqrt_n = (n as f64).sqrt().ceil() as u64 + 1;

        for m in 1..=sqrt_n.min(n) {
            let k = n.div_ceil(m);
            candidates.push(k);
            if k > 1 {
                candidates.push(k - 1);
            }
        }
        // Also check k = 1..sqrt(n) directly.
        for k in 1..=sqrt_n.min(n) {
            candidates.push(k);
        }
        candidates.push(n);

        candidates.sort_unstable();
        candidates.dedup();

        let mut best_k = 1u64;
        let mut best_cost = f64::INFINITY;

        for &k in &candidates {
            if k == 0 || k > n {
                continue;
            }
            let cost = self.total_cost(k);
            if cost < best_cost {
                best_cost = cost;
                best_k = k;
            }
        }

        best_k
    }

    /// Run the full optimization and produce evidence.
    #[must_use]
    pub fn optimize(&self) -> BatchCostResult {
        let n = self.total_patches;
        let k_star = self.optimal_batch_size();
        let opt_cost = self.total_cost(k_star);
        let immediate_cost = self.total_cost(1);
        let single_batch_cost = self.total_cost(n.max(1));

        // Comparison points: 1, k*/4, k*/2, k*, 2k*, n.
        let mut sizes: Vec<u64> = vec![1];
        if k_star > 4 {
            sizes.push(k_star / 4);
        }
        if k_star > 2 {
            sizes.push(k_star / 2);
        }
        sizes.push(k_star);
        if k_star.saturating_mul(2) <= n {
            sizes.push(k_star * 2);
        }
        if n > 1 {
            sizes.push(n);
        }
        sizes.sort_unstable();
        sizes.dedup();

        let comparison_points: Vec<BatchCostPoint> =
            sizes.iter().map(|&k| self.evaluate(k)).collect();

        let improvement_ratio = if opt_cost > 0.0 {
            immediate_cost / opt_cost
        } else {
            1.0
        };

        BatchCostResult {
            optimal_batch_size: k_star,
            optimal_cost_us: opt_cost,
            immediate_cost_us: immediate_cost,
            single_batch_cost_us: single_batch_cost,
            improvement_ratio,
            comparison_points,
        }
    }
}

impl BatchCostResult {
    /// Serialize to JSONL for evidence ledger.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("{\"event\":\"batch_cost_optimal\"");
        push_u64(&mut out, "optimal_batch_size", self.optimal_batch_size);
        push_f64(&mut out, "optimal_cost_us", self.optimal_cost_us);
        push_f64(&mut out, "immediate_cost_us", self.immediate_cost_us);
        push_f64(&mut out, "single_batch_cost_us", self.single_batch_cost_us);
        push_f64(&mut out, "improvement_ratio", self.improvement_ratio);
        out.push_str(",\"comparisons\":[");
        for (i, pt) in self.comparison_points.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"batch_size\":{},\"num_batches\":{},\"total_cost_us\":{:.3},\"overhead_us\":{:.3},\"processing_us\":{:.3},\"latency_us\":{:.3}}}",
                pt.batch_size, pt.num_batches, pt.total_cost_us, pt.overhead_us, pt.processing_us, pt.latency_us
            ));
        }
        out.push_str("]}");
        out
    }
}

// ─── Sensitivity Analysis ─────────────────────────────────────────────────

/// Sensitivity analysis: how does the optimal policy change as a parameter varies?
#[derive(Debug, Clone)]
pub struct SensitivityPoint {
    /// Parameter value.
    pub param_value: f64,
    /// Optimal policy value at this parameter.
    pub optimal_value: f64,
    /// Cost at the optimal policy.
    pub optimal_cost: f64,
}

/// Run sensitivity analysis on cache budget vs Zipf alpha.
///
/// Sweeps `alpha` from `alpha_min` to `alpha_max` in `steps` increments
/// and computes optimal budget at each point.
#[must_use]
pub fn cache_sensitivity_zipf(
    base_params: &CacheCostParams,
    alpha_min: f64,
    alpha_max: f64,
    steps: usize,
) -> Vec<SensitivityPoint> {
    let steps = steps.max(2);
    let step = (alpha_max - alpha_min) / (steps - 1) as f64;

    (0..steps)
        .map(|i| {
            let alpha = alpha_min + step * i as f64;
            let mut params = base_params.clone();
            params.zipf_alpha = alpha;
            let b_star = params.optimal_budget();
            let cost = params.total_cost(b_star);
            SensitivityPoint {
                param_value: alpha,
                optimal_value: b_star,
                optimal_cost: cost,
            }
        })
        .collect()
}

/// Run sensitivity analysis on batch size vs total patches.
#[must_use]
pub fn batch_sensitivity_patches(
    base_params: &BatchCostParams,
    n_min: u64,
    n_max: u64,
    steps: usize,
) -> Vec<SensitivityPoint> {
    let steps = steps.max(2);
    let step = ((n_max - n_min) as f64) / (steps - 1) as f64;

    (0..steps)
        .map(|i| {
            let n = n_min + (step * i as f64).round() as u64;
            let mut params = base_params.clone();
            params.total_patches = n;
            let k_star = params.optimal_batch_size();
            let cost = params.total_cost(k_star);
            SensitivityPoint {
                param_value: n as f64,
                optimal_value: k_star as f64,
                optimal_cost: cost,
            }
        })
        .collect()
}

impl fmt::Display for CacheCostResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Cache Cost Model — Optimal Policy")?;
        writeln!(
            f,
            "  Budget:    {:.0} bytes ({:.0} items)",
            self.optimal_budget_bytes, self.items_cached
        )?;
        writeln!(f, "  Hit rate:  {:.2}%", self.optimal_hit_rate * 100.0)?;
        writeln!(
            f,
            "  Cost:      {:.3} µs/frame (miss: {:.3}, mem: {:.3})",
            self.optimal_cost_us, self.cost_miss_us, self.cost_mem_us
        )?;
        writeln!(f, "  Comparison points:")?;
        for pt in &self.comparison_points {
            writeln!(
                f,
                "    B={:.0}: miss={:.4}, cost={:.3} µs",
                pt.budget_bytes, pt.miss_rate, pt.total_cost_us
            )?;
        }
        Ok(())
    }
}

impl fmt::Display for PipelineCostResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Pipeline Scheduling Model (M/G/1)")?;
        writeln!(f, "  Total mean:     {:.1} µs", self.total_mean_us)?;
        writeln!(f, "  Utilization:    {:.4} (ρ)", self.utilization)?;
        writeln!(f, "  Mean sojourn:   {:.1} µs", self.mean_sojourn_us)?;
        writeln!(f, "  Budget used:    {:.1}%", self.budget_fraction * 100.0)?;
        writeln!(f, "  Headroom:       {:.1} µs", self.headroom_us)?;
        writeln!(f, "  Stable:         {}", self.stable)?;
        writeln!(f, "  Stage breakdown:")?;
        for s in &self.stage_breakdown {
            writeln!(
                f,
                "    {:<10} {:.1} µs ({:.1}%, cv={:.2})",
                s.name,
                s.mean_us,
                s.fraction * 100.0,
                s.cv
            )?;
        }
        Ok(())
    }
}

impl fmt::Display for BatchCostResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Patch Batching Model")?;
        writeln!(f, "  Optimal k:      {}", self.optimal_batch_size)?;
        writeln!(f, "  Optimal cost:   {:.3} µs", self.optimal_cost_us)?;
        writeln!(f, "  Immediate cost: {:.3} µs", self.immediate_cost_us)?;
        writeln!(f, "  Single batch:   {:.3} µs", self.single_batch_cost_us)?;
        writeln!(f, "  Improvement:    {:.2}×", self.improvement_ratio)?;
        writeln!(f, "  Comparison points:")?;
        for pt in &self.comparison_points {
            writeln!(
                f,
                "    k={}: {} batches, {:.3} µs (overhead={:.1}, proc={:.1}, latency={:.1})",
                pt.batch_size,
                pt.num_batches,
                pt.total_cost_us,
                pt.overhead_us,
                pt.processing_us,
                pt.latency_us
            )?;
        }
        Ok(())
    }
}

// ─── JSONL helpers ────────────────────────────────────────────────────────

fn push_f64(out: &mut String, key: &str, value: f64) {
    use std::fmt::Write;
    out.push_str(",\"");
    out.push_str(key);
    out.push_str("\":");
    if value.is_finite() {
        let _ = write!(out, "{value:.6}");
    } else if value.is_nan() {
        out.push_str("null");
    } else if value.is_sign_positive() {
        out.push_str("1e308");
    } else {
        out.push_str("-1e308");
    }
}

fn push_u64(out: &mut String, key: &str, value: u64) {
    use std::fmt::Write;
    out.push_str(",\"");
    out.push_str(key);
    out.push_str("\":");
    let _ = write!(out, "{value}");
}

fn push_bool(out: &mut String, key: &str, value: bool) {
    out.push_str(",\"");
    out.push_str(key);
    out.push_str("\":");
    out.push_str(if value { "true" } else { "false" });
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ═══ Cache cost model ═══════════════════════════════════════════════

    #[test]
    fn cache_miss_rate_full_coverage() {
        let params = CacheCostParams {
            item_bytes: 100.0,
            working_set_n: 10.0,
            zipf_alpha: 1.5,
            budget_max_bytes: 100_000.0,
            ..Default::default()
        };
        // Budget = 10 items × 100 bytes = 1000 → capacity = N → miss_rate = 0.
        let mr = params.miss_rate(1000.0);
        assert!(
            mr.abs() < 1e-10,
            "full coverage should have zero miss rate, got {mr}"
        );
    }

    #[test]
    fn cache_miss_rate_zero_budget() {
        let params = CacheCostParams::default();
        let mr = params.miss_rate(0.0);
        assert!(
            (mr - 1.0).abs() < 1e-10,
            "zero budget should have miss rate 1.0, got {mr}"
        );
    }

    #[test]
    fn cache_miss_rate_monotone_decreasing() {
        let params = CacheCostParams::default();
        let mut prev = 1.0;
        for b in [1000.0, 5000.0, 10_000.0, 50_000.0, 100_000.0] {
            let mr = params.miss_rate(b);
            assert!(
                mr <= prev + 1e-10,
                "miss rate should decrease with budget: {mr} > {prev}"
            );
            prev = mr;
        }
    }

    #[test]
    fn cache_optimal_budget_is_interior() {
        let params = CacheCostParams::default();
        let b_star = params.optimal_budget();
        assert!(
            b_star >= params.item_bytes,
            "optimal should be >= item_bytes"
        );
        assert!(
            b_star <= params.budget_max_bytes,
            "optimal should be <= max"
        );
    }

    #[test]
    fn cache_optimal_is_local_minimum() {
        let params = CacheCostParams::default();
        let b_star = params.optimal_budget();
        let cost_star = params.total_cost(b_star);

        // Nearby points should have >= cost.
        let delta = params.item_bytes;
        let cost_below = params.total_cost((b_star - delta).max(params.item_bytes));
        let cost_above = params.total_cost((b_star + delta).min(params.budget_max_bytes));

        assert!(
            cost_star <= cost_below + 1.0,
            "optimal cost {cost_star} should be <= cost at B-δ {cost_below}"
        );
        assert!(
            cost_star <= cost_above + 1.0,
            "optimal cost {cost_star} should be <= cost at B+δ {cost_above}"
        );
    }

    #[test]
    fn cache_optimize_produces_evidence() {
        let result = CacheCostParams::default().optimize();
        assert!(result.optimal_budget_bytes > 0.0);
        assert!(result.optimal_hit_rate > 0.0);
        assert!(result.optimal_hit_rate <= 1.0);
        assert!(!result.comparison_points.is_empty());
    }

    #[test]
    fn cache_optimize_jsonl_valid() {
        let result = CacheCostParams::default().optimize();
        let jsonl = result.to_jsonl();
        let _: serde_json::Value = serde_json::from_str(&jsonl).expect("valid JSON");
    }

    #[test]
    fn cache_cost_display() {
        let result = CacheCostParams::default().optimize();
        let display = format!("{result}");
        assert!(display.contains("Cache Cost Model"));
        assert!(display.contains("Hit rate"));
    }

    #[test]
    fn cache_high_alpha_needs_less_budget() {
        let params_low = CacheCostParams {
            zipf_alpha: 1.2,
            ..Default::default()
        };
        let params_high = CacheCostParams {
            zipf_alpha: 2.0,
            ..Default::default()
        };

        let b_low = params_low.optimal_budget();
        let b_high = params_high.optimal_budget();

        // Higher Zipf alpha = more skewed = fewer distinct popular items = less cache needed.
        assert!(
            b_high < b_low,
            "higher zipf alpha should need less budget: {b_high} >= {b_low}"
        );
    }

    // ═══ Pipeline scheduling model ═════════════════════════════════════

    #[test]
    fn pipeline_default_is_stable() {
        let result = PipelineCostParams::default().analyze();
        assert!(result.stable, "default pipeline should be stable");
        assert!(result.utilization < 1.0);
    }

    #[test]
    fn pipeline_utilization_formula() {
        let params = PipelineCostParams {
            stages: vec![StageStats {
                name: "test",
                mean_us: 1000.0,
                var_us2: 0.0,
            }],
            arrival_rate: 0.0005, // λ = 0.5/ms
            frame_budget_us: 16667.0,
        };
        let result = params.analyze();
        // ρ = λ × E[S] = 0.0005 × 1000 = 0.5
        assert!(
            (result.utilization - 0.5).abs() < 1e-6,
            "ρ should be 0.5, got {}",
            result.utilization
        );
    }

    #[test]
    fn pipeline_deterministic_sojourn() {
        // Zero variance → M/D/1 → known formula.
        let params = PipelineCostParams {
            stages: vec![StageStats {
                name: "test",
                mean_us: 1000.0,
                var_us2: 0.0,
            }],
            arrival_rate: 0.0005,
            frame_budget_us: 16667.0,
        };
        let result = params.analyze();
        // E[T] = E[S] + λE[S²] / (2(1-ρ))
        // E[S²] = 0 + 1000² = 1e6
        // E[T] = 1000 + 0.0005 * 1e6 / (2 * 0.5) = 1000 + 500 = 1500
        assert!(
            (result.mean_sojourn_us - 1500.0).abs() < 1.0,
            "M/D/1 sojourn should be 1500µs, got {}",
            result.mean_sojourn_us
        );
    }

    #[test]
    fn pipeline_overloaded_is_unstable() {
        let params = PipelineCostParams {
            stages: vec![StageStats {
                name: "test",
                mean_us: 20_000.0,
                var_us2: 0.0,
            }],
            arrival_rate: 1.0 / 16667.0, // 60fps
            frame_budget_us: 16667.0,
        };
        let result = params.analyze();
        assert!(!result.stable, "overloaded pipeline should be unstable");
        assert!(result.utilization > 1.0);
    }

    #[test]
    fn pipeline_stage_fractions_sum_to_one() {
        let result = PipelineCostParams::default().analyze();
        let total_fraction: f64 = result.stage_breakdown.iter().map(|s| s.fraction).sum();
        assert!(
            (total_fraction - 1.0).abs() < 1e-10,
            "fractions should sum to 1.0"
        );
    }

    #[test]
    fn pipeline_headroom_positive_when_stable() {
        let result = PipelineCostParams::default().analyze();
        assert!(
            result.headroom_us > 0.0,
            "stable pipeline should have positive headroom"
        );
    }

    #[test]
    fn pipeline_jsonl_valid() {
        let result = PipelineCostParams::default().analyze();
        let jsonl = result.to_jsonl();
        let _: serde_json::Value = serde_json::from_str(&jsonl).expect("valid JSON");
    }

    #[test]
    fn pipeline_display() {
        let result = PipelineCostParams::default().analyze();
        let display = format!("{result}");
        assert!(display.contains("Pipeline Scheduling Model"));
        assert!(display.contains("Utilization"));
    }

    // ═══ Patch batching model ══════════════════════════════════════════

    #[test]
    fn batch_optimal_between_1_and_n() {
        let params = BatchCostParams::default();
        let k_star = params.optimal_batch_size();
        assert!(k_star >= 1);
        assert!(k_star <= params.total_patches);
    }

    #[test]
    fn batch_optimal_is_local_minimum() {
        let params = BatchCostParams::default();
        let k_star = params.optimal_batch_size();
        let cost_star = params.total_cost(k_star);

        if k_star > 1 {
            let cost_below = params.total_cost(k_star - 1);
            assert!(
                cost_star <= cost_below + 0.01,
                "cost at k*={k_star} ({cost_star}) should be <= cost at k*-1 ({cost_below})"
            );
        }
        if k_star < params.total_patches {
            let cost_above = params.total_cost(k_star + 1);
            assert!(
                cost_star <= cost_above + 0.01,
                "cost at k*={k_star} ({cost_star}) should be <= cost at k*+1 ({cost_above})"
            );
        }
    }

    #[test]
    fn batch_no_overhead_means_immediate() {
        let params = BatchCostParams {
            c_overhead_us: 0.0,
            ..Default::default()
        };
        assert_eq!(params.optimal_batch_size(), 1);
    }

    #[test]
    fn batch_no_latency_means_single_batch() {
        let params = BatchCostParams {
            c_latency_us: 0.0,
            ..Default::default()
        };
        assert_eq!(params.optimal_batch_size(), params.total_patches);
    }

    #[test]
    fn batch_zero_patches() {
        let params = BatchCostParams {
            total_patches: 0,
            ..Default::default()
        };
        let result = params.optimize();
        assert_eq!(result.optimal_batch_size, 1);
        assert!(result.optimal_cost_us.abs() < 1e-10);
    }

    #[test]
    fn batch_optimize_improvement() {
        let result = BatchCostParams::default().optimize();
        assert!(
            result.improvement_ratio >= 1.0,
            "optimal should be at least as good as immediate"
        );
    }

    #[test]
    fn batch_optimize_jsonl_valid() {
        let result = BatchCostParams::default().optimize();
        let jsonl = result.to_jsonl();
        let _: serde_json::Value = serde_json::from_str(&jsonl).expect("valid JSON");
    }

    #[test]
    fn batch_display() {
        let result = BatchCostParams::default().optimize();
        let display = format!("{result}");
        assert!(display.contains("Patch Batching Model"));
        assert!(display.contains("Optimal k"));
    }

    #[test]
    fn batch_cost_formula_manual_check() {
        // n=100, k=10, overhead=20, per_patch=0.05, latency=0.5
        // batches = ceil(100/10) = 10
        // overhead = 10 × 20 = 200
        // processing = 100 × 0.05 = 5
        // latency = (10-1) × 0.5 = 4.5
        // total = 209.5
        let params = BatchCostParams {
            c_overhead_us: 20.0,
            c_per_patch_us: 0.05,
            c_latency_us: 0.5,
            total_patches: 100,
        };
        let cost = params.total_cost(10);
        assert!(
            (cost - 209.5).abs() < 0.01,
            "manual check: expected 209.5, got {cost}"
        );
    }

    // ═══ Sensitivity analysis ══════════════════════════════════════════

    #[test]
    fn cache_sensitivity_zipf_monotone() {
        let params = CacheCostParams::default();
        let points = cache_sensitivity_zipf(&params, 1.0, 3.0, 10);
        assert_eq!(points.len(), 10);
        // Higher alpha → smaller optimal budget.
        for i in 1..points.len() {
            assert!(
                points[i].optimal_value <= points[i - 1].optimal_value + 1.0,
                "optimal budget should decrease with alpha"
            );
        }
    }

    #[test]
    fn batch_sensitivity_patches_grows() {
        let params = BatchCostParams::default();
        let points = batch_sensitivity_patches(&params, 10, 1000, 10);
        assert_eq!(points.len(), 10);
        // Overall trend: more patches → larger optimal batch size (sqrt scaling).
        // Due to ceiling effects, individual steps may be non-monotone,
        // so we compare first vs last.
        assert!(
            points.last().unwrap().optimal_value > points.first().unwrap().optimal_value,
            "optimal batch size should be larger for more patches (overall trend)"
        );
    }

    // ═══ Determinism ═══════════════════════════════════════════════════

    #[test]
    fn all_models_deterministic() {
        let cache1 = CacheCostParams::default().optimize();
        let cache2 = CacheCostParams::default().optimize();
        assert!(
            (cache1.optimal_budget_bytes - cache2.optimal_budget_bytes).abs() < 1e-10,
            "cache model should be deterministic"
        );

        let pipe1 = PipelineCostParams::default().analyze();
        let pipe2 = PipelineCostParams::default().analyze();
        assert!(
            (pipe1.mean_sojourn_us - pipe2.mean_sojourn_us).abs() < 1e-10,
            "pipeline model should be deterministic"
        );

        let batch1 = BatchCostParams::default().optimize();
        let batch2 = BatchCostParams::default().optimize();
        assert_eq!(
            batch1.optimal_batch_size, batch2.optimal_batch_size,
            "batch model should be deterministic"
        );
    }

    // ═══ Edge cases ════════════════════════════════════════════════════

    #[test]
    fn cache_degenerate_params() {
        // Zero working set.
        let params = CacheCostParams {
            working_set_n: 0.0,
            ..Default::default()
        };
        let b = params.optimal_budget();
        assert!(b.is_finite());

        // Zero miss cost → optimal = minimum budget.
        let params2 = CacheCostParams {
            c_miss_us: 0.0,
            ..Default::default()
        };
        let b2 = params2.optimal_budget();
        // With no miss cost, any budget is equally good for miss component,
        // but memory cost drives it to minimum.
        assert!(b2.is_finite());
    }

    #[test]
    fn pipeline_empty_stages() {
        let params = PipelineCostParams {
            stages: vec![],
            ..Default::default()
        };
        let result = params.analyze();
        assert!(result.total_mean_us.abs() < 1e-10);
        assert!(result.stable);
    }

    #[test]
    fn pipeline_zero_arrival() {
        let params = PipelineCostParams {
            arrival_rate: 0.0,
            ..Default::default()
        };
        let result = params.analyze();
        assert!(result.stable);
        // With zero arrivals, sojourn = service time (no queueing delay).
        assert!((result.mean_sojourn_us - result.total_mean_us).abs() < 1e-6);
    }

    #[test]
    fn sensitivity_point_debug() {
        let pt = SensitivityPoint {
            param_value: 1.5,
            optimal_value: 50_000.0,
            optimal_cost: 123.456,
        };
        let dbg = format!("{pt:?}");
        assert!(dbg.contains("SensitivityPoint"));
    }
}
