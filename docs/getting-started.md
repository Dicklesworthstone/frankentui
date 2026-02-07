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
- As of today, only `ftui-core`, `ftui-layout`, and `ftui-i18n` are published on crates.io.
  The rest of the stack (render/runtime/widgets/extras) should be consumed via workspace paths.

## Prereqs

- Rust nightly (required by `rust-toolchain.toml`)
- A terminal with basic ANSI support (tmux/zellij are supported)

## Add The Dependency

Right now only a subset of crates are published on crates.io (`ftui-core`,
`ftui-layout`, `ftui-i18n`). For the full stack (runtime/render/widgets),
prefer a workspace path dependency:

```toml
[dependencies]
ftui = { path = "../frankentui/crates/ftui" }
```

If you only want a small slice, you can depend on internal crates directly
via path as well (same repo):

```toml
[dependencies]
ftui-core = { path = "../frankentui/crates/ftui-core" }
ftui-runtime = { path = "../frankentui/crates/ftui-runtime" }
ftui-render = { path = "../frankentui/crates/ftui-render" }
ftui-widgets = { path = "../frankentui/crates/ftui-widgets" }
```

## Minimal Inline App (Copy/Paste)

This is adapted from `crates/ftui-harness/examples/minimal.rs` but written
against the `ftui` facade so you can depend on a single crate.

```rust
use std::time::Duration;

use ftui::core::event::{Event, KeyCode, KeyEventKind, Modifiers};
use ftui::core::geometry::Rect;
use ftui::render::frame::Frame;
use ftui::runtime::{Every, Subscription};
use ftui::widgets::StatefulWidget;
use ftui::widgets::log_viewer::{LogViewer, LogViewerState};
use ftui::{App, Cmd, Model, ScreenMode};

struct Harness {
    log: LogViewer,
    state: LogViewerState,
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
                if k.modifiers.contains(Modifiers::CTRL) && k.code == KeyCode::Char('c') {
                    return Cmd::quit();
                }
                self.log.push(format!("Key: {:?}", k.code));
            }
            Msg::Tick => self.log.push("Tick..."),
            _ => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let mut state = self.state.clone();
        self.log.render(area, frame, &mut state);
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        vec![Box::new(Every::new(Duration::from_secs(1), || Msg::Tick))]
    }
}

fn main() -> ftui::Result<()> {
    let mut log = LogViewer::new(1000);
    log.push("Started. Press Ctrl+C to quit.");

    App::new(Harness {
        log,
        state: LogViewerState::default(),
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

See `docs/one-writer-rule.md`.

## Examples Index

All of these are runnable and kept aligned with the repo's current APIs:

- `crates/ftui-harness/examples/minimal.rs` (hello world)
- `crates/ftui-harness/examples/streaming.rs` (streaming output + inline UI)
- `crates/ftui-harness/examples/counter.rs` (state updates)
- `crates/ftui-harness/examples/layout.rs` (layout composition)
- `crates/ftui-harness/examples/modal.rs` (modal patterns)

Tutorial:
- `docs/tutorials/agent-harness.md`
