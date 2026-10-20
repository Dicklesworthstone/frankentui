# Task Executor Backend Evaluation (`Spawned` vs `EffectQueue` vs `Asupersync`) — 2026-10-19

## Metadata

- **Date**: 2026-10-19
- **Commit**: `7ad06e82` (working tree with `crates/ftui-runtime/benches/effect_backend_bench.rs`)
- **Host**: Linux `x86_64-unknown-linux-gnu`
- **Toolchain**: `nightly-2026-08-31-x86_64-unknown-linux-gnu` (Criterion `0.8.2`)
- **Commands**:
  - `cargo bench -p ftui-runtime --bench effect_backend_bench -- --noplot --warm-up-time 0.5 --measurement-time 1 --sample-size 20`
  - `cargo bench -p ftui-runtime --features asupersync-executor --bench effect_backend_bench -- --noplot --warm-up-time 0.5 --measurement-time 1 --sample-size 20 asupersync`

---

## Preregistered Decision Rule (`bd-g00-root-epic-ewths.30.2`)

`TaskExecutorBackend::EffectQueue` becomes the default for `RuntimeLane::Structured` **only if all three conditions hold**:
1. **(a) Issue #78 Guard (`runtime/pollers_10x10ms`)**: keystroke-echo p99 latency under 10 concurrent 10 ms polling tasks is within `1.2x` of `Spawned` (`echo_p99(EffectQueue) <= 1.2 * echo_p99(Spawned)`), and strictly below `2x` (`< 2.0 * echo_p99(Spawned)`).
2. **(b) Task Burst Throughput (`runtime/task_burst_200`)**: p99 completion latency for a 200-task burst (100 µs simulated work per task) is at least `1.3x` faster than `Spawned`.
3. **(c) Shutdown / Queue Drain SLO (`runtime/effect_queue_drain`)**: drain latency for 50 queued trivial tasks meets the `tests/baseline.json` `runtime_effect_queue_drain` p99 ceiling (`2,000,000 ns` = `2.00 ms`).

Otherwise, `TaskExecutorBackend::Spawned` (`DEFAULT_TASK_BACKEND`) remains the default for `RuntimeLane::Structured` and `RuntimeLane::Legacy`, while `TaskExecutorBackend::EffectQueue` (and its SRPT / Smith's Rule / aging scheduler in `queueing_scheduler.rs`) is documented as opt-in via `EffectQueueConfig::default().with_backend(TaskExecutorBackend::EffectQueue)`.

---

## Criterion & Tail Results

### 1. `runtime/effect_queue_drain/{spawned,effect_queue,asupersync}`

Enqueues 50 trivial `Cmd::task` closures (`Program::enqueue_headless_event`) and measures the time to drain all 50 completions (`Program::drain_headless_tasks`).

| Backend | Criterion ID | Criterion Mean [95% CI] | Sample p50 (µs) | Sample p99 (µs) | Baseline Ceiling (`2.00 ms`) |
|---|---|---|---|---|---|
| `Spawned` (default) | `runtime/effect_queue_drain/spawned` | `94.11 µs` `[72.63 µs, 116.73 µs]` | `55 µs` | `777 µs` | **PASS** (`-95.29%` vs ceiling) |
| `EffectQueue` | `runtime/effect_queue_drain/effect_queue` | `116.81 µs` `[101.08 µs, 138.43 µs]` | `112 µs` | `277 µs` | **PASS** (`-94.16%` vs ceiling) |
| `Asupersync` (`--features asupersync-executor`) | `runtime/effect_queue_drain/asupersync` | `12.53 µs` `[11.98 µs, 13.48 µs]` | `11 µs` | `31 µs` | **PASS** |

### 2. `runtime/task_burst_200/{spawned,effect_queue,asupersync}`

Submits 200 `Cmd::task` closures each performing `100 µs` of CPU spin work in a single `Cmd::batch` and measures total batch drain time.

| Backend | Criterion ID | Criterion Mean [95% CI] | Sample p50 (µs) | Sample p99 (µs) | Ratio vs `Spawned` p99 |
|---|---|---|---|---|---|
| `Spawned` (default) | `runtime/task_burst_200/spawned` | `9.964 ms` `[9.834 ms, 10.239 ms]` | `10,027 µs` | `15,512 µs` | `1.00x` |
| `EffectQueue` | `runtime/task_burst_200/effect_queue` | `21.336 ms` `[21.289 ms, 21.399 ms]` | `21,241 µs` | `22,203 µs` | `1.43x` **slower** (serialized on 1 worker thread: `200 * 100 µs = 20 ms`) |
| `Asupersync` (`--features asupersync-executor`) | `runtime/task_burst_200/asupersync` | `2.814 ms` `[2.680 ms, 3.037 ms]` | `2,439 µs` | `6,578 µs` | `2.36x` faster (pooled blocking threads) |

### 3. `runtime/pollers_10x10ms/{spawned,effect_queue,asupersync}` (Issue #78 Regression Guard)

Enqueues 10 concurrent polling tasks (each sleeping `10 ms`) alongside 1 interactive keystroke-echo task (`BenchMsg::EchoCompleted`), measuring both the batch drain time and the echo task's submit-to-completion latency (`echo_latencies_us`).

| Backend | Criterion ID | Batch Drain Mean [95% CI] | Echo Latency p50 (µs) | Echo Latency p99 (µs) | Echo p99 Ratio vs `Spawned` |
|---|---|---|---|---|---|
| `Spawned` (default) | `runtime/pollers_10x10ms/spawned` | `10.780 ms` `[10.667 ms, 10.884 ms]` | `374 µs` | `1,024 µs` (`1.02 ms`) | `1.00x` |
| `EffectQueue` | `runtime/pollers_10x10ms/effect_queue` | `101.24 ms` `[101.14 ms, 101.43 ms]` | `101,150 µs` | `102,251 µs` (`102.25 ms`) | **`99.85x` slower** (violates `< 1.2x` and `< 2.0x` guard) |
| `Asupersync` (`--features asupersync-executor`) | `runtime/pollers_10x10ms/asupersync` | `10.610 ms` `[10.383 ms, 10.878 ms]` | `187 µs` | `1,553 µs` (`1.55 ms`) | `1.52x` |

---

## Verdict

Default backend: Spawned because `EffectQueue` serializes blocking tasks on a single worker thread (`ftui-effects`), causing a `99.85x` echo p99 latency regression under the Issue #78 `runtime/pollers_10x10ms` workload (`102,251 µs` vs `1,024 µs`, failing condition (a)) and a `1.43x` slowdown on `runtime/task_burst_200` (`22,203 µs` vs `15,512 µs`, failing condition (b)).

- `DEFAULT_TASK_BACKEND` in `crates/ftui-runtime/src/program.rs` remains `TaskExecutorBackend::Spawned` for `RuntimeLane::Legacy` and `RuntimeLane::Structured`.
- `RuntimeLane::Asupersync` selects `TaskExecutorBackend::Asupersync` when compiled with `--features asupersync-executor` (pooling blocking threads to achieve `12.53 µs` drain, `2.814 ms` burst-200, and `1.55 ms` echo p99 under 10 pollers).
- `tests/baseline.json` binds `runtime_effect_queue_drain` to `runtime/effect_queue_drain/spawned` in `effect_backend_bench` (`94.11 µs` central estimate vs `2.00 ms` ceiling).
