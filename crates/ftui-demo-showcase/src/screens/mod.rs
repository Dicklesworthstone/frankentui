#![forbid(unsafe_code)]

//! Screen modules for the demo showcase.
//!
//! Each screen implements the [`Screen`] trait and can be navigated to via the
//! tab bar or number keys.

pub mod accessibility_panel;
pub mod action_timeline;
pub mod advanced_features;
pub mod advanced_text_editor;
pub mod async_tasks;
pub mod code_explorer;
pub mod command_palette_lab;
pub mod dashboard;
pub mod data_viz;
pub mod determinism_lab;
pub mod drag_drop;
pub mod explainability_cockpit;
pub mod file_browser;
pub mod form_validation;
pub mod forms_input;
pub mod hyperlink_playground;
pub mod i18n_demo;
pub mod inline_mode_story;
pub mod intrinsic_sizing;
pub mod layout_inspector;
pub mod layout_lab;
pub mod log_search;
pub mod macro_recorder;
pub mod markdown_live_editor;
pub mod markdown_rich_text;
pub mod mouse_playground;
pub mod notifications;
pub mod performance;
pub mod performance_hud;
pub mod quake;
pub mod responsive_demo;
pub mod shakespeare;
pub mod snapshot_player;
pub mod table_theme_gallery;
pub mod terminal_capabilities;
pub mod theme_studio;
pub mod virtualized_search;
pub mod visual_effects;
pub mod voi_overlay;
pub mod widget_builder;
pub mod widget_gallery;

use std::sync::OnceLock;

use ftui_core::event::Event;
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;

use crate::app::ScreenId;

/// High-level IA categories for the demo showcase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenCategory {
    Tour,
    Core,
    Visuals,
    Interaction,
    Text,
    Systems,
}

impl ScreenCategory {
    pub const ALL: &'static [ScreenCategory] = &[
        ScreenCategory::Tour,
        ScreenCategory::Core,
        ScreenCategory::Visuals,
        ScreenCategory::Interaction,
        ScreenCategory::Text,
        ScreenCategory::Systems,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ScreenCategory::Tour => "Tour",
            ScreenCategory::Core => "Core",
            ScreenCategory::Visuals => "Visuals",
            ScreenCategory::Interaction => "Interaction",
            ScreenCategory::Text => "Text",
            ScreenCategory::Systems => "Systems",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            ScreenCategory::Tour => "Tour",
            ScreenCategory::Core => "Core",
            ScreenCategory::Visuals => "Visuals",
            ScreenCategory::Interaction => "Interact",
            ScreenCategory::Text => "Text",
            ScreenCategory::Systems => "Systems",
        }
    }
}

/// Registry metadata describing a demo screen.
#[derive(Debug, Clone, Copy)]
pub struct ScreenMeta {
    pub id: ScreenId,
    pub title: &'static str,
    pub short_label: &'static str,
    pub category: ScreenCategory,
    pub palette_tags: &'static [&'static str],
    pub blurb: &'static str,
    pub default_hotkey: Option<&'static str>,
    pub tour_step_hint: Option<&'static str>,
}

/// Screen Registry: single source of truth for screen ordering + metadata.
pub const SCREEN_REGISTRY: &[ScreenMeta] = &[
    ScreenMeta {
        id: ScreenId::GuidedTour,
        title: "Guided Tour",
        short_label: "Tour",
        category: ScreenCategory::Tour,
        palette_tags: &["tour", "storyboard", "autoplay"],
        blurb: "Cinematic auto-play tour across key screens.",
        default_hotkey: Some("1"),
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::Dashboard,
        title: "Dashboard",
        short_label: "Dash",
        category: ScreenCategory::Tour,
        palette_tags: &["overview", "tour", "widgets"],
        blurb: "Cinematic overview of key features and live tiles.",
        default_hotkey: Some("2"),
        tour_step_hint: Some("Start here"),
    },
    ScreenMeta {
        id: ScreenId::Shakespeare,
        title: "Shakespeare",
        short_label: "Shakes",
        category: ScreenCategory::Text,
        palette_tags: &["search", "text", "highlight"],
        blurb: "Live search over Shakespeare with animated highlights.",
        default_hotkey: Some("3"),
        tour_step_hint: Some("Live search + highlights"),
    },
    ScreenMeta {
        id: ScreenId::CodeExplorer,
        title: "Code Explorer",
        short_label: "Code",
        category: ScreenCategory::Text,
        palette_tags: &["code", "explorer", "syntax"],
        blurb: "Code browser with pane routing and syntax preview.",
        default_hotkey: Some("4"),
        tour_step_hint: Some("Live code + syntax"),
    },
    ScreenMeta {
        id: ScreenId::WidgetGallery,
        title: "Widget Gallery",
        short_label: "Widgets",
        category: ScreenCategory::Core,
        palette_tags: &["widgets", "catalog", "layout"],
        blurb: "Library of core widgets in a compact gallery.",
        default_hotkey: Some("5"),
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::LayoutLab,
        title: "Layout Lab",
        short_label: "Layout",
        category: ScreenCategory::Core,
        palette_tags: &["layout", "flex", "grid"],
        blurb: "Hands-on layout experiments with constraints.",
        default_hotkey: Some("6"),
        tour_step_hint: Some("Constraints in motion"),
    },
    ScreenMeta {
        id: ScreenId::FormsInput,
        title: "Forms & Input",
        short_label: "Forms",
        category: ScreenCategory::Interaction,
        palette_tags: &["forms", "input", "controls"],
        blurb: "Form fields, validation cues, and input widgets.",
        default_hotkey: Some("7"),
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::DataViz,
        title: "Data Viz",
        short_label: "DataViz",
        category: ScreenCategory::Visuals,
        palette_tags: &["charts", "graphs", "visuals"],
        blurb: "Dense charts and small-multiple visualizations.",
        default_hotkey: Some("8"),
        tour_step_hint: Some("Dense charts + metrics"),
    },
    ScreenMeta {
        id: ScreenId::FileBrowser,
        title: "File Browser",
        short_label: "Files",
        category: ScreenCategory::Interaction,
        palette_tags: &["files", "tree", "navigation"],
        blurb: "File tree with previews and pane routing.",
        default_hotkey: Some("9"),
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::AdvancedFeatures,
        title: "Advanced",
        short_label: "Adv",
        category: ScreenCategory::Core,
        palette_tags: &["advanced", "widgets", "patterns"],
        blurb: "Advanced widget patterns and composite layouts.",
        default_hotkey: Some("0"),
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::TableThemeGallery,
        title: "Table Theme Gallery",
        short_label: "Tables",
        category: ScreenCategory::Visuals,
        palette_tags: &["tables", "theme", "presets"],
        blurb: "Preset gallery for TableTheme across widget + markdown tables.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::TerminalCapabilities,
        title: "Terminal Capabilities",
        short_label: "Caps",
        category: ScreenCategory::Systems,
        palette_tags: &["terminal", "capabilities", "compat"],
        blurb: "Terminal capability detection and feature matrix.",
        default_hotkey: Some("0"),
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::MacroRecorder,
        title: "Macro Recorder",
        short_label: "Macro",
        category: ScreenCategory::Interaction,
        palette_tags: &["macro", "record", "replay"],
        blurb: "Record, edit, and replay input macros.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::Performance,
        title: "Performance",
        short_label: "Perf",
        category: ScreenCategory::Systems,
        palette_tags: &["performance", "metrics", "budget"],
        blurb: "Render performance metrics and budgets.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::MarkdownRichText,
        title: "Markdown",
        short_label: "MD",
        category: ScreenCategory::Text,
        palette_tags: &["markdown", "render", "text"],
        blurb: "Markdown rendering with styling and layout.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::VisualEffects,
        title: "Visual Effects",
        short_label: "VFX",
        category: ScreenCategory::Visuals,
        palette_tags: &["effects", "particles", "animation"],
        blurb: "High-performance visual effects playground.",
        default_hotkey: None,
        tour_step_hint: Some("Braille plasma + FX"),
    },
    ScreenMeta {
        id: ScreenId::ResponsiveDemo,
        title: "Responsive Layout",
        short_label: "Resp",
        category: ScreenCategory::Core,
        palette_tags: &["responsive", "layout", "breakpoints"],
        blurb: "Responsive layout behavior across sizes.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::LogSearch,
        title: "Log Search",
        short_label: "Logs",
        category: ScreenCategory::Text,
        palette_tags: &["logs", "search", "filter"],
        blurb: "Search and filter logs with live updates.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::Notifications,
        title: "Notifications",
        short_label: "Notify",
        category: ScreenCategory::Interaction,
        palette_tags: &["notifications", "toast", "alerts"],
        blurb: "Toast notifications and transient UI patterns.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::ActionTimeline,
        title: "Action Timeline",
        short_label: "Timeline",
        category: ScreenCategory::Systems,
        palette_tags: &["timeline", "events", "audit"],
        blurb: "Event stream and action timeline viewer.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::IntrinsicSizing,
        title: "Intrinsic Sizing",
        short_label: "Sizing",
        category: ScreenCategory::Core,
        palette_tags: &["layout", "intrinsic", "measure"],
        blurb: "Intrinsic sizing and content measurement.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::LayoutInspector,
        title: "Layout Inspector",
        short_label: "Inspect",
        category: ScreenCategory::Core,
        palette_tags: &["layout", "inspector", "constraints"],
        blurb: "Constraint solver visual inspector.",
        default_hotkey: None,
        tour_step_hint: Some("Layout solver visual"),
    },
    ScreenMeta {
        id: ScreenId::AdvancedTextEditor,
        title: "Advanced Text Editor",
        short_label: "Editor",
        category: ScreenCategory::Text,
        palette_tags: &["editor", "text", "search"],
        blurb: "Advanced multi-line editor with search.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::MousePlayground,
        title: "Mouse Playground",
        short_label: "Mouse",
        category: ScreenCategory::Interaction,
        palette_tags: &["mouse", "hit-test", "interaction"],
        blurb: "Mouse and hit-test interactions.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::FormValidation,
        title: "Form Validation",
        short_label: "Validate",
        category: ScreenCategory::Interaction,
        palette_tags: &["validation", "forms", "errors"],
        blurb: "Form validation states and error cues.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::VirtualizedSearch,
        title: "Virtualized Search",
        short_label: "VirtSearch",
        category: ScreenCategory::Systems,
        palette_tags: &["virtualized", "list", "performance"],
        blurb: "Virtualized list with fast search.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::AsyncTasks,
        title: "Async Tasks",
        short_label: "Tasks",
        category: ScreenCategory::Systems,
        palette_tags: &["async", "jobs", "queue"],
        blurb: "Async tasks and job queue visualization.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::ThemeStudio,
        title: "Theme Studio",
        short_label: "Themes",
        category: ScreenCategory::Visuals,
        palette_tags: &["theme", "colors", "design"],
        blurb: "Live theme and palette studio.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::SnapshotPlayer,
        title: "Time-Travel Studio",
        short_label: "TimeTravel",
        category: ScreenCategory::Visuals,
        palette_tags: &["replay", "snapshot", "diff"],
        blurb: "Time-travel snapshots with replay controls.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::PerformanceHud,
        title: "Performance Challenge",
        short_label: "PerfChal",
        category: ScreenCategory::Systems,
        palette_tags: &["performance", "budget", "degradation", "stress", "tiers"],
        blurb: "Stress harness for degradation tiers and budget recovery.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::ExplainabilityCockpit,
        title: "Explainability Cockpit",
        short_label: "Explain",
        category: ScreenCategory::Systems,
        palette_tags: &["evidence", "bayes", "bocpd", "budget", "diff"],
        blurb: "Diff, resize, and budget evidence in one cockpit.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::I18nDemo,
        title: "i18n Stress Lab",
        short_label: "i18n",
        category: ScreenCategory::Text,
        palette_tags: &["i18n", "unicode", "width"],
        blurb: "International text and width edge cases.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::VoiOverlay,
        title: "VOI Overlay",
        short_label: "VOI",
        category: ScreenCategory::Systems,
        palette_tags: &["voi", "bayes", "overlay"],
        blurb: "Value-of-information overlay and evidence.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::InlineModeStory,
        title: "Inline Mode",
        short_label: "Inline",
        category: ScreenCategory::Tour,
        palette_tags: &["inline", "scrollback", "chrome"],
        blurb: "Inline mode story and scrollback preservation.",
        default_hotkey: None,
        tour_step_hint: Some("Inline mode value"),
    },
    ScreenMeta {
        id: ScreenId::AccessibilityPanel,
        title: "Accessibility",
        short_label: "A11y",
        category: ScreenCategory::Systems,
        palette_tags: &["a11y", "accessibility", "contrast"],
        blurb: "Accessibility settings and telemetry.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::WidgetBuilder,
        title: "Widget Builder",
        short_label: "Builder",
        category: ScreenCategory::Core,
        palette_tags: &["widgets", "builder", "sandbox"],
        blurb: "Interactive widget builder sandbox.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::CommandPaletteLab,
        title: "Command Palette Evidence Lab",
        short_label: "Palette",
        category: ScreenCategory::Interaction,
        palette_tags: &["command", "palette", "ranking"],
        blurb: "Command palette ranking with evidence details.",
        default_hotkey: None,
        tour_step_hint: None,
    },
    ScreenMeta {
        id: ScreenId::DeterminismLab,
        title: "Determinism Lab",
        short_label: "Determinism",
        category: ScreenCategory::Systems,
        palette_tags: &["determinism", "checksum", "replay"],
        blurb: "Checksum equivalence and determinism checks.",
        default_hotkey: None,
        tour_step_hint: Some("Checksum proof"),
    },
    ScreenMeta {
        id: ScreenId::HyperlinkPlayground,
        title: "Hyperlink Playground",
        short_label: "Links",
        category: ScreenCategory::Interaction,
        palette_tags: &["links", "osc8", "hit-test"],
        blurb: "OSC-8 hyperlink playground and hit regions.",
        default_hotkey: None,
        tour_step_hint: None,
    },
];

/// Return the full registry (ordered).
pub fn screen_registry() -> &'static [ScreenMeta] {
    SCREEN_REGISTRY
}

/// Lazily computed ordered screen IDs (derived from the registry).
pub fn screen_ids() -> &'static [ScreenId] {
    static IDS: OnceLock<Vec<ScreenId>> = OnceLock::new();
    IDS.get_or_init(|| SCREEN_REGISTRY.iter().map(|meta| meta.id).collect())
}

/// Lookup a screen by ID in the registry.
pub fn screen_meta(id: ScreenId) -> &'static ScreenMeta {
    SCREEN_REGISTRY
        .iter()
        .find(|meta| meta.id == id)
        .unwrap_or(&SCREEN_REGISTRY[0])
}

/// Convenience: category for a screen.
pub fn screen_category(id: ScreenId) -> ScreenCategory {
    screen_meta(id).category
}

/// Convenience: title for a screen.
pub fn screen_title(id: ScreenId) -> &'static str {
    screen_meta(id).title
}

/// Convenience: short label for tabs.
pub fn screen_tab_label(id: ScreenId) -> &'static str {
    screen_meta(id).short_label
}

/// Index of a screen in the registry.
pub fn screen_index(id: ScreenId) -> usize {
    SCREEN_REGISTRY
        .iter()
        .position(|meta| meta.id == id)
        .unwrap_or(0)
}

/// Iterate screens in a given category, preserving registry order.
pub fn screens_in_category(category: ScreenCategory) -> impl Iterator<Item = &'static ScreenMeta> {
    SCREEN_REGISTRY
        .iter()
        .filter(move |meta| meta.category == category)
}

/// Count screens in a given category.
pub fn screen_count_in_category(category: ScreenCategory) -> usize {
    screens_in_category(category).count()
}

/// Next screen in registry order (wraps).
pub fn next_screen(current: ScreenId) -> ScreenId {
    let ids = screen_ids();
    if ids.is_empty() {
        return current;
    }
    let idx = screen_index(current);
    ids[(idx + 1) % ids.len()]
}

/// Previous screen in registry order (wraps).
pub fn prev_screen(current: ScreenId) -> ScreenId {
    let ids = screen_ids();
    if ids.is_empty() {
        return current;
    }
    let idx = screen_index(current);
    let prev = (idx + ids.len() - 1) % ids.len();
    ids[prev]
}

/// Next category in IA order (wraps).
pub fn next_category(category: ScreenCategory) -> ScreenCategory {
    let idx = ScreenCategory::ALL
        .iter()
        .position(|c| *c == category)
        .unwrap_or(0);
    ScreenCategory::ALL[(idx + 1) % ScreenCategory::ALL.len()]
}

/// Previous category in IA order (wraps).
pub fn prev_category(category: ScreenCategory) -> ScreenCategory {
    let idx = ScreenCategory::ALL
        .iter()
        .position(|c| *c == category)
        .unwrap_or(0);
    let prev = (idx + ScreenCategory::ALL.len() - 1) % ScreenCategory::ALL.len();
    ScreenCategory::ALL[prev]
}

/// First screen in a category (if any).
pub fn first_in_category(category: ScreenCategory) -> Option<ScreenId> {
    screens_in_category(category).next().map(|meta| meta.id)
}

/// Next screen within the same category (wraps).
pub fn next_in_category(current: ScreenId) -> ScreenId {
    let category = screen_category(current);
    let ids: Vec<ScreenId> = screens_in_category(category).map(|meta| meta.id).collect();
    if ids.is_empty() {
        return current;
    }
    let idx = ids.iter().position(|id| *id == current).unwrap_or(0);
    ids[(idx + 1) % ids.len()]
}

/// Previous screen within the same category (wraps).
pub fn prev_in_category(current: ScreenId) -> ScreenId {
    let category = screen_category(current);
    let ids: Vec<ScreenId> = screens_in_category(category).map(|meta| meta.id).collect();
    if ids.is_empty() {
        return current;
    }
    let idx = ids.iter().position(|id| *id == current).unwrap_or(0);
    let prev = (idx + ids.len() - 1) % ids.len();
    ids[prev]
}

/// A help entry describing a keybinding.
pub struct HelpEntry {
    /// Key label (e.g. "Tab", "Ctrl+F").
    pub key: &'static str,
    /// Description of what the key does.
    pub action: &'static str,
}

/// Trait for demo screens.
///
/// Each screen manages its own state, handles its own messages, and renders
/// into the content area provided by the main layout.
pub trait Screen {
    /// Message type for this screen (will be wrapped by the top-level Msg enum).
    type Message: Send + 'static;

    /// Handle a screen-specific event, returning a command.
    fn update(&mut self, event: &Event) -> Cmd<Self::Message>;

    /// Render the screen into the given area.
    fn view(&self, frame: &mut Frame, area: Rect);

    /// Return keybindings specific to this screen for the help overlay.
    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![]
    }

    /// Whether this screen can undo.
    fn can_undo(&self) -> bool {
        false
    }

    /// Whether this screen can redo.
    fn can_redo(&self) -> bool {
        false
    }

    /// Description of the next undo action, if any.
    fn next_undo_description(&self) -> Option<&str> {
        None
    }

    /// Handle an undo request. Return true if the screen owns undo handling.
    fn undo(&mut self) -> bool {
        false
    }

    /// Handle a redo request. Return true if the screen owns redo handling.
    fn redo(&mut self) -> bool {
        false
    }

    /// Called on each application tick (100ms interval) with the global tick count.
    fn tick(&mut self, _tick_count: u64) {}

    /// Title shown in the tab bar.
    fn title(&self) -> &'static str;

    /// Short name for tab display (max ~12 chars).
    fn tab_label(&self) -> &'static str {
        self.title()
    }
}

/// Check if a line contains a query string (case-insensitive) without allocation.
///
/// Callers should pass a pre-lowercased query string to avoid repeated work.
pub(crate) fn line_contains_ignore_case(line: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    let line_bytes = line.as_bytes();
    let query_bytes = query_lower.as_bytes();

    if query_bytes.len() > line_bytes.len() {
        return false;
    }

    for i in 0..=line_bytes.len() - query_bytes.len() {
        let mut match_found = true;
        for j in 0..query_bytes.len() {
            if line_bytes[i + j].to_ascii_lowercase() != query_bytes[j] {
                match_found = false;
                break;
            }
        }
        if match_found {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_order_matches_ids() {
        let ids = screen_ids();
        let expected: Vec<ScreenId> = SCREEN_REGISTRY.iter().map(|meta| meta.id).collect();
        assert_eq!(
            ids,
            expected.as_slice(),
            "screen id order mismatch: expected {expected:?}, got {ids:?}"
        );
    }

    #[test]
    fn screen_meta_roundtrip_matches_registry() {
        for meta in SCREEN_REGISTRY {
            let resolved = screen_meta(meta.id);
            assert_eq!(
                resolved.id, meta.id,
                "screen_meta id mismatch for {:?}",
                meta.id
            );
            assert_eq!(
                resolved.category, meta.category,
                "screen_meta category mismatch for {:?}",
                meta.id
            );
            assert_eq!(
                resolved.title, meta.title,
                "screen_meta title mismatch for {:?}",
                meta.id
            );
        }
    }

    #[test]
    fn screen_index_matches_registry_position() {
        for (idx, meta) in SCREEN_REGISTRY.iter().enumerate() {
            let actual = screen_index(meta.id);
            assert_eq!(
                actual, idx,
                "screen_index mismatch for {:?}: expected {idx}, got {actual}",
                meta.id
            );
        }
    }

    #[test]
    fn category_counts_match_iter() {
        for category in ScreenCategory::ALL {
            let expected = screens_in_category(*category).count();
            let actual = screen_count_in_category(*category);
            assert_eq!(
                actual, expected,
                "category count mismatch for {category:?}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn first_in_category_matches_registry() {
        for category in ScreenCategory::ALL {
            let expected = SCREEN_REGISTRY
                .iter()
                .find(|meta| meta.category == *category)
                .map(|meta| meta.id);
            let actual = first_in_category(*category);
            assert_eq!(
                actual, expected,
                "first_in_category mismatch for {category:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn line_contains_ignore_case_matches() {
        assert!(line_contains_ignore_case("HelloWorld", "world"));
        assert!(line_contains_ignore_case("HelloWorld", "hel"));
        assert!(line_contains_ignore_case("HelloWorld", ""));
        assert!(!line_contains_ignore_case("HelloWorld", "nope"));
        assert!(!line_contains_ignore_case("Hi", "longer"));
    }

    #[test]
    fn registry_matches_screen_list() {
        assert_eq!(SCREEN_REGISTRY.len(), screen_ids().len());
        assert_eq!(screen_ids().len(), SCREEN_REGISTRY.len());
        for &id in screen_ids() {
            assert!(
                SCREEN_REGISTRY.iter().any(|meta| meta.id == id),
                "missing screen in registry: {id:?}"
            );
        }
    }

    #[test]
    fn registry_has_unique_ids() {
        for (idx, meta) in SCREEN_REGISTRY.iter().enumerate() {
            let duplicates = SCREEN_REGISTRY
                .iter()
                .enumerate()
                .filter(|(i, other)| *i != idx && other.id == meta.id)
                .count();
            assert_eq!(
                duplicates,
                0,
                "duplicate id in registry: {id:?}",
                id = meta.id
            );
        }
    }

    #[test]
    fn registry_next_prev_wrap() {
        let ids = screen_ids();
        assert!(!ids.is_empty());
        let first = ids[0];
        let last = ids[ids.len() - 1];
        assert_eq!(next_screen(last), first);
        assert_eq!(prev_screen(first), last);
    }

    #[test]
    fn category_next_prev_wrap() {
        let first_cat = ScreenCategory::ALL[0];
        let last_cat = ScreenCategory::ALL[ScreenCategory::ALL.len() - 1];
        assert_eq!(next_category(last_cat), first_cat);
        assert_eq!(prev_category(first_cat), last_cat);
    }

    #[test]
    fn category_screen_wraps() {
        let category = ScreenCategory::Tour;
        let first = first_in_category(category).expect("tour category has at least one screen");
        let last = screens_in_category(category)
            .last()
            .expect("tour category has at least one screen")
            .id;
        assert_eq!(next_in_category(last), first);
        assert_eq!(prev_in_category(first), last);
    }
}
