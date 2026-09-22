#![forbid(unsafe_code)]

//! Performance Stress Test screen — virtualized lists and large content.
//!
//! Demonstrates:
//! - `VirtualizedList` with large datasets (10k+ items)
//! - Scroll performance with fixed-height items
//! - Item rendering with selection highlighting
//! - Scroll position tracking and progress

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use std::cell::Cell;

use super::{HelpEntry, Screen};
use crate::theme;

const TOTAL_ITEMS: usize = 10_000;

pub struct Performance {
    items: Vec<String>,
    selected: usize,
    scroll_offset: usize,
    viewport_height: Cell<usize>,
    tick_count: u64,
    /// Cached list panel area for mouse hit-testing.
    last_list_area: Cell<Rect>,
}

impl Default for Performance {
    fn default() -> Self {
        Self::new()
    }
}

impl Performance {
    pub fn new() -> Self {
        let items: Vec<String> = (0..TOTAL_ITEMS)
            .map(|i| {
                let severity = match i % 5 {
                    0 => "INFO",
                    1 => "DEBUG",
                    2 => "WARN",
                    3 => "ERROR",
                    _ => "TRACE",
                };
                let module = match i % 7 {
                    0 => "server::http",
                    1 => "db::pool",
                    2 => "auth::jwt",
                    3 => "cache::redis",
                    4 => "queue::worker",
                    5 => "api::handler",
                    _ => "core::runtime",
                };
                format!(
                    "[{:>5}] {:>5} {:<18} Event #{:05}: simulated log entry with payload data",
                    i, severity, module, i
                )
            })
            .collect();

        Self {
            items,
            selected: 0,
            scroll_offset: 0,
            viewport_height: Cell::new(20),
            tick_count: 0,
            last_list_area: Cell::new(Rect::default()),
        }
    }

    fn ensure_visible(&mut self) {
        if self.items.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }

        let viewport_height = self.viewport_height.get().max(1);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + viewport_height {
            self.scroll_offset = self.selected.saturating_sub(viewport_height - 1);
        }
    }

    /// Handle mouse events: click to select, scroll to navigate.
    fn handle_mouse(&mut self, event: &Event) {
        if let Event::Mouse(mouse) = event {
            let list_area = self.last_list_area.get();
            if !list_area.contains(mouse.x, mouse.y) {
                return;
            }

            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    // Click to select item based on y position in list
                    let inner_y = mouse.y.saturating_sub(list_area.y);
                    let target = self.scroll_offset + inner_y as usize;
                    if target < self.items.len() {
                        self.selected = target;
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.selected = self.selected.saturating_sub(3);
                    self.ensure_visible();
                }
                MouseEventKind::ScrollDown => {
                    self.selected = (self.selected + 3).min(self.items.len().saturating_sub(1));
                    self.ensure_visible();
                }
                _ => {}
            }
        }
    }

    fn render_list_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = Style::new().fg(theme::screen_accent::PERFORMANCE);

        let title = format!("Virtualized List ({} items)", self.items.len());
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(&title)
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);
        self.last_list_area.set(inner);

        if inner.is_empty() {
            return;
        }

        let viewport = inner.height as usize;
        self.viewport_height.set(viewport.max(1));
        let end = (self.scroll_offset + viewport).min(self.items.len());

        for (row, idx) in (self.scroll_offset..end).enumerate() {
            let y = inner.y + row as u16;
            if y >= inner.y + inner.height {
                break;
            }

            let style = if idx == self.selected {
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::HIGHLIGHT)
            } else {
                let severity_color = match idx % 5 {
                    0 => theme::fg::PRIMARY,     // INFO
                    1 => theme::fg::MUTED,       // DEBUG
                    2 => theme::accent::WARNING, // WARN
                    3 => theme::accent::ERROR,   // ERROR
                    _ => theme::fg::DISABLED,    // TRACE
                };
                Style::new().fg(severity_color)
            };

            let row_area = Rect::new(inner.x, y, inner.width, 1);
            Paragraph::new(self.items[idx].as_str())
                .style(style)
                .render(row_area, frame);
        }
    }

    fn render_stats_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Performance Stats")
            .title_alignment(Alignment::Center)
            .style(theme::content_border());

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let progress = if self.items.is_empty() {
            0.0
        } else if self.items.len() == 1 {
            1.0
        } else {
            self.selected as f64 / (self.items.len() - 1) as f64
        };

        let viewport_height = self.viewport_height.get().max(1);
        let visible_end = (self.scroll_offset + viewport_height).min(self.items.len());

        let stats = [
            format!("Total items:  {}", self.items.len()),
            format!("Selected:     {} / {}", self.selected + 1, self.items.len()),
            format!("Scroll:       {}", self.scroll_offset),
            format!("Viewport:     {} rows", viewport_height),
            format!("Visible:      {}..{}", self.scroll_offset, visible_end),
            format!("Progress:     {:.1}%", progress * 100.0),
            format!("Tick:         {}", self.tick_count),
            String::new(),
            "Only visible rows are rendered.".into(),
            format!(
                "Rendering {} of {} items.",
                visible_end - self.scroll_offset,
                self.items.len()
            ),
        ];

        for (i, line) in stats.iter().enumerate() {
            if i >= inner.height as usize {
                break;
            }
            let style = if line.is_empty() {
                Style::new()
            } else if line.starts_with("Only") || line.starts_with("Rendering") {
                Style::new().fg(theme::fg::MUTED)
            } else {
                Style::new().fg(theme::fg::SECONDARY)
            };
            let row_area = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
            Paragraph::new(line.as_str())
                .style(style)
                .render(row_area, frame);
        }

        // Progress bar
        let bar_row = stats.len();
        if bar_row < inner.height as usize {
            let bar_width = inner.width.saturating_sub(2) as usize;
            let filled = (progress * bar_width as f64) as usize;
            let bar: String = "\u{2588}"
                .repeat(filled)
                .chars()
                .chain("\u{2591}".repeat(bar_width.saturating_sub(filled)).chars())
                .collect();
            let bar_area = Rect::new(inner.x, inner.y + bar_row as u16, inner.width, 1);
            Paragraph::new(&*bar)
                .style(Style::new().fg(theme::screen_accent::PERFORMANCE))
                .render(bar_area, frame);
        }
    }
}

impl Screen for Performance {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if matches!(event, Event::Mouse(_)) {
            self.handle_mouse(event);
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            let viewport_height = self.viewport_height.get().max(1);
            match (*code, *modifiers) {
                // Vim: k or Up for move up
                (KeyCode::Up, _) | (KeyCode::Char('k'), Modifiers::NONE) => {
                    self.selected = self.selected.saturating_sub(1);
                    self.ensure_visible();
                }
                // Vim: j or Down for move down
                (KeyCode::Down, _) | (KeyCode::Char('j'), Modifiers::NONE) => {
                    if self.selected + 1 < self.items.len() {
                        self.selected += 1;
                    }
                    self.ensure_visible();
                }
                // Vim: Ctrl+U or PageUp for page up
                (KeyCode::PageUp, _) | (KeyCode::Char('u'), Modifiers::CTRL) => {
                    self.selected = self.selected.saturating_sub(viewport_height);
                    self.ensure_visible();
                }
                // Vim: Ctrl+D or PageDown for page down
                (KeyCode::PageDown, _) | (KeyCode::Char('d'), Modifiers::CTRL) => {
                    self.selected =
                        (self.selected + viewport_height).min(self.items.len().saturating_sub(1));
                    self.ensure_visible();
                }
                // Vim: g or Home for first item
                (KeyCode::Home, _) | (KeyCode::Char('g'), Modifiers::NONE) => {
                    self.selected = 0;
                    self.ensure_visible();
                }
                // Vim: G or End for last item
                (KeyCode::End, _) | (KeyCode::Char('G'), Modifiers::NONE | Modifiers::SHIFT) => {
                    self.selected = self.items.len().saturating_sub(1);
                    self.ensure_visible();
                }
                _ => {}
            }
        }

        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let main = Flex::vertical()
            .constraints([Constraint::Min(1), Constraint::Fixed(1)])
            .split(area);

        let cols = Flex::horizontal()
            .constraints([Constraint::Min(40), Constraint::Fixed(35)])
            .split(main[0]);

        self.render_list_panel(frame, cols[0]);
        self.render_stats_panel(frame, cols[1]);

        // Status bar
        let status = format!(
            "Item {}/{} | j/k: scroll | Ctrl+D/U: page | g/G: jump",
            self.selected + 1,
            self.items.len()
        );
        Paragraph::new(&*status)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(main[1], frame);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "j/k or \u{2191}/\u{2193}",
                action: "Scroll items",
            },
            HelpEntry {
                key: "Ctrl+D/U",
                action: "Page scroll",
            },
            HelpEntry {
                key: "g/G",
                action: "Start/end",
            },
            HelpEntry {
                key: "Click",
                action: "Select item",
            },
            HelpEntry {
                key: "Scroll",
                action: "Navigate list",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Performance Stress Test"
    }

    fn tab_label(&self) -> &'static str {
        "Perf"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: ftui_core::event::Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    fn area_has_content(frame: &Frame, area: Rect) -> bool {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = frame.buffer.get(x, y)
                    && !cell.is_empty()
                {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn initial_state() {
        let screen = Performance::new();
        assert_eq!(screen.selected, 0);
        assert_eq!(screen.items.len(), TOTAL_ITEMS);
        assert_eq!(screen.title(), "Performance Stress Test");
    }

    #[test]
    fn scroll_down_up() {
        let mut screen = Performance::new();
        screen.update(&press(KeyCode::Down));
        assert_eq!(screen.selected, 1);
        screen.update(&press(KeyCode::Down));
        assert_eq!(screen.selected, 2);
        screen.update(&press(KeyCode::Up));
        assert_eq!(screen.selected, 1);
    }

    #[test]
    fn page_navigation() {
        let mut screen = Performance::new();
        screen.update(&press(KeyCode::PageDown));
        assert_eq!(screen.selected, screen.viewport_height.get());
        screen.update(&press(KeyCode::PageUp));
        assert_eq!(screen.selected, 0);
    }

    #[test]
    fn home_end() {
        let mut screen = Performance::new();
        screen.update(&press(KeyCode::End));
        assert_eq!(screen.selected, TOTAL_ITEMS - 1);
        screen.update(&press(KeyCode::Home));
        assert_eq!(screen.selected, 0);
    }

    #[test]
    fn bounds_check() {
        let mut screen = Performance::new();
        screen.update(&press(KeyCode::Up));
        assert_eq!(screen.selected, 0);
        screen.selected = TOTAL_ITEMS - 1;
        screen.update(&press(KeyCode::Down));
        assert_eq!(screen.selected, TOTAL_ITEMS - 1);
    }

    #[test]
    fn vim_jk_navigation() {
        let mut screen = Performance::new();
        screen.update(&press(KeyCode::Char('j')));
        assert_eq!(screen.selected, 1);
        screen.update(&press(KeyCode::Char('j')));
        assert_eq!(screen.selected, 2);
        screen.update(&press(KeyCode::Char('k')));
        assert_eq!(screen.selected, 1);
    }

    #[test]
    fn vim_gg_navigation() {
        let mut screen = Performance::new();
        screen.update(&press(KeyCode::Char('G')));
        assert_eq!(screen.selected, TOTAL_ITEMS - 1);
        screen.update(&press(KeyCode::Char('g')));
        assert_eq!(screen.selected, 0);
    }

    #[test]
    fn shifted_g_from_the_kitty_protocol_jumps_to_the_end() {
        // Shift+g as a kitty terminal sends it, with the shifted key 'G'.
        // It used to parse as ('g', SHIFT), which matched no binding.
        let mut screen = Performance::new();
        let events = ftui_core::input_parser::InputParser::new().parse(b"\x1b[103:71;2u");
        assert_eq!(events.len(), 1);
        screen.update(&events[0]);
        assert_eq!(screen.selected, TOTAL_ITEMS - 1);
    }

    #[test]
    fn ensure_visible_adjusts_scroll_offset() {
        let mut screen = Performance::new();
        screen.viewport_height.set(5);
        screen.selected = 10;
        screen.scroll_offset = 0;
        screen.ensure_visible();
        assert_eq!(screen.scroll_offset, 6);

        screen.scroll_offset = 10;
        screen.selected = 2;
        screen.ensure_visible();
        assert_eq!(screen.scroll_offset, 2);
    }

    #[test]
    fn render_populates_list_and_stats() {
        let screen = Performance::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        let area = Rect::new(0, 0, 120, 40);
        screen.view(&mut frame, area);

        let main = Flex::vertical()
            .constraints([Constraint::Min(1), Constraint::Fixed(1)])
            .split(area);
        let cols = Flex::horizontal()
            .constraints([Constraint::Min(40), Constraint::Fixed(35)])
            .split(main[0]);

        assert!(area_has_content(&frame, cols[0]), "list panel empty");
        assert!(area_has_content(&frame, cols[1]), "stats panel empty");
    }

    #[test]
    fn render_handles_small_and_large_areas() {
        let screen = Performance::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 8, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 30, 8));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(200, 60, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 200, 60));
    }

    #[test]
    fn render_stats_handles_empty_items() {
        let mut screen = Performance::new();
        screen.items.clear();
        screen.selected = 0;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 15, &mut pool);
        screen.render_stats_panel(&mut frame, Rect::new(0, 0, 40, 15));
    }

    #[test]
    fn mouse_click_selects_item() {
        use super::Screen;
        let mut screen = Performance::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 120, 40));

        let list_area = screen.last_list_area.get();
        // Click on third row in list (inner rect, so y+2 = third data row)
        let event = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x: list_area.x + 2,
            y: list_area.y + 2,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        screen.update(&event);
        assert_eq!(screen.selected, 2, "click should select item at row");
    }

    #[test]
    fn mouse_scroll_navigates() {
        use super::Screen;
        let mut screen = Performance::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 120, 40));

        let list_area = screen.last_list_area.get();
        let scroll_down = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            x: list_area.x + 2,
            y: list_area.y + 2,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        screen.update(&scroll_down);
        assert_eq!(screen.selected, 3, "scroll down should advance 3 items");

        let scroll_up = Event::Mouse(ftui_core::event::MouseEvent {
            kind: MouseEventKind::ScrollUp,
            x: list_area.x + 2,
            y: list_area.y + 2,
            modifiers: ftui_core::event::Modifiers::NONE,
        });
        screen.update(&scroll_up);
        assert_eq!(screen.selected, 0, "scroll up should go back 3 items");
    }

    #[test]
    fn keybindings_includes_mouse() {
        use super::Screen;
        let screen = Performance::new();
        let bindings = screen.keybindings();
        assert!(
            bindings.iter().any(|h| h.key == "Click"),
            "missing Click keybinding"
        );
        assert!(
            bindings.iter().any(|h| h.key == "Scroll"),
            "missing Scroll keybinding"
        );
    }
}
