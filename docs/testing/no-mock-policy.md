# No-Mock Unit Testing Policy

Bead: bd-2nu8.2

## Policy Statement

FrankenTUI uses **real components** for testing wherever possible. Mocks, fakes, and stubs that hide real behavior are discouraged. Tests should exercise actual code paths with real data structures.

## Allowed Test Patterns

### 1. Output Captures (Write sinks)

`Vec<u8>` or wrapper structs that implement `io::Write` and capture output for assertion are **allowed**. These do not replace real behavior -- they capture it.

**Example:** `MockWriter` in `ftui-core/src/inline_mode.rs:373` captures ANSI escape sequences emitted by inline mode functions. The functions under test run real logic; only the output destination is substituted.

**Rename recommendation:** Rename `MockWriter` to `CaptureWriter` to clarify intent.

### 2. Pure Data Builders

Helper functions that construct preconfigured widgets or state for test convenience are **allowed**. These are factory functions, not behavior substitutions.

**Example:** `panel_stub()` in `ftui-widgets/src/panel.rs:403` creates a `Panel` with standard borders and title for snapshot tests.

**Rename recommendation:** Rename `panel_stub()` to `test_panel()` to avoid confusing vocabulary.

### 3. Minimal Trait Implementations (for wrapper testing)

When testing a wrapper/decorator, a minimal trait implementation of the wrapped type is **allowed**, provided:
- The wrapper logic (not the inner type) is the test target.
- The inner type does not do I/O or have side effects.

**Example:** `StubWidget` in `ftui-widgets/src/debug_overlay.rs:444` implements `StatefulWidget` with a no-op render to test the `DebugOverlay` wrapper's recording behavior.

### 4. Test Subscription Helpers

Subscription implementations that send predetermined messages are **allowed** for testing the subscription infrastructure itself (lifecycle management, message delivery, stop signals).

**Example:** `ChannelSubscription` in `ftui-runtime/src/subscription.rs` (tests) forwards messages from a real `mpsc::Receiver` while honoring `StopSignal`. It exercises the actual subscription lifecycle and event flow.

### 5. Headless Rendering

Using `Buffer`, `Frame`, and `GraphemePool` directly without a terminal is **allowed and encouraged**. The rendering kernel is designed for headless use. The `ftui-render/src/headless.rs` module and `ProgramSimulator` in `ftui-runtime/src/simulator.rs` exist for this purpose.

## Disallowed Patterns

### 1. Mocking Terminal I/O to Avoid PTY Tests

Do NOT create mock terminal sessions that skip raw mode, alt-screen, or cleanup. Terminal lifecycle correctness **must** be tested via PTY (see `ftui-pty` crate and E2E test suite).

### 2. Mocking Event Sources to Avoid Input Testing

Do NOT create fake event streams that bypass `InputParser`. Input parsing has property tests and fuzz targets. New input features need real CSI/escape sequences fed through the parser.

### 3. Mocking the Render Pipeline

Do NOT create fake buffers, fake diffs, or fake presenters. The render kernel is designed for deterministic testing. Use real `Buffer` + `BufferDiff` + `Presenter` with `Vec<u8>` output capture.

### 4. Mocking External Crate Types

Do NOT create trait objects or generics solely to swap real types for test doubles. If a function takes `impl Write`, passing `Vec<u8>` is fine (output capture). Creating `MockTerminalCapabilities` or `MockEventCoalescer` is not.

## Current Inventory

### Mock/Fake/Stub Usage Found

| Location | Name | Type | Status |
|----------|------|------|--------|
| `ftui-runtime/src/subscription.rs` (tests) | `ChannelSubscription` | Event stream fixture | **Allowed** |
| `ftui-core/src/inline_mode.rs:373` | `MockWriter` | Output capture | **Allowed** (rename to `CaptureWriter`) |
| `ftui-widgets/src/debug_overlay.rs:444` | `StubWidget` | Minimal trait impl | **Allowed** (acceptable as-is) |
| `ftui-widgets/src/panel.rs:403` | `panel_stub()` | Data builder | **Allowed** (rename to `test_panel()`) |
| `ftui-runtime/src/log_sink.rs:87` | `create_writer()` | Output capture (Vec) | **Allowed** (no rename needed) |

### Additional Test Helpers (not doubles, fully allowed)

| Location | Name | Type |
|----------|------|------|
| `ftui-runtime/src/render_thread.rs:229` | `TestWriter` | Output capture for render thread tests |
| `ftui-widgets/tests/frame_integration.rs:33` | `BufferWidget` | Concrete Widget impl for buffer testing |
| `ftui-widgets/tests/frame_integration.rs:44` | `HitWidget` | Concrete Widget impl for hit grid testing |
| `ftui-widgets/tests/frame_integration.rs:58` | `CursorWidget` | Concrete Widget impl for cursor testing |
| `ftui-widgets/tests/frame_integration.rs:70` | `DegradationWidget` | Concrete Widget impl for degradation testing |
| `ftui-widgets/tests/tracing_tests.rs:57` | `SpanCapture` | Real tracing Layer for span assertion |
| `ftui-widgets/tests/tracing_tests.rs:103` | `FieldVisitor` | Real tracing field::Visit impl |
| `ftui-render/src/headless.rs` | `HeadlessTerm` | Real terminal model for output verification |
| `ftui-runtime/src/simulator.rs` | `ProgramSimulator` | Real headless runtime for model testing |

### False Positives (not test doubles)

| Location | Pattern | Reason |
|----------|---------|--------|
| `ftui-render/src/grapheme_pool.rs:395` | `fake_id` | Variable name for invalid-ID test |
| `ftui-render/src/sanitize.rs:1020-1035` | "fake prompt" | Adversarial input test scenarios |
| `ftui-text/tests/unicode_width_corpus.rs:759` | `_dummy` | Proptest parameter name |
| `ftui-widgets/src/table.rs:271` | "dummy rect" | Code comment |

### Summary

- **Total mock/fake/stub occurrences found:** 5 distinct patterns
- **Violations of no-mock policy:** 0
- **Recommended renames:** 2 (cosmetic, for vocabulary clarity)
- **Items to remove or replace:** 0

## Action Items

1. **Rename `MockWriter`** to `CaptureWriter` in `ftui-core/src/inline_mode.rs` (test-only, no API impact).
2. **Rename `panel_stub()`** to `test_panel()` in `ftui-widgets/src/panel.rs` (test-only, no API impact).
3. **No mock removals needed.** All current test doubles are legitimate patterns under this policy.

## When to Use PTY vs Headless

| Testing goal | Approach |
|-------------|----------|
| Widget layout/render correctness | Headless (Frame + Buffer + snapshots) |
| ANSI output correctness | Output capture (Vec<u8> + Presenter) |
| Terminal lifecycle (raw mode, cleanup) | PTY (ftui-pty or E2E scripts) |
| Input parsing | Direct InputParser + byte sequences |
| Runtime update/view loop | ProgramSimulator (headless) |
| Full integration (user-visible behavior) | E2E PTY scripts |

## Timing Assertions

Unit tests must not assert wall-clock throughput on shared runners. Use [`LabClock`](../../crates/ftui-core/src/cx.rs) for deterministic timer ordering and tick counts, Criterion benchmarks with `tests/baseline.json` rows (enforced by `scripts/perf_regression_gate.sh`) for throughput/latency SLOs, and `ftui_core::cx::test_time_scale()` (`FTUI_TEST_TIME_SCALE`, default `1`, CI/DSR `4`) to scale residual sleep-based liveness or hang-guard bounds.

