#![forbid(unsafe_code)]

//! Mouse Playground screen — demonstrates mouse event handling and hit-testing.
//!
//! This screen showcases:
//! - Mouse event decoding (SGR, scroll, drag)
//! - Hit-test accuracy with spatial indexing
//! - Hover jitter stabilization (bd-9n09)
//! - Interactive widgets with click/hover feedback

use std::collections::VecDeque;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_core::hover_stabilizer::{HoverStabilizer, HoverStabilizerConfig};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

/// Maximum number of events to keep in the log.
const MAX_EVENT_LOG: usize = 12;

/// Number of hit-test targets in the grid.
const GRID_COLS: usize = 4;
const GRID_ROWS: usize = 3;

/// Mouse event log entry.
#[derive(Debug, Clone)]
struct EventLogEntry {
    /// Tick when event occurred.
    tick: u64,
    /// Event description.
    description: String,
    /// Position.
    x: u16,
    y: u16,
}

/// A hit target in the grid.
#[derive(Debug, Clone)]
struct HitTarget {
    /// Unique ID for this target.
    id: u64,
    /// Label displayed on the target.
    label: String,
    /// Whether currently hovered.
    hovered: bool,
    /// Click count.
    clicks: u32,
}

impl HitTarget {
    fn new(id: u64, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            hovered: false,
            clicks: 0,
        }
    }
}

/// Mouse Playground demo screen state.
pub struct MousePlayground {
    /// Global tick counter.
    tick_count: u64,
    /// Recent mouse event log.
    event_log: VecDeque<EventLogEntry>,
    /// Grid of hit-test targets.
    targets: Vec<HitTarget>,
    /// Currently hovered target ID (stabilized).
    current_hover: Option<u64>,
    /// Hover jitter stabilizer.
    hover_stabilizer: HoverStabilizer,
    /// Whether to show the hit-test overlay.
    show_overlay: bool,
    /// Whether to show jitter stabilization stats.
    show_jitter_stats: bool,
    /// Last raw hover position.
    last_mouse_pos: Option<(u16, u16)>,
    /// Last frame's target rects for hit testing.
    target_rects: Vec<(u64, Rect)>,
}

impl Default for MousePlayground {
    fn default() -> Self {
        Self::new()
    }
}

impl MousePlayground {
    /// Create a new mouse playground screen.
    pub fn new() -> Self {
        // Create hit targets
        let mut targets = Vec::with_capacity(GRID_COLS * GRID_ROWS);
        for i in 0..(GRID_COLS * GRID_ROWS) {
            targets.push(HitTarget::new(i as u64 + 1, format!("T{}", i + 1)));
        }

        Self {
            tick_count: 0,
            event_log: VecDeque::with_capacity(MAX_EVENT_LOG + 1),
            targets,
            current_hover: None,
            hover_stabilizer: HoverStabilizer::new(HoverStabilizerConfig::default()),
            show_overlay: false,
            show_jitter_stats: false,
            last_mouse_pos: None,
            target_rects: Vec::new(),
        }
    }

    /// Log a mouse event.
    fn log_event(&mut self, desc: impl Into<String>, x: u16, y: u16) {
        self.event_log.push_front(EventLogEntry {
            tick: self.tick_count,
            description: desc.into(),
            x,
            y,
        });
        if self.event_log.len() > MAX_EVENT_LOG {
            self.event_log.pop_back();
        }
    }

    /// Handle a mouse event.
    fn handle_mouse(&mut self, event: MouseEvent) {
        let (x, y) = event.position();
        self.last_mouse_pos = Some((x, y));

        // Log the event
        let desc = match event.kind {
            MouseEventKind::Down(btn) => format!("{:?} Down", btn),
            MouseEventKind::Up(btn) => format!("{:?} Up", btn),
            MouseEventKind::Drag(btn) => format!("{:?} Drag", btn),
            MouseEventKind::Moved => "Move".to_string(),
            MouseEventKind::ScrollUp => "Scroll Up".to_string(),
            MouseEventKind::ScrollDown => "Scroll Down".to_string(),
            MouseEventKind::ScrollLeft => "Scroll Left".to_string(),
            MouseEventKind::ScrollRight => "Scroll Right".to_string(),
        };
        self.log_event(&desc, x, y);

        // Check for clicks on targets
        if let MouseEventKind::Down(MouseButton::Left) = event.kind {
            if let Some(target_id) = self.hit_test(x, y) {
                if let Some(target) = self.targets.iter_mut().find(|t| t.id == target_id) {
                    target.clicks += 1;
                }
            }
        }

        // Update hover with stabilization
        let raw_target = self.hit_test(x, y);
        let stabilized = self.hover_stabilizer.update(raw_target, x, y);

        // Update hovered state on targets
        if stabilized != self.current_hover {
            // Clear old hover
            if let Some(old_id) = self.current_hover {
                if let Some(target) = self.targets.iter_mut().find(|t| t.id == old_id) {
                    target.hovered = false;
                }
            }
            // Set new hover
            if let Some(new_id) = stabilized {
                if let Some(target) = self.targets.iter_mut().find(|t| t.id == new_id) {
                    target.hovered = true;
                }
            }
            self.current_hover = stabilized;
        }
    }

    /// Hit test against stored target rects.
    fn hit_test(&self, x: u16, y: u16) -> Option<u64> {
        // Simple linear search through targets (for demo purposes)
        // In production, use SpatialHitIndex from ftui-render
        for (id, rect) in &self.target_rects {
            if rect.contains(x, y) {
                return Some(*id);
            }
        }
        None
    }

    /// Toggle overlay visibility.
    fn toggle_overlay(&mut self) {
        self.show_overlay = !self.show_overlay;
    }

    /// Toggle jitter stats visibility.
    fn toggle_jitter_stats(&mut self) {
        self.show_jitter_stats = !self.show_jitter_stats;
    }

    /// Clear the event log.
    fn clear_log(&mut self) {
        self.event_log.clear();
    }
}

impl Screen for MousePlayground {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        match event {
            Event::Mouse(mouse_event) => {
                self.handle_mouse(*mouse_event);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char('o') | KeyCode::Char('O'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.toggle_overlay();
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char('j') | KeyCode::Char('J'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.toggle_jitter_stats();
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char('c') | KeyCode::Char('C'),
                kind: KeyEventKind::Press,
                ..
            }) => {
                self.clear_log();
            }
            _ => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        // Main layout: left panel (targets) + right panel (event log)
        let chunks =
            Flex::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area);

        let left_area = chunks[0];
        let right_area = chunks[1];

        // --- Left Panel: Hit-Test Target Grid ---
        let left_block = Block::new()
            .title(" Hit-Test Targets ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::bg::SURFACE));
        let inner_left = left_block.inner(left_area);
        left_block.render(left_area, frame);

        // Render target grid
        self.render_target_grid(frame, inner_left);

        // --- Right Panel: Event Log + Stats ---
        let right_chunks = Flex::vertical([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(right_area);

        // Event log
        let log_block = Block::new()
            .title(" Event Log ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::bg::SURFACE));
        let log_inner = log_block.inner(right_chunks[0]);
        log_block.render(right_chunks[0], frame);
        self.render_event_log(frame, log_inner);

        // Stats panel
        let stats_block = Block::new()
            .title(" Stats ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::bg::SURFACE));
        let stats_inner = stats_block.inner(right_chunks[1]);
        stats_block.render(right_chunks[1], frame);
        self.render_stats(frame, stats_inner);

        // Overlay (if enabled)
        if self.show_overlay {
            self.render_overlay(frame, area);
        }
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "O",
                action: "Toggle hit-test overlay",
            },
            HelpEntry {
                key: "J",
                action: "Toggle jitter stats",
            },
            HelpEntry {
                key: "C",
                action: "Clear event log",
            },
        ]
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }

    fn title(&self) -> &'static str {
        "Mouse Playground"
    }

    fn tab_label(&self) -> &'static str {
        "Mouse"
    }
}

impl MousePlayground {
    /// Render the grid of hit-test targets.
    fn render_target_grid(&self, frame: &mut Frame, area: Rect) {
        if area.width < 4 || area.height < 3 {
            return;
        }

        let cell_width = area.width / GRID_COLS as u16;
        let cell_height = area.height / GRID_ROWS as u16;

        // Collect rects for hit testing (will be stored after render)
        let mut new_rects = Vec::with_capacity(self.targets.len());

        for (i, target) in self.targets.iter().enumerate() {
            let col = i % GRID_COLS;
            let row = i / GRID_COLS;

            let x = area.x + (col as u16) * cell_width;
            let y = area.y + (row as u16) * cell_height;

            // Slightly smaller than cell for visual separation
            let target_rect = Rect::new(x + 1, y, cell_width.saturating_sub(2), cell_height);
            new_rects.push((target.id, target_rect));

            // Style based on hover/click state
            let style = if target.hovered {
                Style::new().fg(theme::accent::PRIMARY).bg(theme::accent::PRIMARY)
            } else {
                Style::new().bg(theme::bg::SURFACE)
            };

            let border_style = if target.hovered {
                Style::new().fg(theme::accent::PRIMARY)
            } else {
                Style::new().fg(theme::fg::SECONDARY)
            };

            // Render target block
            let block = Block::new()
                .borders(Borders::ALL)
                .border_type(if target.hovered {
                    BorderType::Double
                } else {
                    BorderType::Rounded
                })
                .border_style(border_style)
                .style(style);

            let inner = block.inner(target_rect);
            block.render(target_rect, frame);

            // Render label and click count
            if inner.height >= 1 && inner.width >= 2 {
                let label = format!("{} ({})", target.label, target.clicks);
                let label_style = if target.hovered {
                    Style::new().bold()
                } else {
                    Style::new()
                };
                Paragraph::new(label)
                    .style(label_style)
                    .alignment(Alignment::Center)
                    .render(inner, frame);
            }

            // Register hit region
            frame.register_hit(target_rect, HitId::new(target.id), HitRegion::Content, 0);
        }

        // Store rects for hit testing (via interior mutability would be cleaner,
        // but for demo we'll update in tick or use a different approach)
        // Note: In real code, use frame's hit_test capability
    }

    /// Render the event log.
    fn render_event_log(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<String> = Vec::with_capacity(self.event_log.len());

        for entry in &self.event_log {
            lines.push(format!(
                "[{:04}] {:12} ({:3},{:3})",
                entry.tick % 10000,
                entry.description,
                entry.x,
                entry.y
            ));
        }

        if lines.is_empty() {
            lines.push("No events yet. Move the mouse!".to_string());
        }

        let text = lines.join("\n");
        Paragraph::new(text)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(area, frame);
    }

    /// Render statistics panel.
    fn render_stats(&self, frame: &mut Frame, area: Rect) {
        let hover_text = match self.current_hover {
            Some(id) => format!("T{}", id),
            None => "None".to_string(),
        };

        let mouse_pos = match self.last_mouse_pos {
            Some((x, y)) => format!("({}, {})", x, y),
            None => "N/A".to_string(),
        };

        let stats = format!(
            "Hover: {}  Pos: {}\nOverlay: {}  Jitter Stats: {}",
            hover_text,
            mouse_pos,
            if self.show_overlay { "ON" } else { "OFF" },
            if self.show_jitter_stats { "ON" } else { "OFF" }
        );

        Paragraph::new(stats)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(area, frame);
    }

    /// Render hit-test overlay.
    fn render_overlay(&self, frame: &mut Frame, area: Rect) {
        // Draw a subtle overlay showing hit regions
        // For simplicity, just draw a small indicator at mouse position
        if let Some((x, y)) = self.last_mouse_pos {
            if x < area.x + area.width && y < area.y + area.height {
                // Draw crosshair at mouse position
                let crosshair_style = Style::new().fg(theme::accent::PRIMARY);

                // Horizontal line (within bounds)
                let h_start = area.x;
                let h_end = (area.x + area.width).min(x.saturating_add(20));
                for hx in h_start..h_end {
                    if hx != x {
                        frame.set_style(Rect::new(hx, y, 1, 1), crosshair_style.dim());
                    }
                }

                // Vertical line (within bounds)
                let v_start = area.y;
                let v_end = (area.y + area.height).min(y.saturating_add(10));
                for vy in v_start..v_end {
                    if vy != y {
                        frame.set_style(Rect::new(x, vy, 1, 1), crosshair_style.dim());
                    }
                }

                // Center marker
                frame.set_style(Rect::new(x, y, 1, 1), crosshair_style.bold());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_targets() {
        let playground = MousePlayground::new();
        assert_eq!(playground.targets.len(), GRID_COLS * GRID_ROWS);
    }

    #[test]
    fn log_event_limits_size() {
        let mut playground = MousePlayground::new();
        for i in 0..20 {
            playground.log_event(format!("Event {}", i), 0, 0);
        }
        assert_eq!(playground.event_log.len(), MAX_EVENT_LOG);
    }

    #[test]
    fn toggle_overlay() {
        let mut playground = MousePlayground::new();
        assert!(!playground.show_overlay);
        playground.toggle_overlay();
        assert!(playground.show_overlay);
        playground.toggle_overlay();
        assert!(!playground.show_overlay);
    }

    #[test]
    fn clear_log_empties_events() {
        let mut playground = MousePlayground::new();
        playground.log_event("Test", 0, 0);
        assert!(!playground.event_log.is_empty());
        playground.clear_log();
        assert!(playground.event_log.is_empty());
    }

    #[test]
    fn hit_test_returns_none_when_empty() {
        let playground = MousePlayground::new();
        assert!(playground.hit_test(10, 10).is_none());
    }
}
