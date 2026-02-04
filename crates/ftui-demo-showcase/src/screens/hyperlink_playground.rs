#![forbid(unsafe_code)]

//! Hyperlink Playground screen — demonstrates OSC-8 links + hit regions.
//!
//! Highlights:
//! - OSC-8 hyperlink rendering via `LinkRegistry`
//! - Hit regions for hover/click feedback
//! - Keyboard navigation for accessibility

use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::Write;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::{Line, Span, Text, WrapMode};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use serde_json::json;

use super::{HelpEntry, Screen};
use crate::theme;

const LINK_HIT_BASE: u32 = 8000;

#[derive(Debug, Clone, Copy)]
struct LinkEntry {
    label: &'static str,
    url: &'static str,
    description: &'static str,
}

const LINK_ENTRIES: [LinkEntry; 5] = [
    LinkEntry {
        label: "FrankenTUI",
        url: "https://ftui.dev",
        description: "Project home + overview",
    },
    LinkEntry {
        label: "Docs",
        url: "https://ftui.dev/docs",
        description: "Reference and guides",
    },
    LinkEntry {
        label: "GitHub",
        url: "https://github.com/Dicklesworthstone/frankentui",
        description: "Source repository",
    },
    LinkEntry {
        label: "OSC 8 Spec",
        url: "https://iterm2.com/documentation-escape-codes.html",
        description: "Terminal hyperlink escape codes",
    },
    LinkEntry {
        label: "ANSI Reference",
        url: "https://vt100.net/docs/vt510-rm/OSC.html",
        description: "OSC control sequences reference",
    },
];

#[derive(Debug, Clone, Copy)]
struct LinkLayout {
    rect: Rect,
    index: usize,
    link_id: u32,
    hit_id: HitId,
}

pub struct HyperlinkPlayground {
    links: &'static [LinkEntry],
    focused_idx: usize,
    hovered_idx: Option<usize>,
    last_action: Option<String>,
    last_mouse_pos: Option<(u16, u16)>,
    link_layouts: RefCell<Vec<LinkLayout>>,
    tick_count: u64,
    log_path: Option<String>,
    run_id: Option<String>,
}

impl Default for HyperlinkPlayground {
    fn default() -> Self {
        Self::new()
    }
}

impl HyperlinkPlayground {
    pub fn new() -> Self {
        let log_path = std::env::var("FTUI_LINK_REPORT_PATH").ok();
        let run_id = std::env::var("FTUI_LINK_RUN_ID").ok();
        Self {
            links: &LINK_ENTRIES,
            focused_idx: 0,
            hovered_idx: None,
            last_action: None,
            last_mouse_pos: None,
            link_layouts: RefCell::new(Vec::new()),
            tick_count: 0,
            log_path,
            run_id,
        }
    }

    fn active_index(&self) -> usize {
        self.hovered_idx.unwrap_or(self.focused_idx)
    }

    fn osc8_open(url: &str) -> String {
        format!("\\x1b]8;;{}\\x1b\\", url)
    }

    fn osc8_close() -> &'static str {
        "\\x1b]8;;\\x1b\\"
    }

    fn move_focus(&mut self, delta: isize) {
        if self.links.is_empty() {
            self.focused_idx = 0;
            return;
        }
        let len = self.links.len() as isize;
        let mut next = self.focused_idx as isize + delta;
        if next < 0 {
            next = len - 1;
        } else if next >= len {
            next = 0;
        }
        self.focused_idx = next as usize;
        self.log_event("focus_move", self.focused_idx, "ok");
    }

    fn activate_focus(&mut self, reason: &'static str) {
        if let Some(link) = self.links.get(self.focused_idx) {
            self.last_action = Some(format!("Activated {} ({reason})", link.label));
            self.log_event("activate_keyboard", self.focused_idx, "ok");
        }
    }

    fn handle_key(&mut self, key: &KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            KeyCode::Up => self.move_focus(-1),
            KeyCode::Down => self.move_focus(1),
            KeyCode::Tab => {
                if key.shift() {
                    self.move_focus(-1);
                } else {
                    self.move_focus(1);
                }
            }
            KeyCode::Enter => self.activate_focus("Enter"),
            KeyCode::Char(' ') => self.activate_focus("Space"),
            KeyCode::Char('c') => {
                if let Some(link) = self.links.get(self.focused_idx) {
                    self.last_action = Some(format!("Copied URL: {}", link.url));
                    self.log_event("copy_url", self.focused_idx, "ok");
                }
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) {
        match mouse.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.last_mouse_pos = Some(mouse.position());
                self.hovered_idx = self.hit_test(mouse.x, mouse.y);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = self.hit_test(mouse.x, mouse.y) {
                    self.focused_idx = idx;
                    if let Some(link) = self.links.get(idx) {
                        self.last_action = Some(format!("Selected {}", link.label));
                        self.log_event("mouse_select", idx, "ok");
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(idx) = self.hit_test(mouse.x, mouse.y) {
                    if let Some(link) = self.links.get(idx) {
                        self.last_action = Some(format!("Activated {} (Mouse)", link.label));
                        self.log_event("mouse_activate", idx, "ok");
                    }
                }
            }
            _ => {}
        }
    }

    fn hit_test(&self, x: u16, y: u16) -> Option<usize> {
        self.link_layouts
            .borrow()
            .iter()
            .find(|layout| layout.rect.contains(x, y))
            .map(|layout| layout.index)
    }

    fn link_id_for_index(&self, index: usize) -> u32 {
        self.link_layouts
            .borrow()
            .iter()
            .find(|layout| layout.index == index)
            .map(|layout| layout.link_id)
            .unwrap_or(0)
    }

    fn log_event(&self, action: &str, focus_idx: usize, outcome: &str) {
        let Some(path) = self.log_path.as_ref() else {
            return;
        };
        let run_id = self.run_id.as_deref().unwrap_or("unknown");
        let payload = json!({
            "run_id": run_id,
            "link_id": self.link_id_for_index(focus_idx),
            "focus_idx": focus_idx,
            "action": action,
            "outcome": outcome,
        });

        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{payload}");
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let title = Line::from_spans([
            Span::styled("Hyperlink Playground", theme::title()),
            Span::raw("  "),
            Span::styled("OSC-8 + Hit Regions", theme::subtitle()),
        ]);
        let hints = Line::from_spans([
            Span::styled("Up/Down", theme::muted()),
            Span::styled(" move", theme::muted()),
            Span::raw(" · "),
            Span::styled("Tab", theme::muted()),
            Span::styled(" cycle", theme::muted()),
            Span::raw(" · "),
            Span::styled("Enter", theme::muted()),
            Span::styled(" activate", theme::muted()),
            Span::raw(" · "),
            Span::styled("Mouse", theme::muted()),
            Span::styled(" hover/click", theme::muted()),
        ]);

        Paragraph::new(Text::from_lines([title, hints]))
            .wrap(WrapMode::Word)
            .render(area, frame);
    }

    fn render_links(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .title("Links (OSC-8)")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme::accent::LINK));
        block.render(area, frame);
        let inner = block.inner(area);
        if inner.is_empty() {
            return;
        }

        let mut layouts = Vec::new();
        for (idx, link) in self.links.iter().enumerate() {
            let y = inner.y.saturating_add(idx as u16);
            if y >= inner.y.saturating_add(inner.height) {
                break;
            }

            let row = Rect::new(inner.x, y, inner.width, 1);
            let link_id = frame.register_link(link.url);
            let hit_id = HitId::new(LINK_HIT_BASE + idx as u32);
            frame.register_hit(row, hit_id, HitRegion::Link, u64::from(link_id));
            layouts.push(LinkLayout {
                rect: row,
                index: idx,
                link_id,
                hit_id,
            });

            let is_focused = idx == self.focused_idx;
            let is_hovered = self.hovered_idx == Some(idx);
            let indicator = if is_focused {
                ">"
            } else if is_hovered {
                "*"
            } else {
                " "
            };

            let base = theme::link();
            let label_style = if is_focused {
                Style::new()
                    .fg(theme::bg::BASE)
                    .bg(theme::accent::LINK)
                    .bold()
            } else if is_hovered {
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::SURFACE)
            } else {
                base
            };

            let line = Line::from_spans([
                Span::styled(indicator, theme::muted()),
                Span::raw(" "),
                Span::styled(link.label, label_style).link(link.url),
            ]);
            Paragraph::new(Text::from_line(line)).render(row, frame);
        }

        self.link_layouts.replace(layouts);
    }

    fn render_details(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .title("Details & Registry")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme::accent::INFO));
        block.render(area, frame);
        let inner = block.inner(area);
        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(9), Constraint::Min(1)])
            .split(inner);
        let detail_area = rows[0];
        let list_area = rows[1];

        let layouts = self.link_layouts.borrow();
        let active_idx = self.active_index();
        let active = self.links.get(active_idx);
        let (link_id, hit_id) = layouts
            .iter()
            .find(|layout| layout.index == active_idx)
            .map(|layout| (layout.link_id, layout.hit_id.id()))
            .unwrap_or((0, 0));

        let mut detail_lines = Vec::new();
        if let Some(link) = active {
            detail_lines.push(Line::from_spans([
                Span::styled("Selected: ", theme::muted()),
                Span::styled(link.label, theme::title()),
            ]));
            detail_lines.push(Line::from_spans([
                Span::styled("URL: ", theme::muted()),
                Span::styled(link.url, theme::body()),
            ]));
            detail_lines.push(Line::from_spans([
                Span::styled("Registry ID: ", theme::muted()),
                Span::styled(format!("{link_id}"), theme::body()),
            ]));
            detail_lines.push(Line::from_spans([
                Span::styled("Hit: ", theme::muted()),
                Span::styled(format!("id={hit_id} region=Link data={link_id}"), theme::body()),
            ]));
            detail_lines.push(Line::from_spans([
                Span::styled("OSC 8 open: ", theme::muted()),
                Span::styled(Self::osc8_open(link.url), theme::code()),
            ]));
            detail_lines.push(Line::from_spans([
                Span::styled("OSC 8 close: ", theme::muted()),
                Span::styled(Self::osc8_close(), theme::code()),
            ]));
            detail_lines.push(Line::from_spans([
                Span::styled("Notes: ", theme::muted()),
                Span::styled(link.description, theme::body()),
            ]));
        }

        let hover_label = self
            .hovered_idx
            .and_then(|idx| self.links.get(idx))
            .map(|link| link.label)
            .unwrap_or("None");
        detail_lines.push(Line::from_spans([
            Span::styled("Hover: ", theme::muted()),
            Span::styled(hover_label, theme::body()),
        ]));
        if let Some(action) = self.last_action.as_ref() {
            detail_lines.push(Line::from_spans([
                Span::styled("Action: ", theme::muted()),
                Span::styled(action.as_str(), theme::body()),
            ]));
        }

        Paragraph::new(Text::from_lines(detail_lines))
            .wrap(WrapMode::Word)
            .render(detail_area, frame);

        let mut registry_lines = Vec::new();
        registry_lines.push(Line::from_spans([
            Span::styled("Registry map", theme::subtitle()),
        ]));
        for layout in layouts.iter() {
            if let Some(link) = self.links.get(layout.index) {
                registry_lines.push(Line::from_spans([
                    Span::styled(format!("[{}] ", layout.link_id), theme::muted()),
                    Span::styled(link.label, theme::body()),
                ]));
            }
        }
        if let Some((x, y)) = self.last_mouse_pos {
            registry_lines.push(Line::from_spans([
                Span::styled("Mouse: ", theme::muted()),
                Span::styled(format!("({x},{y})"), theme::body()),
            ]));
        }

        Paragraph::new(Text::from_lines(registry_lines))
            .wrap(WrapMode::Word)
            .render(list_area, frame);
    }

    // -----------------------------------------------------------------------
    // Public helpers for tests
    // -----------------------------------------------------------------------

    #[must_use]
    pub fn link_layouts(&self) -> Vec<LinkLayout> {
        self.link_layouts.borrow().clone()
    }

    #[must_use]
    pub fn hit_test_at(&self, x: u16, y: u16) -> Option<usize> {
        self.hit_test(x, y)
    }
}

impl Screen for HyperlinkPlayground {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Tick => self.tick_count = self.tick_count.wrapping_add(1),
            _ => {}
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(3), Constraint::Min(1)])
            .split(area);

        self.render_header(frame, rows[0]);

        let body = rows[1];
        if body.is_empty() {
            return;
        }

        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(body);

        self.render_links(frame, cols[0]);
        self.render_details(frame, cols[1]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Up/Down",
                action: "Move focus",
            },
            HelpEntry {
                key: "Tab",
                action: "Cycle links",
            },
            HelpEntry {
                key: "Enter",
                action: "Activate link",
            },
            HelpEntry {
                key: "c",
                action: "Copy URL",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Hyperlink Playground"
    }

    fn tab_label(&self) -> &'static str {
        "Links"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;
    use ftui_render::link_registry::LinkRegistry;

    #[test]
    fn registers_links_in_registry() {
        let screen = HyperlinkPlayground::new();
        let mut pool = GraphemePool::new();
        let mut registry = LinkRegistry::new();
        let mut frame = Frame::with_links(80, 24, &mut pool, &mut registry);
        screen.view(&mut frame, Rect::new(0, 0, 80, 24));

        assert_eq!(registry.len(), LINK_ENTRIES.len());
        let layouts = screen.link_layouts();
        assert_eq!(layouts.len(), LINK_ENTRIES.len());
        assert!(layouts.iter().all(|layout| layout.link_id != 0));
    }

    #[test]
    fn hit_test_returns_link_index() {
        let screen = HyperlinkPlayground::new();
        let mut pool = GraphemePool::new();
        let mut registry = LinkRegistry::new();
        let mut frame = Frame::with_links(80, 24, &mut pool, &mut registry);
        screen.view(&mut frame, Rect::new(0, 0, 80, 24));

        let layouts = screen.link_layouts();
        let first = layouts.first().expect("expected first link layout");
        assert_eq!(screen.hit_test_at(first.rect.x, first.rect.y), Some(0));
    }
}
