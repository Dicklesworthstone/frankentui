# Pane Showcase Scenarios

The demo showcase ships a flagship pane workspace screen — **Layout Lab** —
that exercises the production-grade pane UX end-to-end, plus pane usage in
Dashboard and Widget Gallery. This page catalogs the advanced workflows each
demonstrates and ties every one to its **deterministic regression test**, so
the demos double as living references for future regression checks (the goal of
`bd-1w0w4.3`).

> Why a reference page instead of yet another screen? The advanced pane
> workflows are already demonstrated in `layout_lab.rs` (split-tree editing,
> adapter-driven drag/resize, the four intelligence modes, the interaction
> timeline, and snapshot/save/restore). Per the project's "no file
> proliferation" rule, the rollout value is making them **discoverable and
> regression-anchored**, not duplicating them. Where a workflow is best proven
> headlessly (keyboard, cross-host parity, soak), the canonical demonstration
> *is* the deterministic test — that is the determinism-first philosophy of the
> kernel.

---

## Running the showcase

**Terminal:**

```bash
cargo run -p ftui-demo-showcase                       # full showcase
FTUI_HARNESS_VIEW=layout_lab cargo run -p ftui-demo-showcase   # flagship pane screen
FTUI_HARNESS_VIEW=dashboard  cargo run -p ftui-demo-showcase
FTUI_HARNESS_VIEW=widget_gallery cargo run -p ftui-demo-showcase
```

**Browser (WASM):** the same screens build through `ftui-showcase-wasm`
(see [`build-wasm.sh`](../../build-wasm.sh)). The web host renders the identical
Rust pane core, so the interactions below behave the same — the cross-host
parity runner proves it.

---

## Advanced workflows (flagship: Layout Lab)

Each row links a user-visible workflow in `layout_lab.rs` to the implementation
entry point and the deterministic test that guards it.

| Workflow | Demo implementation (`screens/layout_lab.rs`) | Living-reference regression test |
|----------|------------------------------------------------|----------------------------------|
| **Complex split-tree editing** | `PaneTree` + structural `PaneOperation`s | `ftui-layout/tests/pane_invariant_fuzz.rs` (random op sequences preserve invariants) |
| **Pointer drag-to-resize** | `apply_pane_drag` → `PaneTerminalAdapter` → `operations_for_transition` | `ftui-harness/tests/pane_splitter_drag_pty_e2e.rs` (terminal), `ftui-web/tests/pane_web_e2e.rs` (web) |
| **Keyboard resize / focus nav** | adapter-driven resize (`apply_pane_resize_via_adapter`) | `ftui-harness/tests/pane_input_pty_e2e.rs` (`pty_keymap_*`, 6), `ftui-web/tests/pane_web_e2e.rs` (`web_kbd_*`, 9) |
| **Intelligence modes** (Focus/Compare/Monitor/Compact) | `apply_pane_intelligence_mode(PaneLayoutIntelligenceMode::*)` | covered by determinism + invariant suites (`pane_determinism_matrix.rs`) |
| **Undo / redo / replay** | `PaneInteractionTimeline` | `ftui-layout/tests/pane_checkpoint_integration.rs`, `pane_semantic_replay_harness.rs` |
| **Snapshot / save / restore** | screen snapshot + restore path | `ftui-layout/tests/pane_persistent_equivalence.rs` |
| **Cross-host parity** | same semantic events on both adapters | `ftui-web/tests/pane_cross_host_parity.rs` |
| **Sustained-load stability** | repeated resize/churn while interacting | `ftui-layout/tests/pane_soak_stress.rs`, `pane_soak_rollback.rs` |

All of the above are deterministic: a given input sequence always produces the
same layout/frame hashes, which is what lets the tests serve as regression
anchors.

---

## Scenario walkthrough (Layout Lab)

1. **Split & resize.** Drag a splitter handle: the pointer stream flows through
   `PaneTerminalAdapter::translate` → `PaneDragResizeMachine` →
   `PaneTree::operations_for_transition` → `apply_operation`. Watch the splitter
   diagnostics (total/hovered/active handle counts, work units) update live.
2. **Cycle intelligence modes.** Switch between Focus, Compare, Monitor, and
   Compact preset arrangements over the same tree.
3. **Undo/redo.** Every structural edit lands in the `PaneInteractionTimeline`;
   step back and forward and observe deterministic replay.
4. **Snapshot & restore.** Capture the workspace and restore it, exercising the
   persistence path (schema-versioned via `PANE_TREE_SCHEMA_VERSION`).

For the API behind each step, see [Pane 101](pane-101.md) and the
[cookbook](../cookbook/panes.md).

---

## Parity & both-host consistency

The terminal and web hosts share the canonical pane core. The
[cross-host parity runner](../../crates/ftui-web/tests/pane_cross_host_parity.rs)
drives the canonical scenario corpus through both adapters and asserts
observational identity (topology hash, applied-operation sequence, settle
state). Intentional host differences (pointer-capture lifecycle, cancel reason,
pixel vs cell coordinates) are normalized. The contract is specified in
[`docs/spec/pane-parity-contract-and-program.md`](../spec/pane-parity-contract-and-program.md).

---

## Operator troubleshooting (artifact bundles)

If a showcase scenario regresses in CI, the pane gates emit structured
artifacts (see the [release gate policy](../pane-release-gate-policy.md) and
[operational runbook](../pane-operational-runbook.md)):

- **Parity divergence:** JSON first-divergence diff from the parity runner.
- **Soak/rollback:** operator-grade JSONL under `target/pane-soak/`
  (`scripts/pane_soak_rollback.sh`).
- **Replay/perf:** checksummed symbolized replay index
  (`scripts/pane_replay_artifacts.py`), certified against the golden oracle.

---

## Coverage notes & follow-ups

- Layout Lab is the comprehensive advanced-pane reference; Dashboard and Widget
  Gallery use panes more lightly (splitter primitives / panel layout).
- Keyboard pane control and cross-host parity are proven headlessly by the
  PTY/web E2E and parity suites rather than by interactive demo chrome — those
  tests are the authoritative living references.
- Any future interactive demo additions should reuse the shared
  `ftui-demo-showcase/src/pane_interaction.rs` host-agnostic behavior so they
  inherit parity for free.
</content>
