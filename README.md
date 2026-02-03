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
  <img src="frankentui_illustration.webp" alt="FrankenTUI - Minimal, high-performance terminal UI kernel">
</div>

Minimal, high‑performance terminal UI kernel focused on correctness, determinism, and clean architecture.

![status](https://img.shields.io/badge/status-WIP-yellow)
![rust](https://img.shields.io/badge/rust-nightly-blue)
![license](https://img.shields.io/badge/license-unspecified-lightgrey)

## Quick Run (from source)

```bash
# Download source with curl (no installer yet)
curl -fsSL https://codeload.github.com/Dicklesworthstone/frankentui/tar.gz/main | tar -xz
cd frankentui-main

# Run the reference harness app
cargo run -p ftui-harness
```

**Or clone with git:**

```bash
git clone https://github.com/Dicklesworthstone/frankentui.git
cd frankentui
cargo run -p ftui-harness
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

---

## Quick Example

```bash
# Reference app (inline mode, log streaming)
FTUI_HARNESS_SCREEN_MODE=inline FTUI_HARNESS_UI_HEIGHT=12 cargo run -p ftui-harness

# Try other harness views
FTUI_HARNESS_VIEW=layout-grid cargo run -p ftui-harness
FTUI_HARNESS_VIEW=widget-table cargo run -p ftui-harness
```

---

## Design Philosophy

1. **Correctness over cleverness** — predictable terminal state is non‑negotiable.
2. **Deterministic output** — buffer diffs and explicit presentation over ad‑hoc writes.
3. **Inline first** — preserve scrollback while keeping chrome stable.
4. **Layered architecture** — core → render → runtime → widgets, no cyclic dependencies.
5. **Zero‑surprise teardown** — RAII cleanup, even when apps crash.

---

## Workspace Overview

| Crate | Purpose | Status |
|------|---------|--------|
| `ftui` | Public facade + prelude | Implemented |
| `ftui-core` | Terminal lifecycle, events, capabilities | Implemented |
| `ftui-render` | Buffer, diff, ANSI presenter | Implemented |
| `ftui-style` | Style + theme system | Implemented |
| `ftui-text` | Spans, segments, rope editor | Implemented |
| `ftui-layout` | Flex + Grid solvers | Implemented |
| `ftui-runtime` | Elm/Bubbletea runtime | Implemented |
| `ftui-widgets` | Core widget library | Implemented |
| `ftui-extras` | Feature‑gated add‑ons | Implemented |
| `ftui-harness` | Reference app + snapshots | Implemented |
| `ftui-pty` | PTY test utilities | Implemented |
| `ftui-simd` | Optional safe optimizations | Reserved |

---

## How FrankenTUI Compares

| Feature | FrankenTUI | Ratatui | tui-rs (legacy) | Raw crossterm |
|---------|------------|---------|-----------------|---------------|
| Inline mode w/ scrollback | ✅ First‑class | ⚠️ App‑specific | ⚠️ App‑specific | ❌ Manual |
| Deterministic buffer diff | ✅ Kernel‑level | ✅ | ✅ | ❌ |
| One‑writer rule | ✅ Enforced | ⚠️ App‑specific | ⚠️ App‑specific | ❌ |
| RAII teardown | ✅ TerminalSession | ⚠️ App‑specific | ⚠️ App‑specific | ❌ |
| Snapshot/time‑travel harness | ✅ Built‑in | ❌ | ❌ | ❌ |

**When to use FrankenTUI:**
- You want inline + scrollback without flicker.
- You care about deterministic rendering and teardown guarantees.
- You prefer a kernel you can build your own UI framework on top of.

**When FrankenTUI might not be ideal:**
- You need a huge widget ecosystem today (FrankenTUI is still early stage).
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

---

## Quick Start

1. **Install Rust nightly** (required by `rust-toolchain.toml`).
2. **Clone the repo** and build:
   ```bash
   git clone https://github.com/Dicklesworthstone/frankentui.git
   cd frankentui
   cargo build
   ```
3. **Run the reference harness:**
   ```bash
   cargo run -p ftui-harness
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

## Commands

### Run the Harness

```bash
cargo run -p ftui-harness
```

### Run Harness Examples

```bash
cargo run -p ftui-harness --example minimal
cargo run -p ftui-harness --example streaming
```

### Tests

```bash
cargo test
BLESS=1 cargo test -p ftui-harness  # update snapshot baselines
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
```

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
┌──────────────────────────────────────────────────────────────────┐
│                          Input Layer                              │
│   TerminalSession (crossterm) → Event (ftui-core)                 │
└──────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│                          Runtime Loop                              │
│   Program/Model (ftui-runtime) → Cmd → Subscriptions              │
└──────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│                         Render Kernel                              │
│   Frame → Buffer → BufferDiff → Presenter → ANSI                  │
└──────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│                          Output Layer                              │
│   TerminalWriter (inline or alt-screen)                           │
└──────────────────────────────────────────────────────────────────┘
```

---

## Troubleshooting

### “terminal is corrupted after crash”

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
| Full widget ecosystem | ⚠️ Partial | Expanding |
| Formal compatibility matrix | ⚠️ In progress | Yes |

---

## FAQ

### Why “FrankenTUI”?

It’s a modular kernel assembled from focused, composable parts — a deliberate, engineered “monster.”

### Is this a full framework?

Not yet. It’s a kernel plus core widgets. You can build a framework on top, but expect APIs to evolve.

### Does it work on Windows?

Windows support is tracked in `docs/WINDOWS.md` and is still being validated.

### Can I embed it in an existing CLI tool?

Yes. Inline mode is designed for CLI + UI coexistence.

### How do I update snapshot tests?

```bash
BLESS=1 cargo test -p ftui-harness
```

---

## Key Docs

- `docs/operational-playbook.md`
- `docs/risk-register.md`
- `docs/glossary.md`
- `docs/adr/README.md`
- `docs/concepts/screen-modes.md`
- `docs/spec/state-machines.md`
- `docs/telemetry.md`
- `docs/spec/telemetry.md`
- `docs/spec/telemetry-events.md`
- `docs/testing/coverage-matrix.md`
- `docs/testing/coverage-playbook.md`
- `docs/one-writer-rule.md`
- `docs/ansi-reference.md`
- `docs/WINDOWS.md`
- `docs/testing/e2e-playbook.md`

---

## About Contributions

*About Contributions:* Please don't take this the wrong way, but I do not accept outside contributions for any of my projects. I simply don't have the mental bandwidth to review anything, and it's my name on the thing, so I'm responsible for any problems it causes; thus, the risk-reward is highly asymmetric from my perspective. I'd also have to worry about other "stakeholders," which seems unwise for tools I mostly make for myself for free. Feel free to submit issues, and even PRs if you want to illustrate a proposed fix, but know I won't merge them directly. Instead, I'll have Claude or Codex review submissions via `gh` and independently decide whether and how to address them. Bug reports in particular are welcome. Sorry if this offends, but I want to avoid wasted time and hurt feelings. I understand this isn't in sync with the prevailing open-source ethos that seeks community contributions, but it's the only way I can move at this velocity and keep my sanity.

---

## License

No license file is specified yet. If you plan to use FrankenTUI in production, please open an issue to clarify licensing.
