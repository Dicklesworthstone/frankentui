#![forbid(unsafe_code)]

//! Kanban Board screen — drag-and-drop task management (bd-iuvb.12).
//!
//! Demonstrates a minimal Kanban board with three columns (Todo, In Progress,
//! Done). Cards can be moved between columns using keyboard shortcuts or
//! mouse drag-and-drop. Keeps a deterministic data set for stable snapshots.

use std::cell::Cell;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::display_width;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

/// Column identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Todo,
    InProgress,
    Done,
}

impl Column {
    fn title(self) -> &'static str {
        match self {
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::Done => "Done",
        }
    }

    fn all() -> [Self; 3] {
        [Self::Todo, Self::InProgress, Self::Done]
    }

    fn index(self) -> usize {
        match self {
            Self::Todo => 0,
            Self::InProgress => 1,
            Self::Done => 2,
        }
    }

    fn from_index(i: usize) -> Self {
        match i {
            0 => Self::Todo,
            1 => Self::InProgress,
            _ => Self::Done,
        }
    }
}

/// A single card on the board.
#[derive(Debug, Clone)]
pub struct Card {
    pub id: u32,
    pub title: String,
    pub tag: &'static str,
}

/// State of an active mouse drag operation.
#[derive(Debug, Clone)]
pub struct MouseDragState {
    /// Source column index the card is being dragged from.
    pub source_col: usize,
    /// Source row index the card is being dragged from.
    pub source_row: usize,
    /// Card ID being dragged.
    pub card_id: u32,
    /// Current mouse x position.
    pub current_x: u16,
    /// Current mouse y position.
    pub current_y: u16,
    /// Column index the mouse is currently hovering over (drop target).
    pub hover_col: Option<usize>,
}

/// Kanban Board screen state.
pub struct KanbanBoard {
    /// Cards in each column: [Todo, InProgress, Done].
    columns: [Vec<Card>; 3],
    /// Currently focused column index.
    focus_col: usize,
    /// Currently focused card index within the focused column.
    focus_row: usize,
    /// Move history for undo: (card_id, from_col, to_col, from_row, to_row).
    history: Vec<(u32, usize, usize, usize, usize)>,
    /// Redo stack.
    redo_stack: Vec<(u32, usize, usize, usize, usize)>,
    /// Tick counter.
    tick_count: u64,
    /// Active mouse drag state (None when no drag in progress).
    mouse_drag: Option<MouseDragState>,
    /// Cached column rectangles from the last render (for hit-testing).
    /// Uses `Cell` because `view()` takes `&self`.
    last_col_rects: [Cell<Rect>; 3],
}

impl Default for KanbanBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl KanbanBoard {
    /// Create a new kanban board with deterministic seed data.
    pub fn new() -> Self {
        let todo = vec![
            Card {
                id: 1,
                title: "Design login page".into(),
                tag: "UI",
            },
            Card {
                id: 2,
                title: "Add input validation".into(),
                tag: "Logic",
            },
            Card {
                id: 3,
                title: "Write unit tests".into(),
                tag: "QA",
            },
            Card {
                id: 4,
                title: "Set up CI pipeline".into(),
                tag: "Ops",
            },
        ];
        let in_progress = vec![
            Card {
                id: 5,
                title: "Build nav component".into(),
                tag: "UI",
            },
            Card {
                id: 6,
                title: "Implement auth flow".into(),
                tag: "Logic",
            },
        ];
        let done = vec![Card {
            id: 7,
            title: "Project scaffolding".into(),
            tag: "Ops",
        }];

        Self {
            columns: [todo, in_progress, done],
            focus_col: 0,
            focus_row: 0,
            history: Vec::new(),
            redo_stack: Vec::new(),
            tick_count: 0,
            mouse_drag: None,
            last_col_rects: [
                Cell::new(Rect::new(0, 0, 0, 0)),
                Cell::new(Rect::new(0, 0, 0, 0)),
                Cell::new(Rect::new(0, 0, 0, 0)),
            ],
        }
    }

    /// Number of cards in the focused column.
    fn focused_col_len(&self) -> usize {
        self.columns[self.focus_col].len()
    }

    /// Clamp focus_row to valid range.
    fn clamp_row(&mut self) {
        let len = self.focused_col_len();
        if len == 0 {
            self.focus_row = 0;
        } else if self.focus_row >= len {
            self.focus_row = len - 1;
        }
    }

    /// Move focus to the next column (right).
    fn focus_right(&mut self) {
        if self.focus_col < 2 {
            self.focus_col += 1;
            self.clamp_row();
        }
    }

    /// Move focus to the previous column (left).
    fn focus_left(&mut self) {
        if self.focus_col > 0 {
            self.focus_col -= 1;
            self.clamp_row();
        }
    }

    /// Move focus to the next card (down).
    fn focus_down(&mut self) {
        let len = self.focused_col_len();
        if len > 0 && self.focus_row < len - 1 {
            self.focus_row += 1;
        }
    }

    /// Move focus to the previous card (up).
    fn focus_up(&mut self) {
        if self.focus_row > 0 {
            self.focus_row -= 1;
        }
    }

    /// Move the focused card one column to the right.
    fn move_card_right(&mut self) {
        if self.focus_col >= 2 || self.columns[self.focus_col].is_empty() {
            return;
        }
        let from = self.focus_col;
        let to = self.focus_col + 1;
        let from_row = self.focus_row;
        let card = self.columns[from].remove(from_row);
        let card_id = card.id;
        let to_row = self.columns[to].len();
        self.columns[to].push(card);
        self.history.push((card_id, from, to, from_row, to_row));
        self.redo_stack.clear();
        // Follow the card to the new column
        self.focus_col = to;
        self.focus_row = to_row;
    }

    /// Move the focused card one column to the left.
    fn move_card_left(&mut self) {
        if self.focus_col == 0 || self.columns[self.focus_col].is_empty() {
            return;
        }
        let from = self.focus_col;
        let to = self.focus_col - 1;
        let from_row = self.focus_row;
        let card = self.columns[from].remove(from_row);
        let card_id = card.id;
        let to_row = self.columns[to].len();
        self.columns[to].push(card);
        self.history.push((card_id, from, to, from_row, to_row));
        self.redo_stack.clear();
        // Follow the card to the new column
        self.focus_col = to;
        self.focus_row = to_row;
    }

    /// Move a card from one column to another by row index.
    fn move_card(&mut self, from_col: usize, from_row: usize, to_col: usize) {
        if from_col == to_col {
            return;
        }
        if from_col > 2 || to_col > 2 {
            return;
        }
        if from_row >= self.columns[from_col].len() {
            return;
        }
        let card = self.columns[from_col].remove(from_row);
        let card_id = card.id;
        let to_row = self.columns[to_col].len();
        self.columns[to_col].push(card);
        self.history
            .push((card_id, from_col, to_col, from_row, to_row));
        self.redo_stack.clear();
        self.focus_col = to_col;
        self.focus_row = to_row;
    }

    /// Move a card by stable identity. Searches all columns for `card_id` and
    /// moves it to `to_col`. Returns `true` if the card was found and moved.
    fn move_card_by_id(&mut self, card_id: u32, to_col: usize) -> bool {
        if to_col > 2 {
            return false;
        }
        // Find the card by ID in any column.
        let mut found = None;
        for (col_idx, col) in self.columns.iter().enumerate() {
            if let Some(row_idx) = col.iter().position(|c| c.id == card_id) {
                found = Some((col_idx, row_idx));
                break;
            }
        }
        let Some((from_col, from_row)) = found else {
            return false;
        };
        if from_col == to_col {
            return false;
        }
        self.move_card(from_col, from_row, to_col);
        true
    }

    /// Determine which column a screen-space x,y coordinate falls in.
    fn hit_test_column(&self, x: u16, y: u16) -> Option<usize> {
        for (i, cell) in self.last_col_rects.iter().enumerate() {
            let rect = cell.get();
            if !rect.is_empty()
                && x >= rect.x
                && x < rect.x + rect.width
                && y >= rect.y
                && y < rect.y + rect.height
            {
                return Some(i);
            }
        }
        None
    }

    /// Determine which card within a column a screen-space y falls on.
    fn hit_test_card(&self, col_idx: usize, y: u16) -> Option<usize> {
        let rect = self.last_col_rects[col_idx].get();
        if rect.is_empty() {
            return None;
        }
        // Inner area (1-cell border on each side)
        let inner_y = rect.y + 1;
        if y < inner_y {
            return None;
        }
        let card_height: u16 = 3;
        let offset = y - inner_y;
        let card_idx = (offset / card_height) as usize;
        if card_idx < self.columns[col_idx].len() {
            Some(card_idx)
        } else {
            None
        }
    }

    /// Handle a mouse event, returning true if the event was consumed.
    fn handle_mouse(&mut self, mouse: &MouseEvent) -> bool {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Start drag if clicking on a card
                if let Some(col_idx) = self.hit_test_column(mouse.x, mouse.y) {
                    if let Some(card_idx) = self.hit_test_card(col_idx, mouse.y) {
                        let card_id = self.columns[col_idx][card_idx].id;
                        self.mouse_drag = Some(MouseDragState {
                            source_col: col_idx,
                            source_row: card_idx,
                            card_id,
                            current_x: mouse.x,
                            current_y: mouse.y,
                            hover_col: Some(col_idx),
                        });
                        // Also set keyboard focus to match
                        self.focus_col = col_idx;
                        self.focus_row = card_idx;
                        return true;
                    }
                    // Clicked on column but not a card — set focus to column
                    self.focus_col = col_idx;
                    self.clamp_row();
                    return true;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let hover_col = self.hit_test_column(mouse.x, mouse.y);
                if let Some(ref mut drag) = self.mouse_drag {
                    drag.current_x = mouse.x;
                    drag.current_y = mouse.y;
                    drag.hover_col = hover_col;
                    return true;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(drag) = self.mouse_drag.take() {
                    // Complete the drop using stable card identity so mid-drag
                    // mutations (e.g. keyboard input) can't move the wrong card.
                    if let Some(target_col) = drag.hover_col {
                        self.move_card_by_id(drag.card_id, target_col);
                    }
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    /// Returns true if a mouse drag is currently active.
    pub fn is_dragging(&self) -> bool {
        self.mouse_drag.is_some()
    }

    /// Returns the current drag state, if any.
    pub fn drag_state(&self) -> Option<&MouseDragState> {
        self.mouse_drag.as_ref()
    }

    /// Render a single column.
    fn render_column(&self, frame: &mut Frame, area: Rect, col_idx: usize) {
        if area.is_empty() {
            return;
        }

        let col = Column::from_index(col_idx);
        let is_focused_col = col_idx == self.focus_col;

        // Determine if this column is the drag hover target
        let is_drop_target = self
            .mouse_drag
            .as_ref()
            .is_some_and(|d| d.hover_col == Some(col_idx) && d.source_col != col_idx);

        let border_style = if is_drop_target {
            Style::new().fg(theme::accent::SUCCESS)
        } else if is_focused_col {
            Style::new().fg(theme::accent::INFO)
        } else {
            Style::new().fg(theme::fg::MUTED)
        };

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(if is_drop_target {
                BorderType::Double
            } else if is_focused_col {
                BorderType::Heavy
            } else {
                BorderType::Rounded
            })
            .title(col.title())
            .title_alignment(Alignment::Center)
            .style(border_style.bg(theme::bg::DEEP));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let cards = &self.columns[col_idx];
        if cards.is_empty() {
            let empty_style = Style::new().fg(theme::fg::MUTED);
            Paragraph::new("(empty)")
                .style(empty_style)
                .render(Rect::new(inner.x, inner.y, inner.width, 1), frame);
            return;
        }

        // Each card takes 3 rows (1 title + 1 tag + 1 separator)
        let card_height: u16 = 3;
        for (i, card) in cards.iter().enumerate() {
            let y_offset = i as u16 * card_height;
            if y_offset + 2 > inner.height {
                break;
            }

            // Dim the source card during drag
            let is_drag_source = self
                .mouse_drag
                .as_ref()
                .is_some_and(|d| d.source_col == col_idx && d.source_row == i);

            let is_selected = is_focused_col && i == self.focus_row && !is_drag_source;
            let card_area = Rect::new(
                inner.x,
                inner.y + y_offset,
                inner.width,
                card_height.min(inner.height - y_offset),
            );

            if is_drag_source {
                self.render_card_dimmed(frame, card_area, card);
            } else {
                self.render_card(frame, card_area, card, is_selected);
            }
        }

        // Show drop preview hint when hovering with a card
        if is_drop_target {
            let drop_y = inner.y + cards.len() as u16 * card_height;
            if drop_y < inner.y + inner.height {
                let hint_area = Rect::new(inner.x, drop_y, inner.width, 1);
                Paragraph::new("  + drop here")
                    .style(Style::new().fg(theme::accent::SUCCESS))
                    .render(hint_area, frame);
            }
        }
    }

    /// Render a single card within a column.
    fn render_card(&self, frame: &mut Frame, area: Rect, card: &Card, selected: bool) {
        if area.is_empty() {
            return;
        }

        let (title_style, tag_style) = if selected {
            (
                Style::new()
                    .fg(theme::bg::DEEP)
                    .bg(theme::accent::INFO)
                    .bold(),
                Style::new().fg(theme::bg::DEEP).bg(theme::accent::INFO),
            )
        } else {
            (
                Style::new().fg(theme::fg::PRIMARY),
                Style::new().fg(theme::fg::MUTED),
            )
        };

        // Title row (use display width, not byte length).
        let prefix = if selected { "> " } else { "  " };
        let title_width = display_width(card.title.as_str());
        let prefix_width = display_width(prefix);
        let title_text = if area.width as usize >= title_width + prefix_width {
            format!("{prefix}{}", card.title)
        } else {
            card.title.clone()
        };
        Paragraph::new(title_text.as_str())
            .style(title_style)
            .render(Rect::new(area.x, area.y, area.width, 1), frame);

        // Tag row
        if area.height > 1 {
            let tag_text = format!("  [{}]", card.tag);
            Paragraph::new(tag_text.as_str())
                .style(tag_style)
                .render(Rect::new(area.x, area.y + 1, area.width, 1), frame);
        }
    }

    /// Render a dimmed card (drag source ghost).
    fn render_card_dimmed(&self, frame: &mut Frame, area: Rect, card: &Card) {
        if area.is_empty() {
            return;
        }
        let dim_style = Style::new().fg(theme::fg::MUTED);
        let title_text = format!("  {}", card.title);
        Paragraph::new(title_text.as_str())
            .style(dim_style)
            .render(Rect::new(area.x, area.y, area.width, 1), frame);
        if area.height > 1 {
            let tag_text = format!("  [{}]", card.tag);
            Paragraph::new(tag_text.as_str())
                .style(dim_style)
                .render(Rect::new(area.x, area.y + 1, area.width, 1), frame);
        }
    }

    /// Render the instruction footer.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let move_count = self.history.len();
        let info = if self.mouse_drag.is_some() {
            " Dragging... release over a column to drop | Moves: ".to_string()
                + &move_count.to_string()
        } else {
            format!(
                " h/l: column | j/k: card | H/L: move | u/r: undo/redo | mouse: drag | moves: {}",
                move_count,
            )
        };

        Paragraph::new(info.as_str())
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::bg::DEEP))
            .render(area, frame);
    }
}

impl Screen for KanbanBoard {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        // Handle mouse events first
        if let Event::Mouse(mouse) = event {
            self.handle_mouse(mouse);
            return Cmd::None;
        }

        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                // Navigation
                KeyCode::Char('h') | KeyCode::Left => self.focus_left(),
                KeyCode::Char('l') | KeyCode::Right => self.focus_right(),
                KeyCode::Char('j') | KeyCode::Down => self.focus_down(),
                KeyCode::Char('k') | KeyCode::Up => self.focus_up(),
                // Move card
                KeyCode::Char('H') => self.move_card_left(),
                KeyCode::Char('L') => self.move_card_right(),
                // Undo
                KeyCode::Char('u') => {
                    self.undo();
                }
                // Redo
                KeyCode::Char('r') => {
                    self.redo();
                }
                _ => {}
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Split: main board area + 1-row footer
        let footer_height = 1u16;
        let board_height = area.height.saturating_sub(footer_height);
        if board_height == 0 {
            return;
        }

        let board_area = Rect::new(area.x, area.y, area.width, board_height);
        let footer_area = Rect::new(area.x, area.y + board_height, area.width, footer_height);

        // Split board into 3 equal columns
        let col_chunks = Flex::horizontal()
            .constraints([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .split(board_area);

        // Cache column rects for mouse hit-testing
        for (i, _col) in Column::all().iter().enumerate() {
            self.last_col_rects[i].set(col_chunks[i]);
            self.render_column(frame, col_chunks[i], i);
        }

        self.render_footer(frame, footer_area);
    }

    // H/L move the card; the app would take them to switch screens.
    fn consumes_key(&self, key: char) -> bool {
        matches!(key, 'H' | 'L')
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "h/l",
                action: "Move between columns",
            },
            HelpEntry {
                key: "j/k",
                action: "Move between cards",
            },
            HelpEntry {
                key: "H/L",
                action: "Move card left/right",
            },
            HelpEntry {
                key: "u",
                action: "Undo last move",
            },
            HelpEntry {
                key: "r",
                action: "Redo last undo",
            },
            HelpEntry {
                key: "mouse",
                action: "Drag card between columns",
            },
        ]
    }

    fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn undo(&mut self) -> bool {
        if let Some((card_id, from_col, to_col, from_row, _to_row)) = self.history.pop() {
            // Find the card in to_col and move it back to from_col at from_row
            if let Some(pos) = self.columns[to_col].iter().position(|c| c.id == card_id) {
                let card = self.columns[to_col].remove(pos);
                let insert_at = from_row.min(self.columns[from_col].len());
                self.columns[from_col].insert(insert_at, card);
                self.redo_stack
                    .push((card_id, from_col, to_col, insert_at, pos));
                self.focus_col = from_col;
                self.focus_row = insert_at;
                return true;
            }
        }
        false
    }

    fn redo(&mut self) -> bool {
        if let Some((card_id, from_col, to_col, _from_row, _to_row)) = self.redo_stack.pop()
            && let Some(pos) = self.columns[from_col].iter().position(|c| c.id == card_id)
        {
            let card = self.columns[from_col].remove(pos);
            let insert_at = self.columns[to_col].len();
            self.columns[to_col].push(card);
            self.history
                .push((card_id, from_col, to_col, pos, insert_at));
            self.focus_col = to_col;
            self.focus_row = insert_at;
            return true;
        }
        false
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }

    fn title(&self) -> &'static str {
        "Kanban Board"
    }

    fn tab_label(&self) -> &'static str {
        "Kanban"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn default_board_has_seed_data() {
        let board = KanbanBoard::new();
        assert_eq!(board.columns[0].len(), 4, "Todo should have 4 cards");
        assert_eq!(board.columns[1].len(), 2, "In Progress should have 2 cards");
        assert_eq!(board.columns[2].len(), 1, "Done should have 1 card");
    }

    #[test]
    fn focus_navigation() {
        let mut board = KanbanBoard::new();
        assert_eq!(board.focus_col, 0);
        assert_eq!(board.focus_row, 0);

        board.focus_down();
        assert_eq!(board.focus_row, 1);

        board.focus_down();
        assert_eq!(board.focus_row, 2);

        board.focus_up();
        assert_eq!(board.focus_row, 1);

        board.focus_right();
        assert_eq!(board.focus_col, 1);
        // Row clamps to in-progress column length (2 cards, max row 1)
        assert_eq!(board.focus_row, 1);

        board.focus_left();
        assert_eq!(board.focus_col, 0);
    }

    #[test]
    fn focus_clamping_at_boundaries() {
        let mut board = KanbanBoard::new();

        // Can't go left from column 0
        board.focus_left();
        assert_eq!(board.focus_col, 0);

        // Can't go up from row 0
        board.focus_up();
        assert_eq!(board.focus_row, 0);

        // Go to last column
        board.focus_right();
        board.focus_right();
        assert_eq!(board.focus_col, 2);

        // Can't go right from last column
        board.focus_right();
        assert_eq!(board.focus_col, 2);
    }

    #[test]
    fn move_card_right() {
        let mut board = KanbanBoard::new();
        let card_id = board.columns[0][0].id;

        board.move_card_right();

        // Card moved from Todo to In Progress
        assert_eq!(board.columns[0].len(), 3);
        assert_eq!(board.columns[1].len(), 3);
        assert_eq!(board.columns[1].last().unwrap().id, card_id);
        // Focus follows the card
        assert_eq!(board.focus_col, 1);
        assert_eq!(board.focus_row, 2);
    }

    #[test]
    fn move_card_left() {
        let mut board = KanbanBoard::new();
        // Focus on In Progress column
        board.focus_col = 1;
        board.focus_row = 0;
        let card_id = board.columns[1][0].id;

        board.move_card_left();

        // Card moved from In Progress to Todo
        assert_eq!(board.columns[0].len(), 5);
        assert_eq!(board.columns[1].len(), 1);
        assert_eq!(board.columns[0].last().unwrap().id, card_id);
        assert_eq!(board.focus_col, 0);
    }

    #[test]
    fn move_card_right_from_done_is_noop() {
        let mut board = KanbanBoard::new();
        board.focus_col = 2;
        board.focus_row = 0;

        board.move_card_right();

        // Nothing changes
        assert_eq!(board.columns[2].len(), 1);
        assert_eq!(board.focus_col, 2);
    }

    #[test]
    fn move_card_left_from_todo_is_noop() {
        let mut board = KanbanBoard::new();
        board.focus_col = 0;
        board.focus_row = 0;

        board.move_card_left();

        // Nothing changes
        assert_eq!(board.columns[0].len(), 4);
        assert_eq!(board.focus_col, 0);
    }

    #[test]
    fn undo_redo_cycle() {
        let mut board = KanbanBoard::new();
        let card_id = board.columns[0][0].id;

        // Move card right
        board.move_card_right();
        assert_eq!(board.columns[0].len(), 3);
        assert_eq!(board.columns[1].len(), 3);

        // Undo
        assert!(board.can_undo());
        board.undo();
        assert_eq!(board.columns[0].len(), 4);
        assert_eq!(board.columns[1].len(), 2);
        assert_eq!(board.columns[0][0].id, card_id);

        // Redo
        assert!(board.can_redo());
        board.redo();
        assert_eq!(board.columns[0].len(), 3);
        assert_eq!(board.columns[1].len(), 3);
    }

    #[test]
    fn move_clears_redo_stack() {
        let mut board = KanbanBoard::new();

        board.move_card_right();
        board.undo();
        assert!(board.can_redo());

        // New move should clear redo
        board.move_card_right();
        assert!(!board.can_redo());
    }

    #[test]
    fn key_event_navigation() {
        use super::Screen;
        let mut board = KanbanBoard::new();

        let down = Event::Key(KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        board.update(&down);
        assert_eq!(board.focus_row, 1);

        let right = Event::Key(KeyEvent {
            code: KeyCode::Char('l'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        board.update(&right);
        assert_eq!(board.focus_col, 1);
    }

    #[test]
    fn key_event_move_card() {
        use super::Screen;
        let mut board = KanbanBoard::new();

        let move_right = Event::Key(KeyEvent {
            code: KeyCode::Char('L'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        board.update(&move_right);
        assert_eq!(board.columns[0].len(), 3);
        assert_eq!(board.columns[1].len(), 3);
    }

    #[test]
    fn render_does_not_panic() {
        use super::Screen;
        let board = KanbanBoard::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        board.view(&mut frame, area);
    }

    #[test]
    fn render_zero_area_does_not_panic() {
        use super::Screen;
        let board = KanbanBoard::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let area = Rect::new(0, 0, 0, 0);
        board.view(&mut frame, area);
    }

    #[test]
    fn render_tiny_area_does_not_panic() {
        use super::Screen;
        let board = KanbanBoard::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        board.view(&mut frame, area);
    }

    #[test]
    fn keybindings_returns_entries() {
        use super::Screen;
        let board = KanbanBoard::new();
        let bindings = board.keybindings();
        assert_eq!(bindings.len(), 6);
    }

    #[test]
    fn title_and_label() {
        use super::Screen;
        let board = KanbanBoard::new();
        assert_eq!(board.title(), "Kanban Board");
        assert_eq!(board.tab_label(), "Kanban");
    }

    #[test]
    fn move_from_empty_column_is_noop() {
        let mut board = KanbanBoard::new();
        // Clear the done column
        board.columns[2].clear();
        board.focus_col = 2;
        board.focus_row = 0;

        board.move_card_right();
        assert_eq!(board.columns[2].len(), 0);

        board.move_card_left();
        assert_eq!(board.columns[2].len(), 0);
    }

    #[test]
    fn focus_clamps_when_moving_to_shorter_column() {
        let mut board = KanbanBoard::new();
        // Focus on last card in Todo (index 3)
        board.focus_row = 3;
        assert_eq!(board.focus_col, 0);

        // Move to In Progress (only 2 cards, max row 1)
        board.focus_right();
        assert_eq!(board.focus_col, 1);
        assert_eq!(board.focus_row, 1);
    }

    // -----------------------------------------------------------------------
    // Mouse drag-and-drop tests
    // -----------------------------------------------------------------------

    /// Helper: set up column rects as if the board was rendered at 90x24.
    fn setup_col_rects(board: &mut KanbanBoard) {
        // Simulate 3 equal columns in an 90-wide, 23-tall board area
        board.last_col_rects[0].set(Rect::new(0, 0, 30, 23));
        board.last_col_rects[1].set(Rect::new(30, 0, 30, 23));
        board.last_col_rects[2].set(Rect::new(60, 0, 30, 23));
    }

    #[test]
    fn move_card_between_columns() {
        let mut board = KanbanBoard::new();
        let card_id = board.columns[0][1].id; // "Add input validation"

        board.move_card(0, 1, 2);

        assert_eq!(board.columns[0].len(), 3);
        assert_eq!(board.columns[2].len(), 2);
        assert_eq!(board.columns[2].last().unwrap().id, card_id);
        assert!(board.can_undo());
    }

    #[test]
    fn move_card_same_column_is_noop() {
        let mut board = KanbanBoard::new();
        board.move_card(0, 0, 0);
        assert_eq!(board.columns[0].len(), 4);
        assert!(!board.can_undo());
    }

    #[test]
    fn move_card_invalid_source_row_is_noop() {
        let mut board = KanbanBoard::new();
        board.move_card(0, 99, 1);
        assert_eq!(board.columns[0].len(), 4);
        assert_eq!(board.columns[1].len(), 2);
    }

    #[test]
    fn hit_test_column_identifies_columns() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        assert_eq!(board.hit_test_column(5, 5), Some(0));
        assert_eq!(board.hit_test_column(35, 5), Some(1));
        assert_eq!(board.hit_test_column(65, 5), Some(2));
    }

    #[test]
    fn hit_test_column_returns_none_outside() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // y=23 is outside the 0..23 range
        assert_eq!(board.hit_test_column(5, 23), None);
    }

    #[test]
    fn hit_test_column_no_rects_returns_none() {
        let board = KanbanBoard::new();
        // All rects are empty by default
        assert_eq!(board.hit_test_column(5, 5), None);
    }

    #[test]
    fn hit_test_card_identifies_cards() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // Column 0 inner starts at y=1 (border), card_height=3
        // Card 0: y=1..4, Card 1: y=4..7, Card 2: y=7..10, Card 3: y=10..13
        assert_eq!(board.hit_test_card(0, 1), Some(0));
        assert_eq!(board.hit_test_card(0, 3), Some(0));
        assert_eq!(board.hit_test_card(0, 4), Some(1));
        assert_eq!(board.hit_test_card(0, 7), Some(2));
        assert_eq!(board.hit_test_card(0, 10), Some(3));
    }

    #[test]
    fn hit_test_card_returns_none_past_last_card() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // Column 0 has 4 cards (indices 0-3), card_height=3
        // y=13 and beyond: card index 4+ which doesn't exist
        assert_eq!(board.hit_test_card(0, 13), None);
    }

    #[test]
    fn hit_test_card_returns_none_in_border() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // y=0 is the top border of the column
        assert_eq!(board.hit_test_card(0, 0), None);
    }

    #[test]
    fn mouse_down_starts_drag() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // Click on card 0 in column 0 (inner y=1)
        let mouse = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 1);
        assert!(board.handle_mouse(&mouse));
        assert!(board.is_dragging());

        let drag = board.drag_state().unwrap();
        assert_eq!(drag.source_col, 0);
        assert_eq!(drag.source_row, 0);
        assert_eq!(drag.card_id, 1);
    }

    #[test]
    fn mouse_down_on_empty_area_no_drag() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // Click on column 2, past the single card (y > 1+3=4)
        let mouse = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 65, 20);
        board.handle_mouse(&mouse);
        assert!(!board.is_dragging());
    }

    #[test]
    fn mouse_drag_updates_hover_col() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // Start drag on card 0, column 0
        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 1);
        board.handle_mouse(&down);
        assert!(board.is_dragging());

        // Drag to column 1
        let drag = MouseEvent::new(MouseEventKind::Drag(MouseButton::Left), 35, 5);
        assert!(board.handle_mouse(&drag));

        let state = board.drag_state().unwrap();
        assert_eq!(state.hover_col, Some(1));
        assert_eq!(state.current_x, 35);
        assert_eq!(state.current_y, 5);
    }

    #[test]
    fn mouse_up_completes_drop() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        let card_id = board.columns[0][0].id;

        // Start drag
        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 1);
        board.handle_mouse(&down);

        // Drag to column 2
        let drag = MouseEvent::new(MouseEventKind::Drag(MouseButton::Left), 65, 5);
        board.handle_mouse(&drag);

        // Release
        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 65, 5);
        board.handle_mouse(&up);

        assert!(!board.is_dragging());
        assert_eq!(board.columns[0].len(), 3);
        assert_eq!(board.columns[2].len(), 2);
        assert_eq!(board.columns[2].last().unwrap().id, card_id);
        // Focus follows the card
        assert_eq!(board.focus_col, 2);
    }

    #[test]
    fn mouse_up_same_column_no_move() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // Start drag on column 0, card 0
        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 1);
        board.handle_mouse(&down);

        // Release on same column
        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 5, 10);
        board.handle_mouse(&up);

        assert!(!board.is_dragging());
        // No move happened
        assert_eq!(board.columns[0].len(), 4);
        assert!(!board.can_undo());
    }

    #[test]
    fn mouse_drag_is_undoable() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // Drag card from column 0 to column 1
        let down = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 1);
        board.handle_mouse(&down);
        let drag = MouseEvent::new(MouseEventKind::Drag(MouseButton::Left), 35, 5);
        board.handle_mouse(&drag);
        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 35, 5);
        board.handle_mouse(&up);

        assert!(board.can_undo());
        board.undo();
        assert_eq!(board.columns[0].len(), 4);
        assert_eq!(board.columns[1].len(), 2);
    }

    #[test]
    fn mouse_drag_no_drag_active_ignored() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        // Drag event without prior mousedown
        let drag = MouseEvent::new(MouseEventKind::Drag(MouseButton::Left), 35, 5);
        assert!(!board.handle_mouse(&drag));
    }

    #[test]
    fn mouse_up_no_drag_active_ignored() {
        let mut board = KanbanBoard::new();
        setup_col_rects(&mut board);

        let up = MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 35, 5);
        assert!(!board.handle_mouse(&up));
    }

    #[test]
    fn render_during_drag_does_not_panic() {
        use super::Screen;
        let mut board = KanbanBoard::new();
        // Simulate an active drag
        board.mouse_drag = Some(MouseDragState {
            source_col: 0,
            source_row: 0,
            card_id: 1,
            current_x: 40,
            current_y: 5,
            hover_col: Some(1),
        });

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        board.view(&mut frame, area);
    }

    #[test]
    fn view_caches_col_rects() {
        use super::Screen;
        let board = KanbanBoard::new();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(90, 24, &mut pool);
        let area = Rect::new(0, 0, 90, 24);
        board.view(&mut frame, area);

        // After render, column rects should be non-empty
        let r0 = board.last_col_rects[0].get();
        let r1 = board.last_col_rects[1].get();
        let r2 = board.last_col_rects[2].get();
        assert!(!r0.is_empty(), "col 0 rect should not be empty");
        assert!(!r1.is_empty(), "col 1 rect should not be empty");
        assert!(!r2.is_empty(), "col 2 rect should not be empty");
        // Columns should be side by side
        assert_eq!(r0.x + r0.width, r1.x);
        assert_eq!(r1.x + r1.width, r2.x);
    }

    #[test]
    fn full_mouse_drag_lifecycle_via_events() {
        use super::Screen;
        let mut board = KanbanBoard::new();

        // First render to populate col_rects
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(90, 24, &mut pool);
        board.view(&mut frame, Rect::new(0, 0, 90, 24));

        let card_id = board.columns[0][0].id;
        let col0 = board.last_col_rects[0].get();
        let col2 = board.last_col_rects[2].get();

        // Click on first card in column 0
        let click_x = col0.x + 2;
        let click_y = col0.y + 2; // inner y (past border + first card)
        let down = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            click_x,
            click_y,
        ));
        board.update(&down);
        assert!(board.is_dragging());

        // Drag to column 2
        let drag_x = col2.x + 5;
        let drag_y = col2.y + 5;
        let drag = Event::Mouse(MouseEvent::new(
            MouseEventKind::Drag(MouseButton::Left),
            drag_x,
            drag_y,
        ));
        board.update(&drag);
        assert_eq!(board.drag_state().unwrap().hover_col, Some(2));

        // Release
        let up = Event::Mouse(MouseEvent::new(
            MouseEventKind::Up(MouseButton::Left),
            drag_x,
            drag_y,
        ));
        board.update(&up);
        assert!(!board.is_dragging());

        // Card should have moved
        assert_eq!(board.columns[0].len(), 3);
        assert_eq!(board.columns[2].len(), 2);
        assert_eq!(board.columns[2].last().unwrap().id, card_id);
    }

    #[test]
    fn move_card_by_id_finds_card_after_mutation() {
        let mut board = KanbanBoard::new();
        let card_id = board.columns[0][1].id; // "Add input validation"

        // Simulate mid-drag mutation: another card is removed from the same
        // column, shifting the target card's row index.
        board.columns[0].remove(0);
        // card_id was at row 1 but is now at row 0 due to the removal above.

        // move_card_by_id should still find the correct card.
        assert!(board.move_card_by_id(card_id, 2));
        assert_eq!(board.columns[2].last().unwrap().id, card_id);
    }

    #[test]
    fn move_card_by_id_same_column_is_noop() {
        let mut board = KanbanBoard::new();
        let card_id = board.columns[0][0].id;
        assert!(!board.move_card_by_id(card_id, 0));
        assert_eq!(board.columns[0].len(), 4);
    }

    #[test]
    fn move_card_by_id_missing_card_is_noop() {
        let mut board = KanbanBoard::new();
        assert!(!board.move_card_by_id(9999, 1));
    }
}
