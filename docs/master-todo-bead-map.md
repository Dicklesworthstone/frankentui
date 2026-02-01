# Master TODO Inventory → Bead Map

This document maps the plan’s Master TODO Inventory (A–K) to concrete Bead IDs.
It is the execution tracker so we do not need to reopen the plan to see intent.

How to use:
- Add newly discovered requirements here and create/update the bead(s).
- If a bead is renamed or re-scoped, update this map immediately.
- This is a checklist; the work happens in the referenced beads.

---

## A) Project + Docs
- [ ] Definition of Done (Ship Checklist) → bd-tb84
- [ ] Track Quality Gates (v1 Stop-Ship Criteria) → bd-2gx
- [ ] ADR discipline (locked decisions) → bd-10i.10 + children (bd-10i.10.1..bd-10i.10.6)
- [ ] Operational Playbook (Chapter 17) → bd-10i.12.1
- [ ] Glossary (Appendix B) → bd-10i.12.2
- [ ] Risk Register (Appendix C) → bd-10i.12.3
- [ ] ANSI escape reference (Appendix A) → bd-10i.12.4
- [ ] Migration map (Chapter 14) → bd-10i.12.5
- [ ] One-writer rule user guidance → bd-10i.12.6
- [ ] Inline vs alt-screen tradeoffs → bd-1u5
- [ ] Windows v1 limitations → bd-2ss + ADR-004 bd-10i.10.4
- [ ] Terminal compatibility matrix (Chapter 16) → bd-1un
- [ ] Agent harness tutorial → bd-wo2

## B) Contracts (Public API + Kernel Boundaries)
- [ ] Workspace crate layout → bd-10i.2.1
- [ ] ftui facade crate (prelude + stable API) → bd-10i.2.7
- [ ] Define Cell API contract → bd-10i.2.2
- [ ] Define Buffer API contract → bd-10i.2.3
- [ ] Define Event types contract → bd-10i.2.4
- [ ] Define TerminalCapabilities contract (+ mux flags) → bd-10i.2.5
- [ ] Define TerminalSession lifecycle contract → bd-10i.2.6

## C) Rendering Engine (Kernel)
- [ ] CellContent encoding → bd-10i.3.1
- [ ] PackedRgba blending (Porter-Duff) → bd-10i.3.2
- [ ] CellAttrs + style flags → bd-10i.3.3
- [ ] 16-byte Cell struct → bd-10i.3.4
- [ ] GraphemeId encoding (slot+width packing) → bd-rk95
- [ ] GraphemePool refcounting → bd-10i.3.5
- [ ] Buffer (flat Vec, scissor, opacity, continuation cells) → bd-10i.3.6
- [ ] Rect/Sides/Measurement primitives → bd-10i.3.7
- [ ] Frame struct → bd-3cc
- [ ] Diff engine (row-major scan) → bd-10i.4.1
- [ ] Run grouping + style-run coalescing → bd-2yu
- [ ] Presenter (cursor/style/link tracking; single write per frame) → bd-10i.4.3
- [ ] ANSI encoding helpers (CSI/OSC/DEC) → bd-3ky.1
- [ ] LinkRegistry / OSC 8 policy → bd-10i.4.2
- [ ] TerminalCapabilities detection → bd-10i.4.4
- [ ] Perf budgets + benchmarks → bd-19x
- [ ] Output bytes budgets/monitoring → bd-3aqs
- [ ] Hot-path optimizations without unsafe → bd-2m5

## D) Inline Mode (Scrollback-native)
- [ ] Inline overlay redraw baseline → bd-fbp
- [ ] TerminalWriter with inline mode support → bd-10i.8.1
- [ ] Cursor save/restore strategy (DEC vs ANSI vs emulated) → bd-7u8
- [ ] Scroll-region optimization (optional) → bd-2zd
- [ ] tmux/screen passthrough + mux detection → bd-2wo
- [ ] PTY tests for inline correctness + cleanup under panic → bd-10i.11.2

## E) Input + Terminal
- [ ] InputParser state machine → bd-10i.5.1
- [ ] Bracketed paste + mouse SGR protocol decode → bd-3i6
- [ ] Focus + resize event handling → bd-1tt
- [ ] Kitty keyboard decode (optional) → bd-2ao
- [ ] Event coalescing + RAII terminal guard → bd-1zv
- [ ] Input parser fuzzing → bd-10i.11.3

## F) Style + Text
- [ ] Style merge algorithm (explicit masks) → bd-10x
- [ ] Color downgrade (truecolor→256→16→mono) → bd-1f8
- [ ] Theme system (semantic slots) → bd-22q
- [ ] StyleSheet registry → bd-2yd
- [ ] Markup parsing → bd-3mo
- [ ] Text type (styled spans) → bd-2uk
- [ ] Grapheme segmentation helpers (ftui-text) → bd-6e9.8
- [ ] Unicode width corpus testing → bd-16k
- [ ] LRU width cache → bd-1oz
- [ ] Text wrapping + truncation correctness → bd-e211
- [ ] ASCII width fast-path optimization (safe) → bd-v6y

## G) Runtime + Scheduler
- [ ] Program/Model/Cmd runtime → bd-10i.8.2
- [ ] Subscriptions + async integration → bd-27v
- [ ] ProgramSimulator (deterministic) → bd-10i.8.3

## H) Widgets (v1 essentials)
- [ ] Core widgets (Block/Paragraph/Table) → bd-wr2
- [ ] Viewport/LogViewer → bd-29v
- [ ] TextInput widget → bd-gpe
- [ ] Status Line + Panel widgets → bd-2dc
- [ ] Progress + Spinner widgets → bd-35p
- [ ] HitGrid for mouse hit testing (optional) → bd-1mh

## I) Extras (feature-gated)
- [ ] Markdown renderer → bd-cw8
- [ ] Syntax highlighting → bd-381 (depends on bd-3ky.3, bd-3ky.13, bd-3ky.4)
- [ ] Forms/pickers widgets → bd-jbg
- [ ] Export adapters (HTML/SVG/Text) → bd-2fx
- [ ] Clipboard integration (OSC 52) → bd-2rs

## J) Testing + QA
- [ ] Terminal-model test harness → bd-10i.11.1 + spike bd-10i.1.2
- [ ] PTY test framework → bd-10i.11.2
- [ ] Snapshot/golden test framework → bd-2u4
- [ ] Property tests (diff + presenter roundtrip) → bd-2x0j
- [ ] Invariant tests (Chapter 2/3) → bd-10i.13.2
- [ ] Adversarial escape injection tests → bd-397
- [ ] Sanitize-by-default implementation → bd-10i.8.4
- [ ] Comprehensive E2E suite (runner + scripts) → bd-2vr + bd-2ky9
- [ ] Structured logging with tracing → bd-3e8
- [ ] Per-module coverage requirements → bd-3hy

## K) Formal Specs
- [ ] Spec doc: terminal + pipeline state machines → bd-10i.13.1
- [ ] Cache/layout rationale doc (16-byte cell; row-major scan) → bd-10i.13.3

---

## Acceptance Criteria Checklist
- [ ] Every bullet in the plan’s Master TODO Inventory A–K is mapped here to at least one bead.
- [ ] Missing items are turned into beads immediately (no “TODO later”).
- [ ] This mapping remains up to date as scope changes.
