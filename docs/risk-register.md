# Risk Register

This document tracks the failure modes that destroy trust in a terminal UI kernel. Each risk has explicit mitigations linked to code, tests, and beads.

> **Purpose**: Keep mitigations attached to code. Prevent features from silently reintroducing known risks.

---

## Risk Summary

Summary derived from the rows below; edit rows, then the summary.

| ID | Risk | Severity | Status |
|----|------|----------|--------|
| R1 | Inline mode cursor corruption | Critical | Mitigated |
| R2 | ANSI/OSC injection via untrusted content | Critical | Mitigated |
| R3 | Unicode width bugs | High | Mitigated |
| R4 | Terminal capability mismatches | Medium | Mitigated |
| R5 | Unsafe code creep | Medium | Mitigated |
| R6 | Presenter byte bloat | Low | Mitigated |
| R7 | Interleaved stdout writes | High | Partially mitigated (rows: 4 Implemented, 1 Experimental) |

---

## R1: Inline Mode Cursor Corruption

### What Can Go Wrong

Inline mode overlays UI on scrollback. If cursor position is corrupted:
- UI draws in wrong location
- Scrollback gets overwritten
- Cursor "drifts" over time during sustained operation
- Exit leaves cursor in unexpected position

### Mitigations

| Mitigation | Implementation | Status | Proof |
|------------|----------------|--------|-------|
| PTY integration tests | `ftui-pty` crate (`bd-10i.11.2`) | Implemented | `tests/e2e/scripts/test_cleanup.sh`; `tests/e2e/scripts/test_inline.sh`; `crates/ftui/tests/agent_shell_plain_pty.rs` (`plain_pty_survives_10k_lines_without_scrollback_loss_or_chrome_drift`) |
| Strict cursor policy | DEC ESC7/ESC8 save/restore (ADR-001) | Implemented | `crates/ftui-runtime/tests/e2e_inline_mode_scrollback.rs` (`multiple_render_passes_preserve_scrollback`) |
| Centralized writer API | `TerminalWriter` one-writer (ADR-005) | Implemented | `crates/ftui-runtime/src/terminal_writer.rs` (`present_ui_owns_and_closes_sync_bracket_even_on_early_error`) |
| RAII cleanup guards | `TerminalSession` Drop trait | Implemented | `crates/ftui-core/src/terminal_session.rs` (`terminal_session_panic_cleanup_idempotent`) |
| Hybrid inline strategy | OverlayRedraw baseline + ScrollRegion/Hybrid selection (ADR-001) | Implemented | `crates/ftui-core/src/inline_mode.rs` (`strategy_selection_uses_hybrid_without_sync`); `crates/ftui-runtime/tests/e2e_inline_mode_scrollback.rs` (`overlay_transport_delivers_random_interleavings`) |

### Test Suites

- **PTY tests**: `crates/ftui-pty/` - cursor restored after each frame, cleanup on panic
- **Terminal model tests**: `crates/ftui-render/src/terminal_model.rs` - presenter correctness
- **Sustained output scenario**: Continuous logs + UI redraw + resize events

### Beads

- bd-10i.11.2: Create PTY test framework (ftui-pty)
- bd-10i.1.1: Inline mode cursor stability spike (closed)
- bd-6e9.7: Implement Cursor utilities module

---

## R2: ANSI/OSC Injection via Untrusted Content

### What Can Go Wrong

Untrusted output (LLM streams, user input, tool output) can smuggle control sequences that:
- Manipulate terminal state
- Create fake prompts to deceive users
- Persist changes after app exits
- Enable escape sequence injection attacks

### Mitigations

| Mitigation | Implementation | Status | Proof |
|------------|----------------|--------|-------|
| Sanitize by default | Strip CSI/OSC/DCS/APC/PM/SOS sequences (`write_log`, `LogSink::new`, ADR-006) | Implemented | `crates/ftui-runtime/src/log_sink.rs` (`log_sink_sanitizes_multiple_escapes`); ledger `N06` |
| Semi-trusted SGR-only mode | Bounded `CSI [0-9;:]* m` (<= 64 B) + per-line `\x1b[0m` reset (`write_log_sgr_only`, `LogSink::sgr_only`) | Implemented | `crates/ftui-runtime/src/log_sink.rs` (`sgr_sink_assembles_split_escapes_and_utf8_before_filtering`); ledger `N09` |
| Explicit raw opt-in | `write_log_raw()`, `LogSink::raw()`, `Cmd::log_raw()`, `Text::raw()` require explicit call; emits `ftui.sanitize` debug trace | Implemented | `crates/ftui-harness/tests/pty_terminal_lifecycle.rs` (`pty_log_raw_requires_explicit_opt_in_and_restores_termios`, `raw_log_emits_mode_and_byte_count_trace`); ledger `N07` |
| Adversarial PTY tests | Feed `ADVERSARIAL_PAYLOADS` through PTY log/command paths and verify termios + screen invariants (`pty_terminal_lifecycle.rs`) | Implemented | `crates/ftui-harness/tests/pty_terminal_lifecycle.rs` (`pty_log_injection_preserves_text_style_and_terminal_state`) |
| Inline never clears screen | Property test asserting inline output never emits `2J`, `3J`, `?1049h`, or `ESC c` (`inline_never_clears_screen`) | Implemented | `crates/ftui-harness/tests/proptest_flicker_invariants.rs` (`inline_never_clears_screen`); ledger `N08` |
| C0/C1 control filtering | Only TAB/LF/CR allowed; DEL and C1 controls (`0x80..=0x9F`) stripped | Implemented | `crates/ftui-render/src/sanitize.rs` (`strips_c1_controls_and_del`) |

### Test Suites

- **Adversarial PTY tests**: `crates/ftui-harness/tests/pty_terminal_lifecycle.rs` (`pty_log_injection_preserves_text_style_and_terminal_state`, `pty_model_log_commands_preserve_text_style_and_terminal_state`, `pty_log_raw_requires_explicit_opt_in_and_restores_termios`, `alt_screen_mode_is_the_only_path_to_1049h`)
- **Inline screen-preservation property tests**: `crates/ftui-harness/tests/proptest_flicker_invariants.rs` (`inline_never_clears_screen`) and `crates/ftui-render/src/sanitize.rs` (`proptest_adversarial::*`)
- **Fuzz tests**: `fuzz/fuzz_targets/fuzz_sanitize.rs` and `fuzz/fuzz_targets/fuzz_sanitize_sgr_only.rs`

### Beads

- bd-397: Create adversarial tests for escape injection
- bd-10i.8.5: Implement LogSink for in-process output routing
- bd-g00-root-epic-ewths.33.1: Implement `write_log_raw`, `write_log_sgr_only`, `LogSink::raw/sgr_only`, `sanitize_sgr_only`
- bd-g00-root-epic-ewths.33.2: Adversarial PTY injection tests, `inline_never_clears_screen` proptest, SGR-only fuzz target
- bd-g00-root-epic-ewths.33.3: Accept ADR-006 and update R2 risk register

### Related ADRs

- [ADR-006: Untrusted Output Policy](adr/ADR-006-untrusted-output-policy.md)

---

## R3: Unicode Width Bugs

### What Can Go Wrong

Incorrect display width calculations cause:
- Text overflow past intended boundaries
- Truncation cutting characters incorrectly
- CJK, emoji, and ZWJ sequences rendering wrong
- UI misalignment that's hard to debug

### Mitigations

| Mitigation | Implementation | Status | Proof |
|------------|----------------|--------|-------|
| Curated test corpus | Emoji, ZWJ, CJK, and VS16 width corpus (`bd-16k`) | Implemented | `crates/ftui-core/src/lib.rs` (`grapheme_width_with_cjk_preserves_multi_codepoint_emoji_and_widens_ambiguous`) |
| Property tests | Proptest for width and wrapping invariants | Implemented | `crates/ftui-text/src/wrap.rs`; `crates/ftui-text/src/text.rs` |
| ASCII fast path | Scalar and portable-SIMD (`ftui-simd`) fast path with parity verification | Implemented | `crates/ftui-core/src/lib.rs` (`ascii_width_matches_scalar`, `grapheme_width_identical_with_simd`); `crates/ftui-simd/src/lib.rs` (`all_ascii_simd_equals_scalar`) |
| Width cache | Thread-local S3-FIFO grapheme width cache (`WIDTH_CACHE`) | Implemented | `crates/ftui-core/src/lib.rs` (`display_width_routes_non_ascii_through_s3_fifo_cache`, `width_cache_is_transparent_and_hits_on_repeat`); ledger `S33` |
| Grapheme segmentation | `unicode-segmentation` crate | Implemented | `crates/ftui-text/src/grapheme.rs` |

### Test Suites

- **Corpus tests**: `crates/ftui-core/src/lib.rs` (`text_width::tests`) - width correctness
- **Property tests**: `crates/ftui-text/src/text.rs` - truncation invariants
- **Wrap tests**: `crates/ftui-text/src/wrap.rs` - wrapping correctness

### Beads

- bd-16k: Unicode width corpus testing with WTF-8
- bd-rk95: Implement GraphemeId encoding with slot+width packing
- bd-6e9.8: Implement grapheme segmentation helpers

---

## R4: Terminal Capability Mismatches

### What Can Go Wrong

Different terminals support different features:
- Scroll regions work in some terminals, not muxes
- Synchronized output (DEC 2026) not universally supported
- Color support varies (mono, 16, 256, truecolor)
- Cursor save/restore has terminal-specific quirks

### Mitigations

| Mitigation | Implementation | Status | Proof |
|------------|----------------|--------|-------|
| Conservative defaults | Overlay-redraw baseline when sync output is absent | Implemented | `crates/ftui-core/src/terminal_capabilities.rs` (`dumb_terminal_disables_all_features`) |
| Environment detection | TERM, COLORTERM, mux detection | Implemented | `crates/ftui-demo-showcase/tests/capability_sim_e2e.rs` (`degradation_sync_output_disabled_in_all_muxes`) |
| Robust fallbacks | Graceful inline strategy selection | Implemented | `crates/ftui-core/src/inline_mode.rs` (`strategy_selection_uses_hybrid_without_sync`) |
| Capability probing | Bayesian capability log-odds probe (`caps_probe.rs`, `bd-1rvd`) | Implemented | `crates/ftui-core/src/caps_probe.rs` (`weights_are_unchanged`); `docs/compat-matrix.md`; ledger `V64` |
| Color downgrade | Truecolor → 256 → 16 → mono | Implemented | `crates/ftui-style/src/color.rs` (`sequential_downgrade_truecolor_to_mono`); ledger `V50` |

### Test Suites

- **Capability detection tests**: `crates/ftui-core/src/terminal_capabilities.rs`
- **Mux detection tests**: tmux, screen, zellij environment variables
- **Color downgrade tests**: `crates/ftui-style/`

### Beads

- bd-1po2: Implement Terminal Capability Probing tests
- bd-1rvd: Implement CapabilityProber with timeout handling
- bd-bo6w: Define ProbeableCapability enum and probe sequences
- bd-3227: Terminal Capability Auto-Upgrade with Safe Probing

### Related ADRs

- [ADR-001: Inline Mode Strategy](adr/ADR-001-inline-mode.md)
- [ADR-003: Terminal Backend](adr/ADR-003-terminal-backend.md)

---

## R5: Unsafe Code Creep

### What Can Go Wrong

Unsafe Rust can introduce:
- Memory safety bugs
- Data races
- Undefined behavior
- Security vulnerabilities

### Mitigations

| Mitigation | Implementation | Status | Proof |
|------------|----------------|--------|-------|
| Forbid unsafe by default | `#![forbid(unsafe_code)]` across all 20 workspace crates | Implemented | `docs/claims-ledger.md` (`V33`); `scripts/check_readme_claims.py` |
| Isolated SIMD crate | Safe `std::simd` kernels in `ftui-simd` (`#![forbid(unsafe_code)]`) | Implemented | `crates/ftui-simd/src/lib.rs` (`first_mismatch_simd_equals_scalar`, `all_ascii_simd_equals_scalar`); ledger `N10` |
| Required benchmarks | Criterion benchmark bindings checked in `tests/baseline.json` | Implemented | `scripts/check_baseline_benches.py` (`make baseline-benches`); `docs/perf/simd_kernels_2026-09-18.md` |
| Static claims & lint gates | `cargo clippy -- -D warnings` and `make gates` | Implemented | `scripts/check_readme_claims.py` (`make claims`) |

### Test Suites

- **Compile-time enforcement**: `#![forbid(unsafe_code)]` in each crate root
- **CI check**: `cargo clippy` catches accidental unsafe

### Beads

- bd-2m5: SIMD-accelerated cell comparison (must be safe or in ftui-simd)

### Policy

All workspace crates forbid `unsafe_code`:
1. SIMD kernels in `ftui-simd` use safe `std::simd` (`#![forbid(unsafe_code)]`)
2. Must have benchmark proving benefit (`docs/perf/simd_kernels_2026-09-18.md`)
3. Must have property tests proving parity against scalar twins

---

## R6: Presenter Byte Bloat

### What Can Go Wrong

Inefficient ANSI output causes:
- Increased bandwidth usage
- Slower rendering, especially over SSH
- Flicker from excessive writes

### Mitigations

| Mitigation | Implementation | Status | Proof |
|------------|----------------|--------|-------|
| Run grouping | Group adjacent cell changes into `ChangeRun` spans (`bd-2yu`) | Implemented | `crates/ftui-render/tests/proptest_diff_invariants.rs` (`certified_diff_matches_full_diff_and_respects_span_bound`) |
| Style-run coalescing | Track active SGR/link state in `Presenter` to avoid redundant escapes | Implemented | `crates/ftui-render/tests/no_flicker_proof.rs` (`certified_and_dirty_diff_match_full_diff_and_wrap_sync_brackets`) |
| Output-length benchmarks | Criterion presenter and diff benchmarks bound to `tests/baseline.json` | Implemented | `crates/ftui-render/benches/presenter_bench.rs`; `scripts/check_baseline_benches.py` |
| Buffered writes | Single buffered flush per frame wrapped in DEC 2026 sync brackets | Implemented | `crates/ftui-harness/tests/sync_bracket_completeness_proof.rs` (`theorem1_sync_brackets_wrap_all_content`) |
| CountingWriter | Track emitted output bytes per frame (`bd-3aqs`) | Implemented | `crates/ftui-render/src/counting_writer.rs` |

### Test Suites

- **Output budget tests**: `crates/ftui-render/src/budget.rs`
- **Presenter benchmarks**: Measure bytes per frame
- **Diff optimization tests**: Verify minimal output

### Beads

- bd-2yu: Implement run grouping and style-run coalescing in Presenter
- bd-1yvn: Integrate budget checking into frame loop
- bd-3aqs: Add CountingWriter for output bytes tracking (closed)
- bd-128s: Add budget-aware rendering to all harness widgets

---

## R7: Interleaved Stdout Writes

### What Can Go Wrong

User code or libraries writing directly to stdout:
- Cursor position becomes undefined
- Partial escape sequences corrupt output
- UI and logs interleave unpredictably
- Terminal state becomes inconsistent

### Mitigations

| Mitigation | Implementation | Status | Proof |
|------------|----------------|--------|-------|
| One-writer rule | `TerminalSession`/`TerminalWriter` ownership + `terminal_output_lock()` | Implemented | `crates/ftui/tests/one_writer_pty.rs` (`a_recovered_task_panic_is_reported_after_the_teardown_not_over_the_ui`); ledger `V03` |
| LogSink routing | In-process logs routed through sanitized `LogSink` / `Cmd::log` | Implemented | `crates/ftui-runtime/src/program.rs` (`cmd_log_dispatches_to_writer_modes`); `crates/ftui-runtime/src/log_sink.rs` (`log_sink_sanitizes_multiple_escapes`) |
| PTY capture | Subprocess output routed through `ProcessSubscription::pty(true)` | Implemented | `crates/ftui-runtime/src/process_subscription.rs` (`pty_child_delivers_carriage_return_progress_and_echo`) |
| Stdio capture | Best-effort fd redirection (`crates/ftui-runtime/src/stdio_capture.rs`, feature `stdio-capture`) | Experimental | `crates/ftui-runtime/src/stdio_capture.rs` |
| Documentation | Explicit one-writer contract and routing patterns | Implemented | `docs/one-writer-rule.md`; `docs/adr/ADR-005-one-writer-rule.md` |

### Test Suites

- **PTY sustained output tests**: Validate stability under concurrent activity
- **Routing pattern tests**: LogSink, PTY capture correctness

### Beads

- bd-10i.12.6: Write One-Writer Rule Guidance + Routing Patterns
- bd-10i.8.5: Implement LogSink for in-process output routing
- bd-10i.8.6: Implement PTY capture for subprocess output

### Related ADRs

- [ADR-005: One-Writer Rule Enforcement](adr/ADR-005-one-writer-rule.md)

### Documentation

- [One-Writer Rule Guidance](one-writer-rule.md)

---

## How to Add a New Risk

When you discover a new failure mode:

1. **Assess severity**: Critical (breaks trust), High (major UX impact), Medium (recoverable), Low (minor)

2. **Document the risk**:
   ```markdown
   ## RN: Risk Name

   ### What Can Go Wrong
   Describe the failure mode and its consequences.

   ### Mitigations
   | Mitigation | Implementation | Status | Proof |
   |------------|----------------|--------|-------|
   | Description | Code/ADR reference | Implemented / Experimental / Not implemented | Test or path |

   ### Test Suites
   - List specific test files and what they verify

   ### Beads
   - Link to relevant beads
   ```

3. **Create beads** for any missing mitigations

4. **Link tests** to enforce the mitigations

5. **Update this register** with the new entry

---

## When to Revisit

Review this register when:

| Trigger | Action |
|---------|--------|
| Adding new terminal features | Check if new risks introduced |
| Changing presenter/buffer logic | Verify existing mitigations still apply |
| Adding external dependencies | Assess new attack surface |
| Post-incident | Add new risk entry with lessons learned |
| Quarterly review | Validate status of all mitigations |

### Quarterly Review Checklist

- [ ] All non-implemented mitigations still linked to open beads
- [ ] All linked beads still open or properly closed
- [ ] Test suites still running in DSR verification
- [ ] No new failure modes discovered but undocumented

---

## Related Documentation

- [Operational Playbook](operational-playbook.md) - merge gates and shipping order
- [State Machines Spec](spec/state-machines.md) - invariants and enforcement
- [Coverage Matrix](testing/coverage-matrix.md) - test expectations by crate
- [ADR Index](adr/README.md) - architectural decisions
