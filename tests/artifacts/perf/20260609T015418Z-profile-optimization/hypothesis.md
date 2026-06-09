# Hypothesis Ledger

- **The workload is I/O-bound**: rejects. `strace_profile_sweep_cycles50.txt` reports 218 syscalls and 4.732ms total syscall time; profile_sweep presents into an in-memory sink.
- **Presenter/diff dominate the tail**: rejects for first-pass optimization. Baseline presenter p99 is 104us while whole-frame p99 is 1,479us; isolated dirty diff is already single-digit to low-double-digit microseconds for representative 200x60 cases.
- **Repeated paragraph/text ownership churn is the largest low-hanging allocation target**: supports. Heaptrack reports 439,134 allocation calls through `Text::into_owned`/`Paragraph::new`, with chrome tab/status paths recurring heavily.
- **Markdown math conversion remains a material repeated allocation/CPU target**: supports. Heaptrack reports 159,611 calls through `unicodeit::replace` despite a function named `cached_latex_to_unicode`, so cache scope/effectiveness needs inspection.
- **Performance HUD is allocation-heavy**: supports. It is the top screen by p95 allocations/frame at 6,191, much higher than the next tiers.
- **FrameArena should be expanded first**: rejects. Fresh arena comparison shows `arena-mode on` slightly slower (`2925ms` vs `2899ms`) with identical allocation-count metrics in this workload.
- **Kernel or CPU tuning is required before code work**: rejects for now. Tuning would improve measurement stability, but low-hanging allocation sites are large enough to act on without changing global host state.

