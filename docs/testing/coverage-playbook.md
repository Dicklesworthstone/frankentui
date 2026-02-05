# Coverage Playbook (llvm-cov)

This playbook explains how to run coverage locally, interpret results, and
update the coverage docs so CI stays aligned with the coverage matrix.

## Quick Start

```bash
# Full workspace coverage (JSON summary)
cargo llvm-cov --workspace --all-targets --all-features --summary-only --json \
  --output-path /tmp/ftui_coverage.json

# HTML report (open target/llvm-cov/html/index.html)
cargo llvm-cov --workspace --all-targets --all-features --html

# LCOV output (CI-compatible)
cargo llvm-cov --workspace --lcov --output-path lcov.info
```

If `cargo llvm-cov` is missing:

```bash
cargo install cargo-llvm-cov
```

## What to Update

After running coverage, update these docs:

- `docs/testing/coverage-report.md`
  - Update the run date.
  - Update the summary command and overall numbers.
  - Note any new gaps (especially `ftui-runtime/src/program.rs`).
- `docs/testing/coverage-matrix.md`
  - Update "Last Measured" date and table numbers.
  - Confirm targets still match expectations.
- `docs/testing/coverage-gap-report.md`
  - Regenerate or edit if major gaps moved.

## Interpreting Results

1. **Check crate targets first** (see `coverage-matrix.md`).
2. **Confirm critical files**:
   - `ftui-render` core modules (buffer/diff/presenter)
   - `ftui-core` input/session
   - `ftui-runtime/src/program.rs` (known low coverage)
3. **Look at uncovered lines** in HTML:
   - Open `target/llvm-cov/html/index.html`.
   - Drill down by crate/module.

## CI Alignment

CI uses the LCOV output for Codecov **and** a per-crate threshold gate. The gate
parses `lcov.info`, compares each crate to `coverage-matrix.md`, and fails with
crate + delta guidance. Keep local results consistent with CI by using:

```bash
cargo llvm-cov --workspace --lcov --output-path lcov.info
```

## CI Gate Tightening Checklist

When tightening coverage gates (bd-1mzp6):

- Establish a **fresh baseline** with `cargo llvm-cov --workspace --lcov`.
- Update `docs/testing/coverage-matrix.md` with the new thresholds and dates.
- Prefer incremental bumps on critical crates first (typically `ftui-core`,
  `ftui-render`, then `ftui-runtime`).
- Document rationale for any threshold increase in `coverage-report.md`.
- Verify CI fails on a deliberate local regression (remove a small test and
  confirm the gate trips).

## No-Mock Policy

Coverage must be achieved with real components. See:

- `docs/testing/no-mock-policy.md`

## Common Issues

- **Long runtimes**: limit to a crate during iteration:
  ```bash
  cargo llvm-cov -p ftui-render --all-targets --all-features --html
  ```
- **Feature-gated code**: always use `--all-features` for the full view.
- **Flaky tests**: fix the underlying issue; do not suppress coverage failures.
