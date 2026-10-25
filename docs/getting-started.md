# Getting Started (Library Consumers)

FrankenTUI is early-stage and APIs evolve quickly, but the core loop is stable:
`Event` -> `Model::update` -> `Model::view` -> `BufferDiff` -> ANSI.

If you just want to see what it can do, run the showcase:

```bash
cargo run -p ftui-demo-showcase
```

If you want to embed FrankenTUI in your own Rust app, this page is the shortest
path to a working inline (scrollback-preserving) UI.

## Stability Notes

- Expect breaking API changes: this is pre-1.0 and moving fast.
- These examples target 0.9.0. Use the version dependency below, or a local
  checkout when developing against repository source.
- The facade's default features compile a terminal backend (native `ftui-tty`
  on Unix, Crossterm elsewhere) so `App::run()` works without configuration.

## Crate Map (20 Workspace Crates + External Web Wrapper)

1. **Published Core & Library Crates (17 crates on crates.io)**:
   - `ftui` (facade): recommended entry point; re-exports core APIs, prelude, and `agent_shell`.
   - `ftui-core`: terminal lifecycle, capabilities, and input events.
   - `ftui-render`: 16-byte `Cell`, `Buffer`/`Frame`, diff computation, and ANSI `Presenter`.
   - `ftui-runtime`: Elm-style `App`/`Program` loop, commands, subscriptions, and `TerminalWriter`.
   - `ftui-widgets`: core `Widget` and `StatefulWidget` library.
   - `ftui-layout`: `Flex` and `Grid` constraint solvers and pane workspace geometry.
   - `ftui-style`: styles, semantic `Theme`, `StyleSheet`, color profiles, and WCAG contrast helpers.
   - `ftui-text`: rope-backed text storage, Unicode width, wrapping, cursor/editor, and UAX #9 bidi.
   - `ftui-i18n`: string catalogs, locale context, CLDR plural rules, and CLDR v45.0 number/date formatting.
   - `ftui-a11y`: accessibility tree builder, roles, states, and live-region diff tracking.
   - `ftui-backend`: backend traits (`BackendEventSource`, `BackendPresenter`, `BackendFeatures`).
   - `ftui-tty`: default native Unix `TtyBackend` (`termios`, `poll`, `SIGWINCH`, job-control cleanup).
   - `ftui-extras`: feature-gated add-ons (markdown, syntax highlighting, canvas, charts, mermaid, forms, VFX).
   - `ftui-web`: step-driven WASM backend (`StepProgram`, deterministic clock, flat patch output).
   - `ftui-harness`: headless simulator, snapshot assertions, shadow-run comparison, and runnable examples.
   - `ftui-pty`: PTY process spawning, virtual terminal model, and E2E test utilities.
   - `ftui-simd`: opt-in portable-SIMD kernels for printable-ASCII width and row comparison.

2. **In-Tree Applications & Verification (`publish = false`, 3 workspace crates)**:
   - `ftui-demo-showcase`: reference application and visual snapshot suite (45 screens).
   - `ftui-showcase-wasm`: WASM showcase runner core (`ShowcaseRunner`) and JS/CSS `<pre>` host SDK.
   - `doctor_frankentui`: verification harness (`replay`/`capture`, `seed-demo`, `migrate`/`suite`, `report`, `certify`/`doctor`, `plan`/`import`, `list-profiles`, `performance-diff`, `visual-diff`).

3. **External / Historical Web Wrapper (not a workspace crate)**:
   - `FrankenTermWeb`: standalone WebGPU/Canvas2D browser renderer retrieved from pinned historical source by `build-wasm.sh`.

## Prereqs

- Rust nightly (required by `rust-toolchain.toml`)
- A terminal with basic ANSI support (tmux/zellij are supported)

## Embedding In `frankentui_website` (Next.js + bun)

This section is for the web stack and is explicitly **xterm.js-free**.

Current repo reality:

- This checkout contains the in-tree web foundations: `ftui-web` and `ftui-showcase-wasm`.
- This checkout does **not** currently vendor a local `crates/frankenterm-web` package.
- The build helper retrieves `FrankenTermWeb` from this repository's pinned
  historical source, before that crate was removed from the workspace.

### 1. Build artifacts from this repo

Run these commands inside a native DSR job using the channel in
`rust-toolchain.toml`. Keep GitHub Actions disabled.

```bash
# One-time target install
rustup target add wasm32-unknown-unknown

# Verify ftui-web compiles for wasm32 (backend crate used by the web stack)
cargo check -p ftui-web --target wasm32-unknown-unknown

# Verify the in-tree WASM showcase target also compiles
cargo check -p ftui-showcase-wasm --target wasm32-unknown-unknown

# Optional: emit ftui-web release artifacts into target/wasm32-unknown-unknown/release/deps/
cargo build -p ftui-web --target wasm32-unknown-unknown --release

# Build both real browser packages and a self-contained site into a NEW directory
bash build-wasm.sh /absolute/new-browser-build
# Serve the completed site (not the Rust checkout)
python3 -m http.server --directory /absolute/new-browser-build/site 8080
```

The helper requires Cargo, Python 3.11+, curl, tar and `wasm-bindgen` matching
both checked-in locks (currently `0.2.128`). Install the CLI through Cargo on the
DSR host when needed. It builds the current showcase and renderer source at
`88b402b8be9c70a4405895d4172e449940cab2fe`, verifies the source archive SHA-256,
and applies `crates/ftui-showcase-wasm/renderer.lock` to that isolated source
tree. Both builds use the current repository's exact toolchain pin and
`--locked`. Cargo profile configuration supplies the WASM size override;
the helper never rewrites source manifests or removes previous outputs.

The output retains source, licenses, dependency locks, tool versions and
`source-inputs.json`. Its `site/` directory contains the host, font, text assets,
and both JS/WASM packages together. The host verifies package bytes against
`pkg/manifest.json` before execution and checks the renderer API contract.
Deploy the complete `site/` directory together. Integrity checks detect mixed or
corrupt packages; they do not authenticate a manifest or prevent a coherent
rollback of an entire site. The local host forwards input immediately into the
bounded runner instead of accumulating it in the historical renderer's
drop-oldest queue. It bounds each producer input object's combined text to
768 KiB of UTF-8 before encoding, cancels oversized compositions, and reports rejected input without
printing its contents. Accepted input remaining after quit is downloadable;
later rejections leave that recovery link intact. The standalone renderer's
queue policy, grapheme transport and full browser/device matrix remain open.

For a real browser smoke test, launch Chrome with a retained profile and CDP
endpoint on the DSR host, then use Node 22+ directly (no automation package):

```text
node scripts/browser_showcase_smoke.mjs SITE NEW_EVIDENCE_DIR CDP_URL
```

The test exercises actual rendering startup/resize, quit-tail recovery and a
keyboard-activated browser download, plus missing/corrupt/wrong-revision/ABI
package failures. Input checks cover a 4,098-event burst, partial composition
admission, exact UTF-8 byte boundaries, capacity drain/retry frames, oversized
IME cancellation and visible rejection with logging off. It asserts visible
compositor pixels and retains screenshots and browser observations. Synthetic DOM inputs
do not establish physical keyboard/IME/mobile behavior; inspect the recorded
renderer backend before making GPU claims.

For consumers that want a zero-dependency DOM `<pre>` text host without the
WebGPU/Canvas2D renderer bundle, `crates/ftui-showcase-wasm/sdk/showcase-host.js`
(paired with `showcase-host.css`) decodes `ShowcaseRunner.takeFlatPatches()` into
a `<pre class="ftui-grid">` of `<span>` cells (rendering one Unicode codepoint per
cell with `0x25A1` fallback for multi-codepoint graphemes, per
`docs/spec/wasm-showcase-runner-contract.md`), encodes keyboard/mouse/wheel/paste/focus
events, and drives DPR/zoom resize repaint boundaries. Run its pure-Node unit
tests with `node --test crates/ftui-showcase-wasm/sdk/showcase-host.test.mjs`.

### 2. Initialize in a Next.js client component

The snippet below assumes your website repo already contains a `FrankenTermWeb`
bundle at `@/wasm/frankenterm-web/FrankenTerm` from the pinned build above.

```tsx
"use client";

import { useEffect, useRef } from "react";
import init, { FrankenTermWeb } from "@/wasm/frankenterm-web/FrankenTerm";

export function TerminalCanvas() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    let term: FrankenTermWeb | null = null;
    let disposed = false;

    (async () => {
      await init(); // loads FrankenTerm_bg.wasm
      if (disposed || !canvasRef.current) return;

      term = new FrankenTermWeb();
      await term.init(canvasRef.current, undefined);

      // Contract guardrail: pin to the stable FrankenTermJS API line.
      const contract = term.apiContract();
      if (contract.apiLine !== "frankenterm-js" || !String(contract.apiVersion).startsWith("1.")) {
        throw new Error(`Unsupported FrankenTerm API: ${contract.apiLine}@${contract.apiVersion}`);
      }

      term.resize(120, 40);
      term.render();
    })();

    return () => {
      disposed = true;
      term?.destroy();
    };
  }, []);

  return <canvas ref={canvasRef} className="h-full w-full" />;
}
```

### 3. Feed input and patches

- Forward normalized DOM input with `term.input(...)`.
- For ftui-driven rendering, call `term.applyPatch(patch)` followed by `term.render()`.
- For ANSI-stream mode, call `term.feed(data)` followed by `term.render()`.
- Clipboard integration (browser-safe flow):
  - Copy: call `term.copySelection()` and write the returned text via `navigator.clipboard.writeText(...)` inside a trusted user gesture.
  - Paste: read text from a DOM `paste` event (`event.clipboardData`) and call `term.pasteText(text)` (or `term.input({ kind: "paste", data: text })`).
  - Chrome/Safari/Firefox all gate clipboard APIs behind user gesture/permission rules, so keep clipboard read/write in host JS.

This host integration is separate from native terminal clipboard commands.
`Cmd::set_clipboard(text)` and `Cmd::get_clipboard()` use OSC 52 through the
native runtime's `TerminalWriter`; replies arrive as `Event::Clipboard` when
the terminal permits a read. Multiplexer sessions disable this output by
default. `FTUI_OSC52_CLIPBOARD=1` opts in while retaining tmux/screen wrapping;
the multiplexer must also allow passthrough. `FTUI_OSC52_CLIPBOARD=0` disables
output. Neither command bypasses browser clipboard permissions.

Minimal keyboard event forwarding example:

```ts
term.input({
  kind: "key",
  phase: "down",
  key: event.key,
  code: event.code,
  repeat: event.repeat,
  mods: {
    shift: event.shiftKey,
    ctrl: event.ctrlKey,
    alt: event.altKey,
    meta: event.metaKey,
  },
});
```

### 4. Current `ftui-web` status (important)

`ftui-web` currently provides the WASM-friendly backend core in Rust, but does
not yet expose a public `wasm-bindgen` JS wrapper by itself. The browser-facing
`FrankenTermWeb` wrapper remains adjacent/out-of-tree from this checkout.

Do **not** embed xterm.js as a fallback for this integration path.

## Add The Dependency

The examples in this guide target 0.9.0 through the `ftui` facade:

```toml
[dependencies]
ftui = "=0.9.0"
```

To use a local checkout instead, replace that dependency with a path. Adjust
the path to match your directory layout:

```toml
[dependencies]
ftui = { path = "../frankentui/crates/ftui" }
```

The default features (`runtime`, `extras`, `backend`, `bidi`) are what the examples in
this guide assume. `App::run()` selects native `ftui-tty` on Unix and Crossterm
elsewhere when `backend` is enabled; `ftui::DEFAULT_BACKEND` names the selection.
Headless, WASM, or custom-backend consumers
should use `default-features = false, features = ["runtime"]`, in which case
`App::run()` returns an `Unsupported` error naming the feature to enable.

| Feature | Default | Enables |
|---|---|---|
| `runtime` | Yes | Elm-style `App` / `Program` loop, `Cmd`, subscriptions, and `TerminalWriter` (`ftui-runtime`) |
| `extras` | Yes | Feature-gated add-ons (`ftui-extras`: markdown, syntax, canvas, charts, mermaid, VFX) |
| `backend` | Yes | Both `native-backend` and `crossterm` so `App::run()` works out of the box |
| `native-backend` | Via `backend` | Native Unix `ftui-tty` backend (`termios`, `poll`, `SIGWINCH`, job-control cleanup) |
| `crossterm` | Via `backend` | Crossterm backend (`ftui-core/crossterm`, `ftui-runtime/crossterm-compat`) |
| `bidi` | Yes | UAX #9 bidirectional text layout in `ftui-widgets` |
| `serde` | No | Load and save `KeyMap` bindings as TOML/JSON (`ftui-core/serde`) |
| `simd` | No | Opt-in portable-SIMD printable-ASCII width fast path (`ftui-core/simd`) |
| `experimental` | No | Internal quarantined research modules (unsupported; see README) |

If you only want a small slice, you can depend on individual crates directly:

```toml
[dependencies]
ftui-core = "=0.9.0"
ftui-runtime = "=0.9.0"
ftui-render = "=0.9.0"
ftui-widgets = "=0.9.0"
```

For local development, replace each version with a path to its directory under
`../frankentui/crates/`. Direct `ftui-runtime` dependencies have no default
terminal backend; enable `native-backend` on Unix or `crossterm-compat` for
Crossterm if you need `App::run()`.

## Minimal Inline App (Copy/Paste)

This is adapted from `crates/ftui-harness/examples/minimal.rs` but written
against the `ftui` facade so you can depend on a single crate. The block below
is byte-for-byte `crates/ftui/examples/getting_started.rs`, which the facade's
tests compile (`cargo run -p ftui --example getting_started` runs it from the
repository).

```rust
use std::cell::RefCell;
use std::time::Duration;

use ftui::core::event::{Event, KeyCode, KeyEventKind, Modifiers};
use ftui::render::frame::Frame;
use ftui::runtime::{Every, Subscription};
use ftui::widgets::log_viewer::{LogViewer, LogViewerState};
use ftui::{App, Cmd, FrameExt, Model, ScreenMode};

struct Harness {
    log: LogViewer,
    state: RefCell<LogViewerState>,
}

enum Msg {
    Key(ftui::KeyEvent),
    Tick,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(k) => Msg::Key(k),
            _ => Msg::Tick,
        }
    }
}

impl Model for Harness {
    type Message = Msg;

    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::none()
    }

    fn update(&mut self, msg: Msg) -> Cmd<Self::Message> {
        match msg {
            Msg::Key(k) if k.kind == KeyEventKind::Press => {
                if (k.modifiers.contains(Modifiers::CTRL) && k.code == KeyCode::Char('c'))
                    || k.code == KeyCode::Char('q')
                {
                    return Cmd::quit();
                }
                match k.code {
                    KeyCode::Up => self.log.scroll_up(1),
                    KeyCode::Down => self.log.scroll_down(1),
                    _ => self.log.push(format!("Key: {:?}", k.code)),
                }
            }
            Msg::Tick => self.log.push("Tick..."),
            _ => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        frame.render_stateful_widget(&self.log, frame.area(), &mut self.state.borrow_mut());
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        vec![Box::new(Every::new(Duration::from_secs(1), || Msg::Tick))]
    }
}

fn main() -> ftui::Result<()> {
    let mut log = LogViewer::new(1000);
    log.push("Started. Press q or Ctrl+C to quit.");

    App::new(Harness {
        log,
        state: RefCell::new(LogViewerState::default()),
    })
    .screen_mode(ScreenMode::Inline { ui_height: 5 })
    .run()?;

    Ok(())
}
```

Run it:

```bash
cargo run
```

## Common Patterns

### Inline UI + Scrolling Logs

- Inline mode keeps normal terminal scrollback intact.
- To write to scrollback from your model, use `Cmd::log("...")`.
- To render a scrolling log panel inside the UI region, use `LogViewer`.

### Streaming Output

See `crates/ftui-harness/examples/streaming.rs` for a reference pattern:

```bash
cargo run -p ftui-harness --example streaming
```

### Interactive Input

The runtime delivers `Event::Key`, `Event::Mouse`, and friends via your message
type (`impl From<Event> for Msg`). A typical input flow is:

- Track input state in your `Model` (cursor/selection/history).
- Handle key events in `update()`.
- Render an input widget in `view()`.

## Troubleshooting

### Terminal Looks Corrupted After A Crash

FrankenTUI uses RAII teardown (`TerminalSession`) to restore state, but if you
force-kill the process your terminal may need a reset:

```bash
reset
```

### Nightly Is Required

If you see `-Z is only accepted on the nightly compiler`, install nightly:

```bash
rustup toolchain install nightly
```

### One-Writer Rule

Only one component should own terminal output. If you need to emit logs,
prefer `Cmd::log` so the runtime can keep inline mode correct.

See [one-writer-rule.md](one-writer-rule.md).

## Examples Index

All of these are runnable and kept aligned with the repo's current APIs:

- [`crates/ftui/examples/getting_started.rs`](../crates/ftui/examples/getting_started.rs) (the inline app from this guide, via the `ftui` facade)
- [`crates/ftui/examples/minimal_inline.rs`](../crates/ftui/examples/minimal_inline.rs) (the README's front-page inline counter)
- [`crates/ftui/examples/agent_shell.rs`](../crates/ftui/examples/agent_shell.rs) (canonical inline agent shell + subprocess streaming, with `--pty`)
- [`crates/ftui-harness/examples/minimal.rs`](../crates/ftui-harness/examples/minimal.rs) (hello world)
- [`crates/ftui-harness/examples/streaming.rs`](../crates/ftui-harness/examples/streaming.rs) (streaming output + inline UI)
- [`crates/ftui-harness/examples/counter.rs`](../crates/ftui-harness/examples/counter.rs) (state updates)
- [`crates/ftui-harness/examples/layout.rs`](../crates/ftui-harness/examples/layout.rs) (layout composition)
- [`crates/ftui-harness/examples/modal.rs`](../crates/ftui-harness/examples/modal.rs) (modal patterns)
- [`crates/ftui-demo-showcase/src/screens/`](../crates/ftui-demo-showcase/src/screens/) (45 showcase screens covering widgets, layout, forms, markdown, charts, and VFX)

## Next Steps & Further Reading

- [Agent Harness Tutorial](tutorials/agent-harness.md)
- [One-Writer Rule](one-writer-rule.md)
- [Terminal Compatibility Matrix](compat-matrix.md)
- [Windows Support & Scope](WINDOWS.md)
- [Architecture Decision Records](adr/README.md)
