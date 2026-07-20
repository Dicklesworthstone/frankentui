# FrankenTUI (ftui)

```
███████╗██████╗  █████╗ ███╗   ██╗██╗  ██╗███████╗███╗   ██╗████████╗██╗   ██╗██╗
██╔════╝██╔══██╗██╔══██╗████╗  ██║██║ ██╔╝██╔════╝████╗  ██║╚══██╔══╝██║   ██║██║
█████╗  ██████╔╝███████║██╔██╗ ██║█████╔╝ █████╗  ██╔██╗ ██║   ██║   ██║   ██║██║
██╔══╝  ██╔══██╗██╔══██║██║╚██╗██║██╔═██╗ ██╔══╝  ██║╚██╗██║   ██║   ██║   ██║██║
██║     ██║  ██║██║  ██║██║ ╚████║██║  ██╗███████╗██║ ╚████║   ██║   ╚██████╔╝██║
╚═╝     ╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═══╝╚═╝  ╚═╝╚══════╝╚═╝  ╚═══╝   ╚═╝    ╚═════╝ ╚═╝
```

<div align="center">
  <img src="docs/assets/frankentui_illustration.webp" alt="FrankenTUI - Minimal, high-performance terminal UI kernel">
</div>

High‑performance terminal UI kernel -- 850K+ lines of Rust across 20 crates, 80+ widget/stateful-widget implementations, 46 interactive demo screens, a Bayesian intelligence layer, resizable pane workspaces, and in-tree web/WASM backends -- focused on correctness, determinism, and clean architecture.

![status](https://img.shields.io/badge/status-WIP-yellow)
![rust](https://img.shields.io/badge/rust-nightly-blue)
![license](https://img.shields.io/badge/license-MIT%2BOpenAI%2FAnthropic%20Rider-green)
![crates](https://img.shields.io/badge/crates-20-blue)
![widgets](https://img.shields.io/badge/widgets-80%2B-blue)
![screens](https://img.shields.io/badge/demo_screens-46-blue)

## Quick Run (from source)

The **primary** way to see what the system can do is the demo showcase:
`cargo run -p ftui-demo-showcase` (not the harness).

```bash
# Download source with curl (no installer yet)
curl -fsSL https://codeload.github.com/Dicklesworthstone/frankentui/tar.gz/main | tar -xz
cd frankentui-main

# Run the demo showcase (primary way to see what FrankenTUI can do)
cargo run -p ftui-demo-showcase
```

**Or clone with git:**

```bash
git clone https://github.com/Dicklesworthstone/frankentui.git
cd frankentui
cargo run -p ftui-demo-showcase
```

---

## TL;DR

**The Problem:** Most TUI stacks make it easy to draw widgets, but hard to build *correct*, *flicker‑free*, *inline* UIs with strict terminal cleanup and deterministic rendering.

**The Solution:** FrankenTUI is a kernel‑level TUI foundation with a disciplined runtime, diff‑based renderer, and inline‑mode support that preserves scrollback while keeping UI chrome stable.

### Why Use FrankenTUI?

| Feature | What It Does | Example |
|---------|--------------|---------|
| **Inline mode** | Stable UI at top/bottom while logs scroll above | `ScreenMode::Inline { ui_height: 10 }` in the runtime |
| **Deterministic rendering** | Buffer → Diff → Presenter → ANSI, no hidden I/O | `BufferDiff::compute(&prev, &next)` |
| **One‑writer rule** | Serializes output for correctness | `TerminalWriter` owns all stdout writes |
| **RAII cleanup** | Terminal state restored even on panic | `TerminalSession` in `ftui-core` |
| **Composable crates** | Layout, text, style, runtime, widgets | Add only what you need |
| **80+ widgets** | Block, Paragraph, Table, Input, Tree, Modal, Command Palette, etc. | Direct `Widget` / `StatefulWidget` implementations across `ftui-widgets` |
| **Pane workspaces** | Drag‑to‑resize, docking, magnetic snap, inertial throw, undo/redo | `PaneTree` + `PaneDragResizeMachine` in `ftui-layout` |
| **Web/WASM backend** | Same Rust core renders in browser-oriented environments | In-tree `ftui-web` + `ftui-showcase-wasm` |
| **Bayesian intelligence** | Statistical diff strategy, resize coalescing, capability detection | BOCPD, VOI, conformal prediction, e‑processes |
| **Shadow‑run validation** | Prove rendering determinism across runtime migrations | `ShadowRun::compare()` in `ftui-harness` |
| **46 demo screens** | Dashboard, visual effects, widget gallery, layout lab, and more | `cargo run -p ftui-demo-showcase` |

---

## Getting Started (Library Consumers)

If you want to embed FrankenTUI in your own Rust app (not just run the demo),
start here: [docs/getting-started.md](docs/getting-started.md).

For web embedding into `frankentui_website` (Next.js + bun), see the
`Embedding In frankentui_website` section in
[`docs/getting-started.md`](docs/getting-started.md). It includes:

- in-tree build/check guidance for `ftui-web` and `ftui-showcase-wasm`,
- a clear note that `FrankenTermWeb` is currently adjacent/out-of-tree from this checkout,
- runtime initialization guidance for `FrankenTermWeb` once that bundle is available,
- and explicit no-`xterm.js` guidance.

---

## Quick Example

```bash
# Demo showcase (primary)
cargo run -p ftui-demo-showcase

# Pick a specific demo view
FTUI_HARNESS_VIEW=dashboard cargo run -p ftui-demo-showcase
FTUI_HARNESS_VIEW=visual_effects cargo run -p ftui-demo-showcase
```

---

## Demo Showcase Gallery (46 Screens)

The demo showcase (`cargo run -p ftui-demo-showcase`) ships 46 interactive screens, each demonstrating a different subsystem:

| Category | Screens | What They Show |
|----------|---------|----------------|
| **Overview** | `dashboard`, `widget_gallery`, `advanced_features` | Full-system demos with animated panels, sparklines, markdown streaming |
| **Layout** | `layout_lab`, `layout_inspector`, `intrinsic_sizing`, `responsive_demo` | Flex/Grid solvers, pane workspaces, constraint visualization |
| **Text** | `shakespeare`, `markdown_rich_text`, `markdown_live_editor`, `advanced_text_editor` | Rope editor, syntax highlighting, streaming markdown |
| **Data** | `table_theme_gallery`, `data_viz`, `virtualized_search`, `log_search` | Themed tables, charts, Fenwick-backed virtualization |
| **Input** | `forms_input`, `form_validation`, `command_palette_lab`, `mouse_playground` | Bayesian fuzzy search, gesture recognition, focus management |
| **Visual FX** | `visual_effects`, `theme_studio`, `mermaid_showcase`, `mermaid_mega_showcase` | Gray-Scott reaction-diffusion, metaballs, Clifford attractors, fractal zooms |
| **System** | `terminal_capabilities`, `performance`, `performance_hud`, `determinism_lab` | Capability probing, frame budgets, conformal risk gating |
| **Diagnostics** | `explainability_cockpit`, `voi_overlay`, `snapshot_player`, `accessibility_panel` | Evidence ledgers, VOI sampling visualization, a11y tree |
| **Workflow** | `file_browser`, `kanban_board`, `async_tasks`, `notifications`, `drag_drop` | File picker, task boards, notification toasts, drag handles |
| **Advanced** | `inline_mode_story`, `hyperlink_playground`, `i18n_demo`, `macro_recorder`, `quake` | Inline scrollback, OSC 8 links, locale switching, event recording |
| **3D / Code** | `3d_data`, `code_explorer`, `widget_builder`, `action_timeline` | 3D data views, AST browsing, widget composition, timeline aggregation |

Each screen is also a snapshot test target. `BLESS=1 cargo test -p ftui-demo-showcase` updates baselines.

---

## Use Cases

- Inline UI for CLI tools where logs must keep scrolling.
- Full-screen dashboards that must never flicker.
- Deterministic rendering harnesses for terminal regressions.
- Libraries that want a strict “kernel” but their own widget layer.

## Non-Goals

- Not a full batteries‑included app framework (by design).
- Not a drop‑in replacement for existing widget libraries.
- Not a “best effort” renderer; correctness beats convenience.

## Minimal API Example

```rust
use ftui_core::event::Event;
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_runtime::{App, Cmd, Model, ScreenMode};
use ftui_widgets::paragraph::Paragraph;

struct TickApp {
    ticks: u64,
}

#[derive(Debug, Clone)]
enum Msg {
    Tick,
    Quit,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(k) if k.is_char('q') => Msg::Quit,
            _ => Msg::Tick,
        }
    }
}

impl Model for TickApp {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Tick => {
                self.ticks += 1;
                Cmd::none()
            }
            Msg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let text = format!("Ticks: {}  (press 'q' to quit)", self.ticks);
        let area = Rect::new(0, 0, frame.width(), 1);
        Paragraph::new(text).render(area, frame);
    }
}

fn main() -> std::io::Result<()> {
    App::new(TickApp { ticks: 0 })
        .screen_mode(ScreenMode::Inline { ui_height: 1 })
        .run()
}
```

---

## Design Philosophy

1. **Correctness over cleverness.** Predictable terminal state is non-negotiable.
2. **Deterministic output.** Buffer diffs and explicit presentation over ad-hoc writes.
3. **Inline first.** Preserve scrollback while keeping chrome stable.
4. **Layered architecture.** Core, render, runtime, widgets; no cyclic dependencies.
5. **Zero-surprise teardown.** RAII cleanup, even when apps crash.

---

## Workspace Overview (20 Crates)

### Core Architecture

| Crate | Purpose | Status |
|------|---------|--------|
| `ftui` | Public facade + prelude | Implemented |
| `ftui-core` | Terminal lifecycle, events, capabilities, animation, input parsing, gestures | Implemented |
| `ftui-render` | Buffer, diff, ANSI presenter, frame, grapheme pool, budget system | Implemented |
| `ftui-style` | Style + theme system with CSS‑like cascading | Implemented |
| `ftui-text` | Spans, segments, rope editor, cursor, BiDi, shaping, normalization | Implemented |
| `ftui-layout` | Flex + Grid solvers, **pane workspace system** (9K+ lines), e‑graph optimizer | Implemented |
| `ftui-runtime` | Elm/Bubbletea runtime, effect system, subscriptions, rollout policy, telemetry schema (13K+ line program.rs) | Implemented |
| `ftui-widgets` | 80+ direct `Widget` / `StatefulWidget` implementations across the library | Implemented |
| `ftui-extras` | Feature‑gated add‑ons, VFX rasterizer (opt‑level=3) | Implemented |

### Backend & Platform

| Crate | Purpose | Status |
|------|---------|--------|
| `ftui-backend` | Backend abstraction layer | Implemented |
| `ftui-tty` | TTY terminal backend | Implemented |
| `ftui-web` | Web/WASM adapter with pointer/touch parity | Implemented |
| `ftui-showcase-wasm` | WASM build of the demo showcase | Implemented |

### Testing & Verification

| Crate | Purpose | Status |
|------|---------|--------|
| `ftui-harness` | Test harness, shadow‑run comparison, benchmark gate, rollout scorecard, determinism fixtures | Implemented |
| `ftui-pty` | PTY‑based test utilities | Implemented |
| `ftui-demo-showcase` | 46 interactive demo screens + snapshot tests | Implemented |
| `doctor_frankentui` | Integrated TUI capture, seeding, suite reporting, diagnostics, and coverage gating | Implemented |

### Supporting

| Crate | Purpose | Status |
|------|---------|--------|
| `ftui-a11y` | Accessibility tree and node structures | Implemented |
| `ftui-i18n` | Internationalization support | Implemented |
| `ftui-simd` | SIMD acceleration | Reserved |

---

## How FrankenTUI Compares

| Feature | FrankenTUI | Ratatui | tui-rs (legacy) | Raw crossterm |
|---------|------------|---------|-----------------|---------------|
| Inline mode w/ scrollback | ✅ First‑class | ⚠️ App‑specific | ⚠️ App‑specific | ❌ Manual |
| Deterministic buffer diff | ✅ Kernel‑level | ✅ | ✅ | ❌ |
| One‑writer rule | ✅ Enforced | ⚠️ App‑specific | ⚠️ App‑specific | ❌ |
| RAII teardown | ✅ TerminalSession | ⚠️ App‑specific | ⚠️ App‑specific | ❌ |
| Pane workspaces (drag/resize/dock) | ✅ Built‑in | ❌ | ❌ | ❌ |
| Web/WASM backend | ✅ Shared Rust core | ❌ | ❌ | ❌ |
| Bayesian diff strategy | ✅ Adaptive | ❌ Fixed | ❌ Fixed | ❌ N/A |
| Shadow‑run validation harness | ✅ Built‑in | ❌ | ❌ | ❌ |
| Snapshot/time‑travel harness | ✅ Built‑in | ❌ | ❌ | ❌ |
| Widget count | 80+ direct impls | ~20 | ~12 | 0 |
| Demo screens | 46 | ~5 | ~5 | 0 |

**When to use FrankenTUI:**
- You want inline + scrollback without flicker.
- You care about deterministic rendering and teardown guarantees.
- You need resizable pane workspaces with drag, dock, and undo.
- You want a single Rust codebase targeting both terminal and web.
- You prefer a kernel you can build your own UI framework on top of.

**When FrankenTUI might not be ideal:**
- You need a stable public API today (FrankenTUI is evolving fast).
- You want a fully opinionated application framework rather than a kernel.

---

## Installation

### Quick Install (Source Tarball)

```bash
curl -fsSL https://codeload.github.com/Dicklesworthstone/frankentui/tar.gz/main | tar -xz
cd frankentui-main
cargo build --release
```

### Git Clone

```bash
git clone https://github.com/Dicklesworthstone/frankentui.git
cd frankentui
cargo build --release
```

### Use as a Workspace Dependency

```toml
# Cargo.toml
[dependencies]
ftui = { path = "../frankentui/crates/ftui" }
```

### Crates.io (Published So Far)

All 17 library crates are published on crates.io (the `ftui` facade plus
`ftui-core`, `ftui-render`, `ftui-style`, `ftui-text`, `ftui-layout`,
`ftui-runtime`, `ftui-widgets`, `ftui-extras`, `ftui-backend`, `ftui-tty`,
`ftui-web`, `ftui-harness`, `ftui-pty`, `ftui-a11y`, `ftui-i18n`,
`ftui-simd`):

```toml
[dependencies]
ftui = "0.5"
```

Only the demo/WASM showcase targets remain workspace-local.

---

## Quick Start

1. **Install Rust nightly** (required by `rust-toolchain.toml`).
2. **Clone the repo** and build:
   ```bash
   git clone https://github.com/Dicklesworthstone/frankentui.git
   cd frankentui
   cargo build
   ```
3. **Run the demo showcase (primary way to see the system):**
   ```bash
   cargo run -p ftui-demo-showcase
   ```

---

## Telemetry (Optional)

Telemetry is opt‑in. Enable the `telemetry` feature on `ftui-runtime` and set
OTEL env vars (for example, `OTEL_EXPORTER_OTLP_ENDPOINT`) to export spans.

When the feature is **off**, telemetry code and dependencies are excluded.
When the feature is **on** but env vars are unset, overhead is a single
startup check.

See `docs/telemetry.md` for integration patterns and trace‑parent attachment.

---

## Feature Flags

| Crate | Feature | What It Enables |
|------|---------|------------------|
| `ftui-core` | `tracing` | Structured spans for terminal lifecycle |
| `ftui-core` | `tracing-json` | JSON output via tracing-subscriber |
| `ftui-render` | `tracing` | Performance spans for diff/presenter |
| `ftui-runtime` | `tracing` | Runtime loop instrumentation |
| `ftui-runtime` | `telemetry` | OpenTelemetry export (OTLP) |

Enable features per-crate in your `Cargo.toml` as needed.

---

## Evidence Logs (JSONL Diagnostics)

FrankenTUI can emit structured, deterministic evidence logs for diff strategy
decisions, resize coalescing, and budget alerts. The log sink is shared and
configured at the runtime level.

```rust
use ftui_runtime::{EvidenceSinkConfig, EvidenceSinkDestination, Program, ProgramConfig};

let config = ProgramConfig::default().with_evidence_sink(
    EvidenceSinkConfig::enabled_file("evidence.jsonl")
        .with_destination(EvidenceSinkDestination::file("evidence.jsonl"))
        .with_flush_on_write(true),
);

let mut program = Program::with_config(model, config)?;
program.run()?;
```

Example event line:

```json
{"event":"diff_decision","run_id":"diff-4242","event_idx":12,"strategy":"DirtyRows","cost_full":1.230000,"cost_dirty":0.410000,"cost_redraw":0.000000,"posterior_mean":0.036000,"posterior_variance":0.000340,"alpha":3.500000,"beta":92.500000,"dirty_rows":4,"total_rows":40,"total_cells":3200,"span_count":2,"span_coverage_pct":6.250000,"max_span_len":12,"fallback_reason":"none","scan_cost_estimate":200,"bayesian_enabled":true,"dirty_rows_enabled":true}
```

---

## Commands

### Run the Demo Showcase (Primary)

```bash
cargo run -p ftui-demo-showcase
```

### Run Harness Examples (tests and reference behavior)

```bash
cargo run -p ftui-harness --example minimal
cargo run -p ftui-harness --example streaming
```

### Tests

```bash
cargo test
BLESS=1 cargo test -p ftui-harness  # update snapshot baselines
```

### Deterministic E2E Runs

Use deterministic fixtures for stable hashes and reproducible logs:

```bash
# Full E2E suite with deterministic seeds/time
E2E_DETERMINISTIC=1 E2E_SEED=0 E2E_TIME_STEP_MS=100 ./scripts/e2e_test.sh

# Demo showcase E2E with an explicit seed
E2E_DETERMINISTIC=1 E2E_SEED=42 ./scripts/demo_showcase_e2e.sh
```

### Format + Lint

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
```

### E2E Scripts

```bash
./scripts/e2e_test.sh
./scripts/widget_api_e2e.sh
./scripts/pane_e2e.sh --mode smoke
./scripts/pane_e2e.sh --mode full
./tests/e2e/check_pane_traceability.sh
```

### doctor_frankentui Verification

Run the full `doctor_frankentui` verification stack locally:

Prerequisites:

- `cargo`
- `python3`
- `jq`
- `rg` (ripgrep)
- `cargo-llvm-cov` (`cargo install cargo-llvm-cov`)
- Python TOML parser support:
  - Python `3.11+` (built-in `tomllib`), or
  - `python3 -m pip install tomli` for Python `<3.11`

```bash
# Unit + integration tests
cargo test -p doctor_frankentui --all-targets -- --nocapture

# Workflow-level E2E
./scripts/doctor_frankentui_happy_e2e.sh /tmp/doctor_frankentui_ci/happy
./scripts/doctor_frankentui_failure_e2e.sh /tmp/doctor_frankentui_ci/failure

# Coverage gate
./scripts/doctor_frankentui_coverage.sh /tmp/doctor_frankentui_ci/coverage
```

Artifact contract (CI and local):

- `.../happy/meta/summary.json`: happy-path pass/fail and per-step timing.
- `.../happy/meta/artifact_manifest.json`: checksums, sizes, and mtimes for expected outputs.
- `.../failure/meta/summary.json`: failure-matrix pass/fail counts.
- `.../failure/meta/case_results.json`: per-case expected vs actual exits and key artifacts.
- `.../coverage/coverage_gate_report.json`: machine-readable threshold decision.
- `.../coverage/coverage_gate_report.txt`: human-readable coverage gate details.

Troubleshooting map:

- `doctor`/`capture`/`suite`/`report` chain failures: inspect `.../happy/logs/*.stderr.log` and `.../happy/meta/command_manifest.txt`.
- failure-case assertion mismatches: inspect `.../failure/meta/case_results.json` and `.../failure/cases/<case_id>/logs/`.
- JSON contract regressions: inspect `json_*` case stdout logs under `.../failure/cases/`.
- coverage regressions: inspect `.../coverage/coverage_gate_report.json` for failing group + threshold delta.

---

## Configuration

FrankenTUI is configuration‑light. The harness is configured via environment variables:

```bash
# .env (example)
FTUI_HARNESS_SCREEN_MODE=inline   # inline | alt
FTUI_HARNESS_UI_HEIGHT=12         # rows reserved for UI
FTUI_HARNESS_VIEW=layout-grid     # view selector
FTUI_HARNESS_ENABLE_MOUSE=true
FTUI_HARNESS_ENABLE_FOCUS=true
FTUI_HARNESS_LOG_LINES=25
FTUI_HARNESS_LOG_MARKUP=true
FTUI_HARNESS_LOG_FILE=/path/to/log.txt
FTUI_HARNESS_EXIT_AFTER_MS=0      # 0 disables auto-exit
```

Terminal capability detection uses standard environment variables (`TERM`, `COLORTERM`, `NO_COLOR`, `TMUX`, `ZELLIJ`, `KITTY_WINDOW_ID`).

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                                 INPUT LAYER                                  │
├──────────────────────────────────────────────────────────────────────────────┤
│ TerminalSession (crossterm)                                                  │
│   └─ raw terminal events  →  Event (ftui-core)                               │
└──────────────────────────────────────────────────────────────────────────────┘
                                        │
                                        ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│                                RUNTIME LOOP                                  │
├──────────────────────────────────────────────────────────────────────────────┤
│ Program / Model (ftui-runtime)                                               │
│   update(Event) → (Model', Cmd)                                              │
│   Cmd → Effects                                                              │
│   Subscriptions → Event stream (tick / io / resize / ...)                    │
└──────────────────────────────────────────────────────────────────────────────┘
                                        │
                                        ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│                               RENDER KERNEL                                  │
├──────────────────────────────────────────────────────────────────────────────┤
│ view(Model) → Frame → Buffer → BufferDiff → Presenter → ANSI                 │
│                 (cell grid)    (minimal)       (encode bytes)                │
└──────────────────────────────────────────────────────────────────────────────┘
                                        │
                                        ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│                                OUTPUT LAYER                                  │
├──────────────────────────────────────────────────────────────────────────────┤
│ TerminalWriter                                                               │
│   inline mode (scrollback-friendly)  |  alt-screen mode (classic)            │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## Frame Pipeline (Step-by-Step)

1. **Input** → `TerminalSession` reads `Event`.
2. **Model** → `update()` returns `Cmd` for side effects.
3. **View** → `view()` renders into `Frame`.
4. **Buffer** → `Frame` writes cells into a 2D `Buffer`.
5. **Diff** → `BufferDiff` computes minimal changes.
6. **Presenter** → emits ANSI with state tracking.
7. **Writer** → enforces one‑writer rule, flushes output.

This is the core loop that ensures determinism and flicker‑free output.

---

## Pane Workspace System

FrankenTUI includes a full pane workspace system (9,000+ lines in `ftui-layout/src/pane.rs`) that goes far beyond simple split layouts:

- **Drag‑to‑resize** via splitter handles with cell‑level hit‑testing
- **Drag‑to‑move** with magnetic docking fields and live ghost preview targets
- **Inertial throw**: release mid-drag and panes coast with momentum via `PaneInertialThrow`
- **Pressure-sensitive snap**: snap strength derived from drag speed and direction changes via `PanePressureSnapProfile`
- **Multi‑pane selection** via Shift+Click with `PaneSelectionState`
- **Intelligence modes**: Focus, Compare, Monitor, Compact layout presets via `PaneLayoutIntelligenceMode`
- **Persistent interaction timeline** with full undo/redo/replay via `PaneInteractionTimeline`
- **Right‑click mode cycling** through all four intelligence modes
- **Scroll-wheel magnetic field tuning**: adjust snap strength without leaving the pane
- **Terminal + Web parity**: same pane interactions work in both backends via `PaneTerminalAdapter` and `PanePointerCaptureAdapter`

The pane system is integrated into three of the 46 demo screens (Dashboard, Widget Gallery, Layout Lab) and has dedicated E2E tests (`scripts/pane_e2e.sh`).

### Pane Architecture

```
PaneTree                         ← Spatial layout tree (HSplit / VSplit / Leaf)
  └── PaneOperation              ← Atomic layout mutation (resize, swap, split, close)
       └── PaneInteractionTimeline  ← Undo/redo/replay history of operations
            └── PaneDragResizeMachine   ← State machine for pointer gesture lifecycle
                 └── PaneSemanticInputEvent  ← High‑level input abstraction
```

---

## Runtime Migration & Rollout Infrastructure

FrankenTUI is migrating its execution substrate through three lanes:

| Lane | Description | Status |
|------|-------------|--------|
| `Legacy` | Thread‑based subscriptions with manual stop coordination | Available |
| `Structured` | CancellationToken‑backed subscriptions (current default) | **Active** |
| `Asupersync` | Full Asupersync‑native execution | Future |

### Rollout Policy

Runtime lane transitions are managed through a shadow‑run comparison system to prevent regressions:

```rust
// Operator workflow: Off → Shadow → Evaluate → Enable → Monitor → Rollback
let config = ProgramConfig::default()
    .with_lane(RuntimeLane::Structured)         // Current execution backend
    .with_rollout_policy(RolloutPolicy::Shadow)  // Shadow‑compare before enabling
    .with_env_overrides();                       // FTUI_RUNTIME_LANE, FTUI_ROLLOUT_POLICY
```

| Environment Variable | Values | Default | Purpose |
|---------------------|--------|---------|---------|
| `FTUI_RUNTIME_LANE` | `legacy`, `structured`, `asupersync` | `structured` | Select execution backend |
| `FTUI_ROLLOUT_POLICY` | `off`, `shadow`, `enabled` | `off` | Control rollout behavior |

### Shadow‑Run Validation

Prove rendering determinism across runtime migrations by running the same model through two independent execution paths and comparing frame checksums:

```rust
use ftui_harness::{ShadowRun, ShadowRunConfig, ShadowVerdict};

let config = ShadowRunConfig::new("migration_test", "tick_counter", 42).viewport(80, 24);
let result = ShadowRun::compare(config, || MyModel::new(), |session| {
    session.init();
    session.tick();
    session.capture_frame();
});
assert_eq!(result.verdict, ShadowVerdict::Match);
```

### Rollout Scorecard & Evidence Bundle

Combine shadow evidence + benchmark results into a single go/no‑go release decision:

```rust
use ftui_harness::{RolloutScorecard, RolloutScorecardConfig, RolloutVerdict, RolloutEvidenceBundle};

let mut scorecard = RolloutScorecard::new(
    RolloutScorecardConfig::default().min_shadow_scenarios(3)
);
scorecard.add_shadow_result(shadow_result);
assert_eq!(scorecard.evaluate(), RolloutVerdict::Go);

// Machine‑readable JSON evidence for CI gates
let bundle = RolloutEvidenceBundle {
    scorecard: scorecard.summary(),
    queue_telemetry: Some(ftui_runtime::effect_system::queue_telemetry()),
    requested_lane: "structured".to_string(),
    resolved_lane: "structured".to_string(),
    rollout_policy: "shadow".to_string(),
};
println!("{}", bundle.to_json());  // Self‑contained release decision artifact
```

### Effect Queue Telemetry & Backpressure

The effect executor tracks queue health with monotonic counters and enforces backpressure:

```rust
// Configure backpressure bounds
let config = ProgramConfig::default()
    .with_effect_queue(
        EffectQueueConfig::default()
            .with_enabled(true)
            .with_max_queue_depth(64)  // Drop tasks beyond this depth
    );

// Monitor queue health at runtime
let snap = ftui_runtime::effect_system::queue_telemetry();
// snap.enqueued, snap.processed, snap.dropped, snap.high_water, snap.in_flight
```

### Unified Telemetry Schema

All runtime telemetry uses canonical targets and event names defined in `ftui_runtime::telemetry_schema`:

| Target | Purpose |
|--------|---------|
| `ftui.runtime` | Startup, shutdown, lane resolution |
| `ftui.effect` | Command/subscription execution, queue drops |
| `ftui.process` | Process subscription lifecycle |
| `ftui.decision.resize` | Resize coalescer decisions |
| `ftui.voi` | Value‑of‑information sampling |
| `ftui.bocpd` | Change‑point detection |
| `ftui.eprocess` | E‑process throttle decisions |

---

## Web/WASM Backend

FrankenTUI targets both native terminals and web browsers from a single Rust codebase:

| Crate | Purpose |
|-------|---------|
| `ftui-web` | Web adapter with pointer/touch parity, DPR/zoom handling |
| `ftui-showcase-wasm` | WASM build target for the demo showcase |

The web adapter translates browser pointer events into the same `PaneSemanticInputEvent` stream used by the terminal backend, ensuring interaction parity across platforms.

This checkout does **not** currently vendor local `crates/frankenterm-core` or
`crates/frankenterm-web` workspace members. Those names still appear in specs and
adjacent integration docs, but the in-tree browser surface today is `ftui-web`
plus the `ftui-showcase-wasm` target. `ftui-pty` integrates with an external
`frankenterm-core` dependency rather than a local workspace crate.

---

## FrankenTerm Integration

FrankenTUI shares contracts and backend primitives with the broader
FrankenTerm effort, but this repository currently contains the ftui-side
integration pieces rather than a vendored local browser-terminal crate.

What is in-tree right now:
- `ftui-web` backend primitives, patch formats, and deterministic host-driven runtime support
- `ftui-showcase-wasm` as the WASM demo/showcase target
- `ftui-pty` integration against the external `frankenterm-core` crate

What remains adjacent/out-of-tree from this checkout:
- a browser-facing `FrankenTermWeb` bundle/package
- the local `crates/frankenterm-core` / `crates/frankenterm-web` layout still referenced by some specs

The adjacent terminal-emulator design target still includes:
- **CSI/OSC/DCS sequences** for cursor control, colors, and window operations
- **Alternate screen** buffer switching (like vim/less)
- **Mouse reporting** protocols (X10, SGR, URXVT)
- **Scrollback** with configurable history depth
- **Selection/copy** with Unicode-aware grapheme boundaries

---

## "Alien Artifact" Quality Algorithms

FrankenTUI employs mathematically rigorous algorithms that go far beyond typical TUI implementations. We call this "alien artifact" quality engineering.

### Bayesian Fuzzy Scoring (Command Palette)

The command palette uses a **Bayesian evidence ledger** for match scoring, not simple string distance:

```
Score = P(relevant | evidence) computed via posterior odds:

P(relevant | evidence) / P(not_relevant | evidence)
    = [P(relevant) / P(not_relevant)] × Π_i BF_i

where BF_i = Bayes Factor for evidence type i
          = P(evidence_i | relevant) / P(evidence_i | not_relevant)
```

**Prior odds by match type:**
| Match Type | Prior Odds | P(relevant) | Intuition |
|------------|------------|-------------|-----------|
| Exact | 99:1 | 99% | Almost always what user wants |
| Prefix | 9:1 | 90% | Very likely relevant |
| Word-start | 4:1 | 80% | Probably relevant |
| Substring | 2:1 | 67% | Possibly relevant |
| Fuzzy | 1:3 | 25% | Needs additional evidence |

**Evidence factors that update posterior:**
- **Word boundary bonus** (BF ≈ 2.0): Match at start of word
- **Position bonus** (BF ∝ 1/position): Earlier matches stronger
- **Gap penalty** (BF < 1.0): Gaps between matched chars reduce confidence
- **Tag match bonus** (BF ≈ 3.0): Query matches command tags
- **Length factor** (BF ∝ 1/length): Shorter, more specific titles preferred

**Result:** Every search result includes an explainable evidence ledger showing exactly why it ranked where it did.

### Bayesian Hint Ranking (Keybinding Hints)

Keybinding hints are ranked by **expected utility minus display cost**, with a VOI exploration bonus and hysteresis for stability:

```
Utility posterior:
    U_i ~ Beta(α_i, β_i)
    E[U_i] = α_i / (α_i + β_i)
    VOI_i = sqrt(Var(U_i))

Net value:
    V_i = E[U_i] + w_voi × VOI_i - λ × C_i

Hysteresis:
    swap only if V_i - V_j > ε
```

**Result:** the UI surfaces the most valuable shortcuts without flicker, while still exploring uncertain hints.

### Bayesian Diff Strategy Selection

The renderer adaptively chooses between diff strategies using a **Beta posterior over change rates**:

```
Change-rate model:
    p ~ Beta(α, β)

Prior: α₀ = 1, β₀ = 19  →  E[p] = 5% (expect sparse changes)

Per-frame update:
    α ← α × decay + N_changed
    β ← β × decay + (N_scanned - N_changed)

where decay = 0.95 (exponential forgetting for non-stationary workloads)
```

**Strategy cost model:**
```
Cost = c_scan × cells_scanned + c_emit × cells_emitted

Full Diff:     Cost = c_row × H + c_scan × D × W + c_emit × p × N
Dirty-Row:     Cost = c_scan × D × W + c_emit × p × N
Full Redraw:   Cost = c_emit × N

Decision: argmin { E[Cost_full], E[Cost_dirty], E[Cost_redraw] }
```

**Conservative mode:** Uses 95th percentile of p (not mean) when posterior variance is high, because the system knows when it's uncertain.

### Bayesian Capability Detection (Terminal Caps Probe)

Terminal capability detection uses **log Bayes factors as evidence weights** to combine noisy signals (env vars, DA1/DA2, DECRPM):

```
log BF = ln(P(data | feature) / P(data | ¬feature))

log-odds posterior:
    logit P(feature | evidence) = logit P(feature) + Σ log BF_i

probability:
    P = 1 / (1 + exp(-logit))
```

**Result:** robust capability detection even when individual probes are flaky.

### Dirty-Span Interval Union (Sparse Diff Scans)

For sparse updates, each row tracks **dirty spans** and the diff scans only the union of those spans:

```
Row y spans:
    S_y = union_k [x0_k, x1_k)

Scan cost:
    sum_y |S_y|
```

**Result:** scan work scales with the *actual changed area*, not full row width.

### Summed-Area Table (Tile-Skip Diff)

To skip empty tiles on large screens, a **summed-area table** (2D prefix sum) allows O(1) tile density checks:

```
SAT(x,y) = A(x,y)
         + SAT(x-1,y) + SAT(x,y-1) - SAT(x-1,y-1)
```

Tile sum queries over any rectangle become constant time, so empty tiles are skipped deterministically.

### Fenwick Tree (Prefix Sums for Virtualized Lists)

Variable-height virtualized lists use a **Fenwick tree** (Binary Indexed Tree) for fast prefix sums:

```
sum(i) = sum_{k=1..i} a_k
update(i, Δ): for (j=i; j<=n; j+=j&-j) tree[j]+=Δ
query(i):     for (j=i; j>0; j-=j&-j)  sum+=tree[j]
```

**Result:** O(log n) height lookup and scroll positioning without scanning all rows.

### Bayesian Height Prediction + Conformal Bounds (Virtualized Lists)

Virtualized lists predict unseen row heights to avoid scroll jumps, using a **Normal-Normal** conjugate update plus conformal bounds:

```
Prior:     μ ~ N(μ₀, σ₀²/κ₀)
Posterior: μ_n = (κ₀·μ₀ + n·x̄) / (κ₀ + n)

Conformal interval:
    [μ_n - q_{1-α}, μ_n + q_{1-α}]
```

Variance is tracked online with Welford’s algorithm, and `q` is the empirical quantile of |residuals|.

### BOCPD: Online Change-Point Detection

Resize coalescing uses **Bayesian Online Change-Point Detection** to detect regime transitions:

```
Observation model (inter-arrival times):
    Steady: x_t ~ Exponential(λ_steady)  where μ_steady ≈ 200ms
    Burst:  x_t ~ Exponential(λ_burst)   where μ_burst ≈ 20ms

Run-length posterior (recursive update):
    P(r_t = 0 | x_1:t) ∝ Σᵣ P(r_{t-1} = r) × H(r) × P(x_t | r)
    P(r_t = r+1 | x_1:t) ∝ P(r_{t-1} = r) × (1 - H(r)) × P(x_t | r)

Hazard function (geometric prior):
    H(r) = 1/λ_hazard  where λ_hazard = 50

Complexity: O(K) per update with K=100 run-length truncation
```

**Regime posterior:**
```
P(burst | observations) = Σᵣ P(burst | r, x_1:t) × P(r | x_1:t)

Decision thresholds:
    p_burst > 0.7  →  Burst regime (aggressive coalescing)
    p_burst < 0.3  →  Steady regime (responsive)
    otherwise      →  Transitional (interpolate delay)
```

### Bayes-Factor Evidence Ledger (Resize Coalescer)

Resize coalescing decisions are explained with a **log10 Bayes factor** ledger:

```
LBF = log10(P(evidence | apply_now) / P(evidence | coalesce))

Interpretation:
    LBF > 0  → apply now
    LBF < 0  → coalesce
    |LBF| > 1 strong, |LBF| > 2 decisive
```

**Result:** coalescing is transparent and audit‑friendly, not heuristic black magic.

### Value-of-Information (VOI) Sampling

Expensive operations (height remeasurement, full diff) use **VOI analysis** to decide when to sample:

```
Beta posterior over violation probability:
    p ~ Beta(α, β)

VOI computation:
    variance_before = αβ / ((α+β)² × (α+β+1))
    variance_after  = (α+1)β / ((α+β+2)² × (α+β+3))  [if success]
    VOI = variance_before - E[variance_after]

Decision:
    sample iff (max_interval exceeded) OR (VOI × value_scale ≥ sample_cost)
```

**Tuned defaults for TUI:**
- `prior_alpha=1.0, prior_beta=9.0` (expect 10% violation rate)
- `max_interval_ms=1000` (latency bound)
- `min_interval_ms=100` (prevent over-sampling)
- `sample_cost=0.08` (moderately expensive)

### E-Process: Anytime-Valid Testing

All statistical thresholds use **e-processes** (wealth-based sequential tests):

```
Wealth process:
    W_t = W_{t-1} × (1 + λ_t × (X_t - μ₀))

where λ_t is the betting fraction from GRAPA (General Random Adaptive Proportion Algorithm)

Key guarantee:
    P(∃t: W_t ≥ 1/α) ≤ α   under null hypothesis

This holds at ANY stopping time, with no peeking penalty.
```

**Applications in FrankenTUI:**
- Budget degradation decisions
- Flake detection in tests
- Allocation budget alerts
- Conformal prediction thresholds

### Conformal Alerting

Budget and performance alerts use **distribution-free conformal prediction**:

```
Nonconformity score:
    R_t = |observed_t - predicted_t|

Threshold (finite-sample guarantee):
    q = quantile_{(1-α)(n+1)/n}(R_1, ..., R_n)

Coverage guarantee:
    P(R_{n+1} ≤ q) ≥ 1 - α   for any distribution!

E-process layer (anytime-valid):
    e_t = exp(λ × (z_t - μ₀) - λ²σ²/2)
```

**Why conformal?** No distributional assumptions required. Works for any data pattern.

### Mondrian Conformal Frame-Time Risk Gating

Frame-time risk gating uses **bucketed (Mondrian) conformal prediction** keyed by screen mode, diff strategy, and size:

```
Residuals: r_t = y_t - ŷ_t
Upper bound: ŷ_t^+ = ŷ_t + q_{1-α}(|r|)

Risk if: ŷ_t^+ > budget
```

Buckets fall back from (mode, diff, size) → (mode, diff) → (mode) → global default,
preserving coverage even when data is sparse.

### CUSUM Control Charts

Allocation budget tracking uses **CUSUM** (Cumulative Sum) for fast change detection:

```
One-sided CUSUM:
    S_t = max(0, S_{t-1} + (X_t - μ₀) - k)

Alert when:
    S_t > h (threshold)

Parameters:
    k = allowance (typically σ/2)
    h = threshold (controls sensitivity vs false alarms)

Dual detection:
    Alert iff (CUSUM detects AND e-process confirms)
           OR (e-process alone exceeds 1/α)
```

**Why dual?** CUSUM is fast but can false-alarm; e-process is slower but anytime-valid. Intersection gives speed with guarantees.

### CUSUM Hover Stabilizer (Mouse Jitter)

Hover target flicker is suppressed with a **CUSUM change‑point detector** on boundary‑crossing distance:

```
S_t = max(0, S_{t-1} + d_t - k)
switch if S_t > h
```

where `d_t` is signed distance to the current target boundary, `k` is drift allowance, and `h` is the switch threshold.

**Result:** single‑cell jitter doesn’t cause hover flicker, but intentional crossings still switch within a couple frames.

### Gesture Recognition State Machine

The `GestureRecognizer` in `ftui-core` (2,100+ lines) transforms raw terminal events into semantic events via a multi-phase state machine:

```
Raw Events                    Semantic Events
─────────                    ───────────────
MouseDown(x,y)  ─┬─ idle ──→ Click
MouseDown(x,y)   │          DoubleClick
MouseDown(x,y)  ─┤          TripleClick (select word / line)
MouseMove(x,y)  ─┤─ armed ─→ DragStart
MouseMove(x,y)  ─┤          DragMove
MouseUp(x,y)    ─┤─ drag ──→ DragEnd
                  │
Key(a)          ─┤          KeyChord (multi-key sequences)
Key(Ctrl+x)     ─┘          ModifiedKey
```

**Dead zone:** drag is only recognized after the pointer moves beyond a configurable threshold (default: 2 cells), preventing accidental drags from jittery mice.

**Multi-click timing:** double/triple clicks use a configurable interval window (default: 500ms) with a `click_count` counter that resets on timeout or position change.

**Chord recognition:** multi-key sequences like `g g` (vim-style) use a `KeySequence` buffer with configurable timeout, enabling complex keybinding schemes without blocking single-key shortcuts.

### Input Parser (3,200+ Lines)

The `InputParser` in `ftui-core` handles the full complexity of terminal input encoding:

- **ANSI escape sequences**: CSI, SS3, DCS, OSC, APC parsing with timeout-based disambiguation
- **Kitty keyboard protocol**: repeat/release events, modifier encoding, functional key disambiguation
- **Bracketed paste**: captures pasted text as a single `Paste` event, preventing paste injection attacks
- **Mouse protocols**: X10, SGR, URXVT, SGR-Pixels with automatic protocol detection
- **UTF-8 streaming**: multi-byte character assembly across partial reads
- **Ambiguous prefix handling**: `ESC` alone vs `ESC [` (Alt+key vs CSI) resolved by timing

### Keybinding System (1,900+ Lines)

The `Keybinding` module supports:

- **Declarative binding maps** with priority levels (global, mode, widget)
- **Chord sequences** (`g g`, `Ctrl+x Ctrl+s`) with configurable timeout
- **Context-sensitive activation**: bindings active only in specific modes/focus states
- **Conflict detection**: warns when bindings shadow each other
- **Serialization**: load/save binding maps for user customization

### Damped Spring Dynamics (Animation System)

Animation transitions use a **damped harmonic oscillator** for natural motion:

```
F = -k(x - x*) - c v
⇒ x'' + c x' + k(x - x*) = 0
```

Critical damping (fastest convergence without overshoot) is:

```
c_crit = 2√k
```

We integrate with **semi‑implicit Euler** and clamp large `dt` by subdividing
into small steps for stability. The result is deterministic, smooth motion
without frame‑rate sensitivity.

### Easing Curves + Stagger Distributions

Base animations use analytic easing curves:

```
ease_in(t)  = t²
ease_out(t) = 1 - (1 - t)²
ease_in_out(t) =
    2t²                (t < 0.5)
    1 - (-2t + 2)²/2   (t ≥ 0.5)
```

Staggered lists distribute start offsets by applying easing to normalized
indices:

```
offset_i = D · ease(i / (n - 1))
```

Optional deterministic jitter is added with a xorshift PRNG and clamped,
so cascades feel organic but remain reproducible in tests.

### Sine Pulse Sequences (Attention Cues)

Attention pulses are a single **half‑cycle sine**:

```
p(t) = sin(πt),  t ∈ [0, 1]
```

This produces a smooth 0→1→0 emphasis without sharp edges or flicker.

### Perceived Luminance (Terminal Background Probe)

Background probing converts RGB to perceived luminance:

```
Y = 0.299R + 0.587G + 0.114B
```

That classification feeds capability detection for dark/light defaults.

### Jain's Fairness Index (Input Guard)

Input fairness monitoring uses **Jain's Fairness Index**:

```
F(x₁, ..., xₙ) = (Σxᵢ)² / (n × Σxᵢ²)

Properties:
    F = 1.0  →  Perfect fairness (all equal)
    F = 1/n  →  Complete unfairness (one dominates)

Intervention:
    if input_latency > threshold OR F < 0.8:
        force_coalescer_yield()
```

**Why Jain's?** Scale-independent, bounded [1/n, 1], interpretable.

---

## Troubleshooting

### "terminal is corrupted after crash"

FrankenTUI uses RAII cleanup via `TerminalSession`. If you see a broken terminal, make sure you are not force‑killing the process.

```bash
# Reset terminal state
reset
```

### “error: the option `-Z` is only accepted on the nightly compiler”

FrankenTUI requires nightly. Install and use nightly or let `rust-toolchain.toml` select it.

```bash
rustup toolchain install nightly
```

### “raw mode not restored”

Ensure your app exits normally (or panics) and does not call `process::exit()` before `TerminalSession` drops.

### “no mouse events”

Mouse must be enabled in the session and supported by your terminal.

```bash
FTUI_HARNESS_ENABLE_MOUSE=true cargo run -p ftui-harness
```

### “output flickers”

Inline mode uses synchronized output where supported. If you’re in a very old terminal or multiplexer, expect reduced capability.

---

## Limitations

### What FrankenTUI Doesn’t Do (Yet)

- **Stable public API**: APIs are evolving quickly.
- **Full widget ecosystem**: Core widgets exist, but the ecosystem is still growing.
- **Guaranteed behavior on every terminal**: Capability detection is conservative; older terminals may degrade.

### Known Limitations

| Capability | Current State | Planned |
|------------|---------------|---------|
| Stable API | ❌ Not yet | Yes (post‑v1) |
| Widget ecosystem | ✅ 80+ direct widget implementations | Expanding |
| Formal compatibility matrix | ⚠️ In progress | Yes |
| Asupersync execution lane | ⚠️ Falls back to Structured | Migration infrastructure complete, executor pending |
| crates.io publishing | ✅ 17 of 20 crates (all libraries) | Showcase targets stay workspace-local |

---

## FAQ

### Why “FrankenTUI”?

A modular kernel assembled from focused, composable parts. A deliberate, engineered “monster.”

### Is this a full framework?

It’s a kernel plus a large widget library plus a demo showcase with 46 screens plus a full pane workspace system. You can build a framework on top, but expect APIs to evolve.

### Does it work on Windows?

Windows support is tracked in `docs/WINDOWS.md` and the deferred native-backend
strategy is documented in `docs/spec/frankenterm-architecture.md` (Section 13.5).

### Can I embed it in an existing CLI tool?

Yes. Inline mode is designed for CLI + UI coexistence.

### Can it run in a browser?

Yes. `ftui-web` provides a WASM adapter that renders through the same Rust core. `ftui-showcase-wasm` is the WASM build target for the demo showcase.

### How do I update snapshot tests?

```bash
BLESS=1 cargo test -p ftui-demo-showcase
```

### How many lines of code is it?

850,000+ lines of Rust across 20 crates, with 80+ direct widget implementations, 46 demo screens, and a broad PTY/scripted E2E surface.

### What's the performance like?

The 16‑byte cell design puts 4 cells per cache line. Bayesian diff strategy selection avoids scanning unchanged regions. The presenter uses cost‑optimal cursor positioning. Frame‑time budgets are enforced via conformal prediction with automatic degradation (Full → SimpleBorders → NoColors → TextOnly).

### How does the rollout system work?

The runtime supports three execution lanes (Legacy, Structured, Asupersync) with a shadow‑run comparison system that proves determinism before enabling a new lane. The `RolloutScorecard` combines shadow evidence with benchmark results into a machine‑readable go/no‑go verdict. See the "Runtime Migration & Rollout Infrastructure" section above.

---

## Key Docs

- `docs/operational-playbook.md`
- `docs/risk-register.md`
- `docs/glossary.md`
- `docs/adr/README.md`
- `docs/concepts/screen-modes.md`
- `docs/spec/state-machines.md`
- `docs/spec/frankenterm-correctness.md`
- `docs/telemetry.md`
- `docs/spec/telemetry.md`
- `docs/spec/telemetry-events.md`
- `docs/testing/coverage-matrix.md`
- `docs/testing/coverage-playbook.md`
- `docs/one-writer-rule.md`
- `docs/ansi-reference.md`
- `docs/WINDOWS.md`
- `docs/testing/e2e-playbook.md`

**Pane workspace:**
- `docs/guides/pane-101.md` — getting started with panes
- `docs/guides/pane-showcase-scenarios.md` — demo scenarios + regression-test map
- `docs/cookbook/panes.md` — task-oriented pane recipes
- `docs/api/pane-stability-contract.md` — supported surface, schema versions, deprecation policy
- `docs/migration/flex-to-pane-and-versioning.md` — Flex/Grid → panes + persisted-workspace versioning
- `docs/spec/pane-parity-contract-and-program.md` — terminal/web parity guarantee
- `docs/pane-release-gate-policy.md` — go/no-go release gate clauses + staged rollout
- `docs/pane-operational-runbook.md` — incident response + emergency rollback

---

## E-Graph Layout Optimizer

The layout engine includes an **equality saturation** optimizer (1,700+ lines in `ftui-layout/src/egraph.rs`) that finds optimal constraint solutions through algebraic rewriting:

```
Expression Language:
  Expr ::= Num(u16)           -- concrete pixel value
         | Var(NodeId)         -- widget reference
         | Add(Expr, Expr)     -- constraint arithmetic
         | Sub(Expr, Expr)
         | Max(Expr, Expr)     -- competing constraints
         | Min(Expr, Expr)     -- bounded constraints

Rewrite Rules (equality saturation):
  Add(a, Num(0)) → a                    -- identity
  Add(Num(x), Num(y)) → Num(x + y)     -- constant folding
  Max(a, a) → a                          -- idempotence
  Add(a, b) = Add(b, a)                  -- commutativity
  ...plus ~20 more domain-specific rules
```

**How it works:** rather than applying rewrites greedily (which can miss global optima), the e-graph compactly represents *all* equivalent forms simultaneously. After saturation, the cheapest expression is extracted using a cost model that penalizes deep nesting and prefers constant propagation.

**Result:** complex constraint layouts (nested flex + grid + min/max) are optimized to simpler equivalent forms before the solver runs, reducing both computation and allocation.

---

## Text Engine

The `ftui-text` crate provides a full text processing stack:

### Rope-Backed Storage

Large text buffers (e.g., the advanced text editor demo) use a **rope** data structure for efficient editing:

```
Rope (balanced tree of chunks):
  ┌──────┐
  │ Node │  ← weight = total chars in left subtree
  ├──┬───┤
  │  │   │
 ┌┴┐ ┌┴┐
 │A│ │B│   ← leaf chunks (typically 512–2048 chars)
 └─┘ └─┘

Insert at position i:  O(log n) — split + rebalance
Delete range [i,j):    O(log n) — split + drop + rebalance
Index by position:     O(log n) — walk tree using weights
```

**Why rope?** For a 100K-line log viewer, inserting at the cursor is O(log n) vs O(n) for a flat `String`. The rope also enables efficient line-index lookups and range extraction.

### Text Editor Core

The `Editor` module (1,800+ lines) provides:

- **Cursor model** with visual position (column) vs byte offset tracking
- **Selection** with anchor/head semantics (Shift+Arrow, Shift+Click)
- **Word/line/paragraph movement** with Unicode word-boundary detection
- **Undo/redo** with operation coalescing (typing "hello" = one undo step, not five)
- **Clipboard integration** via `Cmd::SetClipboard` / `Cmd::GetClipboard`

### BiDi & Shaping

- **BiDi** (`bidi.rs`, 1,100+ lines): Unicode Bidirectional Algorithm for mixed LTR/RTL text
- **Shaping** (`shaping.rs`, 1,500+ lines): script/run segmentation for cluster-aware rendering
- **Normalization** (`normalization.rs`): NFC/NFD Unicode normalization for consistent comparison

### Width Calculation

Grapheme width calculation uses a **W-TinyLFU admission cache** for expensive `unicode-width` lookups:

```
Cache Architecture:
  Doorkeeper (Bloom filter) → Count-Min Sketch → LRU cache

Admission:
  New item admitted only if CMS frequency ≥ eviction candidate frequency
  → High hit-rate even under adversarial access patterns

Width Embedding:
  GraphemeId packs display width into bits [31:25], avoiding pool lookup for width queries
```

---

## Degradation Cascade

When frame rendering exceeds its time budget, FrankenTUI executes a **principled degradation cascade** that preserves correctness while shedding visual fidelity:

```
Conformal Frame Guard
  │ "frame will likely exceed budget"
  ▼
Budget Controller (PID)
  │ computes control signal from frame-time error
  ▼
Degradation Level Selection
  │ Full → SimpleBorders → NoColors → TextOnly
  ▼
Widget Priority Filtering
  │ high-priority widgets rendered first
  ▼
Evidence Emission
    structured JSONL documenting every decision
```

**Key properties:**
- **Recoverable**: when load drops, the cascade automatically restores visual fidelity
- **Observable**: every degradation event is logged with conformal prediction context
- **Widget-aware**: critical widgets (input fields, status bars) degrade last
- **Deterministic**: same input sequence always produces the same degradation path

---

## Formal Cost Models

The `cost_model` module (1,800 lines) provides closed-form cost models for three subsystems:

### Cache Cost Model

```
Loss function:
  L(h, m) = c_miss × (1 - h) + c_memory × m

Optimal cache size (LRU under Zipf workload):
  m* = argmin_m { c_miss × (1 - h(m)) + c_memory × m }

where h(m) is the hit-rate function derived from the characteristic time approximation.
```

### Pipeline Scheduling Model

```
M/G/1 queue model:
  ρ = λ × E[S]                           -- utilization
  W = (λ × E[S²]) / (2 × (1 - ρ))      -- Pollaczek-Khinchine waiting time
  T = W + E[S]                            -- mean response time

Applies to: effect queue, render pipeline, subscription dispatch
```

### Batching Cost Model

```
Batch-and-process cost:
  C(b) = c_setup / b + c_per_item × b    -- amortized setup vs holding cost

Optimal batch size:
  b* = √(c_setup / c_per_item)           -- square root law

Applies to: ANSI emission, change run coalescing, event drain bursts
```

---

## Flake Detection & Sequential FDR Control

### Anytime-Valid Flake Detector

E2E timing tests use an **e-process** to detect flaky regressions without inflating false positives across the hundreds of frames tested:

```
Sub-Gaussian e-value:
  e_t = exp(λ × r_t − λ²σ²/2)

Cumulative evidence:
  E_t = ∏ᵢ eᵢ

Reject H₀ when E_t ≥ 1/α, valid at ANY stopping time.
```

**Why this matters:** traditional significance tests become unreliable when you check p-values after every frame (the "peeking problem"). E-processes eliminate this entirely.

### Alpha-Investing (Sequential FDR Control)

When many monitors fire simultaneously (budget alerts, degradation triggers, capability decisions), testing each at a fixed alpha inflates false discoveries. Alpha-Investing treats significance as a **spendable resource**:

```
Wealth process:
  W₀ = initial_wealth          (e.g. 0.5)

Per-test:
  αᵢ = min(W, α_max)           -- spend from wealth
  W ← W - αᵢ                    -- deduct cost
  if test i rejects:
    W ← W + reward              -- earn back on discovery

FDR guarantee:
  E[FDP] ≤ initial_wealth / (initial_wealth + reward_total)
```

**Result:** FrankenTUI can safely run dozens of simultaneous statistical monitors (BOCPD, CUSUM, conformal, e-process) without false-alarm inflation.

---

## Rough-Path Signatures

The `rough_path` module implements **rough-path signatures** for sequential trace feature extraction, a technique from stochastic analysis:

```
Given a d-dimensional path X: [0,T] → ℝᵈ, the signature is:

S(X)^{i₁,...,iₖ} = ∫₀<t₁<...<tₖ<T dX^{i₁}_{t₁} ⊗ ... ⊗ dX^{iₖ}_{tₖ}

Truncated at depth K:
  S_K(X) = (1, S¹(X), S²(X), ..., Sᴷ(X))
```

**Properties:**
- **Parameterization invariance**: `S(X)` is the same regardless of time warping
- **Universality**: signatures separate paths; different paths always have different signatures
- **Efficient computation**: Chen's identity enables O(nK²d²) incremental updates

**Applications in FrankenTUI:**
- **Workload characterization**: frame time series → signature → anomaly detection
- **Trace comparison**: compare two execution traces without aligning timestamps
- **Regression detection**: signature distance between baseline and candidate runs

---

## Core Algorithms & Data Structures

FrankenTUI is built on carefully chosen algorithms and data structures optimized for terminal rendering constraints.

## Math-Driven Performance

FrankenTUI deliberately uses “heavy” math where it buys real-world speed or determinism. The core idea is: spend a little compute on principled decisions that prevent expensive work later.

### Bayesian Match Scoring (Command Palette)

Instead of raw string distance, the palette asks “how likely is this the right command?” Each clue (word start, tags, position) is a multiplier on confidence.

$$
\frac{P(R\mid E)}{P(\neg R\mid E)} = \frac{P(R)}{P(\neg R)} \prod_i BF_i, \quad
BF_i = \frac{P(E_i\mid R)}{P(E_i\mid \neg R)}
$$

Intuition: add a few strong clues and the right command jumps to the top without expensive rescoring passes.

### Evidence Ledger (Explainable Bayes)

Every probabilistic decision records its “why” as a ledger of factors. Internally this is just log‑odds arithmetic:

$$
\log \frac{P(R\mid E)}{P(\neg R\mid E)} = \log \frac{P(R)}{P(\neg R)} + \sum_i \log BF_i
$$

Intuition: you can read a human‑friendly list of reasons instead of debugging a black‑box score.

### Bayesian Cost Models (Diff Strategy)

The renderer learns the change rate instead of guessing. It keeps a **Beta posterior** and chooses the cheapest strategy (full diff vs dirty rows vs redraw).

$$
p \sim \mathrm{Beta}(\alpha,\beta), \quad
\alpha \leftarrow \alpha\cdot\gamma + k, \quad
\beta \leftarrow \beta\cdot\gamma + (n-k)
$$

$$
E[\text{cost}] = c_{scan}\,N_{scan} + c_{emit}\,N_{emit}
$$

Intuition: when the screen is stable we avoid scanning; when it’s noisy we switch to the cheapest path.

### Presenter Cost Modeling (Cursor/Byte Economy)

Even after diffing, there are multiple ways to emit ANSI. We compute a cheap byte‑level cost for cursor moves vs merged runs.

$$
\text{cost} = c_{scan}\,N_{scan} + c_{emit}\,N_{emit}
$$

Intuition: fewer cursor moves and shorter sequences means less output and lower latency.

### BOCPD for Resize Regimes

Resize storms are handled by **Bayesian Online Change‑Point Detection**. It detects when the stream changes from steady to burst, and only then coalesces aggressively.

$$
H(r)=\frac{1}{\lambda}, \quad
P(r_t=0\mid x_{1:t}) \propto \sum_r P(r_{t-1}=r)\,H(r)\,P(x_t\mid r)
$$

Intuition: no brittle thresholds; the model smoothly adapts to drag vs pause behavior.

### Run‑Length Posterior + Hazard Function (BOCPD Core)

BOCPD’s main state is the **run‑length posterior**, which tracks how long the current regime has lasted.

$$
P(r_t=r\mid x_{1:t}) \propto P(r_{t-1}=r-1)\,(1-H(r-1))\,P(x_t\mid r)
$$

Intuition: long steady streaks increase confidence; a sudden timing change collapses the posterior and triggers coalescing.

### Conformal Prediction (Risk Bounds)

Alerts are not hard‑coded. The threshold is learned from recent residuals so false‑alarm rates stay stable under distribution shifts.

$$
q = \text{Quantile}_{\lceil(1-\alpha)(n+1)\rceil}(R_1,\dots,R_n)
$$

Intuition: the system learns what “normal” looks like and updates the bar automatically.

### E‑Processes + GRAPA (Anytime‑Valid Monitoring)

We can check alerts continuously without “peeking penalties” using a test‑martingale (e‑process). GRAPA tunes the betting fraction.

$$
W_t = W_{t-1}\bigl(1 + \lambda_t (X_t-\mu_0)\bigr)
$$

Intuition: we can look after every frame, and the false‑alarm guarantees still hold.

### GRAPA (Adaptive Betting Fraction)

GRAPA adjusts the betting fraction to keep the e‑process sensitive but stable.

$$
\lambda_{t+1} = \lambda_t + \eta\,\nabla_{\lambda}\,\log W_t
$$

Intuition: it auto‑tunes how aggressively we test, instead of locking a single sensitivity.

### CUSUM (Fast Drift Detection)

CUSUM accumulates small deviations until they add up, catching sustained drift quickly.

$$
S_t = \max\bigl(0,\,S_{t-1} + (X_t-\mu_0) - k\bigr)
$$

Intuition: small problems that persist trigger quickly, while isolated noise is ignored.

### Value‑of‑Information (VOI) Sampling

Expensive measurements are taken only when the expected information gain is worth the cost.

$$
\mathrm{Var}(p)=\frac{\alpha\beta}{(\alpha+\beta)^2(\alpha+\beta+1)},\quad
\mathrm{VOI}=\mathrm{Var}(p)-\mathbb{E}[\mathrm{Var}(p\mid 1\ \text{sample})]
$$

Intuition: if a measurement won’t change our decision, we skip it and stay fast.

### Jain’s Fairness Index (Input Guarding)

We watch whether rendering is starving input processing.

$$
F=\frac{(\sum x_i)^2}{n\sum x_i^2}
$$

Intuition: a single metric tells us when to yield so the UI feels responsive.

### PID / PI Control (Frame Pacing)

Frame‑time control is classic feedback control.

$$
u_t = K_p e_t + K_i \sum e_t + K_d \Delta e_t
$$

Intuition: if we’re too slow, dial down; if we’re too fast, allow more detail. PI is the default because it’s robust and cheap.

### MPC (Model Predictive Control) Evaluation

We test MPC vs PI to prove we’re not leaving performance on the table.

$$
\min_{u_{t:t+H}} \sum_{k=0}^H \|y_{t+k}-y^*\|^2 + \rho\,\|u_{t+k}\|^2
$$

Intuition: MPC looks ahead but costs more; the tests show PI is already good enough for TUI pacing.

### Count‑Min Sketch (Approximate Counts)

We track hot items with a probabilistic sketch, then tighten error bounds with PAC‑Bayes.

$$
\hat f(x)=\min_j C_{j,h_j(x)},\quad
$$

Intuition: a tiny data structure gives you “close enough” frequencies at huge scale.

### PAC‑Bayes Calibration (Error Tightening)

We tighten sketch error bounds using PAC‑Bayes.

$$
\mathbb{E}[\text{err}] \le \bar e + \sqrt{\frac{\mathrm{KL}(q\|\|p)}{2n}}
$$

Intuition: the bound shrinks as we observe more data, without assuming a specific distribution.

### Scheduling Math (Smith’s Rule + Aging)

Background work is ordered by “importance per remaining time,” with aging to prevent starvation.

$$
\text{priority}=\frac{w}{r}+a\cdot\text{wait}
$$

Intuition: short, important jobs finish quickly, but long‑waiting jobs still rise.

Every one of these is directly tied to throughput, latency, and determinism under real terminal workloads.

### Visual FX Math At a Glance

The visual effects screen is deterministic math, not “random shader noise.” Each effect is a concrete dynamical system or PDE with explicit time‑stepping.

| Effect | Core Equation (MathJax) | What It Produces |
|--------|--------------------------|------------------|
| **Metaballs** | $F(x,y)=\sum_i \frac{r_i^2}{(x-x_i)^2+(y-y_i)^2}$, render iso‑surface $F\ge \tau$ | Smooth, organic blob fields |
| **Plasma** | $v=\frac{1}{6}\sum_{k=1}^6 \sin(\phi_k(x,y,t))$ (wave interference in 2D) | Psychedelic interference bands |
| **Gray‑Scott** | $\partial_t u = D_u\nabla^2u - uv^2 + F(1-u)$; $\partial_t v = D_v\nabla^2v + uv^2 - (F+k)v$ | Reaction‑diffusion morphogenesis |
| **Clifford Attractor** | $x_{t+1}=\sin(a y_t)+c\cos(a x_t)$; $y_{t+1}=\sin(b x_t)+d\cos(b y_t)$ | Chaotic strange‑attractor filaments |
| **Mandelbrot / Julia** | $z_{n+1}=z_n^2+c$ (escape‑time coloring) | Fractal boundaries + deep zooms |
| **Lissajous / Harmonograph** | $x=A\sin(a t+\delta)$, $y=B\sin(b t+\phi)$ (optionally $e^{-\gamma t}$ damping) | Elegant phase‑locked curves |
| **Flow Field** | $\vec v(x,y)=(\cos 2\pi N,\ \sin 2\pi N)$; $p_{t+1}=p_t+\vec v\,\Delta t$ | Particle ribbons through a vector field |
| **Wave Interference** | $I(x,t)=\sum_i \sin(k_i\|x-s_i\|-\omega_i t)$ | Multi‑source ripple patterns |
| **Spiral Galaxy** | $r=a e^{b\theta}$ with $\theta(t)=\theta_0+\omega t$ | Logarithmic spiral starfields |
| **Spin Lattice (LLG)** | $\frac{d\vec S}{dt}=-\vec S\times \vec H-\alpha\,\vec S\times(\vec S\times\vec H)$ | Magnetic domain dynamics |

### Math At a Glance

| Technique | Where It’s Used | Core Formula / Idea (MathJax) | Performance Impact |
|----------|------------------|-------------------------------|--------------------|
| **Bayes Factors** | Command palette scoring | $\frac{P(R\mid E)}{P(\neg R\mid E)}=\frac{P(R)}{P(\neg R)}\prod_i BF_i$ | Better ranking with fewer re‑sorts |
| **Evidence Ledger** | Explanations for probabilistic decisions | $\log\frac{P(R\mid E)}{P(\neg R\mid E)}=\log\frac{P(R)}{P(\neg R)}+\sum_i\log BF_i$ | Debuggable, auditable scoring |
| **Log‑BF Capability Probe** | Terminal caps detection | $\log BF=\log \frac{P(data\mid H)}{P(data\mid \neg H)}$ | Robust detection from noisy probes |
| **Log10‑BF Coalescer** | Resize scheduler evidence ledger | $LBF=\log_{10}\frac{P(E\mid apply)}{P(E\mid coalesce)}$ | Explainable, stable resize decisions |
| **Bayesian Hint Ranking** | Keybinding hint ordering | $V_i=E[U_i]+w_{voi}\sqrt{Var(U_i)}-\lambda C_i$ | Stable, utility‑aware hints |
| **Conformal Rank Confidence** | Command palette stability | $p_i=\frac{1}{n}\sum_j \mathbf{1}[g_j\le g_i]$ (gap‑based p‑value) | Deterministic tie‑breaks + stable top‑k |
| **Beta-Binomial** | Diff strategy selection | $p\sim\mathrm{Beta}(\alpha,\beta)$ with binomial updates | Avoids slow strategies as workload shifts |
| **Interval Union** | Dirty-span diff scan | $S_y=\bigcup_k [x_{0k},x_{1k})$ | Scan proportional to changed segments |
| **Summed-Area Table** | Tile-skip diff | $SAT(x,y)=A(x,y)+SAT(x-1,y)+SAT(x,y-1)-SAT(x-1,y-1)$ | Skip empty tiles on large screens |
| **Fenwick Tree** | Virtualized lists | Prefix sums with $i\pm (i\&-i)$ | O(log n) scroll + height queries |
| **Bayesian Height Predictor** | Virtualized list preallocation | $\mu_n=\frac{\kappa_0\mu_0+n\bar{x}}{\kappa_0+n}$ + conformal $q_{1-\alpha}$ | Fewer scroll jumps |
| **BOCPD** | Resize coalescing | Run‑length posterior + hazard $H(r)$ | Fewer redundant renders during drags |
| **Run‑Length Posterior** | BOCPD core | $P(r_t=r\mid x_{1:t})$ recursion | Fast regime switches without thresholds |
| **E‑Process** | Budget alerts, throttle | $W_t=W_{t-1}(1+\lambda_t(X_t-\mu_0))$ | Safe early exits under continuous monitoring |
| **GRAPA** | Adaptive e‑process | $\lambda_{t+1}=\lambda_t+\eta\nabla_{\lambda}\log W_t$ | Self‑tuning sensitivity |
| **Conformal Prediction** | Risk bounds | $q=\text{Quantile}_{\lceil(1-\alpha)(n+1)\rceil}(R)$ | Stable thresholds without tuning |
| **Mondrian Conformal** | Frame‑time risk gating | $\hat y^+=\hat y+q_{1-\alpha}(|r|)$ per bucket | Safe budget gating with sparse data |
| **CUSUM** | Budget change detection | $S_t=\max(0,S_{t-1}+X_t-\mu_0-k)$ | Fast drift detection |
| **CUSUM Hover Stabilizer** | Mouse hover jitter | $S_t=\max(0,S_{t-1}+d_t-k)$ | Stable hover targets without lag |
| **Damped Spring** | Animation transitions | $x''+c x' + k(x-x^*)=0$ | Natural motion without frame‑rate artifacts |
| **Easing Curves** | Fade/slide timing | $t^2$, $1-(1-t)^2$, cubic variants | Predictable velocity shaping |
| **Staggered Cascades** | List animations | $offset_i=D\cdot ease(i/(n-1))$ | Coordinated, non‑uniform entrances |
| **Sine Pulse** | Attention pulses | $p(t)=\sin(\pi t)$ | Smooth 0→1→0 emphasis |
| **Perceived Luminance** | Dark/light probe | $Y=0.299R+0.587G+0.114B$ | Reliable theme defaults |
| **PID / PI** | Degradation control | $u_t=K_pe_t+K_i\sum e_t+K_d\Delta e_t$ | Smooth frame‑time stabilization |
| **MPC** | Control evaluation | $\min_{u_{t:t+H}}\sum\|y_{t+k}-y^*\|^2+\rho\|u_{t+k}\|^2$ | Confirms PI is sufficient |
| **VOI Sampling** | Expensive measurements | $\mathrm{VOI}=\mathrm{Var}-\mathbb{E}[\mathrm{Var}\mid\text{sample}]$ | Lower overhead in steady state |
| **Jain’s Fairness** | Input guard | $F=(\sum x_i)^2/(n\sum x_i^2)$ | Prevents UI render from starving input |
| **Count‑Min Sketch** | Width cache + timeline aggregation | $\hat f(x)=\min_j C_{j,h_j(x)}$ | Fast approximate counts |
| **W‑TinyLFU Admission** | Width cache admission | admit if $\hat f(x)\ge \hat f(y)$ (Doorkeeper → CMS) | Higher cache hit‑rate, fewer width recomputes |
| **PAC‑Bayes** | Sketch calibration | $\bar e+\sqrt{\mathrm{KL}(q\|\|p)/(2n)}$ | Tighter error bounds |
| **Smith’s Rule + Aging** | Queueing scheduler | $priority=\frac{w}{r}+a\cdot\text{wait}$ | Fair throughput under load |
| **Cost Modeling** | Presenter decisions | $cost=c_{scan}N_{scan}+c_{emit}N_{emit}$ | Minimizes cursor bytes |

### The Cell: A 16-Byte Cache-Optimized Unit

Every terminal cell is exactly **16 bytes**, fitting 4 cells per 64-byte cache line:

```
┌──────────────┬──────────────┬──────────────┬──────────────┬─────────┐
│              │              │              │              │         │
│  CellContent │      fg      │      bg      │    attrs     │ link_id │
│   (4 bytes)  │  PackedRgba  │  PackedRgba  │  CellAttrs   │  (2B)   │
│   char/gid   │   (4 bytes)  │   (4 bytes)  │  (2 bytes)   │         │
│              │              │              │              │         │
└──────────────┴──────────────┴──────────────┴──────────────┴─────────┘
                              Cell (16 bytes)
4 cells per 64-byte cache line. SIMD-friendly 128-bit equality via bits_eq().
```

**Why 16 bytes?**
- **Cache efficiency:** 4 cells per cache line means sequential row scans hit L1 cache optimally
- **SIMD comparison:** Single 128-bit comparison via `bits_eq()` for cell equality
- **No heap allocation:** 99% of cells store their character inline; only complex graphemes (emoji, ZWJ sequences) use the grapheme pool

### Block-Based Diff Algorithm

The diff engine processes cells in **4-cell blocks** (64 bytes) for autovectorization:

```
for each row:
  if rows_equal(old[y], new[y]):       ← Fast path: skip unchanged rows
    continue

  for each 4-cell block:
    compare 4 × 128-bit cells          ← SIMD-friendly
    if any changed:
      coalesce into ChangeRun          ← Minimize cursor positioning
```

**Key optimizations:**
- **Row-skip fast path:** Unchanged rows detected with single comparison, no cell iteration
- **Dirty row tracking:** Mathematical invariant ensures only mutated rows are checked
- **Change coalescing:** Adjacent changed cells become single `ChangeRun` (one cursor move vs many)

### Presenter Cost Model

The ANSI presenter dynamically chooses the cheapest cursor positioning strategy:

```rust
// CUP (Cursor Position): CSI {row+1};{col+1}H
fn cup_cost(row, col) → 4 + digits(row+1) + digits(col+1)   // e.g., "\x1b[12;45H" = 9 bytes

// CHA (Column Absolute): CSI {col+1}G
fn cha_cost(col) → 3 + digits(col+1)                        // e.g., "\x1b[45G" = 6 bytes

// Per-row decision: sparse runs vs merged write-through
strategy = argmin(sparse_cost, merged_cost)
```

This ensures expensive operations (like full diff computation) only run when the information gain justifies the cost.

---

## Bayesian Intelligence Layer

FrankenTUI uses principled statistical methods for runtime decisions, replacing ad-hoc heuristics with Bayesian inference.

### BOCPD: Bayesian Online Change-Point Detection

The resize coalescer uses BOCPD to detect regime changes (steady typing vs burst resizing):

```
Observation Model:
  inter-arrival times ~ Exponential(λ_steady) or Exponential(λ_burst)

Run-Length Posterior:
  P(r_t | x_1:t) with truncation at K=100 for O(K) complexity

Regime Decision:
  P(burst | observations) → coalescing delay selection
```

**Why Bayesian?**
- **No magic thresholds:** Prior beliefs updated with evidence
- **Smooth transitions:** Probability-weighted decisions, not binary switches
- **Principled uncertainty:** Knows when it doesn't know

### E-Process: Anytime-Valid Statistical Testing

Budget decisions and alert thresholds use **e-processes** (betting-based sequential tests):

```
Wealth Process:
  W_t = W_{t-1} × (1 + λ_t(X_t - μ₀))

Guarantee:
  P(∃t: W_t ≥ 1/α) ≤ α under null hypothesis

Key Property:
  Valid at ANY stopping time (not just fixed sample sizes)
```

**Practical benefit:** You can check the e-process after every frame without inflating false positive rates.

### VOI Sampling: Value of Information

The runtime decides when to sample expensive metrics using VOI:

```
Beta posterior over violation probability:
    p ~ Beta(α, β)

VOI computation:
    variance_before = αβ / ((α+β)² × (α+β+1))
    variance_after  = (α+1)β / ((α+β+2)² × (α+β+3))  [if success]
    VOI = variance_before - E[variance_after]

Decision:
    sample iff (max_interval exceeded) OR (VOI × value_scale ≥ sample_cost)
```

This ensures expensive operations (like full diff computation) only run when the information gain justifies the cost.

---

## Performance Engineering

### Dirty Row Tracking

Every buffer mutation marks its row dirty in O(1):

```rust
fn set(&mut self, x: u16, y: u16, cell: Cell) {
    self.cells[y as usize * self.width as usize + x as usize] = cell;
    self.dirty_rows.set(y as usize, true);  // O(1) bitmap write
}
```

**Invariant:** If `is_row_dirty(y) == false`, row y is guaranteed unchanged since last clear.

**Cost:** O(height) space, <2% runtime overhead, but enables skipping 90%+ of cells in typical frames.

### Grapheme Pooling

Complex graphemes (emoji, ZWJ sequences) are reference-counted in a pool:

```
GraphemeId (4 bytes):
┌────────────────────────────────────────┐
│ [31-25: width] [24-0: pool slot index] │
└────────────────────────────────────────┘

Capacity: 16M slots, display widths 0-127
Lookup:   O(1) via HashMap deduplication
```

**Why pooling?**
- Most cells are ASCII (stored inline, no pool lookup)
- Complex graphemes deduplicated (same emoji = same GraphemeId)
- Width embedded in ID (no pool lookup for width queries)

### Synchronized Output

Frames are wrapped in DEC 2026 sync brackets for atomic display:

```
CSI ? 2026 h    ←Begin synchronized update
[all frame output]
CSI ? 2026 l    ← End synchronized update (terminal displays atomically)
```

**Guarantee:** No partial frames ever visible, eliminating flicker even on slow terminals.

---

## The Elm Architecture in Rust

FrankenTUI implements the **Elm/Bubbletea** architecture with Rust's type system:

### The Model Trait

```rust
pub trait Model: Sized {
    type Message: From<Event> + Send + 'static;

    fn init(&mut self) -> Cmd<Self::Message>;
    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message>;
    fn view(&self, frame: &mut Frame);
    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>>;
}
```

### Update/View Loop

```
┌─────────┐    ┌─────────┐    ┌─────────┐    ┌─────────┐
│  Event  │───▶│ Message │───▶│ Update  │───▶│  View   │
│ (input) │    │ (enum)  │    │ (model) │    │ (frame) │
└─────────┘    └─────────┘    └─────────┘    └─────────┘
                                   │              │
                                   ▼              ▼
                              ┌─────────┐    ┌─────────┐
                              │   Cmd   │    │ Render  │
                              │ (async) │    │ (diff)  │
                              └─────────┘    └─────────┘
```

### Commands & Side Effects

```rust
Cmd::none()                    // No side effect
Cmd::perform(future, mapper)   // Async operation → Message
Cmd::quit()                    // Exit program
Cmd::batch(vec![...])          // Multiple commands
```

### Subscriptions

Declarative, long-running event sources:

```rust
fn subscriptions(&self) -> Vec<Box<dyn Subscription<Message>>> {
    vec![
        tick_every(Duration::from_millis(16)),   // 60fps timer
        file_watcher("/path/to/watch"),          // FS events
    ]
}
```

Subscriptions are automatically started/stopped based on what `subscriptions()` returns each frame.

---

## Safety & Correctness Guarantees

### Zero Unsafe Code Policy

```rust
// ftui-render/src/lib.rs
#![forbid(unsafe_code)]

// ftui-runtime/src/lib.rs
#![forbid(unsafe_code)]

// ftui-layout/src/lib.rs
#![forbid(unsafe_code)]
```

The entire render pipeline, runtime, and layout engine contain **zero `unsafe` blocks**.

### Integer Overflow Protection

All coordinate arithmetic uses saturating or checked operations:

```rust
// Cursor positioning (saturating)
let next_x = current_x.saturating_add(width as u16);

// Bounds checking (checked)
let Some(target_x) = x.checked_add(offset) else { continue };

// Intentional wrapping (PRNG only)
seed.wrapping_mul(6364136223846793005).wrapping_add(1)
```

### Flicker-Free Proof Sketch

The codebase includes formal proof sketches in `no_flicker_proof.rs`:

**Theorem 1 (Sync Bracket Completeness):** Every byte emitted by Presenter is wrapped in DEC 2026 sync brackets.

**Theorem 2 (Diff Completeness):** `BufferDiff::compute(old, new)` produces exactly `{(x,y) | old[x,y] ≠ new[x,y]}`.

**Theorem 3 (Dirty Tracking Soundness):** If any cell in row y was mutated, `is_row_dirty(y) == true`.

**Theorem 4 (Diff-Dirty Equivalence):** `compute()` and `compute_dirty()` produce identical output when dirty invariants hold.

---

## Test Infrastructure

### Property-Based Testing

```rust
#[test]
fn prop_diff_soundness() {
    proptest!(|(
        width in 10u16..200,
        height in 5u16..100,
        change_pct in 0.0f64..1.0
    )| {
        // Generate random buffers with controlled change percentage
        // Verify diff output matches actual differences
    });
}
```

### Snapshot Testing

```bash
# Run tests, auto-update baselines
BLESS=1 cargo test -p ftui-harness

# Snapshots stored as .txt files for easy diff review
tests/snapshots/
├── layout_flex_horizontal.txt
├── layout_grid_spanning.txt
└── widget_table_styled.txt
```

### Formal Verification Patterns

```rust
// Proof by counterexample: if this test fails, the theorem is false
#[test]
fn counterexample_dirty_soundness() {
    let mut buf = Buffer::new(10, 10);
    buf.set(5, 5, Cell::from_char('X'));
    assert!(buf.is_row_dirty(5), "Theorem 3 violated: mutation without dirty flag");
}
```

### Benchmark Suite

```bash
cargo bench -p ftui-render

# Output:
# diff/identical_100x50    time: [1.2 µs]   throughput: [4.2 Mcells/s]
# diff/sparse_5pct_100x50  time: [8.3 µs]   throughput: [602 Kcells/s]
# diff/dense_100x50        time: [45 µs]    throughput: [111 Kcells/s]
```

---

## Runtime Systems

### Resize Coalescing

Rapid resize events (e.g., window drag) are coalesced to prevent render thrashing:

```
Event Stream:    R1 ─ R2 ─ R3 ─ R4 ─ R5 ─ [gap] ─ R6
                 └───────────────────┘           │
                        coalesced               applied
                      (only R5 rendered)

Regimes:
  Steady (200ms delay)  ← Responsive to deliberate resizes
  Burst  (20ms delay)   ← Aggressive coalescing during drag
```

### Budget-Based Degradation

Frame time is regulated with a PID controller:

```
Error:        e_t = target_ms - actual_ms
Control:      u_t = Kp·e_t + Ki·Σe + Kd·Δe
Degradation:  Full → SimpleBorders → NoColors → TextOnly

Gains: Kp=0.5, Ki=0.05, Kd=0.2 (tuned for 16ms / 60fps)
```

When frames exceed budget, the renderer automatically degrades visual fidelity to maintain responsiveness.

### Input Fairness Guard

Prevents render work from starving input processing:

```
Fairness Index: F = (Σx_i)² / (n × Σx_i²)   ← Jain's Fairness Index

Intervention: if input_latency > threshold OR F < 0.8:
  force_resize_coalescer_yield()
```

---

## Widget System (80+ Direct Implementations)

FrankenTUI ships 80+ direct `Widget` and `StatefulWidget` implementations across `ftui-widgets`.

### Core Widgets

| Widget | Description | Key Feature |
|--------|-------------|-------------|
| `Block` | Container with borders/title | 9 border styles, title alignment |
| `Paragraph` | Text with wrapping | Word/char wrap, scroll |
| `List` | Selectable items | Virtualized, custom highlight |
| `Table` | Columnar data | Column constraints, row selection, themed |
| `Input` | Text input | Cursor, selection, history |
| `Textarea` | Multi-line input | Line numbers, syntax hooks |
| `Tabs` | Tab bar | Closeable, reorderable |
| `Progress` | Progress bars | Determinate/indeterminate |
| `Sparkline` | Inline charts | Min/max markers |
| `Tree` | Hierarchical data | Expand/collapse, lazy loading |
| `CommandPalette` | Fuzzy search | Bayesian scoring with evidence ledger |
| `Modal` | Dialog/overlay system | Stack‑based, focus capture |
| `JsonView` | JSON tree viewer | Collapse/expand nodes |
| `FilePicker` | File browser | Directory navigation |
| `VirtualizedList` | Large lists | Fenwick tree scroll, Bayesian height prediction |
| `Toast` | Notifications | Timed, dismissable |
| `Spinner` | Activity indicator | Multiple styles |
| `Scrollbar` | Scroll position | Proportional thumb |

Plus: `Align`, `Badge`, `Cached`, `Columns`, `ConstraintOverlay`, `DebugOverlay`, `DecisionCard`, `DragHandle`, `Emoji`, `ErrorBoundary`, `Group`, `Help`, `HistoryPanel`, `Inspector`, `LogViewer`, `NotificationQueue`, `Padding`, `Paginator`, `Panel`, `Pretty`, `Rule`, `StatusLine`, `Stopwatch`, `Timer`, `ValidationError`, `VoiDebugOverlay`, `DriftVisualization`, and more.

### Table Theming System

The table widget has a dedicated theme engine (3,500+ lines in `ftui-style/src/table_theme.rs`) that goes far beyond simple row striping:

| Theme Feature | What It Controls |
|---------------|-----------------|
| **Row striping** | Alternating background colors with configurable period |
| **Column emphasis** | Per-column foreground/background overrides |
| **Header styling** | Separate style for header row with bottom border |
| **Selection highlight** | Active row/cell highlight with blend modes |
| **Hover state** | Mouse-over styling with CUSUM-stabilized transitions |
| **Border variants** | 9 built-in border styles per table edge |
| **Cell padding** | Per-cell horizontal/vertical padding |
| **Truncation** | Ellipsis, clip, or wrap per column |
| **Alignment** | Left/center/right per column with Unicode-aware width |

Themes are composable; a base theme can be overlaid with per-instance overrides:

```rust
let theme = TableTheme::modern()
    .with_stripe_period(2)
    .with_header_style(Style::new().bold().fg(Color::Cyan))
    .with_selection_style(Style::new().bg(Color::DarkGray));
```

### Stylesheet System

Named styles are registered in a `Stylesheet` for consistent theming across widgets:

```rust
let mut sheet = Stylesheet::new();
sheet.register("heading", Style::new().bold().fg(Color::Blue));
sheet.register("error", Style::new().fg(Color::Red).bold());
sheet.register("muted", Style::new().fg(Color::DarkGray));

// Apply by name anywhere in the widget tree
let style = sheet.get("heading").unwrap_or_default();
```

### Widget Composition

```rust
// Widgets compose via Frame's render method
fn view(&self, frame: &mut Frame) {
    let chunks = Layout::horizontal([
        Constraint::Percentage(30),
        Constraint::Percentage(70),
    ]).split(frame.area());

    frame.render_widget(sidebar, chunks[0]);
    frame.render_widget(main_content, chunks[1]);
}
```

### Stateful Widgets

```rust
// State lives in your Model, widget borrows it
struct MyModel {
    list_state: ListState,
}

fn view(&self, frame: &mut Frame) {
    frame.render_stateful_widget(
        List::new(items),
        area,
        &mut self.list_state,
    );
}
```

---

## Advanced Features

### Hyperlink Support

```rust
let link_id = frame.link_registry().register("https://example.com");
cell.link_id = link_id;
// Emits OSC 8 hyperlink sequences for supporting terminals
```

### Focus Management

```rust
// Declarative focus graph
focus_manager.register("input1", FocusNode::new());
focus_manager.register("input2", FocusNode::new());
focus_manager.set_next("input1", "input2");  // Tab order

// Navigation
focus_manager.focus_next();  // Tab
focus_manager.focus_prev();  // Shift+Tab
```

### Modal System

```rust
modal_stack.push(ConfirmDialog::new("Delete file?"));
// Modals capture input, render above main content
// Escape or button press pops the stack
```

### Time-Travel Debugging

```rust
// Record frames for debugging
let mut recorder = TimeTravel::new();
recorder.record(frame.clone());

// Replay
recorder.seek(frame_index);
let historical_frame = recorder.current();
```

---

## Accessibility

The `ftui-a11y` crate provides an accessibility tree that mirrors the widget render tree:

- **Semantic nodes** for every interactive widget (button, input, list item)
- **Role/state/label** properties following WAI-ARIA semantics
- **Focus graph** with keyboard navigation order (Tab/Shift+Tab)
- **Live regions** for announcing dynamic content changes to screen readers
- **Contrast checking** using WCAG 2.1 luminance ratios (`ftui-style/src/color.rs`)

The `accessibility_panel` demo screen visualizes the a11y tree in real time as you navigate the UI.

---

## Internationalization

The `ftui-i18n` crate provides locale-aware rendering:

- **Locale context** propagated through the runtime (`ProgramConfig::with_locale("fr")`)
- **Number/date formatting** respecting locale conventions
- **Text direction** (LTR/RTL) integrated with the BiDi module in `ftui-text`
- **String table** support for message translation

The `i18n_demo` screen demonstrates live locale switching between English, French, German, Japanese, and Arabic.

---

## Queueing-Theoretic Scheduler (Deep Dive)

The effect queue scheduler (1,900+ lines) implements multiple scheduling disciplines from queueing theory:

### SRPT (Shortest Remaining Processing Time)

```
Optimal for minimizing mean response time in M/G/1 queues:
  E[T_SRPT] ≤ E[T_FCFS]  for any service-time distribution

Selection rule:
  next_job = argmin { remaining_time(j) : j ∈ ready_queue }
```

Problem: can starve long jobs indefinitely.

### Smith's Rule (Weighted SRPT)

```
Maximizes weighted throughput:
  priority(j) = weight(j) / remaining_time(j)

next_job = argmax { priority(j) : j ∈ ready_queue }
```

### Aging for Starvation Prevention

```
Effective priority:
  priority_eff(j) = weight(j) / remaining_time(j) + aging_factor × wait_time(j)

Wait time grows linearly, so even low-priority jobs eventually rise above high-priority ones.
Guaranteed service: every job completes within O(N × max_weight / aging_factor) time.
```

### Queue Telemetry

```rust
let snap = ftui_runtime::effect_system::queue_telemetry();
// QueueTelemetry {
//   enqueued: 1042,          -- total tasks submitted
//   processed: 1038,         -- total tasks completed
//   dropped: 2,              -- tasks dropped (backpressure/shutdown)
//   high_water: 12,          -- peak queue depth observed
//   in_flight: 2,            -- currently executing
// }
```

Backpressure kicks in when `in_flight ≥ max_queue_depth`, preventing unbounded memory growth under burst load.

---

## Inline Mode: How Scrollback Preservation Works

Most TUI frameworks take over the alternate screen, destroying the user's scrollback history. FrankenTUI's inline mode keeps the UI stable while letting log output scroll naturally above it. Three strategies are implemented and selected automatically based on terminal capabilities:

### Strategy A: Scroll Region (DECSTBM)

```
Terminal viewport (24 rows):
  ┌──────────────────────────┐
  │ log line 47              │ ← scrollable region (rows 1-20)
  │ log line 48              │    DECSTBM constrains scrolling here
  │ log line 49              │
  │ ...                      │
  │ log line 66              │
  ├──────────────────────────┤
  │ ▌Status: 3 tasks  FPS:60│ ← fixed UI region (rows 21-24)
  │ ▌[Tab] switch  [q] quit │    cursor never enters this region
  └──────────────────────────┘

CSI sequence: ESC [ top ; bottom r   (set scrolling region)
```

When new log output arrives, the terminal scrolls only within the designated region. The UI rows below are untouched.

### Strategy B: Overlay Redraw

For terminals that don't support scroll regions reliably (some multiplexers, older emulators), FrankenTUI saves the cursor, clears the UI area, writes new log lines, redraws the UI, and restores the cursor. Wrapped in DEC 2026 sync brackets, this appears atomic to the user.

### Strategy C: Hybrid

Uses scroll-region for the fast path but falls back to overlay-redraw when capability probing detects an unreliable DECSTBM implementation. This is the default.

### Key Invariants

- Scrollback history is never destroyed
- UI region never flickers (sync brackets guarantee atomicity)
- Cursor position is restored exactly after each render cycle
- Log output above the UI region is genuine terminal scrollback (you can scroll up to see it)

---

## Incremental View Maintenance (IVM)

Rather than recomputing layouts, styled text, and visibility flags from scratch every frame, FrankenTUI can propagate *deltas* through a DAG of view operators:

```
Observable<Theme>   Observable<Content>   Observable<Constraint>
       │                    │                      │
       ▼                    ▼                      ▼
   ┌────────┐         ┌─────────┐           ┌───────────┐
   │StyleMap │         │ TextWrap │           │ FlexSolve │
   └────┬───┘         └────┬────┘           └─────┬─────┘
        │                  │                       │
        └──────────┬───────┘───────────────────────┘
                   ▼
            ┌────────────┐
            │ RenderPlan │  ← only dirty nodes recomputed
            └────────────┘
```

When only the theme changes, the style map operator emits deltas that flow to `RenderPlan` without re-running text wrapping or constraint solving. When only a single text cell changes, only that cell's wrapping is recomputed.

This is the same technique used by materialized-view databases (e.g., Materialize, Noria), adapted for frame-rate rendering.

---

## SOS Barrier Certificates

Frame-budget admissibility is checked using a **sum-of-squares (SOS) polynomial barrier certificate**, precomputed offline via semidefinite programming:

```
State space:
  x₁ = budget_remaining ∈ [0, 1]    (fraction of frame budget left)
  x₂ = workload_estimate ∈ [0, 1]   (estimated render cost)

Barrier certificate B(x₁, x₂):
  B(x₁, x₂) = Σ cᵢⱼ x₁ⁱ x₂ʲ     (polynomial, degree ≤ 6)

Safety:
  B(x) ≤ 0  ⟹  state is admissible (safe to render at full fidelity)
  B(x) > 0  ⟹  state is in degradation region (shed visual fidelity)

Guarantee:
  B is a valid barrier certificate iff B(x) ≥ 0 on the unsafe set
  AND dB/dt ≤ 0 on the boundary (Lyapunov-like decrease condition)
```

The polynomial coefficients are solved by `scripts/solve_sos_barrier.py` using SOS/SDP relaxation. The Rust evaluator (`sos_barrier.rs`) is 257 lines and runs in constant time per frame with no allocations.

Why SOS instead of a simple threshold? A polynomial barrier can encode nonlinear safe/unsafe boundaries that accurately reflect the interaction between budget remaining and workload estimate. A flat threshold either triggers too early (wasting visual quality) or too late (missing the deadline).

---

## S3-FIFO Cache

Terminal capability detection and grapheme width lookups use an **S3-FIFO** eviction policy, which was shown to match or outperform W-TinyLFU and ARC on most workloads while being simpler to implement:

```
Three queues:
  Small  (10% capacity) ← new entries land here
  Main   (90% capacity) ← promoted entries (accessed ≥ 1 time in Small)
  Ghost  (keys only)    ← recently evicted keys for frequency tracking

Eviction from Small:
  if accessed ≥ 1 time → promote to Main
  else → evict (key goes to Ghost)

Eviction from Main (FIFO + frequency):
  if freq > 0 → decrement freq, re-insert at tail
  else → evict permanently
```

The key insight: S3-FIFO is scan-resistant without the overhead of an LRU doubly-linked list. Sequential access patterns (like scanning a large buffer) don't flush the cache.

---

## Flat Combining

When multiple event sources (timers, background tasks, input) post operations concurrently, **flat combining** batches them into a single pass. One thread becomes the "combiner" and executes ALL pending operations while holding the state lock:

```
Thread 1: post(op_a) → wait
Thread 2: post(op_b) → wait           combiner (Thread 3):
Thread 3: post(op_c) → becomes          lock state
           combiner                      execute op_a
                                         execute op_b
                                         execute op_c
                                         unlock state
                                         wake threads 1, 2
```

Benefits over a bare `Mutex`:
- Data stays hot in L1 cache (one thread touches everything)
- Lock acquisition happens once per batch, not once per operation
- Natural coalescing: redundant operations (multiple redraws) collapse

---

## Bidirectional Lenses

The `lens` module provides algebraic lenses for binding widgets to model subfields:

```rust
use ftui_runtime::lens::{Lens, field_lens, compose};

struct Config { volume: u8, brightness: u8 }

// A lens focuses on a part of a larger structure
let volume_lens = field_lens!(Config, volume);

// Algebraic guarantees:
//   GetPut: setting the value you just read is a no-op
//   PutGet: reading after a set returns the value you set

let config = Config { volume: 75, brightness: 50 };
assert_eq!(volume_lens.view(&config), 75);

let updated = volume_lens.set(&config, 100);
assert_eq!(updated.volume, 100);
assert_eq!(updated.brightness, 50);  // other fields untouched
```

Lenses compose, so `compose(config_lens, volume_lens)` creates a lens from `AppState` directly to `volume` through an intermediate `Config` struct.

---

## Input Macro Recording & Playback

The `InputMacro` system records terminal events with timing for deterministic replay:

```rust
// Record
let mut recorder = MacroRecorder::new("login_flow");
recorder.record_event(key_event_username);
// ... 200ms passes ...
recorder.record_event(key_event_tab);
recorder.record_event(key_event_password);
recorder.record_event(key_event_enter);
let macro_data = recorder.finish();

// Replay through ProgramSimulator
let mut player = MacroPlayer::new(macro_data);
while let Some((event, delay)) = player.next() {
    std::thread::sleep(delay);
    simulator.send_event(event);
}
```

Uses: regression testing, demo recording, user workflow capture. The `macro_recorder` demo screen shows this in action.

---

## State Persistence

Widget state survives across sessions via the `StateRegistry`:

```
┌───────────────────────────────────────────────────────────────┐
│                       StateRegistry                           │
│   In-memory cache of widget states (HashMap<WidgetId, State>) │
│   Delegates to StorageBackend for persistence                 │
└───────────────────────────────┬───────────────────────────────┘
                                │
                ┌───────────────┼───────────────┐
                ▼               ▼               ▼
          FileBackend     MemoryBackend   CustomBackend
          (JSON on disk)  (tests only)   (user-provided)
```

Configuration:

```rust
let config = ProgramConfig::default().with_persistence(
    PersistenceConfig::new()
        .with_auto_save(true)
        .with_auto_load(true)
        .with_backend(FileBackend::new("~/.config/myapp/state.json"))
);
```

Widgets opt in by implementing the `Stateful` trait. On program start, the registry loads saved state; on exit (or periodic checkpoints), it flushes back to the backend.

---

## SLO Schema & Breach Detection

FrankenTUI supports machine-readable **Service Level Objectives** for runtime behavior:

```yaml
# slo.yaml
objectives:
  - name: frame_render_p99
    metric: frame_render_us
    budget_us: 16000          # 16ms = 60fps
    window_seconds: 60
    error_budget_pct: 1.0     # allow 1% of frames to exceed

  - name: shutdown_latency
    metric: shutdown_us
    budget_us: 5000           # 5ms shutdown target
    window_seconds: 300
    error_budget_pct: 0.1
```

The SLO engine checks observations against budgets and tracks error-budget consumption:

```
slo.yaml  ──parse──▶  SloSchema
                         │
        observations ──▶ check_breach() ──▶ BreachResult
                                              │
                                   ┌──────────┴──────────┐
                                   ▼                     ▼
                              No breach             Breach detected
                              (continue)            (enter safe mode)
```

When an SLO is breached, the runtime can enter safe mode (reduced rendering, aggressive coalescing) until the error budget recovers.

---

## Multi-Stage Conformal Monitoring

Individual render pipeline stages have independent conformal monitors:

```
view() → [Layout] → Buffer → [Diff] → Changes → [Present] → ANSI
           ↑               ↑                ↑
      stage monitor   stage monitor   stage monitor
      (calibration)   (calibration)   (calibration)
```

Each stage maintains its own Mondrian-bucketed residual set, so a regression in layout computation is detected independently from diff or presenter regressions. Buckets are keyed by (screen mode, diff strategy, terminal size) and fall back to coarser groupings when data is sparse.

This granularity means the runtime can identify *which* pipeline stage is responsible for a slowdown, rather than just flagging "frame was slow."

---

## Headless Simulator

The `ProgramSimulator` (1,700+ lines) runs a `Model` without a real terminal, enabling deterministic testing:

```rust
let mut sim = ProgramSimulator::new(MyModel::new());
sim.init();
sim.send(Msg::LoadData);
sim.tick();

// Capture rendered output without a terminal
let frame = sim.capture_frame(80, 24);
assert_eq!(sim.model().items.len(), 42);
assert!(sim.is_running());

// Frame hashes for regression detection
let hash = frame.checksum();
```

The simulator is the backbone of the shadow-run comparison system and the rollout scorecard. It powers every harness test without needing a PTY or terminal emulator.

---

## Frame Arena Allocator

The render hot path uses a **bump allocator** reset at frame boundaries, eliminating per-frame allocator churn:

```rust
let mut arena = FrameArena::new(256 * 1024); // 256 KB initial

// During frame rendering:
let styled_spans = arena.alloc_slice(&computed_spans);
let layout_rects = arena.alloc_slice(&solved_rects);

// At frame boundary:
arena.reset();  // O(1), no individual deallocations
```

Why bump allocation? The render path produces many small, short-lived allocations (styled text spans, layout rectangles, change runs). A bump allocator satisfies these in O(1) with zero fragmentation, and `reset()` reclaims everything in a single pointer write.

---

## Color System

The `ftui-style` color module supports multiple color profiles with automatic downgrade:

| Profile | Colors | When Used |
|---------|--------|-----------|
| `TrueColor` | 16M (24-bit RGB) | Modern terminals with `COLORTERM=truecolor` |
| `Ansi256` | 256 | Terminals with 256-color support |
| `Ansi16` | 16 | Basic terminals |
| `Mono` | 2 | `NO_COLOR` set, or dumb terminals |

Color downgrade is automatic based on terminal capability detection:

```
TrueColor RGB(128, 0, 255)
  → Ansi256: round channels to the 6×6×6 cube; exact grays choose the closer cube/ramp candidate by weighted RGB distance
  → Ansi16: map to closest basic color
  → Mono: drop color entirely, keep bold/underline for emphasis
```

The module includes **WCAG 2.1 contrast ratio** utilities for accessibility checking:

```rust
let ratio = contrast_ratio(foreground_rgb, background_rgb);
// WCAG AA: ratio ≥ 4.5 for normal text, ≥ 3.0 for large text
// WCAG AAA: ratio ≥ 7.0 for normal text, ≥ 4.5 for large text
```

Perceived luminance uses the standard formula: `Y = 0.2126R + 0.7152G + 0.0722B` (linearized sRGB).

---

## Evidence Sink Architecture

Every probabilistic decision in FrankenTUI is logged to a **shared evidence sink**, a structured JSONL stream that captures the reasoning behind runtime behavior:

```rust
let config = ProgramConfig::default().with_evidence_sink(
    EvidenceSinkConfig::enabled_file("evidence.jsonl")
);
```

Evidence categories:
- `diff_decision`: which diff strategy was chosen and why (Beta posterior, cost estimates)
- `resize_decision`: coalesce vs apply, with Bayes factor ledger
- `conformal_gate`: frame-time risk gating with bucket, upper bound, budget
- `degradation_event`: which visual tier was selected and why
- `queue_select`: scheduler job selection with priority breakdown
- `voi_sample`: VOI computation and sampling decision

**Why evidence?** When a frame is slow, operators can `grep` the JSONL for that frame index and see exactly which decisions were made and what statistical state drove them. No black boxes.

---

## About Contributions

*About Contributions:* Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos that seeks community contributions, but it's the only way I can move at this velocity and keep my sanity.

---

## License

MIT License (with OpenAI/Anthropic Rider) © 2026 Jeffrey Emanuel. See `LICENSE`.
