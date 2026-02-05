//! Mermaid Mega Showcase Screen — interactive layout lab.
//!
//! A comprehensive, over-the-top Mermaid diagram demo with:
//! - Full sample library with metadata and filtering
//! - Interactive node navigation and edge highlighting
//! - Split-panel layout with diagram, controls, metrics, and detail panels
//! - All configuration knobs exposed as keybindings
//! - Help overlay driven by the canonical keymap spec

use ftui_core::geometry::Rect;
use ftui_extras::mermaid::{
    DiagramPalettePreset, MermaidConfig, MermaidGlyphMode, MermaidRenderMode, MermaidTier,
    MermaidWrapMode, ShowcaseMode,
};
use ftui_extras::mermaid_render::{DiagramPalette, MermaidRenderer, SelectionState};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::{BorderChars, Draw};

use crate::screens::{Cmd, Event, Frame, HelpEntry, Screen};

// ── Layout constants ────────────────────────────────────────────────

/// Minimum terminal width for full layout (below this, panels collapse).
const MIN_FULL_WIDTH: u16 = 100;
/// Minimum terminal height for full layout.
const MIN_FULL_HEIGHT: u16 = 20;
/// Side panel width when visible.
const SIDE_PANEL_WIDTH: u16 = 28;
/// Footer height (status + hints).
const FOOTER_HEIGHT: u16 = 2;
/// Controls panel height when visible.
const CONTROLS_PANEL_HEIGHT: u16 = 6;

// ── Panel visibility ────────────────────────────────────────────────

/// Which panels are currently visible.
#[derive(Debug, Clone, Copy)]
struct PanelVisibility {
    controls: bool,
    metrics: bool,
    detail: bool,
    status_log: bool,
    help_overlay: bool,
}

impl Default for PanelVisibility {
    fn default() -> Self {
        Self {
            controls: true,
            metrics: true,
            detail: false,
            status_log: false,
            help_overlay: false,
        }
    }
}

// ── Layout mode ─────────────────────────────────────────────────────

/// Layout density preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Dense,
    Normal,
    Spacious,
    Auto,
}

impl LayoutMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Dense => "dense",
            Self::Normal => "normal",
            Self::Spacious => "spacious",
            Self::Auto => "auto",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Dense => Self::Normal,
            Self::Normal => Self::Spacious,
            Self::Spacious => Self::Auto,
            Self::Auto => Self::Dense,
        }
    }
}

// ── Computed layout regions ─────────────────────────────────────────

/// Regions computed from the terminal area and panel visibility.
#[derive(Debug, Clone, Copy, Default)]
struct LayoutRegions {
    /// Main diagram rendering area.
    diagram: Rect,
    /// Right-side panel area (metrics + detail).
    side_panel: Rect,
    /// Top controls strip.
    controls: Rect,
    /// Bottom footer (status line + key hints).
    footer: Rect,
}

impl LayoutRegions {
    /// Compute layout regions from available area and panel state.
    fn compute(area: Rect, panels: &PanelVisibility) -> Self {
        if area.width < 10 || area.height < 5 {
            return Self {
                diagram: area,
                ..Default::default()
            };
        }

        let mut x = area.x;
        let mut y = area.y;
        let mut w = area.width;
        let mut h = area.height;

        // Footer always present.
        let footer_h = FOOTER_HEIGHT.min(h.saturating_sub(3));
        h = h.saturating_sub(footer_h);
        let footer = Rect::new(x, y + h, w, footer_h);

        // Controls strip at top.
        let controls_h = if panels.controls && h > 8 {
            CONTROLS_PANEL_HEIGHT.min(h / 3)
        } else {
            0
        };
        let controls = Rect::new(x, y, w, controls_h);
        y += controls_h;
        h = h.saturating_sub(controls_h);

        // Side panel on right.
        let side_w = if (panels.metrics || panels.detail) && w >= MIN_FULL_WIDTH {
            SIDE_PANEL_WIDTH.min(w / 3)
        } else {
            0
        };
        let side_panel = if side_w > 0 {
            w -= side_w;
            Rect::new(x + w, y, side_w, h)
        } else {
            Rect::default()
        };

        let diagram = Rect::new(x, y, w, h);

        Self {
            diagram,
            side_panel,
            controls,
            footer,
        }
    }
}

// ── State ───────────────────────────────────────────────────────────

/// State for the Mermaid Mega Showcase screen.
#[derive(Debug)]
pub struct MermaidMegaState {
    /// Current interaction mode.
    mode: ShowcaseMode,
    /// Panel visibility flags.
    panels: PanelVisibility,
    /// Layout density mode.
    layout_mode: LayoutMode,
    /// Fidelity tier.
    tier: MermaidTier,
    /// Glyph mode (Unicode / ASCII).
    glyph_mode: MermaidGlyphMode,
    /// Render mode (Cell / Braille / Block / etc).
    render_mode: MermaidRenderMode,
    /// Wrap mode for labels.
    wrap_mode: MermaidWrapMode,
    /// Color palette preset.
    palette: DiagramPalettePreset,
    /// Whether classDef/style rendering is enabled.
    styles_enabled: bool,
    /// Viewport zoom level (1.0 = 100%).
    viewport_zoom: f32,
    /// Selected sample index.
    selected_sample: usize,
    /// Selected node index for inspect mode.
    selected_node: Option<usize>,
    /// Search query (when in search mode).
    search_query: Option<String>,
    /// Epoch counters for cache invalidation.
    analysis_epoch: u64,
    layout_epoch: u64,
    render_epoch: u64,
}

impl Default for MermaidMegaState {
    fn default() -> Self {
        Self {
            mode: ShowcaseMode::Normal,
            panels: PanelVisibility::default(),
            layout_mode: LayoutMode::Auto,
            tier: MermaidTier::Auto,
            glyph_mode: MermaidGlyphMode::Unicode,
            render_mode: MermaidRenderMode::Auto,
            wrap_mode: MermaidWrapMode::WordChar,
            palette: DiagramPalettePreset::Default,
            styles_enabled: true,
            viewport_zoom: 1.0,
            selected_sample: 0,
            selected_node: None,
            search_query: None,
            analysis_epoch: 0,
            layout_epoch: 0,
            render_epoch: 0,
        }
    }
}

impl MermaidMegaState {
    /// Build a MermaidConfig from the current state.
    fn to_config(&self) -> MermaidConfig {
        let mut config = MermaidConfig::default();
        config.glyph_mode = self.glyph_mode;
        config.render_mode = self.render_mode;
        config.tier_override = self.tier;
        config.wrap_mode = self.wrap_mode;
        config.enable_styles = self.styles_enabled;
        config.palette = self.palette;
        config
    }

    /// Bump render epoch (triggers re-render without re-layout).
    fn bump_render(&mut self) {
        self.render_epoch = self.render_epoch.wrapping_add(1);
    }

    /// Bump layout epoch (triggers re-layout + re-render).
    fn bump_layout(&mut self) {
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
        self.bump_render();
    }

    /// Bump analysis epoch (triggers full re-parse + re-layout + re-render).
    fn bump_analysis(&mut self) {
        self.analysis_epoch = self.analysis_epoch.wrapping_add(1);
        self.bump_layout();
    }
}

// ── Actions ─────────────────────────────────────────────────────────

/// Actions the mega showcase screen can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MegaAction {
    NextSample,
    PrevSample,
    CycleTier,
    ToggleGlyphMode,
    CycleRenderMode,
    CycleWrapMode,
    ToggleStyles,
    CycleLayoutMode,
    ForceRelayout,
    CyclePalette,
    PrevPalette,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    FitToView,
    ToggleMetrics,
    ToggleControls,
    ToggleDetail,
    ToggleStatusLog,
    ToggleHelp,
    SelectNextNode,
    SelectPrevNode,
    DeselectNode,
    EnterSearch,
    ExitSearch,
    CollapsePanels,
}

impl MermaidMegaState {
    /// Apply an action to the state.
    fn apply(&mut self, action: MegaAction) {
        match action {
            MegaAction::NextSample => {
                self.selected_sample = self.selected_sample.wrapping_add(1);
                self.selected_node = None;
                self.bump_analysis();
            }
            MegaAction::PrevSample => {
                self.selected_sample = self.selected_sample.wrapping_sub(1);
                self.selected_node = None;
                self.bump_analysis();
            }
            MegaAction::CycleTier => {
                self.tier = match self.tier {
                    MermaidTier::Auto => MermaidTier::Compact,
                    MermaidTier::Compact => MermaidTier::Normal,
                    MermaidTier::Normal => MermaidTier::Rich,
                    MermaidTier::Rich => MermaidTier::Auto,
                };
                self.bump_layout();
            }
            MegaAction::ToggleGlyphMode => {
                self.glyph_mode = match self.glyph_mode {
                    MermaidGlyphMode::Unicode => MermaidGlyphMode::Ascii,
                    MermaidGlyphMode::Ascii => MermaidGlyphMode::Unicode,
                };
                self.bump_render();
            }
            MegaAction::CycleRenderMode => {
                self.render_mode = match self.render_mode {
                    MermaidRenderMode::Auto => MermaidRenderMode::CellOnly,
                    MermaidRenderMode::CellOnly => MermaidRenderMode::Braille,
                    MermaidRenderMode::Braille => MermaidRenderMode::Block,
                    MermaidRenderMode::Block => MermaidRenderMode::HalfBlock,
                    MermaidRenderMode::HalfBlock => MermaidRenderMode::Auto,
                };
                self.bump_render();
            }
            MegaAction::CycleWrapMode => {
                self.wrap_mode = match self.wrap_mode {
                    MermaidWrapMode::None => MermaidWrapMode::Word,
                    MermaidWrapMode::Word => MermaidWrapMode::Char,
                    MermaidWrapMode::Char => MermaidWrapMode::WordChar,
                    MermaidWrapMode::WordChar => MermaidWrapMode::None,
                };
                self.bump_layout();
            }
            MegaAction::ToggleStyles => {
                self.styles_enabled = !self.styles_enabled;
                self.bump_render();
            }
            MegaAction::CycleLayoutMode => {
                self.layout_mode = self.layout_mode.next();
                self.bump_layout();
            }
            MegaAction::ForceRelayout => {
                self.bump_layout();
            }
            MegaAction::CyclePalette => {
                self.palette = self.palette.next();
                self.bump_render();
            }
            MegaAction::PrevPalette => {
                self.palette = self.palette.prev();
                self.bump_render();
            }
            MegaAction::ZoomIn => {
                self.viewport_zoom = (self.viewport_zoom * 1.25).min(4.0);
                self.bump_render();
            }
            MegaAction::ZoomOut => {
                self.viewport_zoom = (self.viewport_zoom / 1.25).max(0.25);
                self.bump_render();
            }
            MegaAction::ZoomReset => {
                self.viewport_zoom = 1.0;
                self.bump_render();
            }
            MegaAction::FitToView => {
                self.viewport_zoom = 1.0;
                self.bump_render();
            }
            MegaAction::ToggleMetrics => {
                self.panels.metrics = !self.panels.metrics;
            }
            MegaAction::ToggleControls => {
                self.panels.controls = !self.panels.controls;
            }
            MegaAction::ToggleDetail => {
                self.panels.detail = !self.panels.detail;
            }
            MegaAction::ToggleStatusLog => {
                self.panels.status_log = !self.panels.status_log;
            }
            MegaAction::ToggleHelp => {
                self.panels.help_overlay = !self.panels.help_overlay;
            }
            MegaAction::SelectNextNode => {
                self.selected_node = Some(self.selected_node.map_or(0, |n| n + 1));
                self.mode = ShowcaseMode::Inspect;
                self.bump_render();
            }
            MegaAction::SelectPrevNode => {
                self.selected_node =
                    Some(self.selected_node.map_or(0, |n| n.saturating_sub(1)));
                self.mode = ShowcaseMode::Inspect;
                self.bump_render();
            }
            MegaAction::DeselectNode => {
                self.selected_node = None;
                self.mode = ShowcaseMode::Normal;
                self.bump_render();
            }
            MegaAction::EnterSearch => {
                self.mode = ShowcaseMode::Search;
                self.search_query = Some(String::new());
            }
            MegaAction::ExitSearch => {
                self.mode = ShowcaseMode::Normal;
                self.search_query = None;
                self.bump_render();
            }
            MegaAction::CollapsePanels => {
                if self.mode == ShowcaseMode::Inspect {
                    self.selected_node = None;
                    self.mode = ShowcaseMode::Normal;
                    self.bump_render();
                } else if self.mode == ShowcaseMode::Search {
                    self.search_query = None;
                    self.mode = ShowcaseMode::Normal;
                    self.bump_render();
                } else {
                    self.panels.controls = false;
                    self.panels.metrics = false;
                    self.panels.detail = false;
                    self.panels.status_log = false;
                    self.panels.help_overlay = false;
                }
            }
        }
    }
}

// ── Screen ──────────────────────────────────────────────────────────

/// Mermaid Mega Showcase — the over-the-top interactive diagram lab.
pub struct MermaidMegaShowcaseScreen {
    state: MermaidMegaState,
}

impl Default for MermaidMegaShowcaseScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl MermaidMegaShowcaseScreen {
    /// Create a new mega showcase screen.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: MermaidMegaState::default(),
        }
    }

    /// Map a key event to an action.
    fn handle_key(&self, event: &ftui_core::event::KeyEvent) -> Option<MegaAction> {
        use ftui_core::event::KeyCode;
        match event.code {
            // Sample navigation
            KeyCode::Down | KeyCode::Char('j') => Some(MegaAction::NextSample),
            KeyCode::Up | KeyCode::Char('k') => Some(MegaAction::PrevSample),
            // Render config
            KeyCode::Char('t') => Some(MegaAction::CycleTier),
            KeyCode::Char('g') => Some(MegaAction::ToggleGlyphMode),
            KeyCode::Char('b') => Some(MegaAction::CycleRenderMode),
            KeyCode::Char('s') => Some(MegaAction::ToggleStyles),
            KeyCode::Char('w') => Some(MegaAction::CycleWrapMode),
            KeyCode::Char('l') => Some(MegaAction::CycleLayoutMode),
            KeyCode::Char('r') => Some(MegaAction::ForceRelayout),
            // Theme
            KeyCode::Char('p') => Some(MegaAction::CyclePalette),
            KeyCode::Char('P') => Some(MegaAction::PrevPalette),
            // Viewport
            KeyCode::Char('+') | KeyCode::Char('=') => Some(MegaAction::ZoomIn),
            KeyCode::Char('-') => Some(MegaAction::ZoomOut),
            KeyCode::Char('0') => Some(MegaAction::ZoomReset),
            KeyCode::Char('f') => Some(MegaAction::FitToView),
            // Panels
            KeyCode::Char('m') => Some(MegaAction::ToggleMetrics),
            KeyCode::Char('c') => Some(MegaAction::ToggleControls),
            KeyCode::Char('d') => Some(MegaAction::ToggleDetail),
            KeyCode::Char('i') => Some(MegaAction::ToggleStatusLog),
            KeyCode::Char('?') => Some(MegaAction::ToggleHelp),
            // Node inspection
            KeyCode::Tab => Some(MegaAction::SelectNextNode),
            KeyCode::BackTab => Some(MegaAction::SelectPrevNode),
            // Search
            KeyCode::Char('/') => Some(MegaAction::EnterSearch),
            // Escape is context-dependent
            KeyCode::Escape => Some(MegaAction::CollapsePanels),
            _ => None,
        }
    }

    /// Render the controls strip at the top.
    fn render_controls(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let border = Cell::from_char(' ').with_fg(PackedRgba::rgb(80, 80, 100));
        buf.draw_border(area, BorderChars::SQUARE, border);

        let s = &self.state;
        let status = format!(
            " Tier:{} Glyph:{} Render:{} Wrap:{} Layout:{} Palette:{} Zoom:{:.0}% ",
            s.tier, s.glyph_mode, s.render_mode, s.wrap_mode,
            s.layout_mode.as_str(), s.palette, (s.viewport_zoom * 100.0),
        );
        let text_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(180, 200, 220));
        let y = area.y + 1;
        let x = area.x + 1;
        for (i, ch) in status.chars().enumerate() {
            let col = x + i as u16;
            if col >= area.x + area.width - 1 {
                break;
            }
            buf.set(col, y, text_cell.with_char(ch));
        }
    }

    /// Render the side panel (metrics / detail).
    fn render_side_panel(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let border = Cell::from_char(' ').with_fg(PackedRgba::rgb(80, 80, 100));
        buf.draw_border(area, BorderChars::SQUARE, border);

        let title = if self.state.panels.detail {
            " Detail "
        } else {
            " Metrics "
        };
        let title_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(140, 180, 220));
        for (i, ch) in title.chars().enumerate() {
            let col = area.x + 1 + i as u16;
            if col >= area.x + area.width - 1 {
                break;
            }
            buf.set(col, area.y, title_cell.with_char(ch));
        }

        // Placeholder content
        let lines = [
            format!("Mode: {}", self.state.mode.as_str()),
            format!("Sample: #{}", self.state.selected_sample),
            format!("Palette: {}", self.state.palette),
            format!(
                "Node: {}",
                self.state
                    .selected_node
                    .map_or("-".to_string(), |n| format!("#{n}"))
            ),
            format!("Epoch: a{}/l{}/r{}", self.state.analysis_epoch, self.state.layout_epoch, self.state.render_epoch),
        ];
        let info_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(160, 160, 180));
        for (row, line) in lines.iter().enumerate() {
            let y = area.y + 2 + row as u16;
            if y >= area.y + area.height - 1 {
                break;
            }
            for (col, ch) in line.chars().enumerate() {
                let x = area.x + 1 + col as u16;
                if x >= area.x + area.width - 1 {
                    break;
                }
                buf.set(x, y, info_cell.with_char(ch));
            }
        }
    }

    /// Render the footer with mode indicator and key hints.
    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let mode_str = match self.state.mode {
            ShowcaseMode::Normal => "NORMAL",
            ShowcaseMode::Inspect => "INSPECT",
            ShowcaseMode::Search => "SEARCH",
        };
        let mode_color = match self.state.mode {
            ShowcaseMode::Normal => PackedRgba::rgb(80, 200, 120),
            ShowcaseMode::Inspect => PackedRgba::rgb(80, 180, 255),
            ShowcaseMode::Search => PackedRgba::rgb(255, 200, 80),
        };
        let mode_cell = Cell::from_char(' ').with_fg(mode_color);
        for (i, ch) in mode_str.chars().enumerate() {
            let col = area.x + i as u16;
            if col >= area.x + area.width {
                break;
            }
            buf.set(col, area.y, mode_cell.with_char(ch));
        }

        let hints = " j/k:sample t:tier g:glyph p:palette Tab:node ?:help";
        let hint_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(100, 100, 120));
        let hint_x = area.x + mode_str.len() as u16 + 2;
        for (i, ch) in hints.chars().enumerate() {
            let col = hint_x + i as u16;
            if col >= area.x + area.width {
                break;
            }
            buf.set(col, area.y, hint_cell.with_char(ch));
        }
    }

    /// Render a placeholder diagram area.
    fn render_diagram_placeholder(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let border = Cell::from_char(' ').with_fg(PackedRgba::rgb(60, 60, 80));
        buf.draw_border(area, BorderChars::SQUARE, border);

        let palette = DiagramPalette::from_preset(self.state.palette);
        let title = format!(
            " Diagram #{} [{}] ",
            self.state.selected_sample,
            self.state.palette
        );
        let title_cell = Cell::from_char(' ').with_fg(palette.node_border);
        for (i, ch) in title.chars().enumerate() {
            let col = area.x + 1 + i as u16;
            if col >= area.x + area.width - 1 {
                break;
            }
            buf.set(col, area.y, title_cell.with_char(ch));
        }

        // Show palette preview as colored blocks
        let y = area.y + 2;
        if y < area.y + area.height - 1 {
            let label = "Palette: ";
            let label_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(140, 140, 160));
            for (i, ch) in label.chars().enumerate() {
                let col = area.x + 2 + i as u16;
                if col < area.x + area.width - 1 {
                    buf.set(col, y, label_cell.with_char(ch));
                }
            }
            let block_x = area.x + 2 + label.len() as u16;
            for (i, fill) in palette.node_fills.iter().enumerate() {
                let col = block_x + (i as u16 * 3);
                if col + 2 < area.x + area.width - 1 {
                    let cell = Cell::from_char('█').with_fg(*fill);
                    buf.set(col, y, cell);
                    buf.set(col + 1, y, cell);
                }
            }
        }
    }
}

impl Screen for MermaidMegaShowcaseScreen {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(key) = event {
            if let Some(action) = self.handle_key(key) {
                self.state.apply(action);
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        let regions = LayoutRegions::compute(area, &self.state.panels);

        // Render in layer order: background panels first, then diagram, then overlay.
        if !regions.controls.is_empty() {
            self.render_controls(regions.controls, frame.buffer_mut());
        }
        if !regions.side_panel.is_empty() {
            self.render_side_panel(regions.side_panel, frame.buffer_mut());
        }
        self.render_diagram_placeholder(regions.diagram, frame.buffer_mut());
        self.render_footer(regions.footer, frame.buffer_mut());
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry { key: "j/↓", action: "Next sample" },
            HelpEntry { key: "k/↑", action: "Previous sample" },
            HelpEntry { key: "t", action: "Cycle tier" },
            HelpEntry { key: "g", action: "Toggle glyph mode" },
            HelpEntry { key: "b", action: "Cycle render mode" },
            HelpEntry { key: "p/P", action: "Cycle palette" },
            HelpEntry { key: "Tab", action: "Select next node" },
            HelpEntry { key: "S-Tab", action: "Select previous node" },
            HelpEntry { key: "/", action: "Search" },
            HelpEntry { key: "+/-", action: "Zoom in/out" },
            HelpEntry { key: "m", action: "Toggle metrics" },
            HelpEntry { key: "c", action: "Toggle controls" },
            HelpEntry { key: "d", action: "Toggle detail" },
            HelpEntry { key: "?", action: "Toggle help" },
            HelpEntry { key: "Esc", action: "Deselect / collapse" },
        ]
    }

    fn title(&self) -> &'static str {
        "Mermaid Mega Showcase"
    }

    fn tab_label(&self) -> &'static str {
        "MermaidMega"
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_regions_full_size() {
        let area = Rect::new(0, 0, 120, 40);
        let panels = PanelVisibility::default();
        let regions = LayoutRegions::compute(area, &panels);

        assert!(regions.diagram.width > 0);
        assert!(regions.diagram.height > 0);
        assert!(regions.footer.height > 0);
        assert!(regions.controls.height > 0);
        assert!(regions.side_panel.width > 0, "metrics panel should be visible at 120 cols");
    }

    #[test]
    fn layout_regions_narrow_collapses_side() {
        let area = Rect::new(0, 0, 80, 24);
        let panels = PanelVisibility::default();
        let regions = LayoutRegions::compute(area, &panels);

        assert_eq!(regions.side_panel.width, 0, "side panel should collapse at 80 cols");
        assert!(regions.diagram.width > 60);
    }

    #[test]
    fn layout_regions_tiny_gives_all_to_diagram() {
        let area = Rect::new(0, 0, 8, 4);
        let panels = PanelVisibility::default();
        let regions = LayoutRegions::compute(area, &panels);

        assert_eq!(regions.diagram, area);
    }

    #[test]
    fn layout_regions_no_panels() {
        let area = Rect::new(0, 0, 120, 40);
        let panels = PanelVisibility {
            controls: false,
            metrics: false,
            detail: false,
            status_log: false,
            help_overlay: false,
        };
        let regions = LayoutRegions::compute(area, &panels);

        assert_eq!(regions.controls.height, 0);
        assert_eq!(regions.side_panel.width, 0);
        assert!(regions.diagram.height > 30);
    }

    #[test]
    fn state_default_is_normal_mode() {
        let state = MermaidMegaState::default();
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert_eq!(state.palette, DiagramPalettePreset::Default);
        assert_eq!(state.selected_node, None);
    }

    #[test]
    fn state_apply_cycle_palette() {
        let mut state = MermaidMegaState::default();
        let epoch_before = state.render_epoch;
        state.apply(MegaAction::CyclePalette);
        assert_eq!(state.palette, DiagramPalettePreset::Corporate);
        assert!(state.render_epoch > epoch_before);
    }

    #[test]
    fn state_apply_select_node_enters_inspect() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::SelectNextNode);
        assert_eq!(state.mode, ShowcaseMode::Inspect);
        assert_eq!(state.selected_node, Some(0));
    }

    #[test]
    fn state_apply_deselect_returns_normal() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::SelectNextNode);
        state.apply(MegaAction::DeselectNode);
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert_eq!(state.selected_node, None);
    }

    #[test]
    fn state_apply_enter_search_mode() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::EnterSearch);
        assert_eq!(state.mode, ShowcaseMode::Search);
        assert!(state.search_query.is_some());
    }

    #[test]
    fn state_apply_escape_from_search() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::EnterSearch);
        state.apply(MegaAction::CollapsePanels);
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert!(state.search_query.is_none());
    }

    #[test]
    fn state_to_config_applies_palette() {
        let mut state = MermaidMegaState::default();
        state.palette = DiagramPalettePreset::Neon;
        let config = state.to_config();
        assert_eq!(config.palette, DiagramPalettePreset::Neon);
    }

    #[test]
    fn screen_new_does_not_panic() {
        let _screen = MermaidMegaShowcaseScreen::new();
    }

    #[test]
    fn layout_mode_cycles_through_all() {
        let mut mode = LayoutMode::Dense;
        let start = mode;
        for _ in 0..4 {
            mode = mode.next();
        }
        assert_eq!(mode, start);
    }

    #[test]
    fn layout_regions_deterministic() {
        let area = Rect::new(0, 0, 120, 40);
        let panels = PanelVisibility::default();
        let r1 = LayoutRegions::compute(area, &panels);
        let r2 = LayoutRegions::compute(area, &panels);
        assert_eq!(r1.diagram, r2.diagram);
        assert_eq!(r1.footer, r2.footer);
        assert_eq!(r1.controls, r2.controls);
        assert_eq!(r1.side_panel, r2.side_panel);
    }
}
