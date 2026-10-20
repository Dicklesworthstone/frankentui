# Conformal frame-time risk gating: `conformal_off` vs `conformal_on` (2026-09)

Benchmark:
`cargo bench -p ftui-runtime --bench render_frame_bench`
(Criterion 0.8.2, release/bench profile on remote x86_64 worker `vmi1227854`, paired in the same invocation; 300 pre-warmed frames so the 256-sample sliding window is full and steady-state calibrated quantile computation runs on every iteration).

Workload:
Full `Program::step_headless_frame()` on an 80×24 grid mutating ~5% of cells (96 cells across 4 rows) per frame, including:
- `RenderBudget::next_frame()` (PID + e-process budget controller)
- `FrameGuardrails::check_frame()`
- `ConformalPredictor::predict()` + `ConformalPredictor::observe()` (when enabled)
- `Model::view()` + `BufferDiff` + `TerminalPresenter` emission
- `LoadGovernorState::observe()` + `BudgetDecisionSnapshot` update

## Paired Same-Invocation Results (`conformal_off` vs `conformal_on`)

| Variant | Criterion ID | Mean [95% CI] | p50 | p95 | p99 | Delta vs `conformal_off` (p50) | Share of 16 ms Frame Budget |
|---|---|---|---|---|---|---|---|
| `conformal_off` | `runtime/render_frame_headless_80x24/conformal_off` | 34.43 µs [33.17, 35.93 µs] | 33.10 µs (33,103 ns) | 42.68 µs (42,680 ns) | 49.37 µs (49,372 ns) | — | 0.207% |
| `conformal_on` (default) | `runtime/render_frame_headless_80x24/conformal_on` | 38.02 µs [36.74, 39.49 µs] | 37.56 µs (37,560 ns) | 53.96 µs (53,963 ns) | 60.48 µs (60,476 ns) | +4.46 µs (+4,457 ns) | 0.235% (+0.028% of 16 ms) |

## Optimization Applied (`conformal_predictor.rs`)

Prior to this benchmark pass, `ConformalPredictor::quantile_for` allocated a `Vec<f64>` on the heap every frame (even during warm-up) and `conformal_quantile` called `slice::sort_by(f64::total_cmp)` (merge sort with a temporary heap allocation). In the same benchmark harness:
- Before optimization (`sort_by` + heap `Vec<f64>`): `conformal_on` p50 = `42.26 µs` (`42,258 ns`), p99 = `91.57 µs` (`91,574 ns`).
- After optimization (256-element stack buffer on the exact-bucket hot path + $O(N)$ `select_nth_unstable_by` in `conformal_quantile`): `conformal_on` p50 = `37.56 µs` (`37,560 ns`), p99 = `60.48 µs` (`60,476 ns`) — an **11.0% reduction** in end-to-end headless frame time and zero heap allocations per frame on the default `window_size = 256`.

## Baseline Entry (`tests/baseline.json`)

Added `runtime_render_frame_headless_80x24` bound to `runtime/render_frame_headless_80x24/conformal_on` (`bench_file = "render_frame_bench"`, `crate = "ftui-runtime"`, `slo_metric = "render_frame_p99_us"`).

## E2E Telemetry Verification (`scripts/demo_showcase_e2e.sh` Step 18)

Step 18 (`Explainability cockpit evidence JSONL`) validates `budget_decision` rows in `explainability_evidence.jsonl`:
- `conformal_fallback_distribution`: `total=22 warmup=20 calibrated=2 level0=1,level1=0,level2=1,level3=20` (first 20 frames in warm-up at `fallback_level = 3`, followed by calibrated predictions at `fallback_level = 2` on bucket change and `fallback_level = 0` on exact bucket).
- `conformal_warmup_harmless`: during all warm-up rows (`conformal_status != "calibrated"`), conformal risk gating never triggers a `degrade` decision when `decision_controller != "degrade"`.

