# Baseline - profile_sweep - 2026-06-09 - 824e3d4d

| Metric | Value | Notes |
|--------|------:|-------|
| frames | 18,000 | 45 screens x 2 sizes x 200 cycles |
| elapsed | 5.569s | from `profile_sweep_baseline_cycles200.json` |
| throughput | 3,231.9 renders/sec | in-memory presenter sink |
| frame p50 | 225us | warm-cache run |
| frame p95 | 701us | primary latency metric |
| frame p99 | 1,479us | tail metric |
| frame max | 23,501us | outlier-sensitive |
| allocations/frame p50 | 259 | `stats_alloc` in profile_sweep |
| allocations/frame p95 | 966 | allocation pressure target |
| allocations/frame p99 | 6,191 | allocation tail |
| allocated bytes/frame p95 | 536,132 | allocation volume target |
| presenter p99 | 104us | presenter is not the dominant tail |
| peak RSS | 39,524 KiB | `/usr/bin/time -v` |
| syscalls | 218 | `strace_profile_sweep_cycles50.txt` |
| syscall time | 4.732ms | I/O/syscalls rejected as primary bottleneck |

## Run command

```bash
/usr/bin/time -v /data/tmp/cargo-target/release-perf/profile_sweep --cycles 200 --render-mode pipeline --arena-mode off --json
```

## Hyperfine variance

`hyperfine --warmup 3 --runs 20` on 100-cycle runs:

- mean: `3.091s`
- stddev: `0.270s`
- median: `3.030s`
- min: `2.699s`
- max: `3.619s`

Variance is high enough to treat small wall-clock wins cautiously. Per-frame JSON allocation and latency counters are more actionable for early passes.

## Environment Caveats

- CPU governor: `powersave`
- SMT: active
- `perf_event_paranoid=4`, so `samply`/`perf` CPU sampling was unavailable without global kernel tuning.
- No kernel/governor tuning was applied because that requires explicit approval.
- A broad `cargo bench --workspace --no-run` was interrupted after proving too expensive and noisy for profiling. Focused Criterion artifacts may be partial where noted.
