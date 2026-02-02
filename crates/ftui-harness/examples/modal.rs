//! Modal Alt-Screen Example
//!
//! Demonstrates the AltScreen mode for full-screen modal UI.
//! In AltScreen mode, the UI takes over the entire terminal and restores
//! the original screen content on exit.
//!
//! Run: `cargo run -p ftui-harness --example modal`
//!
//! This is useful for:
//! - File pickers
//! - Full-screen help views
//! - Rich interactive dialogs
//! - Any UI that needs the full terminal

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::{App, Cmd, Model, ScreenMode};
use ftui_style::Style;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::{StatefulWidget, Widget};

/// A modal file picker demonstration.
struct ModalPicker {
    files: Vec<String>,
    list_state: ListState,
    #[allow(dead_code)]
    selected_file: Option<String>,
}

#[derive(Debug)]
enum Msg {
    Key(ftui_core::event::KeyEvent),
    Noop,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(k) => Msg::Key(k),
            _ => Msg::Noop,
        }
    }
}

impl ModalPicker {
    fn new() -> Self {
        let files = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "src/components/mod.rs".to_string(),
            "src/components/log_viewer.rs".to_string(),
            "src/components/status_bar.rs".to_string(),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
            ".gitignore".to_string(),
        ];

        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            files,
            list_state,
            selected_file: None,
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let current = self.list_state.selected().unwrap_or(0);
        let new_idx = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            (current + delta as usize).min(self.files.len().saturating_sub(1))
        };
        self.list_state.select(Some(new_idx));
    }

    fn confirm_selection(&mut self) {
        if let Some(idx) = self.list_state.selected() {
            self.selected_file = Some(self.files[idx].clone());
        }
    }
}

impl Model for ModalPicker {
    type Message = Msg;

    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::None
    }

    fn update(&mut self, msg: Msg) -> Cmd<Self::Message> {
        match msg {
            Msg::Key(k) if k.kind == KeyEventKind::Press => {
                // Quit shortcuts
                if k.modifiers.contains(Modifiers::CTRL) && k.code == KeyCode::Char('c') {
                    return Cmd::Quit;
                }

                match k.code {
                    KeyCode::Char('q') | KeyCode::Escape => return Cmd::Quit,
                    KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                    KeyCode::PageUp => self.move_selection(-5),
                    KeyCode::PageDown => self.move_selection(5),
                    KeyCode::Home => self.list_state.select(Some(0)),
                    KeyCode::End => {
                        self.list_state
                            .select(Some(self.files.len().saturating_sub(1)));
                    }
                    KeyCode::Enter => {
                        self.confirm_selection();
                        return Cmd::Quit;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());

        // Create centered modal area (60% width, 70% height)
        let modal_width = (area.width as f32 * 0.6) as u16;
        let modal_height = (area.height as f32 * 0.7) as u16;
        let modal_x = (area.width.saturating_sub(modal_width)) / 2;
        let modal_y = (area.height.saturating_sub(modal_height)) / 2;
        let modal_area = Rect::new(modal_x, modal_y, modal_width, modal_height);

        // Modal container
        let modal_block = Block::new()
            .title(" Select File ")
            .borders(Borders::ALL)
            .border_type(BorderType::Double);

        let inner = modal_block.inner(modal_area);
        modal_block.render(modal_area, frame);

        // Split inner area for list and help text
        let chunks = Flex::vertical()
            .constraints([Constraint::Min(3), Constraint::Fixed(3)])
            .split(inner);

        // File list - use ListItem::new() for each file
        let items: Vec<ListItem> = self
            .files
            .iter()
            .map(|f| ListItem::new(format!(" {} ", f)))
            .collect();

        let list = List::new(items)
            .highlight_style(Style::new().bold().reverse())
            .highlight_symbol("▶ ");

        let mut state = self.list_state.clone();
        StatefulWidget::render(&list, chunks[0], frame, &mut state);

        // Help text
        let help_block = Block::new().title(" Controls ").borders(Borders::TOP);

        let help_inner = help_block.inner(chunks[1]);
        help_block.render(chunks[1], frame);

        let help = Paragraph::new("↑/↓: Navigate  Enter: Select  Esc/q: Cancel");
        help.render(help_inner, frame);
    }
}

fn main() -> std::io::Result<()> {
    let picker = ModalPicker::new();

    // Run in AltScreen mode for full-screen modal experience
    // Original terminal content is preserved and restored on exit
    App::new(picker).screen_mode(ScreenMode::AltScreen).run()
}
