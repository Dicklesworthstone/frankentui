//! The README's API snippets, compiled against the published facade and
//! checked byte-for-byte against `README.md`, so the two cannot drift apart
//! silently. Each snippet lives between `// README-SNIPPET: <name>` and
//! `// README-SNIPPET-END: <name>` markers; the test dedents that region and
//! asserts the README contains it verbatim. Change the code here first, run
//! `cargo fmt`, then paste the formatted region into the README block.
//!
//! The Minimal API Example is checked against `examples/minimal_inline.rs`
//! (which `scripts/consumer_smoke_e2e.sh` also runs under a real PTY).

#![cfg(feature = "runtime")]

use std::cell::RefCell;
use std::time::Duration;

use ftui::layout::{Constraint, Flex};
use ftui::prelude::*;
use ftui::runtime::{EffectQueueConfig, ProgramConfig, RolloutPolicy, RuntimeLane};
use ftui::widgets::list::{List, ListState};
use ftui::widgets::paragraph::Paragraph;

const README: &str = include_str!("../../../README.md");
const SELF: &str = include_str!("readme_snippets.rs");

/// Extract the dedented text between the markers for `name`.
fn snippet(name: &str) -> String {
    let start_marker = format!("// README-SNIPPET: {name}\n");
    let end_marker = format!("// README-SNIPPET-END: {name}");
    let start = SELF
        .find(&start_marker)
        .unwrap_or_else(|| panic!("missing start marker for `{name}`"))
        + start_marker.len();
    let end = SELF[start..]
        .find(&end_marker)
        .unwrap_or_else(|| panic!("missing end marker for `{name}`"))
        + start;
    let mut lines: Vec<&str> = SELF[start..end].lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                ""
            } else {
                &l[indent..]
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Assert the README carries the snippet (one or more regions joined by a
/// blank line) exactly as compiled here.
fn assert_in_readme(regions: &[&str]) {
    let text = regions
        .iter()
        .map(|r| snippet(r))
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(
        README.contains(&text),
        "README.md drifted from the compiled snippet {regions:?}; expected this block verbatim:\n\
         ```rust\n{text}\n```"
    );
}

#[test]
fn readme_minimal_example_is_the_shipped_example() {
    const EXAMPLE: &str = include_str!("../examples/minimal_inline.rs");
    assert!(
        README.contains(EXAMPLE.trim_end()),
        "README Minimal API Example must equal crates/ftui/examples/minimal_inline.rs"
    );
}

// ---------------------------------------------------------------------------
// Shared model types used by several snippets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Msg {
    Tick,
    Quit,
    LoadData,
    ConfigChanged(FileEvent),
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(k) if k.is_char('q') => Msg::Quit,
            _ => Msg::Tick,
        }
    }
}

// README-SNIPPET: stateful_struct
// State lives in your Model; `view(&self)` borrows it mutably through a RefCell
struct MyModel {
    items: Vec<String>,
    list_state: RefCell<ListState>,
}
// README-SNIPPET-END: stateful_struct

impl Default for MyModel {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            list_state: RefCell::new(ListState::default()),
        }
    }
}

impl Model for MyModel {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::LoadData => {
                self.items = (0..42).map(|i| format!("item {i}")).collect();
                Cmd::none()
            }
            Msg::Quit => Cmd::quit(),
            Msg::ConfigChanged(event) => Cmd::log(format!("config file {event:?}")),
            Msg::Tick => Cmd::none(),
        }
    }

    // README-SNIPPET: stateful_view
    fn view(&self, frame: &mut Frame) {
        let list = List::new(self.items.iter().map(String::as_str));
        frame.render_stateful_widget(&list, frame.area(), &mut self.list_state.borrow_mut());
    }
    // README-SNIPPET-END: stateful_view

    // README-SNIPPET: subscriptions
    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Msg>>> {
        vec![
            tick_every(Duration::from_millis(16), || Msg::Tick), // 60fps timer
            file_watcher("config.toml", Msg::ConfigChanged), // FileEvent::{Created, Modified, Removed}
        ]
    }
    // README-SNIPPET-END: subscriptions
}

struct Dashboard {
    sidebar: Paragraph<'static>,
    main_content: Paragraph<'static>,
}

impl Model for Dashboard {
    type Message = Msg;

    fn update(&mut self, _msg: Msg) -> Cmd<Msg> {
        Cmd::none()
    }

    // README-SNIPPET: composition
    fn view(&self, frame: &mut Frame) {
        let chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(30.0), Constraint::Percentage(70.0)])
            .split(frame.area());

        frame.render_widget(&self.sidebar, chunks[0]);
        frame.render_widget(&self.main_content, chunks[1]);
    }
    // README-SNIPPET-END: composition
}

// ---------------------------------------------------------------------------
// Snippets that only need to compile (they would open a terminal if run)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn evidence_sink_example(model: MyModel) -> std::io::Result<()> {
    // README-SNIPPET: evidence_sink
    use ftui::runtime::EvidenceSinkConfig;

    App::new(model)
        .with_evidence_sink(
            EvidenceSinkConfig::enabled_file("evidence.jsonl").with_flush_on_write(true),
        )
        .run()
    // README-SNIPPET-END: evidence_sink
}

// ---------------------------------------------------------------------------
// Snippets that run
// ---------------------------------------------------------------------------

#[test]
fn readme_model_snippets_match() {
    assert_in_readme(&["stateful_struct", "stateful_view"]);
    assert_in_readme(&["composition"]);
    assert_in_readme(&["subscriptions"]);
    assert_in_readme(&["evidence_sink"]);

    let mut dashboard = Dashboard {
        sidebar: Paragraph::new("Sidebar"),
        main_content: Paragraph::new("Main"),
    };
    let _ = dashboard.update(Msg::Tick);
    let subs = MyModel::default().subscriptions();
    assert_eq!(subs.len(), 2);
}

#[test]
fn readme_runtime_lanes_snippet() {
    // README-SNIPPET: runtime_lanes
    // Operator workflow: Off → Shadow → Evaluate → Enable → Monitor → Rollback
    let config = ProgramConfig::default()
        .with_lane(RuntimeLane::Structured) // Current execution backend
        .with_rollout_policy(RolloutPolicy::Shadow) // Shadow‑compare before enabling
        .with_env_overrides(); // FTUI_RUNTIME_LANE, FTUI_ROLLOUT_POLICY
    // README-SNIPPET-END: runtime_lanes
    let _ = config;
    assert_in_readme(&["runtime_lanes"]);
}

#[test]
fn readme_effect_queue_snippet() {
    // README-SNIPPET: effect_queue
    // Configure backpressure bounds
    let config = ProgramConfig::default().with_effect_queue(
        EffectQueueConfig::default()
            .with_enabled(true)
            .with_max_queue_depth(64), // Drop tasks beyond this depth
    );

    // Monitor queue health at runtime
    let snap = ftui::runtime::effect_system::queue_telemetry();
    // snap.enqueued, snap.processed, snap.dropped, snap.high_water, snap.in_flight
    // README-SNIPPET-END: effect_queue
    let _ = (config, snap.in_flight);
    assert_in_readme(&["effect_queue"]);
}

#[test]
fn readme_evidence_short_snippet() {
    use ftui::runtime::EvidenceSinkConfig;
    // README-SNIPPET: evidence_short
    let config = ProgramConfig::default()
        .with_evidence_sink(EvidenceSinkConfig::enabled_file("evidence.jsonl"));
    // README-SNIPPET-END: evidence_short
    let _ = config;
    assert_in_readme(&["evidence_short"]);
}

#[test]
fn readme_table_theme_snippet() {
    // README-SNIPPET: table_theme
    // Six built-in presets (Aurora, Graphite, Neon, Slate, Solar, Orchard), each a
    // complete set of header / stripe / selection effect rules
    let theme = TableTheme::preset(TablePresetId::Aurora);
    let resolver = theme.effect_resolver();
    // README-SNIPPET-END: table_theme
    let _ = resolver;
    assert_in_readme(&["table_theme"]);
}

#[test]
fn readme_stylesheet_snippet() {
    // README-SNIPPET: stylesheet
    use ftui::render::cell::PackedRgba;
    use ftui::style::stylesheet::StyleSheet;

    let sheet = StyleSheet::new();
    let heading = Style::new().bold().fg(PackedRgba::rgb(80, 160, 255));
    let error = Style::new().bold().fg(PackedRgba::rgb(220, 60, 60));
    sheet.define("heading", heading);
    sheet.define("error", error);
    sheet.define("muted", Style::new().fg(PackedRgba::rgb(128, 128, 128)));

    // Resolve by name anywhere in the widget tree; compose layers left to right
    let resolved = sheet.get_or_default("heading");
    let loud_error = sheet.compose(&["heading", "error"]);
    // README-SNIPPET-END: stylesheet
    let _ = (resolved, loud_error);
    assert!(sheet.contains("muted"));
    assert_in_readme(&["stylesheet"]);
}

#[test]
fn readme_lens_snippet() {
    // README-SNIPPET: lens
    use ftui::runtime::lens::{Lens, field_lens};

    struct Config {
        volume: u8,
        brightness: u8,
    }

    // A lens focuses on one part of a larger structure
    let volume = field_lens(|c: &Config| c.volume, |c: &mut Config, v| c.volume = v);

    // Laws: GetPut (setting what you just read is a no-op),
    //       PutGet (you read back exactly what you set)
    let mut config = Config {
        volume: 75,
        brightness: 50,
    };
    assert_eq!(volume.view(&config), 75);
    volume.set(&mut config, 100);
    assert_eq!(config.volume, 100);
    assert_eq!(config.brightness, 50); // other fields untouched
    // README-SNIPPET-END: lens
    assert_in_readme(&["lens"]);
}

#[test]
fn readme_persistence_snippet() {
    // README-SNIPPET: persistence
    use ftui::runtime::{PersistenceConfig, StateRegistry};
    use std::sync::Arc;

    // In-memory registry needs no feature; `StateRegistry::with_file(path)` (JSON on
    // disk, atomic writes) needs ftui-runtime's `state-persistence` feature.
    let registry = Arc::new(StateRegistry::in_memory());
    let config = ProgramConfig::default().with_persistence(
        PersistenceConfig::with_registry(registry)
            .auto_load(true)
            .auto_save(true)
            .checkpoint_every(Duration::from_secs(30)),
    );
    // README-SNIPPET-END: persistence
    let _ = config;
    assert_in_readme(&["persistence"]);
}

#[test]
fn readme_simulator_snippet() {
    // README-SNIPPET: simulator
    use ftui::runtime::ProgramSimulator;

    let mut sim = ProgramSimulator::new(MyModel::default());
    sim.init();
    sim.send(Msg::LoadData);
    sim.tick();

    // Capture rendered output without a terminal
    let frame = sim.capture_frame(80, 24);
    assert_eq!(frame.width(), 80);
    assert_eq!(sim.model().items.len(), 42);
    assert!(sim.is_running());
    // README-SNIPPET-END: simulator
    assert_in_readme(&["simulator"]);
}
