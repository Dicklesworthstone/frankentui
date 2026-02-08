# Performance Baselines (bd-1rz0.13)

Scope: **Core Responsive Reflow** baseline measurements and opportunity matrix.

## Status
- **Blocked:** `cargo build -p ftui-harness` fails because `Buffer::content_height()` is referenced in `crates/ftui-runtime/src/program.rs` but not implemented.
- Once build is green, run the baselines below and append results + opportunity matrix.

## Environment (to record when runs succeed)
- Date:
- Host/CPU:
- OS:
- Rust toolchain: (see `rust-toolchain.toml`)
- Binary: `./target/debug/ftui-harness`

## Baseline Commands (PTY + fixed terminal size)

### 80x24
```
script -q -c 'stty rows 24 cols 80; \
  FTUI_HARNESS_EXIT_AFTER_MS=800 \
  FTUI_HARNESS_VIEW=layout-grid \
  ./target/debug/ftui-harness' /dev/null
```

Hyperfine wrapper (after build success):
```
hyperfine --warmup 3 --min-runs 20 --export-json docs/testing/perf-baselines.80x24.hyperfine.json \
  "script -q -c 'stty rows 24 cols 80; FTUI_HARNESS_EXIT_AFTER_MS=800 FTUI_HARNESS_VIEW=layout-grid ./target/debug/ftui-harness' /dev/null"
```

### 120x40
```
script -q -c 'stty rows 40 cols 120; \
  FTUI_HARNESS_EXIT_AFTER_MS=800 \
  FTUI_HARNESS_VIEW=layout-grid \
  ./target/debug/ftui-harness' /dev/null
```

Hyperfine wrapper (after build success):
```
hyperfine --warmup 3 --min-runs 20 --export-json docs/testing/perf-baselines.120x40.hyperfine.json \
  "script -q -c 'stty rows 40 cols 120; FTUI_HARNESS_EXIT_AFTER_MS=800 FTUI_HARNESS_VIEW=layout-grid ./target/debug/ftui-harness' /dev/null"
```

## Results (p50/p95/p99)
- 80x24: p50=22.459ms p95=23.645ms p99=24.105ms (n=131)
- 120x40: p50=22.330ms p95=22.992ms p99=24.351ms (n=132)

## Flamegraph + Allocation Profile
- CPU flamegraph: **failed** due to `perf_event_paranoid=4` (no perf access). See JSONL log.
- Allocation profile: **blocked** by same perf permissions; retry when perf access is enabled.

## Opportunity Matrix
| Hotspot | Impact | Confidence | Effort | Score | Evidence |
|---|---:|---:|---:|---:|---|
| TBD | - | - | - | - | - |

## Formal Cost Model Ledger (bd-lff4p.5.6)

This ledger tracks explicit priors/posteriors for cache + scheduler policies.

### Objective Definitions

- Scheduler:
  `priority = (weight / remaining_time) + aging_factor * wait_time`
  and `loss_proxy = 1 / max(priority, w_min)`.
- Glyph cache:
  `cache_loss = miss_rate + 0.25*eviction_rate + 0.5*pressure_ratio`.

### Priors (current defaults)

| Model | Parameter | Prior value | Source |
|---|---|---:|---|
| Scheduler | `aging_factor` | `0.1` | `SchedulerConfig::default()` |
| Scheduler | `starve_boost_ratio` | `1.5` | `SchedulerConfig::default()` |
| Cache | miss weight | `1.0` | `CACHE_LOSS_MISS_WEIGHT` |
| Cache | eviction weight | `0.25` | `CACHE_LOSS_EVICTION_WEIGHT` |
| Cache | pressure weight | `0.5` | `CACHE_LOSS_PRESSURE_WEIGHT` |

### Posterior / Experiment Template

| Date | Candidate policy | Dataset/run id | Before loss | After loss | Determinism checksum status | Decision |
|---|---|---|---:|---:|---|---|
| 2026-02-08 | Baseline instrumentation only | local unit tests | N/A | N/A | unchanged | keep priors |

### Repro Commands

```bash
# Scheduler objective evidence sanity
cargo test -p ftui-runtime evidence_reports_priority_objective_terms

# Cache objective evidence sanity
cargo test -p frankenterm-web objective_tracks_pressure_and_evictions
```
