#![forbid(unsafe_code)]

//! Forms and Input screen — interactive form widgets and text editing.
//!
//! Demonstrates:
//! - `Form` with Text, Checkbox, Radio, Select, Number fields
//! - `TextInput` (single-line, with password mask)
//! - `TextArea` (multi-line editor with line numbers)
//! - Panel-based focus management

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_extras::forms::{Form, FormField, FormState, FormValue, ValidationError};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_text::CursorPosition;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::textarea::TextArea;
use ftui_widgets::{Badge, StatefulWidget, TextInputUndoExt, Widget};

use super::{HelpEntry, Screen};
use crate::theme;

/// Which panel currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPanel {
    Form,
    SearchInput,
    PasswordInput,
    TextEditor,
}

impl FocusPanel {
    fn next(self) -> Self {
        match self {
            Self::Form => Self::SearchInput,
            Self::SearchInput => Self::PasswordInput,
            Self::PasswordInput => Self::TextEditor,
            Self::TextEditor => Self::Form,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Form => Self::TextEditor,
            Self::SearchInput => Self::Form,
            Self::PasswordInput => Self::SearchInput,
            Self::TextEditor => Self::PasswordInput,
        }
    }
}

const UNDO_HISTORY_LIMIT: usize = 50;

#[derive(Debug, Clone, Copy)]
struct KeyChord {
    code: KeyCode,
    modifiers: Modifiers,
}

impl KeyChord {
    const fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self { code, modifiers }
    }

    fn matches(self, code: KeyCode, modifiers: Modifiers) -> bool {
        self.code == code && self.modifiers == modifiers
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UndoKeybindings {
    undo: KeyChord,
    redo_primary: KeyChord,
    redo_secondary: KeyChord,
}

impl Default for UndoKeybindings {
    fn default() -> Self {
        Self {
            undo: KeyChord::new(KeyCode::Char('z'), Modifiers::CTRL),
            redo_primary: KeyChord::new(KeyCode::Char('y'), Modifiers::CTRL),
            redo_secondary: KeyChord::new(KeyCode::Char('Z'), Modifiers::CTRL | Modifiers::SHIFT),
        }
    }
}

#[derive(Debug, Clone)]
struct FormsInputSnapshot {
    focus: FocusPanel,
    form_values: Vec<(String, FormValue)>,
    form_focused: usize,
    form_text_cursor: usize,
    form_submitted: bool,
    form_cancelled: bool,
    form_errors: Vec<ValidationError>,
    search_value: String,
    search_cursor: usize,
    password_value: String,
    password_cursor: usize,
    textarea_text: String,
    textarea_cursor: CursorPosition,
}

impl FormsInputSnapshot {
    fn is_equivalent(&self, other: &Self) -> bool {
        self.form_values == other.form_values
            && self.search_value == other.search_value
            && self.password_value == other.password_value
            && self.textarea_text == other.textarea_text
    }
}

#[derive(Debug, Clone)]
struct UndoEntry {
    description: String,
    snapshot: FormsInputSnapshot,
}

pub struct FormsInput {
    focus: FocusPanel,
    form: Form,
    /// `RefCell` because `StatefulWidget::render` needs `&mut FormState`
    /// but `Screen::view` only has `&self`.
    form_state: RefCell<FormState>,
    search_input: TextInput,
    password_input: TextInput,
    textarea: TextArea,
    status_text: String,
    undo_stack: VecDeque<UndoEntry>,
    redo_stack: VecDeque<UndoEntry>,
    undo_panel_visible: bool,
    undo_keys: UndoKeybindings,
    layout_form: Cell<Rect>,
    layout_input: Cell<Rect>,
    layout_editor: Cell<Rect>,
    layout_search: Cell<Rect>,
    layout_password: Cell<Rect>,
}

impl Default for FormsInput {
    fn default() -> Self {
        Self::new()
    }
}

impl FormsInput {
    pub fn new() -> Self {
        let mut form = Form::new(vec![
            FormField::text_with_placeholder("Name", "Enter your name..."),
            FormField::text_with_placeholder("Email", "user@example.com"),
            FormField::select(
                "Role",
                vec![
                    "Developer".into(),
                    "Designer".into(),
                    "Manager".into(),
                    "QA Engineer".into(),
                ],
            ),
            FormField::radio(
                "Theme",
                vec!["Light".into(), "Dark".into(), "System".into()],
            ),
            FormField::number_bounded("Age", 25, 0, 120),
            FormField::checkbox("Accept Terms", false),
        ])
        .validate(
            0,
            Box::new(|field| {
                if let FormField::Text { value, .. } = field
                    && value.trim().is_empty()
                {
                    return Some("Name is required".into());
                }
                None
            }),
        )
        .validate(
            1,
            Box::new(|field| {
                if let FormField::Text { value, .. } = field {
                    if value.trim().is_empty() {
                        return Some("Email is required".into());
                    }
                    if !value.contains('@') || !value.contains('.') {
                        return Some("Enter a valid email".into());
                    }
                }
                None
            }),
        )
        .validate(
            4,
            Box::new(|field| {
                if let FormField::Number { value, .. } = field
                    && *value < 18
                {
                    return Some("Must be 18+".into());
                }
                None
            }),
        )
        .validate(
            5,
            Box::new(|field| {
                if let FormField::Checkbox { checked, .. } = field
                    && !*checked
                {
                    return Some("Required to continue".into());
                }
                None
            }),
        );

        form.set_required(0, true);
        form.set_required(1, true);
        form.set_required(5, true);
        form.set_disabled(3, true);

        let search_input = TextInput::new()
            .with_placeholder("Search...")
            .with_style(Style::new().fg(theme::fg::PRIMARY))
            .with_focused(false);

        let password_input = TextInput::new()
            .with_placeholder("Password")
            .with_mask('*')
            .with_style(Style::new().fg(theme::fg::PRIMARY))
            .with_focused(false);

        let textarea = TextArea::new()
            .with_text(
                "Hello, world!\n\
                 \n\
                 This is a multi-line text editor.\n\
                 You can type, select, undo/redo, and more.\n\
                 \n\
                 Try Shift+Arrow to select text.\n\
                 Ctrl+A selects all.\n\
                 Ctrl+Z to undo, Ctrl+Y to redo.",
            )
            .with_placeholder("Type something...")
            .with_line_numbers(true)
            .with_style(Style::new().fg(theme::fg::PRIMARY))
            .with_focus(false);

        let mut state = Self {
            focus: FocusPanel::Form,
            form,
            form_state: RefCell::new(FormState::default()),
            search_input,
            password_input,
            textarea,
            status_text: "Ctrl+\u{2190}/\u{2192}: switch panels | Form: Tab/\u{2191}/\u{2193} navigate, Space toggle, Enter submit".into(),
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            undo_panel_visible: false,
            undo_keys: UndoKeybindings::default(),
            layout_form: Cell::new(Rect::default()),
            layout_input: Cell::new(Rect::default()),
            layout_editor: Cell::new(Rect::default()),
            layout_search: Cell::new(Rect::default()),
            layout_password: Cell::new(Rect::default()),
        };
        state.apply_theme();
        state.form_state.borrow_mut().init_tracking(&state.form);
        state.update_form_validation(false);
        state.update_status();
        state
    }

    /// Configure undo/redo keybindings (customization support).
    #[allow(dead_code)]
    fn with_undo_keybindings(mut self, bindings: UndoKeybindings) -> Self {
        self.undo_keys = bindings;
        self
    }

    pub fn apply_theme(&mut self) {
        self.form.set_style(
            Style::new()
                .fg(theme::fg::PRIMARY)
                .bg(theme::alpha::SURFACE),
        );
        self.form
            .set_label_style(Style::new().fg(theme::fg::SECONDARY));
        self.form.set_focused_style(
            Style::new()
                .fg(theme::fg::PRIMARY)
                .bg(theme::alpha::HIGHLIGHT)
                .attrs(StyleFlags::BOLD),
        );
        self.form
            .set_error_style(Style::new().fg(theme::accent::ERROR).bold());
        self.form
            .set_success_style(Style::new().fg(theme::accent::SUCCESS));
        self.form
            .set_disabled_style(Style::new().fg(theme::fg::MUTED).attrs(StyleFlags::DIM));
        self.form
            .set_required_style(Style::new().fg(theme::accent::WARNING).bold());

        let input_style = Style::new()
            .fg(theme::fg::PRIMARY)
            .bg(theme::alpha::SURFACE);
        let placeholder_style = Style::new().fg(theme::fg::MUTED);
        let cursor_style = Style::new()
            .fg(theme::bg::BASE)
            .bg(theme::screen_accent::FORMS_INPUT);
        let selection_style = Style::new()
            .bg(theme::alpha::HIGHLIGHT)
            .fg(theme::fg::PRIMARY);
        self.search_input = self
            .search_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style)
            .with_cursor_style(cursor_style)
            .with_selection_style(selection_style);
        self.password_input = self
            .password_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style)
            .with_cursor_style(cursor_style)
            .with_selection_style(selection_style);
        self.textarea = self
            .textarea
            .clone()
            .with_style(
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::SURFACE),
            )
            .with_cursor_line_style(Style::new().bg(theme::alpha::HIGHLIGHT))
            .with_selection_style(
                Style::new()
                    .bg(theme::alpha::HIGHLIGHT)
                    .fg(theme::fg::PRIMARY),
            );
    }

    fn update_focus_states(&mut self) {
        self.search_input
            .set_focused(self.focus == FocusPanel::SearchInput);
        self.password_input
            .set_focused(self.focus == FocusPanel::PasswordInput);
        self.textarea
            .set_focused(self.focus == FocusPanel::TextEditor);
    }

    fn update_status(&mut self) {
        let form_state = self.form_state.borrow();
        let base = match self.focus {
            FocusPanel::Form => {
                if form_state.submitted {
                    let data = self.form.data();
                    format!(
                        "Form submitted! Name={}",
                        data.get("Name")
                            .map_or_else(|| "(empty)".into(), |v| format!("{v:?}"))
                    )
                } else if form_state.cancelled {
                    "Form cancelled.".into()
                } else if let Some(field) = self.form.field(form_state.focused) {
                    format!(
                        "Editing: {} (field {}/{})",
                        field.label(),
                        form_state.focused + 1,
                        self.form.field_count()
                    )
                } else {
                    "Form panel active".into()
                }
            }
            FocusPanel::SearchInput => {
                format!(
                    "Search: \"{}\" ({} chars)",
                    self.search_input.value(),
                    self.search_input.value().len()
                )
            }
            FocusPanel::PasswordInput => {
                format!(
                    "Password: {} chars entered",
                    self.password_input.value().len()
                )
            }
            FocusPanel::TextEditor => {
                let cursor = self.textarea.cursor();
                format!(
                    "Editor: line {}, col {} | {} lines",
                    cursor.line + 1,
                    cursor.grapheme + 1,
                    self.textarea.line_count()
                )
            }
        };
        let undo_info = format!(
            "Undo:{} Redo:{}",
            self.undo_stack.len(),
            self.redo_stack.len()
        );
        let error_info = format!("Errors: {}", form_state.errors.len());
        let (required_done, required_total) = self.required_progress();
        let required_info = if required_total == 0 {
            "Required: n/a".to_string()
        } else {
            format!("Required: {required_done}/{required_total}")
        };
        let history_hint = if self.undo_panel_visible {
            "Ctrl+U: Hide history"
        } else {
            "Ctrl+U: Show history"
        };
        self.status_text =
            format!("{base} | {required_info} | {undo_info} | {error_info} | {history_hint}");
    }

    fn update_form_validation(&mut self, force_all: bool) {
        let errors = self.form.validate_all();
        let mut state = self.form_state.borrow_mut();
        if force_all {
            state.errors = errors;
            return;
        }

        state.errors = errors
            .into_iter()
            .filter(|err| state.is_touched(err.field) || state.is_dirty(err.field))
            .collect();
    }

    fn field_is_filled(field: &FormField) -> bool {
        match field {
            FormField::Text { value, .. } => !value.trim().is_empty(),
            FormField::Checkbox { checked, .. } => *checked,
            FormField::Radio {
                options, selected, ..
            } => !options.is_empty() && *selected < options.len(),
            FormField::Select {
                options, selected, ..
            } => !options.is_empty() && *selected < options.len(),
            FormField::Number { .. } => true,
        }
    }

    fn required_progress(&self) -> (usize, usize) {
        let mut total = 0usize;
        let mut filled = 0usize;
        for idx in 0..self.form.field_count() {
            if !self.form.is_required(idx) {
                continue;
            }
            total += 1;
            if let Some(field) = self.form.field(idx)
                && Self::field_is_filled(field)
            {
                filled += 1;
            }
        }
        (filled, total)
    }

    fn filled_progress(&self) -> (usize, usize) {
        let total = self.form.field_count();
        let mut filled = 0usize;
        for idx in 0..total {
            if let Some(field) = self.form.field(idx)
                && Self::field_is_filled(field)
            {
                filled += 1;
            }
        }
        (filled, total)
    }

    fn render_badge_row(&self, frame: &mut Frame, area: Rect, badges: &[(&str, Style)]) {
        if area.is_empty() {
            return;
        }

        let mut x = area.x;
        let max_x = area.right();
        for (label, style) in badges {
            let badge = Badge::new(label).with_style(*style);
            let width = badge.width().min(area.width);
            if x >= max_x || x.saturating_add(width) > max_x {
                break;
            }
            badge.render(Rect::new(x, area.y, width, 1), frame);
            x = x.saturating_add(width).saturating_add(1);
        }
    }

    fn render_form_summary(&self, frame: &mut Frame, area: Rect, state: &FormState) {
        if area.is_empty() {
            return;
        }

        let styles = theme::semantic_styles();
        let error_count = state.errors.len();
        let (required_filled, required_total) = self.required_progress();
        let (filled_fields, total_fields) = self.filled_progress();

        let (status_label, status_style) = if state.submitted {
            ("SUBMITTED", styles.intent.success.badge_style)
        } else if state.cancelled {
            ("CANCELLED", styles.intent.warning.badge_style)
        } else if error_count > 0 {
            ("NEEDS ATTN", styles.intent.error.badge_style)
        } else if required_total > 0 && required_filled == required_total {
            ("READY", styles.intent.success.badge_style)
        } else {
            ("DRAFT", styles.intent.info.badge_style)
        };

        let required_label = format!("REQ {required_filled}/{required_total}");
        let error_label = format!("ERR {error_count}");
        let filled_label = format!("FIELDS {filled_fields}/{total_fields}");

        let required_style = if required_total == 0 {
            styles.intent.info.badge_style
        } else if required_filled == required_total {
            styles.intent.success.badge_style
        } else {
            styles.intent.warning.badge_style
        };
        let error_style = if error_count == 0 {
            styles.intent.success.badge_style
        } else {
            styles.intent.error.badge_style
        };
        let filled_style = if filled_fields == total_fields {
            styles.intent.success.badge_style
        } else {
            styles.intent.info.badge_style
        };

        let badges = [
            (status_label, status_style),
            (required_label.as_str(), required_style),
            (error_label.as_str(), error_style),
            (filled_label.as_str(), filled_style),
        ];

        self.render_badge_row(frame, area, &badges);
    }

    fn snapshot(&self) -> FormsInputSnapshot {
        let form_state = self.form_state.borrow();
        FormsInputSnapshot {
            focus: self.focus,
            form_values: self.form.data().values,
            form_focused: form_state.focused,
            form_text_cursor: form_state.text_cursor,
            form_submitted: form_state.submitted,
            form_cancelled: form_state.cancelled,
            form_errors: form_state.errors.clone(),
            search_value: self.search_input.value().to_string(),
            search_cursor: self.search_input.cursor(),
            password_value: self.password_input.value().to_string(),
            password_cursor: self.password_input.cursor(),
            textarea_text: self.textarea.text(),
            textarea_cursor: self.textarea.cursor(),
        }
    }

    fn restore_snapshot(&mut self, snapshot: &FormsInputSnapshot) {
        self.focus = snapshot.focus;

        for idx in 0..self.form.field_count() {
            let Some(field) = self.form.field_mut(idx) else {
                continue;
            };
            let Some((_, value)) = snapshot
                .form_values
                .iter()
                .find(|(label, _)| label == field.label())
            else {
                continue;
            };

            match (field, value) {
                (FormField::Text { value: text, .. }, FormValue::Text(next)) => {
                    *text = next.clone();
                }
                (FormField::Checkbox { checked, .. }, FormValue::Bool(next)) => {
                    *checked = *next;
                }
                (
                    FormField::Radio {
                        options, selected, ..
                    },
                    FormValue::Choice { index, .. },
                )
                | (
                    FormField::Select {
                        options, selected, ..
                    },
                    FormValue::Choice { index, .. },
                ) => {
                    if !options.is_empty() {
                        *selected = (*index).min(options.len().saturating_sub(1));
                    } else {
                        *selected = 0;
                    }
                }
                (
                    FormField::Number {
                        value, min, max, ..
                    },
                    FormValue::Number(next),
                ) => {
                    let mut clamped = *next;
                    if let Some(min) = min {
                        clamped = clamped.max(*min);
                    }
                    if let Some(max) = max {
                        clamped = clamped.min(*max);
                    }
                    *value = clamped;
                }
                _ => {}
            }
        }

        {
            let mut state = self.form_state.borrow_mut();
            state.focused = snapshot
                .form_focused
                .min(self.form.field_count().saturating_sub(1));
            state.text_cursor = snapshot.form_text_cursor;
            state.submitted = snapshot.form_submitted;
            state.cancelled = snapshot.form_cancelled;
            state.errors = snapshot.form_errors.clone();
            state.scroll = 0;
            state.reset_dirty(&self.form);
        } // Drop state borrow before calling self methods

        self.search_input.set_value(snapshot.search_value.clone());
        self.search_input
            .set_cursor_position(snapshot.search_cursor);
        self.password_input
            .set_value(snapshot.password_value.clone());
        self.password_input
            .set_cursor_position(snapshot.password_cursor);

        self.textarea.set_text(&snapshot.textarea_text);
        self.textarea.set_cursor_position(snapshot.textarea_cursor);

        self.update_focus_states();
        self.update_status();
    }

    fn undo_description_for_focus(&self) -> &'static str {
        match self.focus {
            FocusPanel::Form => "Edit form",
            FocusPanel::SearchInput => "Edit search input",
            FocusPanel::PasswordInput => "Edit password input",
            FocusPanel::TextEditor => "Edit text area",
        }
    }

    fn record_undo(&mut self, description: &str, snapshot: FormsInputSnapshot) {
        self.undo_stack.push_back(UndoEntry {
            description: description.to_string(),
            snapshot,
        });
        self.redo_stack.clear();

        while self.undo_stack.len() > UNDO_HISTORY_LIMIT {
            self.undo_stack.pop_front();
        }
    }

    fn undo_description(&self) -> Option<&str> {
        self.undo_stack
            .back()
            .map(|entry| entry.description.as_str())
    }

    fn perform_undo(&mut self) {
        let Some(entry) = self.undo_stack.pop_back() else {
            self.update_status();
            return;
        };

        let current = self.snapshot();
        self.redo_stack.push_back(UndoEntry {
            description: entry.description.clone(),
            snapshot: current,
        });
        self.restore_snapshot(&entry.snapshot);
    }

    fn perform_redo(&mut self) {
        let Some(entry) = self.redo_stack.pop_back() else {
            self.update_status();
            return;
        };

        let current = self.snapshot();
        self.undo_stack.push_back(UndoEntry {
            description: entry.description.clone(),
            snapshot: current,
        });
        self.restore_snapshot(&entry.snapshot);
    }

    fn render_undo_panel(&self, frame: &mut Frame, area: Rect) {
        if area.height < 4 || area.width < 12 {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Undo History ")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::accent::INFO));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("Undo ({})", self.undo_stack.len()));
        for entry in self.undo_stack.iter().rev().take(4) {
            lines.push(format!("  • {}", entry.description));
        }

        lines.push(String::new());
        lines.push(format!("Redo ({})", self.redo_stack.len()));
        for entry in self.redo_stack.iter().rev().take(4) {
            lines.push(format!("  • {}", entry.description));
        }

        Paragraph::new(lines.join("\n"))
            .style(theme::body())
            .render(inner, frame);
    }

    fn render_form_panel(&self, frame: &mut Frame, area: Rect) {
        self.layout_form.set(area);
        let border_style = theme::panel_border_style(
            self.focus == FocusPanel::Form,
            theme::screen_accent::FORMS_INPUT,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Registration Form")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut state = self.form_state.borrow_mut();
        let show_header = inner.height >= 4;
        let show_footer = inner.height >= 6;

        let mut constraints = Vec::new();
        if show_header {
            constraints.push(Constraint::Fixed(1));
        }
        constraints.push(Constraint::Min(1));
        if show_footer {
            constraints.push(Constraint::Fixed(2));
        }

        let chunks = Flex::vertical().constraints(constraints).split(inner);
        let mut idx = 0usize;

        if show_header {
            self.render_form_summary(frame, chunks[idx], &state);
            idx = idx.saturating_add(1);
        }

        if let Some(form_area) = chunks.get(idx) {
            StatefulWidget::render(&self.form, *form_area, frame, &mut state);
            idx = idx.saturating_add(1);
        }

        let legend = "Legend: * required | red=error | green=valid | dim=disabled";
        let hint = if state.submitted {
            format!("Form submitted successfully!\n{legend}")
        } else if state.cancelled {
            format!("Form cancelled\n{legend}")
        } else {
            format!("Tab: next | Enter: submit | Esc: cancel\n{legend}")
        };
        let hint_style = if state.submitted {
            Style::new().fg(theme::accent::SUCCESS)
        } else if state.cancelled {
            Style::new().fg(theme::accent::WARNING)
        } else {
            theme::muted()
        };
        if show_footer && let Some(footer_area) = chunks.get(idx) {
            Paragraph::new(hint)
                .style(hint_style)
                .render(*footer_area, frame);
        }
    }

    fn render_input_panel(&self, frame: &mut Frame, area: Rect) {
        self.layout_input.set(area);
        let input_focused =
            self.focus == FocusPanel::SearchInput || self.focus == FocusPanel::PasswordInput;
        let border_style =
            theme::panel_border_style(input_focused, theme::screen_accent::FORMS_INPUT);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Text Inputs")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(1), Constraint::Fixed(1)])
            .split(inner);

        let styles = theme::semantic_styles();
        let search_style = if self.focus == FocusPanel::SearchInput {
            styles.intent.info.badge_style.bold()
        } else {
            styles.intent.info.badge_style
        };
        let password_style = if self.focus == FocusPanel::PasswordInput {
            styles.intent.warning.badge_style.bold()
        } else {
            styles.intent.warning.badge_style
        };

        // Search row
        if !rows[0].is_empty() {
            self.layout_search.set(rows[0]);
            let cols = Flex::horizontal()
                .constraints([Constraint::Fixed(10), Constraint::Min(1)])
                .split(rows[0]);
            let badge = Badge::new("Search").with_style(search_style);
            badge.render(cols[0], frame);
            Widget::render(&self.search_input, cols[1], frame);
        }

        // Password row
        if rows.len() > 1 && !rows[1].is_empty() {
            self.layout_password.set(rows[1]);
            let cols = Flex::horizontal()
                .constraints([Constraint::Fixed(10), Constraint::Min(1)])
                .split(rows[1]);
            let badge = Badge::new("Password").with_style(password_style);
            badge.render(cols[0], frame);
            Widget::render(&self.password_input, cols[1], frame);
        }
    }

    fn render_editor_panel(&self, frame: &mut Frame, area: Rect) {
        self.layout_editor.set(area);
        let border_style = theme::panel_border_style(
            self.focus == FocusPanel::TextEditor,
            theme::screen_accent::FORMS_INPUT,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Text Editor")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        Widget::render(&self.textarea, inner, frame);
    }
}

impl Screen for FormsInput {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let form = self.layout_form.get();
                let input = self.layout_input.get();
                let editor = self.layout_editor.get();
                let search = self.layout_search.get();
                let password_rect = self.layout_password.get();
                if form.contains(mouse.x, mouse.y) {
                    self.focus = FocusPanel::Form;
                } else if search.contains(mouse.x, mouse.y) {
                    self.focus = FocusPanel::SearchInput;
                } else if password_rect.contains(mouse.x, mouse.y) {
                    self.focus = FocusPanel::PasswordInput;
                } else if input.contains(mouse.x, mouse.y) {
                    self.focus = FocusPanel::SearchInput;
                } else if editor.contains(mouse.x, mouse.y) {
                    self.focus = FocusPanel::TextEditor;
                }
                self.update_focus_states();
                self.update_status();
            }
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            if self.undo_keys.undo.matches(*code, *modifiers) {
                self.perform_undo();
                return Cmd::None;
            }
            if self.undo_keys.redo_primary.matches(*code, *modifiers)
                || self.undo_keys.redo_secondary.matches(*code, *modifiers)
            {
                self.perform_redo();
                return Cmd::None;
            }

            if *code == KeyCode::Char('u') && modifiers.contains(Modifiers::CTRL) {
                self.undo_panel_visible = !self.undo_panel_visible;
                self.update_status();
                return Cmd::None;
            }
        }

        let before = self.snapshot();

        if let Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
        {
            self.focus = self.focus.next();
            self.update_focus_states();
            self.update_status();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::ALT)
        {
            self.focus = self.focus.next();
            self.update_focus_states();
            self.update_status();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Left,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
        {
            self.focus = self.focus.prev();
            self.update_focus_states();
            self.update_status();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::ALT)
        {
            self.focus = self.focus.prev();
            self.update_focus_states();
            self.update_status();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.is_empty()
            && !matches!(self.focus, FocusPanel::Form | FocusPanel::TextEditor)
        {
            self.focus = self.focus.next();
            self.update_focus_states();
            self.update_status();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.is_empty()
            && !matches!(self.focus, FocusPanel::Form | FocusPanel::TextEditor)
        {
            self.focus = self.focus.prev();
            self.update_focus_states();
            self.update_status();
            return Cmd::None;
        }

        let mut form_changed = false;
        match self.focus {
            FocusPanel::Form => {
                let mut state = self.form_state.borrow_mut();
                form_changed = state.handle_event(&mut self.form, event);
            }
            FocusPanel::SearchInput => {
                self.search_input.handle_event(event);
            }
            FocusPanel::PasswordInput => {
                self.password_input.handle_event(event);
            }
            FocusPanel::TextEditor => {
                self.textarea.handle_event(event);
            }
        }

        if form_changed {
            let show_all = self.form_state.borrow().submitted;
            self.update_form_validation(show_all);
        }

        let after = self.snapshot();
        if !before.is_equivalent(&after) {
            let description = self.undo_description_for_focus();
            self.record_undo(description, before);
        }
        self.update_status();
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let main_chunks = Flex::vertical()
            .constraints([Constraint::Min(1), Constraint::Fixed(1)])
            .split(area);

        let content_chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(main_chunks[0]);

        self.render_form_panel(frame, content_chunks[0]);

        if self.undo_panel_visible {
            let right_chunks = Flex::vertical()
                .constraints([
                    Constraint::Fixed(5),
                    Constraint::Fixed(6),
                    Constraint::Min(5),
                ])
                .split(content_chunks[1]);
            self.render_input_panel(frame, right_chunks[0]);
            self.render_undo_panel(frame, right_chunks[1]);
            self.render_editor_panel(frame, right_chunks[2]);
        } else {
            let right_chunks = Flex::vertical()
                .constraints([Constraint::Fixed(5), Constraint::Min(5)])
                .split(content_chunks[1]);
            self.render_input_panel(frame, right_chunks[0]);
            self.render_editor_panel(frame, right_chunks[1]);
        }

        Paragraph::new(&*self.status_text)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(main_chunks[1], frame);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Ctrl+\u{2190}/\u{2192} or Alt+\u{2191}/\u{2193}",
                action: "Switch panel",
            },
            HelpEntry {
                key: "\u{2191}/\u{2193}",
                action: "Switch input panel",
            },
            HelpEntry {
                key: "Ctrl+Z",
                action: "Undo",
            },
            HelpEntry {
                key: "Ctrl+Y",
                action: "Redo",
            },
            HelpEntry {
                key: "Ctrl+Shift+Z",
                action: "Redo (alt)",
            },
            HelpEntry {
                key: "Ctrl+U",
                action: "Toggle undo history",
            },
            HelpEntry {
                key: "Tab/S-Tab",
                action: "Navigate form fields",
            },
            HelpEntry {
                key: "Space",
                action: "Toggle checkbox",
            },
            HelpEntry {
                key: "\u{2191}/\u{2193}",
                action: "Radio/select/number",
            },
            HelpEntry {
                key: "Enter",
                action: "Submit form",
            },
            HelpEntry {
                key: "Esc",
                action: "Cancel form",
            },
            HelpEntry {
                key: "Mouse",
                action: "Click panel to focus",
            },
        ]
    }

    fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn next_undo_description(&self) -> Option<&str> {
        self.undo_description()
    }

    fn undo(&mut self) -> bool {
        self.perform_undo();
        true
    }

    fn redo(&mut self) -> bool {
        self.perform_redo();
        true
    }

    fn consumes_text_input(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Forms and Input"
    }

    fn tab_label(&self) -> &'static str {
        "Forms"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::Screen;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    fn ctrl_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        })
    }

    fn alt_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::ALT,
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn initial_state() {
        let screen = FormsInput::new();
        assert_eq!(screen.focus, FocusPanel::Form);
        assert_eq!(screen.title(), "Forms and Input");
        assert_eq!(screen.tab_label(), "Forms");
    }

    #[test]
    fn focus_panel_cycles_forward() {
        assert_eq!(FocusPanel::Form.next(), FocusPanel::SearchInput);
        assert_eq!(FocusPanel::SearchInput.next(), FocusPanel::PasswordInput);
        assert_eq!(FocusPanel::PasswordInput.next(), FocusPanel::TextEditor);
        assert_eq!(FocusPanel::TextEditor.next(), FocusPanel::Form);
    }

    #[test]
    fn focus_panel_cycles_backward() {
        assert_eq!(FocusPanel::Form.prev(), FocusPanel::TextEditor);
        assert_eq!(FocusPanel::TextEditor.prev(), FocusPanel::PasswordInput);
        assert_eq!(FocusPanel::PasswordInput.prev(), FocusPanel::SearchInput);
        assert_eq!(FocusPanel::SearchInput.prev(), FocusPanel::Form);
    }

    #[test]
    fn ctrl_right_switches_panel() {
        let mut screen = FormsInput::new();
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, FocusPanel::SearchInput);
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, FocusPanel::PasswordInput);
    }

    #[test]
    fn ctrl_left_switches_panel_back() {
        let mut screen = FormsInput::new();
        screen.update(&ctrl_press(KeyCode::Left));
        assert_eq!(screen.focus, FocusPanel::TextEditor);
    }

    #[test]
    fn alt_arrow_switches_panel() {
        let mut screen = FormsInput::new();
        screen.update(&alt_press(KeyCode::Down));
        assert_eq!(screen.focus, FocusPanel::SearchInput);
        screen.update(&alt_press(KeyCode::Up));
        assert_eq!(screen.focus, FocusPanel::Form);
    }

    #[test]
    fn form_has_six_fields() {
        let screen = FormsInput::new();
        assert_eq!(screen.form.field_count(), 6);
    }

    #[test]
    fn form_tab_navigates_fields() {
        let mut screen = FormsInput::new();
        assert_eq!(screen.form_state.borrow().focused, 0);
        screen.update(&press(KeyCode::Tab));
        assert_eq!(screen.form_state.borrow().focused, 1);
    }

    #[test]
    fn search_input_receives_chars() {
        let mut screen = FormsInput::new();
        // Switch to search input
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, FocusPanel::SearchInput);
        // Type a character
        screen.update(&press(KeyCode::Char('h')));
        assert_eq!(screen.search_input.value(), "h");
    }

    #[test]
    fn textarea_has_content() {
        let screen = FormsInput::new();
        assert!(!screen.textarea.is_empty());
        assert!(screen.textarea.line_count() > 1);
    }

    #[test]
    fn keybindings_non_empty() {
        let screen = FormsInput::new();
        assert!(!screen.keybindings().is_empty());
    }

    #[test]
    fn undo_redo_restores_textarea() {
        let mut screen = FormsInput::new();
        // Switch to text editor panel.
        screen.update(&ctrl_press(KeyCode::Right));
        screen.update(&ctrl_press(KeyCode::Right));
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, FocusPanel::TextEditor);

        let before = screen.textarea.text();
        screen.update(&press(KeyCode::Char('X')));
        assert_ne!(screen.textarea.text(), before);

        Screen::undo(&mut screen);
        assert_eq!(screen.textarea.text(), before);

        Screen::redo(&mut screen);
        assert_ne!(screen.textarea.text(), before);
    }

    #[test]
    fn validation_errors_after_touch() {
        let mut screen = FormsInput::new();

        // Touch the Name field by tabbing away while empty.
        screen.update(&press(KeyCode::Tab));

        let errors = &screen.form_state.borrow().errors;
        assert!(errors.iter().any(|e| e.field == 0));
        assert!(!errors.iter().any(|e| e.field == 1));
    }

    #[test]
    fn disabled_field_ignores_input() {
        let mut screen = FormsInput::new();

        // Move focus to Theme (disabled) field.
        screen.update(&press(KeyCode::Tab)); // Email
        screen.update(&press(KeyCode::Tab)); // Role
        screen.update(&press(KeyCode::Tab)); // Theme (disabled)

        let before = if let Some(FormField::Radio { selected, .. }) = screen.form.field(3) {
            *selected
        } else {
            0
        };

        screen.update(&press(KeyCode::Char(' ')));

        let after = if let Some(FormField::Radio { selected, .. }) = screen.form.field(3) {
            *selected
        } else {
            0
        };

        assert_eq!(before, after, "disabled field should not change");
    }
}
