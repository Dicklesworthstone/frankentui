#![forbid(unsafe_code)]

//! Theme Studio — Live palette editor and theme inspector.
//!
//! Demonstrates:
//! - Preset theme list with live switching
//! - Token inspector with color swatches
//! - WCAG contrast ratio validation
//! - Export to FrankenTUI JSON and Ghostty formats

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::Widget;

use super::{HelpEntry, Screen};
use crate::theme::{self, ThemeId};

/// Focus panel in the Theme Studio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Focus {
    #[default]
    Presets,
    TokenInspector,
}

impl Focus {
    fn toggle(self) -> Self {
        match self {
            Self::Presets => Self::TokenInspector,
            Self::TokenInspector => Self::Presets,
        }
    }
}

/// A semantic color token for display.
#[derive(Debug, Clone)]
struct ColorToken {
    name: &'static str,
    category: &'static str,
    get_color: fn() -> PackedRgba,
}

impl ColorToken {
    const fn new(name: &'static str, category: &'static str, get_color: fn() -> PackedRgba) -> Self {
        Self {
            name,
            category,
            get_color,
        }
    }
}

/// Theme Studio demo screen state.
pub struct ThemeStudioDemo {
    /// Current focus panel.
    focus: Focus,
    /// Selected preset index.
    preset_index: usize,
    /// Selected token index.
    token_index: usize,
    /// List of color tokens for inspection.
    tokens: Vec<ColorToken>,
    /// Tick counter for animations.
    tick_count: u64,
    /// Export status message.
    export_status: Option<String>,
}

impl Default for ThemeStudioDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeStudioDemo {
    /// Create a new theme studio demo.
    pub fn new() -> Self {
        let tokens = Self::build_token_list();
        Self {
            focus: Focus::default(),
            preset_index: theme::current_theme().index(),
            token_index: 0,
            tokens,
            tick_count: 0,
            export_status: None,
        }
    }

    /// Build the list of inspectable color tokens.
    fn build_token_list() -> Vec<ColorToken> {
        vec![
            // Foreground colors
            ColorToken::new("fg::PRIMARY", "Foreground", || theme::fg::PRIMARY.resolve()),
            ColorToken::new("fg::SECONDARY", "Foreground", || theme::fg::SECONDARY.resolve()),
            ColorToken::new("fg::MUTED", "Foreground", || theme::fg::MUTED.resolve()),
            ColorToken::new("fg::DISABLED", "Foreground", || theme::fg::DISABLED.resolve()),
            // Background colors
            ColorToken::new("bg::DEEP", "Background", || theme::bg::DEEP.resolve()),
            ColorToken::new("bg::BASE", "Background", || theme::bg::BASE.resolve()),
            ColorToken::new("bg::SURFACE", "Background", || theme::bg::SURFACE.resolve()),
            ColorToken::new("bg::OVERLAY", "Background", || theme::bg::OVERLAY.resolve()),
            ColorToken::new("bg::HIGHLIGHT", "Background", || theme::bg::HIGHLIGHT.resolve()),
            // Accent colors
            ColorToken::new("accent::PRIMARY", "Accent", || theme::accent::PRIMARY.resolve()),
            ColorToken::new("accent::SECONDARY", "Accent", || theme::accent::SECONDARY.resolve()),
            ColorToken::new("accent::SUCCESS", "Accent", || theme::accent::SUCCESS.resolve()),
            ColorToken::new("accent::WARNING", "Accent", || theme::accent::WARNING.resolve()),
            ColorToken::new("accent::ERROR", "Accent", || theme::accent::ERROR.resolve()),
            ColorToken::new("accent::INFO", "Accent", || theme::accent::INFO.resolve()),
            ColorToken::new("accent::LINK", "Accent", || theme::accent::LINK.resolve()),
            // Status colors
            ColorToken::new("StatusOpen", "Status", || theme::ColorToken::StatusOpen.resolve()),
            ColorToken::new("StatusInProgress", "Status", || theme::ColorToken::StatusInProgress.resolve()),
            ColorToken::new("StatusBlocked", "Status", || theme::ColorToken::StatusBlocked.resolve()),
            ColorToken::new("StatusClosed", "Status", || theme::ColorToken::StatusClosed.resolve()),
            // Priority colors
            ColorToken::new("PriorityP0", "Priority", || theme::ColorToken::PriorityP0.resolve()),
            ColorToken::new("PriorityP1", "Priority", || theme::ColorToken::PriorityP1.resolve()),
            ColorToken::new("PriorityP2", "Priority", || theme::ColorToken::PriorityP2.resolve()),
            ColorToken::new("PriorityP3", "Priority", || theme::ColorToken::PriorityP3.resolve()),
            ColorToken::new("PriorityP4", "Priority", || theme::ColorToken::PriorityP4.resolve()),
        ]
    }

    /// Calculate WCAG contrast ratio between two colors.
    fn contrast_ratio(fg: PackedRgba, bg: PackedRgba) -> f32 {
        fn linearize(v: f32) -> f32 {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(c: PackedRgba) -> f32 {
            let r = linearize(c.r() as f32 / 255.0);
            let g = linearize(c.g() as f32 / 255.0);
            let b = linearize(c.b() as f32 / 255.0);
            0.2126 * r + 0.7152 * g + 0.0722 * b
        }
        let l1 = luminance(fg);
        let l2 = luminance(bg);
        let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Get WCAG rating for a contrast ratio.
    fn wcag_rating(ratio: f32) -> (&'static str, PackedRgba) {
        if ratio >= 7.0 {
            ("AAA", PackedRgba::rgb(0, 200, 83)) // Green
        } else if ratio >= 4.5 {
            ("AA", PackedRgba::rgb(100, 180, 100)) // Light green
        } else if ratio >= 3.0 {
            ("AA Large", PackedRgba::rgb(255, 193, 7)) // Yellow
        } else {
            ("Fail", PackedRgba::rgb(244, 67, 54)) // Red
        }
    }

    /// Format color as hex string.
    fn color_hex(c: PackedRgba) -> String {
        format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
    }

    /// Export current theme to JSON format.
    fn export_json(&self) -> String {
        let theme_id = theme::current_theme();
        let palette = theme::palette(theme_id);
        format!(
            r#"{{
  "name": "{}",
  "version": "1.0.0",
  "colors": {{
    "bg_base": "{}",
    "bg_surface": "{}",
    "fg_primary": "{}",
    "fg_secondary": "{}",
    "accent_primary": "{}",
    "accent_secondary": "{}",
    "accent_success": "{}",
    "accent_warning": "{}",
    "accent_error": "{}"
  }}
}}"#,
            theme_id.name(),
            Self::color_hex(palette.bg_base),
            Self::color_hex(palette.bg_surface),
            Self::color_hex(palette.fg_primary),
            Self::color_hex(palette.fg_secondary),
            Self::color_hex(palette.accent_primary),
            Self::color_hex(palette.accent_secondary),
            Self::color_hex(palette.accent_success),
            Self::color_hex(palette.accent_warning),
            Self::color_hex(palette.accent_error),
        )
    }

    /// Export current theme to Ghostty format.
    fn export_ghostty(&self) -> String {
        let theme_id = theme::current_theme();
        let palette = theme::palette(theme_id);
        format!(
            r#"# Ghostty theme: {}
# Generated by FrankenTUI Theme Studio

background = {}
foreground = {}
selection-background = {}
selection-foreground = {}

# ANSI Colors
palette = 0={}
palette = 1={}
palette = 2={}
palette = 3={}
palette = 4={}
palette = 5={}
palette = 6={}
palette = 7={}
"#,
            theme_id.name(),
            Self::color_hex(palette.bg_base),
            Self::color_hex(palette.fg_primary),
            Self::color_hex(palette.bg_highlight),
            Self::color_hex(palette.fg_primary),
            Self::color_hex(palette.bg_deep),
            Self::color_hex(palette.accent_error),
            Self::color_hex(palette.accent_success),
            Self::color_hex(palette.accent_warning),
            Self::color_hex(palette.accent_primary),
            Self::color_hex(palette.accent_secondary),
            Self::color_hex(palette.accent_info),
            Self::color_hex(palette.fg_secondary),
        )
    }

    /// Apply the selected theme preset.
    fn apply_preset(&mut self) {
        let theme_id = ThemeId::from_index(self.preset_index);
        theme::set_theme(theme_id);
    }

    /// Render the preset list panel.
    fn render_presets(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.focus == Focus::Presets;
        let border_style = if is_focused {
            Style::new().fg(theme::accent::PRIMARY.resolve())
        } else {
            Style::new().fg(theme::fg::MUTED.resolve())
        };

        let block = Block::new()
            .title(" Presets ")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        // Render preset list
        for (i, theme_id) in ThemeId::ALL.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }

            let is_selected = i == self.preset_index;
            let is_current = *theme_id == theme::current_theme();

            let prefix = if is_selected && is_focused {
                "▶ "
            } else if is_current {
                "● "
            } else {
                "  "
            };

            let style = if is_selected && is_focused {
                Style::new()
                    .fg(theme::fg::PRIMARY.resolve())
                    .bg(theme::alpha::HIGHLIGHT.resolve())
                    .attrs(StyleFlags::BOLD)
            } else if is_current {
                Style::new()
                    .fg(theme::accent::PRIMARY.resolve())
                    .attrs(StyleFlags::BOLD)
            } else {
                Style::new().fg(theme::fg::PRIMARY.resolve())
            };

            let text = format!("{}{}", prefix, theme_id.name());
            let line_area = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
            Paragraph::new(text).style(style).render(line_area, frame);
        }
    }

    /// Render the token inspector panel.
    fn render_token_inspector(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.focus == Focus::TokenInspector;
        let border_style = if is_focused {
            Style::new().fg(theme::accent::PRIMARY.resolve())
        } else {
            Style::new().fg(theme::fg::MUTED.resolve())
        };

        let block = Block::new()
            .title(" Token Inspector ")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        let bg_color = theme::bg::BASE.resolve();

        // Render token list with contrast info
        for (i, token) in self.tokens.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }

            let is_selected = i == self.token_index && is_focused;
            let color = (token.get_color)();
            let contrast = Self::contrast_ratio(color, bg_color);
            let (rating, rating_color) = Self::wcag_rating(contrast);

            let prefix = if is_selected { "▶ " } else { "  " };

            // Format: "▶ token::NAME     #RRGGBB  4.52:1 AA"
            let hex = Self::color_hex(color);
            let name_width = 20;
            let padded_name = format!("{:<width$}", token.name, width = name_width);

            let line_area = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);

            // Render prefix
            let prefix_style = if is_selected {
                Style::new()
                    .fg(theme::fg::PRIMARY.resolve())
                    .bg(theme::alpha::HIGHLIGHT.resolve())
            } else {
                Style::new().fg(theme::fg::MUTED.resolve())
            };
            Paragraph::new(prefix)
                .style(prefix_style)
                .render(Rect::new(line_area.x, line_area.y, 2, 1), frame);

            // Render color swatch (2 cells of solid color)
            let swatch_x = line_area.x + 2;
            if swatch_x + 2 <= line_area.x + line_area.width {
                for dx in 0..2 {
                    if let Some(cell) = frame.buffer.get_mut(swatch_x + dx, line_area.y) {
                        *cell = Cell::from_char(' ').with_bg(color);
                    }
                }
            }

            // Render token name
            let name_x = swatch_x + 3;
            let name_style = if is_selected {
                Style::new()
                    .fg(theme::fg::PRIMARY.resolve())
                    .bg(theme::alpha::HIGHLIGHT.resolve())
            } else {
                Style::new().fg(theme::fg::PRIMARY.resolve())
            };
            if name_x < line_area.x + line_area.width {
                let available = (line_area.x + line_area.width).saturating_sub(name_x);
                Paragraph::new(&*padded_name)
                    .style(name_style)
                    .render(Rect::new(name_x, line_area.y, available.min(name_width as u16), 1), frame);
            }

            // Render hex value
            let hex_x = name_x + name_width as u16 + 1;
            if hex_x + 8 <= line_area.x + line_area.width {
                let hex_style = Style::new().fg(theme::fg::SECONDARY.resolve());
                Paragraph::new(&*hex)
                    .style(hex_style)
                    .render(Rect::new(hex_x, line_area.y, 8, 1), frame);
            }

            // Render contrast ratio
            let ratio_x = hex_x + 9;
            if ratio_x + 8 <= line_area.x + line_area.width {
                let ratio_text = format!("{:.1}:1", contrast);
                let ratio_style = Style::new().fg(theme::fg::MUTED.resolve());
                Paragraph::new(&*ratio_text)
                    .style(ratio_style)
                    .render(Rect::new(ratio_x, line_area.y, 6, 1), frame);
            }

            // Render WCAG rating
            let rating_x = ratio_x + 7;
            if rating_x + rating.len() as u16 <= line_area.x + line_area.width {
                let rating_style = Style::new().fg(rating_color).attrs(StyleFlags::BOLD);
                Paragraph::new(rating)
                    .style(rating_style)
                    .render(Rect::new(rating_x, line_area.y, rating.len() as u16, 1), frame);
            }
        }
    }

    /// Render the export status bar.
    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let status = self
            .export_status
            .as_deref()
            .unwrap_or("Press E to export theme");

        let style = Style::new()
            .fg(theme::fg::MUTED.resolve())
            .bg(theme::alpha::SURFACE.resolve());

        Paragraph::new(status).style(style).render(area, frame);
    }
}

/// Message type for Theme Studio.
#[derive(Debug, Clone)]
pub enum ThemeStudioMsg {
    Noop,
}

impl Screen for ThemeStudioDemo {
    type Message = ThemeStudioMsg;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        // Clear export status on any key
        self.export_status = None;

        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers,
        }) = event
        {
            match code {
                // Tab to switch focus
                KeyCode::Tab => {
                    self.focus = self.focus.toggle();
                }
                // Navigation
                KeyCode::Up | KeyCode::Char('k') => match self.focus {
                    Focus::Presets => {
                        if self.preset_index > 0 {
                            self.preset_index -= 1;
                        }
                    }
                    Focus::TokenInspector => {
                        if self.token_index > 0 {
                            self.token_index -= 1;
                        }
                    }
                },
                KeyCode::Down | KeyCode::Char('j') => match self.focus {
                    Focus::Presets => {
                        if self.preset_index < ThemeId::ALL.len() - 1 {
                            self.preset_index += 1;
                        }
                    }
                    Focus::TokenInspector => {
                        if self.token_index < self.tokens.len() - 1 {
                            self.token_index += 1;
                        }
                    }
                },
                // Apply preset
                KeyCode::Enter => {
                    if self.focus == Focus::Presets {
                        self.apply_preset();
                        self.export_status = Some(format!(
                            "Applied theme: {}",
                            theme::current_theme().name()
                        ));
                    }
                }
                // Cycle theme globally (Ctrl+T)
                KeyCode::Char('t') if modifiers.contains(Modifiers::CTRL) => {
                    theme::cycle_theme();
                    self.preset_index = theme::current_theme().index();
                    self.export_status = Some(format!(
                        "Switched to: {}",
                        theme::current_theme().name()
                    ));
                }
                // Export
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    let json = self.export_json();
                    // In a real app, we'd write to clipboard or file
                    // For demo, just show status
                    self.export_status = Some(format!(
                        "Exported {} ({} bytes)",
                        theme::current_theme().name(),
                        json.len()
                    ));
                }
                _ => {}
            }
        }

        Cmd::none()
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.width < 4 || area.height < 4 {
            return;
        }

        // Layout: left panel (presets) | right panel (token inspector)
        // Bottom: status bar
        let status_height = 1;
        let main_height = area.height.saturating_sub(status_height);

        let main_area = Rect::new(area.x, area.y, area.width, main_height);
        let status_area = Rect::new(area.x, area.y + main_height, area.width, status_height);

        // Split main area into two columns
        let preset_width = 25.min(area.width / 3);
        let inspector_width = area.width.saturating_sub(preset_width).saturating_sub(1);

        let preset_area = Rect::new(main_area.x, main_area.y, preset_width, main_area.height);
        let inspector_area = Rect::new(
            main_area.x + preset_width + 1,
            main_area.y,
            inspector_width,
            main_area.height,
        );

        // Render panels
        self.render_presets(frame, preset_area);
        self.render_token_inspector(frame, inspector_area);
        self.render_status(frame, status_area);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Tab",
                action: "Switch panel",
            },
            HelpEntry {
                key: "j/k",
                action: "Navigate",
            },
            HelpEntry {
                key: "Enter",
                action: "Apply theme",
            },
            HelpEntry {
                key: "Ctrl+T",
                action: "Cycle theme",
            },
            HelpEntry {
                key: "E",
                action: "Export theme",
            },
        ]
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }

    fn title(&self) -> &'static str {
        "Theme Studio"
    }

    fn tab_label(&self) -> &'static str {
        "Themes"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_valid_instance() {
        let demo = ThemeStudioDemo::new();
        assert!(!demo.tokens.is_empty());
        assert_eq!(demo.focus, Focus::Presets);
    }

    #[test]
    fn contrast_ratio_calculation() {
        // Black on white should be maximum contrast
        let white = PackedRgba::rgb(255, 255, 255);
        let black = PackedRgba::rgb(0, 0, 0);
        let ratio = ThemeStudioDemo::contrast_ratio(black, white);
        assert!(ratio > 20.0, "Black on white should have high contrast");
        assert!(ratio < 22.0, "Contrast ratio should be ~21:1");
    }

    #[test]
    fn wcag_rating_thresholds() {
        assert_eq!(ThemeStudioDemo::wcag_rating(7.5).0, "AAA");
        assert_eq!(ThemeStudioDemo::wcag_rating(5.0).0, "AA");
        assert_eq!(ThemeStudioDemo::wcag_rating(3.5).0, "AA Large");
        assert_eq!(ThemeStudioDemo::wcag_rating(2.0).0, "Fail");
    }

    #[test]
    fn color_hex_format() {
        let color = PackedRgba::rgb(255, 128, 0);
        let hex = ThemeStudioDemo::color_hex(color);
        assert_eq!(hex, "#FF8000");
    }

    #[test]
    fn export_json_produces_valid_output() {
        let demo = ThemeStudioDemo::new();
        let json = demo.export_json();
        assert!(json.contains("\"name\":"));
        assert!(json.contains("\"colors\":"));
        assert!(json.contains("\"bg_base\":"));
    }

    #[test]
    fn focus_toggle_cycles() {
        let focus = Focus::Presets;
        assert_eq!(focus.toggle(), Focus::TokenInspector);
        assert_eq!(focus.toggle().toggle(), Focus::Presets);
    }
}
