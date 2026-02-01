# Risk Register

This document tracks the failure modes that destroy trust in a terminal UI kernel. Each risk has explicit mitigations linked to code, tests, and beads.

> **Purpose**: Keep mitigations attached to code. Prevent features from silently reintroducing known risks.

---

## Risk Summary

| ID | Risk | Severity | Status |
|----|------|----------|--------|
| R1 | Inline mode cursor corruption | Critical | Mitigated |
| R2 | ANSI/OSC injection via untrusted content | Critical | Mitigated |
| R3 | Unicode width bugs | High | Mitigated |
| R4 | Terminal capability mismatches | Medium | Mitigated |
| R5 | Unsafe code creep | Medium | Mitigated |
| R6 | Presenter byte bloat | Low | Mitigated |
| R7 | Interleaved stdout writes | High | Mitigated |

---

## R1: Inline Mode Cursor Corruption

### What Can Go Wrong

Inline mode overlays UI on scrollback. If cursor position is corrupted:
- UI draws in wrong location
- Scrollback gets overwritten
- Cursor "drifts" over time during sustained operation
- Exit leaves cursor in unexpected position

### Mitigations

| Mitigation | Implementation | Status |
|------------|----------------|--------|
| PTY integration tests | `ftui-pty` crate (bd-10i.11.2) | In progress |
| Strict cursor policy | DEC ESC7/ESC8 save/restore (ADR-001) | Implemented |
| Centralized writer API | `TerminalWriter` one-writer (ADR-005) | Implemented |
| RAII cleanup guards | `TerminalSession` Drop trait | Implemented |
| Hybrid inline strategy | Overlay-redraw baseline (ADR-001) | Implemented |

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

| Mitigation | Implementation | Status |
|------------|----------------|--------|
| Sanitize by default | Strip CSI/OSC/DCS/APC sequences (ADR-006) | Implemented |
| Explicit raw opt-in | `write_raw()` requires explicit call | Designed |
| Adversarial PTY tests | Feed malicious sequences, verify invariants | Planned |
| C0 control filtering | Only TAB/LF/CR allowed | Implemented |

### Test Suites

- **Adversarial tests**: bd-397 - escape injection scenarios
- **Fuzz tests**: Random bytes through log paths
- **State leakage tests**: Verify no terminal state changes after malicious content

### Beads

- bd-397: Create adversarial tests for escape injection
- bd-10i.8.5: Implement LogSink for in-process output routing

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

| Mitigation | Implementation | Status |
|------------|----------------|--------|
| Curated test corpus | WTF-8 corpus, emoji, ZWJ sequences (bd-16k) | Planned |
| Property tests | Proptest for width invariants | Implemented |
| ASCII fast path | Only when proven safe, with verification | Implemented |
| Width cache | LRU cache for repeated measurements | Implemented |
| Grapheme segmentation | `unicode-segmentation` crate | Implemented |

### Test Suites

- **Corpus tests**: `crates/ftui-text/tests/` - width correctness
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

| Mitigation | Implementation | Status |
|------------|----------------|--------|
| Conservative defaults | Overlay-redraw as baseline | Implemented |
| Environment detection | TERM, COLORTERM, mux detection | Implemented |
| Robust fallbacks | Graceful degradation chain | Implemented |
| Capability probing | Bounded timeout probing (bd-1rvd) | Planned |
| Color downgrade | Truecolor → 256 → 16 → mono | Implemented |

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

| Mitigation | Implementation | Status |
|------------|----------------|--------|
| Forbid unsafe by default | `#![forbid(unsafe_code)]` in all crates | Implemented |
| Isolated SIMD crate | `ftui-simd` for any optimizations | Planned |
| Required benchmarks | Performance claims must have benches | Policy |
| Code review | Any unsafe requires justification | Policy |

### Test Suites

- **Compile-time enforcement**: `#![forbid(unsafe_code)]` in each crate root
- **CI check**: `cargo clippy` catches accidental unsafe

### Beads

- bd-2m5: SIMD-accelerated cell comparison (must be safe or in ftui-simd)

### Policy

Any performance optimization requiring unsafe:
1. Must be isolated in `ftui-simd` crate
2. Must have benchmark proving benefit
3. Must have tests proving correctness
4. Must be reviewed for safety

---

## R6: Presenter Byte Bloat

### What Can Go Wrong

Inefficient ANSI output causes:
- Increased bandwidth usage
- Slower rendering, especially over SSH
- Flicker from excessive writes

### Mitigations

| Mitigation | Implementation | Status |
|------------|----------------|--------|
| Run grouping | Group adjacent changes (bd-2yu) | Planned |
| Style-run coalescing | Minimize SGR switches | Planned |
| Output-length benchmarks | CI enforces budgets | Planned |
| Buffered writes | Single flush per frame | Implemented |
| CountingWriter | Track output bytes (bd-3aqs) | Implemented |

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

| Mitigation | Implementation | Status |
|------------|----------------|--------|
| One-writer rule | `TerminalSession`/`TerminalWriter` ownership | Implemented |
| LogSink routing | In-process logs through ftui | Designed |
| PTY capture | Subprocess output through PTY | Designed |
| Stdio capture | Best-effort capture (feature-gated) | Planned |
| Documentation | Clear undefined behavior warnings | Implemented |

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
   | Mitigation | Implementation | Status |
   |------------|----------------|--------|
   | Description | Code/ADR reference | Status |

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

- [ ] All "Planned" mitigations still on roadmap
- [ ] All linked beads still open or properly closed
- [ ] Test suites still running in CI
- [ ] No new failure modes discovered but undocumented

---

## Related Documentation

- [Operational Playbook](operational-playbook.md) - merge gates and shipping order
- [State Machines Spec](spec/state-machines.md) - invariants and enforcement
- [Coverage Matrix](testing/coverage-matrix.md) - test expectations by crate
- [ADR Index](adr/README.md) - architectural decisions
