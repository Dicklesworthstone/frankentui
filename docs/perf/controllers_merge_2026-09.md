# Controller & Teardown Consolidation (`epic(G13)`): `runtime_render_frame_headless_80x24` Before/After (2026-09)

## Overview

`epic(G13)` (`bd-g00-root-epic-ewths.17`) consolidated the six duplicate runtime/render control and teardown paths onto single production owners while keeping quarantined experimental modules behind `#[cfg(feature = "experimental")]` (per Refinement 4 and Rule 1):

1. **E-Process with Predictable Gaussian GRAPA (`crates/ftui-render/src/budget.rs`)**:
   - `EProcessConfig` carries `adaptive_lambda: bool = true`, `grapa_eta: f64 = 0.1`, `lambda_min: f64 = 0.05`, and `lambda_max: f64 = 2.0`.
   - `EProcessState` computes $E_t = E_{t-1}\exp(\lambda_{t-1} r_t - \lambda_{t-1}^2 / 2)$ from the $\mathcal{F}_{t-1}$-measurable betting fraction $\lambda_{t-1}$ before updating $\lambda_t = \mathrm{clamp}(\lambda_{t-1} + \eta(r_t - \lambda_{t-1}), \lambda_{\min}, \lambda_{\max})$ after warmup (retaining $\lambda_t \ge \lambda_0$ while draining post-burst overload wealth $E_t > 1$ on recovery frames $r_t < 0$).
   - `budget_decision` evidence JSONL emits `"eprocess_lambda"` alongside `"recovery_streak"`, `"total_degrades"`, and `"total_recoveries"`.
2. **Degradation Ladder & Essential-Widget Filtering (`crates/ftui-render/src/budget.rs`, `crates/ftui-widgets/src/{sparkline,scrollbar}.rs`)**:
   - `DegradationLevel::allows_widget(essential: bool)` gates decorative widgets at `EssentialOnly` and all widgets at `SkipFrame`; `Sparkline` and `Scrollbar` short-circuit under `EssentialOnly`.
3. **Bayesian Diff Regime Classification & Transition Telemetry (`crates/ftui-render/src/diff_strategy.rs`, `crates/ftui-runtime/src/terminal_writer.rs`)**:
   - `DiffRegime::classify` classifies `StableFrame`, `DirtyRows`, `FullDiff`, `FullRedraw`, and `Transitional`, emitting `"regime"` on `diff_decision` and `"diff_regime_transition"` once per regime change.
4. **Allocation Leak Detection on Live Frame Guardrails (`crates/ftui-render/src/alloc_budget.rs`, `crates/ftui-render/src/frame_guardrails.rs`)**:
   - `AllocLeakDetector` (CUSUM + e-process) runs inside `FrameGuardrails::check_frame()`, resets after soft memory trims, and emits `"leak_e_value"`, `"leak_cusum_upper"`, `"leak_mean_bytes"`, and `"leak_alert"` in `guardrail_snapshot` JSONL rows.
5. **Canonical Session Teardown Plan (`crates/ftui-core/src/session_teardown.rs`, `terminal_session.rs`, `crates/ftui-tty/src/lib.rs`)**:
   - Both `ftui-core::terminal_session::TerminalSession` (crossterm) and `ftui-tty::NativeTerminalSession` delegate teardown byte emission to `SessionTeardownPlan`, producing byte-identical 127-byte cleanup sequences (`[DECSC, DECSTBM-reset, DECRC, SGR0, kitty-pop, focus-off, paste-off, mouse-off, cursor-show, alt-leave]`) with mux suppression of kitty keyboard and focus reporting.
6. **BackendPresenter Seam (`crates/ftui-runtime/src/terminal_presenter.rs`, `program.rs`, `crates/ftui-web/src/lib.rs`)**:
   - `Program<M, E, P: BackendPresenter>` presents frames and logs through `BackendPresenter`, implemented by `TerminalPresenter<W>` (over `TerminalWriter<W>`) and `WebPresenter`.

## Benchmark: `runtime_render_frame_headless_80x24`

Command:
```bash
rch exec --worker vmi1149989 -- cargo bench -j 2 -p ftui-runtime --bench render_frame_bench -- --sample-size 40 --warm-up-time 1 --measurement-time 2
```

Workload:
Full `Program::step_headless_frame()` at 80×24 mutating ~5% of cells (96 cells across 4 rows) per frame after 300 warmup frames, exercising:
- `RenderBudget::next_frame()` (`BudgetController` PID + predictable Gaussian GRAPA `EProcessState`)
- `FrameGuardrails::check_frame()` (including `AllocLeakDetector::observe()`)
- `ConformalPredictor::predict()` + `ConformalPredictor::observe()` (when enabled)
- `Model::view()` + `BufferDiff` + `TerminalPresenter` emission
- `LoadGovernorState::observe()` + `BudgetDecisionSnapshot` update

### Before vs. After Results

| Benchmark ID | Pre-G13 Baseline (`docs/perf/conformal_default_2026-09.md`) | Post-G13 (`vmi1149989`, 40 samples, 64k–67k iters) | Relative Change | 10% Regression Gate |
|---|---|---|---|---|
| `runtime/render_frame_headless_80x24/conformal_off` | 34.43 µs [33.17, 35.93 µs] (p50 33.10 µs) | 30.29 µs [28.85, 32.08 µs] | -12.0% | PASS (≤ +10%) |
| `runtime/render_frame_headless_80x24/conformal_on` (default) | 38.02 µs [36.74, 39.49 µs] (p50 37.56 µs) | 32.06 µs [30.20, 34.24 µs] | -15.7% | PASS (≤ +10%) |

The GRAPA betting-fraction update (`1` branch + `3` scalar `f64` operations per frame) and `AllocLeakDetector::observe()` add negligible per-frame overhead, keeping `runtime/render_frame_headless_80x24/conformal_on` at `32.06 µs` (~0.20% of a 16 ms frame budget) and well within the 10% regression threshold.
