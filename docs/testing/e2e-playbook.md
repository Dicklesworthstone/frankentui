# E2E Playbook

This playbook describes how to run the PTY-backed E2E suite, interpret logs,
and add or debug scenarios.

## Entry Points

```bash
# Main entry point (wraps tests/e2e/scripts/run_all.sh)
./scripts/e2e_test.sh

# Quick run (inline + cleanup only)
./scripts/e2e_test.sh --quick

# Direct invocation
tests/e2e/scripts/run_all.sh
```

## Suite Structure

- `tests/e2e/scripts/` — individual suites (inline, cleanup, input, resize, etc.)
- `tests/e2e/lib/pty.sh` — PTY runner (python-based capture)
- `tests/e2e/lib/logging.sh` — structured logs + JSON results
- `tests/e2e/fixtures/` — input fixtures for unicode/paste/etc.

## Script Inventory

| Script | Fixtures | JSONL | Coverage Notes |
| --- | --- | --- | --- |
| `scripts/e2e_test.sh` | yes | yes | Main PTY suite runner + budget/span/tile scenarios; JSONL run_end + schema v1 validation |
| `scripts/demo_showcase_e2e.sh` | yes | yes | Demo showcase end-to-end steps; VFX/layout inspector JSONL artifacts with schema v1 fields |
| `scripts/e2e_demo_tour.sh` | yes | yes | Guided tour run + report validation |
| `scripts/demo_text_effects_e2e.sh` | yes | yes | Text effects demo checks |
| `scripts/widget_api_e2e.sh` | pending | pending | Needs fixture + JSONL wiring |
| `scripts/command_palette_e2e.sh` | pending | pending | Needs fixture + JSONL wiring |
| `scripts/hover_stabilizer_e2e.sh` | pending | pending | Needs fixture + JSONL wiring |
| `scripts/a11y_transitions_e2e.sh` | pending | pending | Needs fixture + JSONL wiring |

Inventory scope: this table tracks top-level scripts in `scripts/`. Nested suites under
`tests/e2e/scripts/` are covered by `scripts/e2e_test.sh` and the gap analysis doc.

## Prerequisites Checklist

- Deterministic fixtures live in `tests/e2e/lib/common.sh` and `tests/e2e/lib/logging.sh`.
- JSONL schema is `tests/e2e/lib/e2e_jsonl_schema.json` with validator `tests/e2e/lib/validate_jsonl.py`.
- CI runs strict JSONL validation when `CI=1` (via `jsonl_run_end` → `jsonl_validate_current`).
- Local validation: set `E2E_JSONL_VALIDATE=1` or `E2E_JSONL_VALIDATE_MODE=strict` when running scripts.
- Example JSONL lines: `tests/e2e/lib/e2e_jsonl_examples.jsonl`.

## Environment Controls

Common variables:

- `E2E_LOG_DIR` — log directory (default: `/tmp/ftui_e2e_<timestamp>`)
- `E2E_RESULTS_DIR` — JSON results directory
- `E2E_HARNESS_BIN` — path to `ftui-harness` binary (skip build)
- `LOG_LEVEL=DEBUG` — verbose suite logging
- `E2E_ONLY_CASE` — run a single case in scripts that support filtering
- `E2E_DETERMINISTIC=1` — emit deterministic timestamps/run IDs in JSONL logs
- `E2E_SEED=<n>` — fixed seed for deterministic runs (propagated to harnesses)
- `E2E_AUTO_SEED=1` — automatically propagate deterministic seed/env defaults when sourcing `tests/e2e/lib/logging.sh`
- `FTUI_DEMO_DETERMINISTIC=1` — deterministic mode for demo showcase screens
- `FTUI_DEMO_SEED=<n>` — global demo seed override (used by tour/VFX + screens)
- `FTUI_TEST_DETERMINISTIC=1` — deterministic mode for Rust E2E/integration tests
- `FTUI_TEST_SEED=<n>` — seed override for Rust E2E/integration fixtures
- `FTUI_TEST_TIME_STEP_MS=<n>` — deterministic clock step for fixture timestamps

Script defaults (unless overridden):

- `scripts/e2e_test.sh` and `scripts/demo_showcase_e2e.sh` set
  `E2E_DETERMINISTIC=1`, `E2E_SEED=0`, and `E2E_TIME_STEP_MS=100` to keep
  JSONL logs and frame hashes stable across runs.

PTY runner controls (from `tests/e2e/lib/pty.sh`):

- `PTY_COLS` / `PTY_ROWS` — initial terminal size
- `PTY_RESIZE_COLS` / `PTY_RESIZE_ROWS` / `PTY_RESIZE_DELAY_MS` — scheduled resize
- `PTY_SEND` / `PTY_SEND_FILE` — input payloads
- `PTY_TIMEOUT` / `PTY_DRAIN_TIMEOUT_MS` — timeouts and drain windows

## Capability Profile Matrix

Use `FTUI_TEST_PROFILE` to force a terminal capability profile in tests and
snapshots. When set, snapshot filenames are automatically suffixed with
`__<profile>` so each profile keeps its own baselines.

Examples:

```bash
# Run the demo showcase snapshots as a dumb terminal
FTUI_TEST_PROFILE=dumb cargo test -p ftui-demo-showcase

# Run harness widget snapshots as tmux
FTUI_TEST_PROFILE=tmux cargo test -p ftui-harness widget_snapshots
```

Cross-profile comparison mode (for tests that use `profile_matrix_text`):

```bash
# Report diffs without failing
FTUI_TEST_PROFILE_COMPARE=report cargo test -p ftui-harness profile_matrix

# Fail fast on differences
FTUI_TEST_PROFILE_COMPARE=strict cargo test -p ftui-harness profile_matrix
```

### CI Integration Guide

Run the test suite across a profile matrix to catch capability regressions:

```yaml
strategy:
  matrix:
    profile: [modern, xterm-256color, screen, tmux, dumb, windows-console]

steps:
  - run: FTUI_TEST_PROFILE=${{ matrix.profile }} cargo test -p ftui-demo-showcase
```

## Artifacts

Each run produces:

- `00_environment.log` — environment + git state
- `e2e.log` — suite log
- `results/*.json` — per-test JSON entries
- `results/summary.json` — aggregated pass/fail counts
- `*.pty` — raw PTY captures (binary)

## Deterministic Coverage Matrix

The authoritative mode/size matrix and per-script cell mapping live in:
`docs/testing/e2e-gap-analysis.md`.

The per-screen demo coverage matrix and artifact checklist live in:
`docs/testing/e2e-coverage-matrix.md`.

Target cells are alt/inline across 80x24, 120x40, and 200x50. Use
`FTUI_HARNESS_SCREEN_MODE` and `PTY_COLS/PTY_ROWS` to pin a test to a cell.

## JSONL Event Logging

The target JSONL schema for step/input/frame/hash/timing/environment events is
defined in `docs/testing/e2e-summary-schema.md`. Scripts may emit only a subset
today; new E2E work should log events that conform to that schema.

Validation:

```bash
python3 tests/e2e/lib/validate_jsonl.py "$E2E_JSONL_FILE" --schema tests/e2e/lib/e2e_jsonl_schema.json --warn
```

Validator self-test (unit coverage for required/optional fields + malformed lines):

```bash
python3 tests/e2e/lib/validate_jsonl.py --self-test
```

Validation mode (default): warn locally, strict in CI.

Environment controls:

- `E2E_JSONL_VALIDATE_MODE=warn|strict` — force validation mode.
- `E2E_JSONL_SCHEMA_FILE` — override schema file path.
- `E2E_JSONL_VALIDATOR` — override validator script path.
- `E2E_JSONL_SCHEMA_VERSION` — override schema version string in emitted JSONL.

When `E2E_DETERMINISTIC=1` is set, JSONL timestamps and run IDs become stable,
and step logs include `hash_key` in the format `mode-<cols>x<rows>-seed<seed>`.

On failures, the suite prints:

- Hex dumps of the first 512 bytes of PTY output
- Printable tail excerpts (via `strings`)

## Running a Single Suite

```bash
E2E_HARNESS_BIN=target/debug/ftui-harness \
tests/e2e/scripts/test_resize.sh
```

Other useful suites:

- `tests/e2e/scripts/test_mouse_sgr.sh`
- `tests/e2e/scripts/test_focus_events.sh`
- `tests/e2e/scripts/test_paste.sh`
- `tests/e2e/scripts/test_kitty_keyboard.sh`

## Troubleshooting

- **Missing python**: ensure `python3` is available.
- **No PTY output**: increase `PTY_TIMEOUT` or `PTY_DRAIN_TIMEOUT_MS`.
- **Mux interference**: unset `TMUX`, `ZELLIJ`, or `STY` in tests that rely on
  scroll regions or pass-through behavior.
- **Binary missing**: build once or set `E2E_HARNESS_BIN`.

## Adding a New Scenario

1. Create a new script in `tests/e2e/scripts/`.
2. Use `pty_run` from `tests/e2e/lib/pty.sh`.
3. Add the suite to `tests/e2e/scripts/run_all.sh`.
4. Add any fixtures to `tests/e2e/fixtures/`.
5. Document the new scenario in `docs/testing/e2e-gap-analysis.md` if needed.
