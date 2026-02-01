# FrankenTUI (ftui)

FrankenTUI is a deliberately minimal, high-performance terminal UI kernel.
This repository is early-stage and focuses on correctness, determinism, and
clean architecture across multiple crates.

## Workspace Overview

- `crates/ftui` — public facade and prelude
- `crates/ftui-core` — terminal lifecycle, capabilities, events
- `crates/ftui-render` — cell grid, diff, presenter kernel
- `crates/ftui-style` — style system (planned)
- `crates/ftui-text` — text measurement and spans (planned)
- `crates/ftui-layout` — layout solver (planned)
- `crates/ftui-runtime` — update loop + scheduling (planned)
- `crates/ftui-widgets` — widgets (planned)
- `crates/ftui-extras` — optional extras

## Key Docs

- **Operational Playbook**: `docs/operational-playbook.md` — merge gates, shipping order, process
- **Risk Register**: `docs/risk-register.md` — failure modes and mitigations
- **Glossary**: `docs/glossary.md` — terminology and definitions
- Architecture Decision Records: `docs/adr/README.md`
- Screen modes (inline vs alt-screen): `docs/concepts/screen-modes.md`
- Rendering/terminal state machines: `docs/spec/state-machines.md`
- Test coverage expectations: `docs/testing/coverage-matrix.md`
- One-Writer Rule guidance: `docs/one-writer-rule.md`
- ANSI escape reference: `docs/ansi-reference.md`
- Windows compatibility: `docs/WINDOWS.md`
