# DEFINE - profile_sweep optimization pass

## Scenario
Render every `ftui-demo-showcase` screen at `80x24` and `120x40` through the full view, dirty-buffer diff, and ANSI presenter pipeline using `profile_sweep --render-mode pipeline --arena-mode off`. This is the broadest in-repo whole-system workload for the terminal UI stack.

## Metric
Primary metrics are p50/p95/p99 frame time, renders per second, allocations per frame, allocated bytes per frame, presenter time, peak RSS, and CPU/allocation hotspot attribution. Focused Criterion benches cover render, text, layout, widgets, runtime, and extras surfaces after the whole-system run ranks candidate areas.

## Budget
Initial decision budget: optimize only targets with `Impact * Confidence / Effort >= 2.0`, no p95 regression greater than the same-host 10% variance envelope, and behavior-preserving golden checks after each change.

## Golden output
The `profile_sweep` binary checksum is recorded in `golden_checksums.txt`; optimization passes must also run crate/unit/snapshot tests relevant to the changed code. For render-path changes, snapshot or deterministic JSON profile output must remain behavior-equivalent.

## Scope boundary
This run does not tune global CPU governor, kernel perf settings, turbo, SMT, or filesystem cache state. It does not treat compile time as an application hotspot. It excludes networked workloads and external terminal I/O because `profile_sweep` writes to an in-memory sink.

## Variance envelope
- <=10% drift vs prior same-host run: noise
- >10% drift: investigate
- >20%, or 3 consecutive >10%: escalate

## Stakeholder / requester
Requested by the repo owner to feed at least 10 serial applications of `extreme-software-optimization`, with a fresh profile after pass 5.
