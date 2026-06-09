# Scaling Law

Measured with `profile_sweep --render-mode pipeline --arena-mode off --json`.

| cycles | frames | elapsed ms | renders/sec | frame p95 us | frame p99 us | alloc p95 | alloc bytes p95 | presenter p95 us |
|-------:|-------:|-----------:|------------:|-------------:|-------------:|----------:|----------------:|-----------------:|
| 1 | 90 | 286.651 | 313.971 | 3,244 | 23,611 | 5,128 | 937,569 | 100 |
| 10 | 900 | 544.941 | 1,651.556 | 928 | 2,228 | 1,298 | 582,234 | 73 |
| 50 | 4,500 | 1,579.070 | 2,849.778 | 705 | 1,641 | 1,083 | 536,132 | 64 |
| 100 | 9,000 | 3,232.408 | 2,784.302 | 841 | 2,117 | 966 | 536,132 | 67 |
| 200 | 18,000 | 6,243.421 | 2,883.035 | 820 | 1,872 | 966 | 536,132 | 67 |

Warm steady-state begins after the first full screen sweep. Use 50+ cycles for before/after comparisons.
