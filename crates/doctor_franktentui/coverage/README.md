# doctor_franktentui Coverage Gate

This directory contains the reproducible coverage policy for `doctor_franktentui`.

## Artifacts

- `thresholds.toml` — required minimum coverage percentages.
- `baseline_summary.json` — current baseline snapshot from `cargo llvm-cov` (`--branch --summary-only --json`).

## Local Command

From repo root:

```bash
./scripts/doctor_franktentui_coverage.sh
```

Optional output directory override:

```bash
./scripts/doctor_franktentui_coverage.sh /tmp/doctor_franktentui_coverage_gate
```

The script writes:

- `coverage_summary.json` (machine-readable source-of-truth)
- `coverage_gate_report.json` (threshold evaluation details)
- `coverage_gate_report.txt` (human-readable report)

and exits non-zero if any configured threshold fails.
