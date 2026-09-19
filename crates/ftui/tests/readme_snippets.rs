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
use ftui::runtime::{
    AccessibilityFrame, EffectQueueConfig, ProgramConfig, RolloutPolicy, RuntimeLane,
    ScreenReaderPolicy,
};
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

// README-SNIPPET: not_in_readme
// This region is deliberately absent from README.md; it proves the checker bites.
// README-SNIPPET-END: not_in_readme

/// A region the README does not carry must fail the identity check, so a
/// green run means every pinned block really is in the README.
#[test]
#[should_panic(expected = "drifted")]
fn readme_checker_fails_on_a_missing_snippet() {
    assert_in_readme(&["not_in_readme"]);
}

#[test]
fn readme_minimal_example_is_the_shipped_example() {
    const EXAMPLE: &str = include_str!("../examples/minimal_inline.rs");
    assert!(
        README.contains(EXAMPLE.trim_end()),
        "README Minimal API Example must equal crates/ftui/examples/minimal_inline.rs"
    );
}

/// `docs/getting-started.md`'s inline program is `examples/getting_started.rs`
/// (compiled with the facade's tests), so the guide cannot drift from a
/// program that builds.
#[test]
fn getting_started_example_is_the_shipped_example() {
    const GUIDE: &str = include_str!("../../../docs/getting-started.md");
    const EXAMPLE: &str = include_str!("../examples/getting_started.rs");
    assert!(
        GUIDE.contains(EXAMPLE.trim_end()),
        "docs/getting-started.md example must equal crates/ftui/examples/getting_started.rs"
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

impl MyModel {
    /// The README's harness fences build models through `MyModel::new`, which
    /// is the shape a consumer's own model would have.
    fn new() -> Self {
        Self::default()
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

/// A model that mirrors the runtime's screen-reader announcements into its
/// own state (the README's accessibility hook example).
struct Announcer {
    announcements: Vec<String>,
}

impl Model for Announcer {
    type Message = Msg;

    fn update(&mut self, _msg: Msg) -> Cmd<Msg> {
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        frame.render_widget(&Paragraph::new("hello"), frame.area());
    }

    // README-SNIPPET: accessibility_hook
    fn on_accessibility(&mut self, a11y: AccessibilityFrame<'_>) -> Cmd<Msg> {
        // Runs after each frame whose tree changed. Forward the bounded
        // announcements to a host bridge, a log, or an on-screen live region.
        self.announcements
            .extend(a11y.announcements.iter().map(|a| a.text.clone()));
        Cmd::none()
    }
    // README-SNIPPET-END: accessibility_hook
}

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
fn readme_accessibility_snippets() {
    use ftui::a11y::node::LiveRegion;
    use ftui::a11y::tree::{A11yTreeBuilder, AnnouncementReason, ScreenReaderAnnouncement};

    // README-SNIPPET: accessibility_config
    let config = ProgramConfig::default().with_accessibility(ScreenReaderPolicy::default());
    // README-SNIPPET-END: accessibility_config
    assert!(config.accessibility.is_some());
    assert!(ProgramConfig::default().accessibility.is_none());
    assert_in_readme(&["accessibility_config"]);
    assert_in_readme(&["accessibility_hook"]);

    // The hook receives the frame's tree and its announcements.
    let tree = A11yTreeBuilder::new().build();
    let announcements = [ScreenReaderAnnouncement {
        node_id: Some(7),
        urgency: LiveRegion::Polite,
        reason: AnnouncementReason::FocusChanged,
        text: "button: OK. focused".to_string(),
    }];
    let mut announcer = Announcer {
        announcements: Vec::new(),
    };
    let _ = announcer.on_accessibility(AccessibilityFrame {
        frame_idx: 1,
        tree: &tree,
        order: &[],
        announcements: &announcements,
        dropped: 0,
    });
    assert_eq!(announcer.announcements, ["button: OK. focused"]);
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

#[cfg(feature = "experimental")]
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

// ── Advanced Features ───────────────────────────────────────────────────
//
// The blocks below were fences the README carried but nothing compiled. Each
// one now either proves the API is real or fails the build; there is no third
// outcome where the README quietly describes something that does not exist.

#[test]
fn readme_hyperlink_snippet() {
    use ftui::render::cell::Cell;
    use ftui::render::frame::Frame;
    use ftui::render::grapheme_pool::GraphemePool;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 3, &mut pool);

    // README-SNIPPET: hyperlinks
    let link_id = frame.register_link("https://example.com");
    let mut cell = Cell::from_char('x');
    cell.attrs = cell.attrs.with_link(link_id);
    // Emits OSC 8 hyperlink sequences for supporting terminals
    // README-SNIPPET-END: hyperlinks

    assert_eq!(cell.attrs.link_id(), link_id);
    assert_in_readme(&["hyperlinks"]);
}

#[test]
fn readme_focus_graph_snippet() {
    use ftui::widgets::focus::{FocusManager, FocusNode, NavDirection};

    let input1_area = Rect::new(0, 0, 10, 1);
    let input2_area = Rect::new(0, 2, 10, 1);
    let mut focus = FocusManager::new();

    // README-SNIPPET: focus_graph
    // Declarative focus graph: FocusManager owns a FocusGraph of nodes and nav edges
    let graph = focus.graph_mut();
    let input1 = graph.insert(FocusNode::new(1, input1_area));
    let input2 = graph.insert(FocusNode::new(2, input2_area));
    graph.connect(input1, NavDirection::Next, input2); // Tab order

    // Navigation
    focus.focus_next(); // Tab
    focus.focus_prev(); // Shift+Tab
    // README-SNIPPET-END: focus_graph

    assert_ne!(input1, input2);
    assert_in_readme(&["focus_graph"]);
}

#[test]
fn readme_accessibility_program_config_snippet() {
    // README-SNIPPET: accessibility_program_config
    use ftui::{ProgramConfig, ScreenReaderPolicy};

    let config = ProgramConfig::default().with_accessibility(ScreenReaderPolicy::default());
    // README-SNIPPET-END: accessibility_program_config

    let _ = config;
    assert_in_readme(&["accessibility_program_config"]);
}

#[test]
fn readme_contrast_ratio_snippet() {
    use ftui::style::color::{Rgb, contrast_ratio};

    // README-SNIPPET: contrast_ratio
    let ratio = contrast_ratio(Rgb::new(220, 220, 220), Rgb::new(30, 30, 30));
    // WCAG AA: ratio ≥ 4.5 for normal text, ≥ 3.0 for large text
    // WCAG AAA: ratio ≥ 7.0 for normal text, ≥ 4.5 for large text
    // README-SNIPPET-END: contrast_ratio

    // Light-on-dark at these values clears AAA, which is what makes the
    // thresholds in the comment worth quoting next to the call.
    assert!(ratio > 7.0, "expected AAA contrast, got {ratio}");
    assert_in_readme(&["contrast_ratio"]);
}

#[test]
fn readme_queue_telemetry_snippet() {
    // README-SNIPPET: queue_telemetry
    let snap = ftui_runtime::effect_system::queue_telemetry();
    // QueueTelemetry {
    //   enqueued: 1042,          -- total tasks submitted
    //   processed: 1038,         -- total tasks completed
    //   dropped: 2,              -- tasks dropped (backpressure/shutdown)
    //   high_water: 12,          -- peak queue depth observed
    //   in_flight: 2,            -- currently executing
    // }
    // README-SNIPPET-END: queue_telemetry

    // The counters are monotonic, so processed can never outrun enqueued.
    assert!(snap.processed <= snap.enqueued);
    assert_in_readme(&["queue_telemetry"]);
}

// ── Harness claims from the comparison table ────────────────────────────
//
// "Shadow-run validation harness" and "Snapshot/time-travel harness" are rows
// in the README's How FrankenTUI Compares table, i.e. competitive claims. They
// are compiled here so the table cannot outlive the API it advertises.
// ftui-harness is a dev-dependency for exactly this reason.

#[test]
fn readme_shadow_run_snippet() {
    // README-SNIPPET: shadow_run
    use ftui_harness::{ShadowRun, ShadowRunConfig, ShadowVerdict};

    let config = ShadowRunConfig::new("migration_test", "tick_counter", 42).viewport(80, 24);
    let result = ShadowRun::compare(config, MyModel::new, |session| {
        session.init();
        session.tick();
        session.capture_frame();
    });
    assert_eq!(result.verdict, ShadowVerdict::Match);
    // README-SNIPPET-END: shadow_run

    assert_in_readme(&["shadow_run"]);
}

#[test]
fn readme_rollout_scorecard_snippet() {
    use ftui_harness::{ShadowRun, ShadowRunConfig};

    // The snippet's `min_shadow_scenarios(3)` means three matching scenarios
    // are what a Go verdict costs. Supplying exactly that makes the README's
    // `assert_eq!(.., RolloutVerdict::Go)` a claim this test actually proves,
    // rather than a line that merely type-checks.
    let shadow_results: Vec<_> = ["tick_counter", "resize", "quit"]
        .into_iter()
        .enumerate()
        .map(|(i, scenario)| {
            ShadowRun::compare(
                ShadowRunConfig::new("rollout_doc", scenario, 7 + i as u64).viewport(80, 24),
                MyModel::new,
                |session| {
                    session.init();
                    session.tick();
                    session.capture_frame();
                },
            )
        })
        .collect();

    // README-SNIPPET: rollout_scorecard
    use ftui_harness::{
        RolloutEvidenceBundle, RolloutScorecard, RolloutScorecardConfig, RolloutVerdict,
    };

    let mut scorecard =
        RolloutScorecard::new(RolloutScorecardConfig::default().min_shadow_scenarios(3));
    for shadow_result in shadow_results {
        scorecard.add_shadow_result(shadow_result);
    }
    assert_eq!(scorecard.evaluate(), RolloutVerdict::Go);

    // Machine-readable JSON evidence for CI gates
    let bundle = RolloutEvidenceBundle {
        scorecard: scorecard.summary(),
        queue_telemetry: Some(ftui_runtime::effect_system::queue_telemetry()),
        requested_lane: "structured".to_string(),
        resolved_lane: "structured".to_string(),
        rollout_policy: "shadow".to_string(),
    };
    println!("{}", bundle.to_json()); // Self-contained release decision artifact
    // README-SNIPPET-END: rollout_scorecard

    assert!(bundle.to_json().contains("\"requested_lane\""));
    assert_in_readme(&["rollout_scorecard"]);
}

#[test]
fn readme_time_travel_snippet() {
    use ftui::render::frame::Frame;
    use ftui::render::grapheme_pool::GraphemePool;
    use ftui_harness::time_travel::{FrameMetadata, TimeTravel};

    let mut pool = GraphemePool::new();
    let frame = Frame::new(20, 3, &mut pool);
    let (frame_number, render_time, frame_index) = (0_u64, std::time::Duration::ZERO, 0_usize);

    // README-SNIPPET: time_travel
    // Record frames for debugging (ftui-harness; delta-compressed ring of 256 frames)
    let mut history = TimeTravel::new(256);
    history.record(&frame.buffer, FrameMetadata::new(frame_number, render_time));

    // Replay
    let historical_frame = history.get(frame_index);
    // README-SNIPPET-END: time_travel

    assert!(
        historical_frame.is_some(),
        "the frame just recorded is there"
    );
    assert_in_readme(&["time_travel"]);
}

#[test]
fn readme_frame_arena_snippet() {
    use ftui::render::arena::FrameArena;

    let (done, total, value) = (3_u32, 10_u32, 42_u32);

    // Two regions rather than one: the borrows below end where the assertions
    // do, which is what lets `reset` take `&mut` straight afterwards. The
    // checker rejoins them with the blank line the README already has between
    // the allocation block and the frame boundary.
    // README-SNIPPET: frame_arena_alloc
    let mut arena = FrameArena::new(256 * 1024); // 256 KB initial

    // During frame rendering: bump-allocate transient strings
    let label: &str = arena.alloc_str(&format!("{done}/{total}"));
    let cell_text: &str = arena.alloc_fmt(format_args!("{value:>8}"));
    // README-SNIPPET-END: frame_arena_alloc

    assert_eq!(label, "3/10");
    assert_eq!(cell_text, "      42");

    // README-SNIPPET: frame_arena_reset
    // At frame boundary:
    arena.reset(); // O(1), no individual deallocations
    // README-SNIPPET-END: frame_arena_reset

    // The arena is still usable after the reset, which is the other half of
    // the claim: reset recycles the bump, it does not retire the allocator.
    assert_eq!(arena.alloc_str("reused"), "reused");
    assert_in_readme(&["frame_arena_alloc", "frame_arena_reset"]);
}

/// Every version the README quotes for a workspace crate must be this
/// workspace's version.
///
/// This exists because the drift is real and recent: a 0.8.0 -> 0.9.0 bump
/// updated all 20 manifests and four of the five README version references,
/// leaving `ftui-runtime = { version = "0.8" }` in the experimental-modules
/// snippet. A reader copying that line pins a version that no longer matches
/// the crate they are reading about, and nothing failed.
///
/// Both forms are accepted, since the README legitimately uses each:
/// a full `0.9.0` pin and a `0.9` minor-series requirement.
#[test]
fn readme_versions_match_the_workspace() {
    const VERSION: &str = env!("CARGO_PKG_VERSION");
    let (major_minor, _) = VERSION
        .rsplit_once('.')
        .expect("crate version should be major.minor.patch");

    let mut checked = 0;
    for (line_no, line) in README.lines().enumerate() {
        // `ftui... = "=0.9.0"` / `version = "0.9.0"` / `version = "0.9"`
        for quoted in line.split('"').skip(1).step_by(2) {
            let candidate = quoted.trim_start_matches('=');
            let looks_like_version = candidate
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
                && candidate.contains('.');
            if !looks_like_version || !line.contains("ftui") {
                continue;
            }
            assert!(
                candidate == VERSION || candidate == major_minor,
                "README.md:{} pins {candidate:?} for a workspace crate, but this \
                 workspace is {VERSION}. Line: {}",
                line_no + 1,
                line.trim()
            );
            checked += 1;
        }
    }

    // A silent zero would mean the scan stopped matching the README's shape and
    // this test had quietly stopped guarding anything.
    assert!(
        checked >= 2,
        "expected to find README version pins to check, found {checked}"
    );
}
