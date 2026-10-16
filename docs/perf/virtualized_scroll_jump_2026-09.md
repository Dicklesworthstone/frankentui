# Virtualized Scroll Positioning & Height-Prediction Scroll Jump (`bd-g00-root-epic-ewths.14.4`)

**Date:** 2026-10-16  
**Benchmark target:** `cargo bench -p ftui-widgets --bench virtualized_bench`  
**Implementation:** `crates/ftui-widgets/src/fenwick.rs`, `crates/ftui-widgets/src/height_predictor.rs`, `crates/ftui-widgets/src/virtualized.rs`

---

## 1. Benchmark Methodology

`crates/ftui-widgets/benches/virtualized_bench.rs` evaluates two properties of `Virtualized<T>` on a deterministic long-tail corpus (80% 1-row items in category 0, 15% 2..=4-row items in category 1, and 5% 5..=30-row items tiered across categories 2..=4):

1. **`virtualized/scroll_to/{fixed,variable_linear,variable_fenwick}/{10000,100000}`**:
   Measures `scroll_to(n / 2)` followed by `offset_of_item(n / 2)` across 10,000 and 100,000 rows, comparing `ItemHeight::Fixed`, legacy linear-scan `ItemHeight::Variable(HeightCache)`, and default `ItemHeight::VariableFenwick(VariableHeightsFenwick)`.
2. **`virtualized/scroll_jump/{fixed_1,mean_baseline,predictor}/100000`**:
   Trains on a 10% random sample (`10,000` rows), calls `scroll_to(50_000)`, and then reveals the remaining `90,000` row heights in random 50-row batches while summing the absolute change in `offset_of_item(50_000)` (`jump_cells`) after each batch.
   - `fixed_1`: `VariableHeightsFenwick` initialized with `default_height = 1`, no predictor.
   - `mean_baseline`: `VariableHeightsFenwick` initialized with `default_height = round(mean(true_heights)) = 2`, no predictor.
   - `predictor`: `VariableHeightsFenwick` paired with `HeightPredictor` (`PredictorConfig::default()`), refreshing category-specific unmeasured row predictions via `scroll_to`.

---

## 2. Results (`scroll_to` + `offset_of_item` Latency)

| Benchmark ID | Rows (`n`) | p50 / Mean | 95% CI | Speedup vs `variable_linear` |
|---|---:|---:|---:|---:|
| `virtualized/scroll_to/fixed/10000` | 10,000 | 4.39 ns | `[4.26 ns, 4.53 ns]` | — |
| `virtualized/scroll_to/variable_linear/10000` | 10,000 | 4.87 µs | `[4.71 µs, 5.04 µs]` | 1.0× |
| `virtualized/scroll_to/variable_fenwick/10000` | 10,000 | **20.67 ns** | `[18.17 ns, 24.37 ns]` | **235.6×** |
| `virtualized/scroll_to/fixed/100000` | 100,000 | 4.57 ns | `[4.45 ns, 4.70 ns]` | — |
| `virtualized/scroll_to/variable_linear/100000` | 100,000 | 52.70 µs | `[51.17 µs, 54.26 µs]` | 1.0× |
| `virtualized/scroll_to/variable_fenwick/100000` | 100,000 | **16.63 ns** | `[16.31 ns, 16.95 ns]` | **3,168.9×** |

`virtualized/scroll_to/variable_fenwick/100000` completes in ~16.6 ns (`O(log n)` Fenwick prefix sum over 100k items), well within the `< 1 µs` (`1,000 ns`) p99 budget recorded in `tests/baseline.json` (`virtualized_scroll_to_100k`).

---

## 3. Results (Cumulative Scroll Jump Under Late Measurements, `n = 100,000`)

| Variant | Rows (`n`) | Measurements | Cumulative `jump_cells` | Jump per 1k Measurements | Reduction vs `fixed_1` | Reduction vs `mean_baseline` |
|---|---:|---:|---:|---:|---:|---:|
| `fixed_1` | 100,000 | 100,000 | 48,250 | 482.50 cells | 0.0% (1.00×) | — |
| `mean_baseline` | 100,000 | 100,000 | 27,896 | 278.96 cells | 42.2% (1.73×) | 0.0% (1.00×) |
| `predictor` | 100,000 | 100,000 | **2,497** | **24.97 cells** | **94.8% (19.32×)** | **91.0% (11.17×)** |

Raw JSON artifact written by the benchmark: `target/criterion/virtualized_scroll_jump.json`.
