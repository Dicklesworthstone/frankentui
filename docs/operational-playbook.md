# Operational Playbook

This document encodes the process and quality gates that keep FrankenTUI on track toward a stable deliverable. It prevents the common failure mode of kernel projects: infinite refinement without shipping.

> **Purpose**: Prevent churn, enforce gates, ship in the right order.

---

## 1. Merge-Gate Rules

Any change touching the following **critical paths** must include appropriate tests and cite which invariants are preserved:

| Critical Path | Required Tests | Invariants to Cite |
|---------------|----------------|-------------------|
| Inline mode cursor policy | PTY tests | Cursor restored after each frame, no drift over sustained operation |
| Presenter output sequencing | Terminal-model tests, property tests | Front buffer matches desired grid after present |
| Input parser | Unit tests, DoS boundary tests | Bounded CSI/OSC/DCS parsing, no unbounded allocations |
| Width measurement | Corpus tests, property tests | Width correctness for ZWJ/emoji/combining characters |
| Buffer/Cell operations | Unit tests, SIMD comparison tests | 16-byte Cell layout, scissor monotonicity, opacity bounds |
| TerminalSession lifecycle | PTY panic/exit tests | RAII cleanup guarantees |

### Gate Checklist

Before merging PRs that touch critical paths:

- [ ] Unit tests cover the changed behavior
- [ ] Property tests cover edge cases where applicable
- [ ] PTY tests verify terminal state for lifecycle changes
- [ ] The PR description cites which invariants are preserved
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo fmt --check` passes
- [ ] Coverage thresholds maintained (see [coverage-matrix.md](testing/coverage-matrix.md))

### Performance Work (Extreme Optimization Loop)

Any PR that claims performance improvements MUST follow the mandatory loop in:

- `docs/profiling/extreme-optimization-loop.md`

In particular: baseline + profile + one-lever change + isomorphism proof + trace replay/checksum gates.

### Why These Gates

| Gate | Reason |
|------|--------|
| Cursor policy tests | Cursor corruption destroys trust in inline mode |
| Presenter validation | Wrong ANSI output is invisible until it corrupts a terminal |
| Input parser bounds | Unbounded parsing enables DoS attacks |
| Width correctness | Wrong widths cause UI misalignment that's hard to debug |
| Lifecycle tests | Leaked terminal modes make terminals unusable |

---

## 2. ADR Discipline

Architecture Decision Records lock controversial or high-impact decisions to prevent churn.

### When an ADR is Required

Create an ADR when:

1. The decision affects multiple crates or cross-cutting concerns
2. There are reasonable alternatives that someone might revisit later
3. The decision trades off competing goals (performance vs. portability, simplicity vs. features)
4. The decision is influenced by external constraints (terminal compatibility, mux behavior)

### ADR Template

```markdown
# ADR-NNN: Title

## Status

PROPOSED | ACCEPTED | SUPERSEDED by ADR-XXX

## Context

What is the issue or problem we're addressing?
What constraints or forces are at play?

## Decision

What is the change we're making?
Include code examples where helpful.

## Alternatives Considered

What other options did we evaluate?
Why didn't we choose them?

## Consequences

### Positive
- What becomes easier or better?

### Negative
- What becomes harder or worse?
- What tradeoffs are we accepting?

## Test Plan

How do we verify this decision is implemented correctly?
Link to specific test suites or beads.

## References

- Related beads, spikes, or external resources
```

### ADR Governance

- **Proposing**: Create as `PROPOSED`, link from relevant bead
- **Accepting**: Merge to main after review; change status to `ACCEPTED`
- **Revisiting**: Create a new ADR that supersedes; never silently change accepted ADRs

**Current ADRs**: [docs/adr/](adr/README.md)

---

## 3. Minimal Deliverables Order

Ship in this order to avoid the "widget trap" (endless refinement of nice-to-haves before core is stable).

### Phase 1: Kernel (Ship First)

**Goal**: Stable buffer/diff/presenter + terminal session + inline mode stability

- Cell, Buffer, Frame (16-byte cells, scissor/opacity stacks)
- Diff engine (row-major, run grouping)
- Presenter (state-tracked ANSI emission)
- TerminalSession (RAII lifecycle, raw mode, cleanup)
- Inline mode (hybrid strategy, cursor contract)
- Terminal capabilities detection

**Exit criteria**:
- PTY tests pass for cleanup on normal exit and panic
- Presenter correctness validated by terminal-model tests
- Inline mode doesn't corrupt scrollback or cursor

**Beads**: bd-10i.3 (Phase 1 epic)

### Phase 2: Runtime

**Goal**: Update/view loop + deterministic simulator + snapshot testing

- Program/Model/Cmd runtime (Elm-like)
- Tick scheduling
- Batch/sequence command handling
- Headless simulator for deterministic tests
- Snapshot test infrastructure

**Exit criteria**:
- Simulator runs without real terminal
- Snapshot tests catch UI regressions
- Cmd ordering is deterministic

**Beads**: bd-10i.4 (Phase 2 epic)

### Phase 3: Harness

**Goal**: Reference application proving inline mode + log streaming + UI

- Agent harness reference app (ftui-harness)
- Demonstrates streaming logs + pinned UI
- Proves the "agent-ergonomic" promise

**Exit criteria**:
- Hello world harness in <200 LOC
- Works under tmux/screen/zellij
- Sustained log streaming without cursor drift

**Beads**: bd-2kj

### Phase 4: Widgets (Harness Essentials Only)

**Goal**: Only widgets needed for agent harness

- Viewport/LogViewer (scrolling log display)
- Panel/Status (bordered containers)
- TextInput/Prompt (user input)
- Progress/Spinner (activity indicators)

**Exit criteria**:
- Harness app uses these widgets
- Snapshot tests for each widget

**Beads**: bd-10i.7 (Phase 5 epic), bd-29v, bd-35p, bd-2dc

### Phase 5: Extras (Feature-Gated)

**Goal**: Optional features behind feature gates

- Markdown renderer
- Syntax highlighting
- Canvas drawing
- Image protocols
- Forms system
- SSH integration

**Exit criteria**:
- Each extra is feature-gated
- Core crate size unaffected when extras disabled

**Beads**: bd-10i.9 (Phase 7 epic)

---

## 4. Agent-Ergonomic Checklist

FrankenTUI's core promise is being "agent-ergonomic" - easy to use for AI coding agent harnesses.

### Concrete UX Requirements

- [ ] **Hello world harness in <200 LOC**: A working agent UI with log streaming and status panel
- [ ] **Tool output streaming is one function call**: `app.log_sink().write(bytes)`
- [ ] **Pinned bottom UI is one config option**: `ScreenMode::Inline { ui_height: 6 }`
- [ ] **Temporary full-screen modal supported**: Clean transition to/from alt-screen for focus modes
- [ ] **PTY capture for subprocess output**: Built-in way to stream tool output safely

### Anti-Patterns to Avoid

| Anti-Pattern | Why It's Bad | Solution |
|--------------|--------------|----------|
| Requiring custom event loop | Forces users to understand internals | Provide `App::run()` |
| Widget tree construction for simple UIs | Overkill for harness use case | Provide high-level presets |
| Manual ANSI handling | Error-prone, defeats purpose of library | Always sanitize by default |
| Separate log/UI coordination | Causes cursor corruption | Unified one-writer API |

---

## 5. Immediate Next Steps

These are the "first PRs" that unblock everything else. Work on these in priority order.

### Infrastructure (Now)

- [x] Workspace crate layout established
- [x] ADR process documented
- [x] Coverage matrix defined
- [x] State machine spec written

### Kernel (Current Focus)

- [ ] Complete CellContent with char/grapheme encoding (bd-10i.3.1)
- [ ] Complete GraphemeId slot+width packing (bd-rk95)
- [ ] Complete Rect, Sides, Measurement primitives (bd-10i.3.7)
- [ ] Implement terminal raw mode handling (bd-3ky.10)
- [ ] Create PTY test framework (bd-10i.11.2)
- [ ] Integrate budget checking into frame loop (bd-1yvn)

### Docs (Parallel)

- [ ] Operational Playbook (this document, bd-10i.12.1)
- [ ] Risk Register with mitigation links (bd-10i.12.3)
- [ ] Complete One-Writer Rule guidance (bd-10i.12.6)

### Next Wave (After Kernel Stable)

- [ ] Program/Model/Cmd runtime (bd-10i.8.2)
- [ ] Snapshot testing framework (bd-2u4)
- [ ] Agent harness reference app (bd-2kj)

---

## 6. Out of Scope (v1)

The following are explicitly **not** v1 goals. Do not work on these until core is stable.

### Not Now

| Feature | Why Deferred | When Revisit |
|---------|--------------|--------------|
| Widget library beyond harness essentials | Widget trap risk | After harness proves concept |
| Advanced layout (grid with named areas) | Flex is sufficient for v1 | v2 if demand exists |
| Markdown renderer | Extra, not kernel | After Phase 4 |
| Syntax highlighting | Extra, not kernel | After Phase 4 |
| SSH integration | Extra, not kernel | After Phase 4 |
| Forms system | Extra, not kernel | After Phase 4 |
| Canvas/Image widgets | Extra, not kernel | After Phase 4 |

### Explicitly Unsupported

| Feature | Reason |
|---------|--------|
| Multiple simultaneous terminals | Violates one-writer rule |
| Direct stdout access while ftui is active | Undefined behavior by design |
| Windows ConHost (legacy) | ConPTY only (see ADR-004) |
| Async-only runtime | Must support sync for simplicity |

---

## 7. Related Documentation

- [Architecture Decision Records](adr/README.md)
- [State Machines Spec](spec/state-machines.md)
- [Coverage Matrix](testing/coverage-matrix.md)
- [One-Writer Rule Guidance](one-writer-rule.md)
- [Windows Compatibility](WINDOWS.md)

---

## 8. Process Notes

### For Human Contributors

- Check `br ready` for available work before starting
- Create beads for discovered work with `br create`
- Update bead status as you work
- Run quality gates before pushing: `cargo check && cargo clippy && cargo test`
- Push to remote before ending session

### For AI Agents

- Read AGENTS.md for coding discipline
- Register with MCP Agent Mail for coordination
- Use `bv --robot-triage` for prioritized work
- Respect file reservations from other agents
- Send messages for handoffs and blockers
- Run `br sync --flush-only && git add .beads/` at session end
