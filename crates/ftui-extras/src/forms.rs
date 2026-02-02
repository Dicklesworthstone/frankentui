#![forbid(unsafe_code)]

//! Form and picker widgets for interactive data entry.
//!
//! Provides a `Form` widget with field types (text, checkbox, radio, select, number),
//! validation, tab navigation, and submit/cancel actions. Also includes a `ConfirmDialog`
//! for simple yes/no prompts.
//!
//! Feature-gated under `forms`.

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_widgets::{StatefulWidget, Widget};

// ---------------------------------------------------------------------------
// FormField – the individual field types
// ---------------------------------------------------------------------------

/// A single form field definition.
#[derive(Debug, Clone)]
pub enum FormField {
    /// Single-line text input.
    Text {
        label: String,
        value: String,
        placeholder: Option<String>,
    },
    /// Boolean toggle.
    Checkbox { label: String, checked: bool },
    /// Single-choice from a group of options.
    Radio {
        label: String,
        options: Vec<String>,
        selected: usize,
    },
    /// Single-choice from a dropdown-style list.
    Select {
        label: String,
        options: Vec<String>,
        selected: usize,
    },
    /// Numeric input with optional bounds.
    Number {
        label: String,
        value: i64,
        min: Option<i64>,
        max: Option<i64>,
        step: i64,
    },
}

impl FormField {
    /// Create a text field.
    pub fn text(label: impl Into<String>) -> Self {
        Self::Text {
            label: label.into(),
            value: String::new(),
            placeholder: None,
        }
    }

    /// Create a text field with a default value.
    pub fn text_with_value(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Text {
            label: label.into(),
            value: value.into(),
            placeholder: None,
        }
    }

    /// Create a text field with placeholder.
    pub fn text_with_placeholder(label: impl Into<String>, placeholder: impl Into<String>) -> Self {
        Self::Text {
            label: label.into(),
            value: String::new(),
            placeholder: Some(placeholder.into()),
        }
    }

    /// Create a checkbox field.
    pub fn checkbox(label: impl Into<String>, checked: bool) -> Self {
        Self::Checkbox {
            label: label.into(),
            checked,
        }
    }

    /// Create a radio field.
    pub fn radio(label: impl Into<String>, options: Vec<String>) -> Self {
        Self::Radio {
            label: label.into(),
            options,
            selected: 0,
        }
    }

    /// Create a select field.
    pub fn select(label: impl Into<String>, options: Vec<String>) -> Self {
        Self::Select {
            label: label.into(),
            options,
            selected: 0,
        }
    }

    /// Create a number field.
    pub fn number(label: impl Into<String>, value: i64) -> Self {
        Self::Number {
            label: label.into(),
            value,
            min: None,
            max: None,
            step: 1,
        }
    }

    /// Create a number field with bounds.
    pub fn number_bounded(label: impl Into<String>, value: i64, min: i64, max: i64) -> Self {
        Self::Number {
            label: label.into(),
            value: value.clamp(min, max),
            min: Some(min),
            max: Some(max),
            step: 1,
        }
    }

    /// Get the label for this field.
    pub fn label(&self) -> &str {
        match self {
            Self::Text { label, .. }
            | Self::Checkbox { label, .. }
            | Self::Radio { label, .. }
            | Self::Select { label, .. }
            | Self::Number { label, .. } => label,
        }
    }
}

// ---------------------------------------------------------------------------
// FormData – collected values after submit
// ---------------------------------------------------------------------------

/// A single value extracted from a form field.
#[derive(Debug, Clone, PartialEq)]
pub enum FormValue {
    Text(String),
    Bool(bool),
    Choice { index: usize, label: String },
    Number(i64),
}

/// Collected data from all form fields.
#[derive(Debug, Clone, Default)]
pub struct FormData {
    /// Field values indexed by position.
    pub values: Vec<(String, FormValue)>,
}

impl FormData {
    /// Get a value by field label.
    pub fn get(&self, label: &str) -> Option<&FormValue> {
        self.values.iter().find(|(l, _)| l == label).map(|(_, v)| v)
    }
}

// ---------------------------------------------------------------------------
// ValidationError
// ---------------------------------------------------------------------------

/// A validation error for a specific field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Field index.
    pub field: usize,
    /// Error message.
    pub message: String,
}

/// Validation function type. Returns `None` if valid, `Some(message)` if invalid.
pub type ValidateFn = Box<dyn Fn(&FormField) -> Option<String>>;

// ---------------------------------------------------------------------------
// Form – the main form widget
// ---------------------------------------------------------------------------

/// A form widget that manages multiple fields with tab navigation.
pub struct Form {
    fields: Vec<FormField>,
    validators: Vec<Option<ValidateFn>>,
    style: Style,
    label_style: Style,
    focused_style: Style,
    error_style: Style,
    label_width: u16,
}

impl Form {
    /// Create a form from a list of fields.
    pub fn new(fields: Vec<FormField>) -> Self {
        let count = fields.len();
        Self {
            fields,
            validators: (0..count).map(|_| None).collect(),
            style: Style::default(),
            label_style: Style::default(),
            focused_style: Style::default(),
            error_style: Style::default(),
            label_width: 0, // auto-detect
        }
    }

    /// Set base style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set label style.
    pub fn label_style(mut self, style: Style) -> Self {
        self.label_style = style;
        self
    }

    /// Set focused field style.
    pub fn focused_style(mut self, style: Style) -> Self {
        self.focused_style = style;
        self
    }

    /// Set error message style.
    pub fn error_style(mut self, style: Style) -> Self {
        self.error_style = style;
        self
    }

    /// Set fixed label width (0 = auto-detect from longest label).
    pub fn label_width(mut self, width: u16) -> Self {
        self.label_width = width;
        self
    }

    /// Attach a validator to a field by index.
    pub fn validate(mut self, field_index: usize, f: ValidateFn) -> Self {
        if field_index < self.validators.len() {
            self.validators[field_index] = Some(f);
        }
        self
    }

    /// Number of fields.
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }

    /// Access a field by index.
    pub fn field(&self, index: usize) -> Option<&FormField> {
        self.fields.get(index)
    }

    /// Access a field mutably by index.
    pub fn field_mut(&mut self, index: usize) -> Option<&mut FormField> {
        self.fields.get_mut(index)
    }

    /// Collect all form values.
    pub fn data(&self) -> FormData {
        let values = self
            .fields
            .iter()
            .map(|f| {
                let label = f.label().to_string();
                let value = match f {
                    FormField::Text { value, .. } => FormValue::Text(value.clone()),
                    FormField::Checkbox { checked, .. } => FormValue::Bool(*checked),
                    FormField::Radio {
                        options, selected, ..
                    } => FormValue::Choice {
                        index: *selected,
                        label: options.get(*selected).cloned().unwrap_or_default(),
                    },
                    FormField::Select {
                        options, selected, ..
                    } => FormValue::Choice {
                        index: *selected,
                        label: options.get(*selected).cloned().unwrap_or_default(),
                    },
                    FormField::Number { value, .. } => FormValue::Number(*value),
                };
                (label, value)
            })
            .collect();
        FormData { values }
    }

    /// Run all validators. Returns errors (empty vec = valid).
    pub fn validate_all(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        for (i, (field, validator)) in self.fields.iter().zip(self.validators.iter()).enumerate() {
            if let Some(vf) = validator {
                if let Some(msg) = vf(field) {
                    errors.push(ValidationError {
                        field: i,
                        message: msg,
                    });
                }
            }
        }
        errors
    }

    /// Compute the effective label column width.
    fn effective_label_width(&self) -> u16 {
        if self.label_width > 0 {
            return self.label_width;
        }
        self.fields
            .iter()
            .map(|f| unicode_width::UnicodeWidthStr::width(f.label()) as u16)
            .max()
            .unwrap_or(0)
            .saturating_add(2) // ": " suffix
    }
}

// ---------------------------------------------------------------------------
// FormState
// ---------------------------------------------------------------------------

/// Mutable state for a Form.
#[derive(Debug, Clone, Default)]
pub struct FormState {
    /// Currently focused field index.
    pub focused: usize,
    /// Scroll offset for forms taller than the viewport.
    pub scroll: usize,
    /// Whether the form has been submitted.
    pub submitted: bool,
    /// Whether the form has been cancelled.
    pub cancelled: bool,
    /// Current validation errors.
    pub errors: Vec<ValidationError>,
    /// Cursor position within a text field (grapheme index).
    pub text_cursor: usize,
}

impl FormState {
    /// Focus the next field.
    pub fn focus_next(&mut self, field_count: usize) {
        if field_count > 0 {
            self.focused = (self.focused + 1) % field_count;
        }
    }

    /// Focus the previous field.
    pub fn focus_prev(&mut self, field_count: usize) {
        if field_count > 0 {
            self.focused = self.focused.checked_sub(1).unwrap_or(field_count - 1);
        }
    }

    /// Handle a terminal event for the form. Returns `true` if state changed.
    pub fn handle_event(&mut self, form: &mut Form, event: &Event) -> bool {
        if self.submitted || self.cancelled {
            return false;
        }

        if let Event::Key(key) = event
            && (key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat)
        {
            return self.handle_key(form, key);
        }
        false
    }

    fn handle_key(&mut self, form: &mut Form, key: &KeyEvent) -> bool {
        match key.code {
            // Tab / Shift+Tab: navigate fields
            KeyCode::Tab => {
                self.focus_next(form.field_count());
                self.sync_text_cursor(form);
                true
            }
            KeyCode::BackTab => {
                self.focus_prev(form.field_count());
                self.sync_text_cursor(form);
                true
            }
            // Up/Down: navigate fields (or radio/select options)
            KeyCode::Up => self.handle_up(form),
            KeyCode::Down => self.handle_down(form),
            // Enter: submit
            KeyCode::Enter => {
                self.errors = form.validate_all();
                if self.errors.is_empty() {
                    self.submitted = true;
                }
                true
            }
            // Escape: cancel
            KeyCode::Escape => {
                self.cancelled = true;
                true
            }
            // Space: toggle checkbox / radio
            KeyCode::Char(' ') if !key.modifiers.contains(Modifiers::CTRL) => {
                self.handle_space(form)
            }
            // Left/Right for number fields and select
            KeyCode::Left => self.handle_left(form),
            KeyCode::Right => self.handle_right(form),
            // Character input for text fields
            KeyCode::Char(c) if !key.modifiers.contains(Modifiers::CTRL) => {
                self.handle_text_char(form, c)
            }
            KeyCode::Backspace => self.handle_text_backspace(form),
            KeyCode::Delete => self.handle_text_delete(form),
            KeyCode::Home => self.handle_text_home(form),
            KeyCode::End => self.handle_text_end(form),
            _ => false,
        }
    }

    fn handle_up(&mut self, form: &mut Form) -> bool {
        if let Some(field) = form.fields.get_mut(self.focused) {
            match field {
                FormField::Radio {
                    options, selected, ..
                } => {
                    if !options.is_empty() {
                        *selected = selected
                            .checked_sub(1)
                            .unwrap_or(options.len().saturating_sub(1));
                    }
                    return true;
                }
                FormField::Select {
                    options, selected, ..
                } => {
                    if !options.is_empty() {
                        *selected = selected
                            .checked_sub(1)
                            .unwrap_or(options.len().saturating_sub(1));
                    }
                    return true;
                }
                FormField::Number {
                    value, max, step, ..
                } => {
                    let new_val = value.saturating_add(*step);
                    *value = max.map_or(new_val, |m| new_val.min(m));
                    return true;
                }
                _ => {}
            }
        }
        // Default: move focus up
        self.focus_prev(form.field_count());
        self.sync_text_cursor(form);
        true
    }

    fn handle_down(&mut self, form: &mut Form) -> bool {
        if let Some(field) = form.fields.get_mut(self.focused) {
            match field {
                FormField::Radio {
                    options, selected, ..
                } => {
                    if !options.is_empty() {
                        *selected = (*selected + 1) % options.len();
                    }
                    return true;
                }
                FormField::Select {
                    options, selected, ..
                } => {
                    if !options.is_empty() {
                        *selected = (*selected + 1) % options.len();
                    }
                    return true;
                }
                FormField::Number {
                    value, min, step, ..
                } => {
                    let new_val = value.saturating_sub(*step);
                    *value = min.map_or(new_val, |m| new_val.max(m));
                    return true;
                }
                _ => {}
            }
        }
        // Default: move focus down
        self.focus_next(form.field_count());
        self.sync_text_cursor(form);
        true
    }

    fn handle_space(&mut self, form: &mut Form) -> bool {
        if let Some(field) = form.fields.get_mut(self.focused) {
            match field {
                FormField::Checkbox { checked, .. } => {
                    *checked = !*checked;
                    return true;
                }
                FormField::Text { value, .. } => {
                    let byte_offset = grapheme_byte_offset(value, self.text_cursor);
                    value.insert(byte_offset, ' ');
                    self.text_cursor += 1;
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    fn handle_left(&mut self, form: &mut Form) -> bool {
        if let Some(field) = form.fields.get_mut(self.focused) {
            match field {
                FormField::Number {
                    value, min, step, ..
                } => {
                    let new_val = value.saturating_sub(*step);
                    *value = min.map_or(new_val, |m| new_val.max(m));
                    return true;
                }
                FormField::Select {
                    options, selected, ..
                } => {
                    if !options.is_empty() {
                        *selected = selected
                            .checked_sub(1)
                            .unwrap_or(options.len().saturating_sub(1));
                    }
                    return true;
                }
                FormField::Text { .. } => {
                    if self.text_cursor > 0 {
                        self.text_cursor -= 1;
                    }
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    fn handle_right(&mut self, form: &mut Form) -> bool {
        if let Some(field) = form.fields.get_mut(self.focused) {
            match field {
                FormField::Number {
                    value, max, step, ..
                } => {
                    let new_val = value.saturating_add(*step);
                    *value = max.map_or(new_val, |m| new_val.min(m));
                    return true;
                }
                FormField::Select {
                    options, selected, ..
                } => {
                    if !options.is_empty() {
                        *selected = (*selected + 1) % options.len();
                    }
                    return true;
                }
                FormField::Text { value, .. } => {
                    let count = grapheme_count(value);
                    if self.text_cursor < count {
                        self.text_cursor += 1;
                    }
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    fn handle_text_char(&mut self, form: &mut Form, c: char) -> bool {
        if let Some(FormField::Text { value, .. }) = form.fields.get_mut(self.focused) {
            let byte_offset = grapheme_byte_offset(value, self.text_cursor);
            value.insert(byte_offset, c);
            self.text_cursor += 1;
            return true;
        }
        false
    }

    fn handle_text_backspace(&mut self, form: &mut Form) -> bool {
        if let Some(FormField::Text { value, .. }) = form.fields.get_mut(self.focused) {
            if self.text_cursor > 0 {
                let byte_start = grapheme_byte_offset(value, self.text_cursor - 1);
                let byte_end = grapheme_byte_offset(value, self.text_cursor);
                value.drain(byte_start..byte_end);
                self.text_cursor -= 1;
                return true;
            }
        }
        false
    }

    fn handle_text_delete(&mut self, form: &mut Form) -> bool {
        if let Some(FormField::Text { value, .. }) = form.fields.get_mut(self.focused) {
            let count = grapheme_count(value);
            if self.text_cursor < count {
                let byte_start = grapheme_byte_offset(value, self.text_cursor);
                let byte_end = grapheme_byte_offset(value, self.text_cursor + 1);
                value.drain(byte_start..byte_end);
                return true;
            }
        }
        false
    }

    fn handle_text_home(&mut self, form: &Form) -> bool {
        if matches!(form.fields.get(self.focused), Some(FormField::Text { .. })) {
            self.text_cursor = 0;
            return true;
        }
        false
    }

    fn handle_text_end(&mut self, form: &Form) -> bool {
        if let Some(FormField::Text { value, .. }) = form.fields.get(self.focused) {
            self.text_cursor = grapheme_count(value);
            return true;
        }
        false
    }

    /// Sync the text cursor when switching to a text field.
    fn sync_text_cursor(&mut self, form: &Form) {
        if let Some(FormField::Text { value, .. }) = form.fields.get(self.focused) {
            let count = grapheme_count(value);
            self.text_cursor = self.text_cursor.min(count);
        } else {
            self.text_cursor = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl StatefulWidget for Form {
    type State = FormState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if area.is_empty() || self.fields.is_empty() {
            return;
        }

        // Apply base style
        set_style_area(&mut frame.buffer, area, self.style);

        let label_w = self.effective_label_width();
        let value_x = area.x.saturating_add(label_w);
        let value_width = area.width.saturating_sub(label_w);

        let visible_rows = area.height as usize;

        // Ensure focused field is visible
        if state.focused >= state.scroll + visible_rows {
            state.scroll = state.focused - visible_rows + 1;
        } else if state.focused < state.scroll {
            state.scroll = state.focused;
        }

        // Clamp focus
        if state.focused >= self.fields.len() {
            state.focused = self.fields.len().saturating_sub(1);
        }

        for (i, field) in self
            .fields
            .iter()
            .enumerate()
            .skip(state.scroll)
            .take(visible_rows)
        {
            let y = area.y + (i - state.scroll) as u16;
            let is_focused = i == state.focused;

            // Find error for this field
            let error_msg = state
                .errors
                .iter()
                .find(|e| e.field == i)
                .map(|e| e.message.as_str());

            // Draw label
            let label_style = if is_focused {
                self.focused_style
            } else {
                self.label_style
            };

            let label = field.label();
            draw_str(
                frame,
                area.x,
                y,
                label,
                label_style,
                label_w.saturating_sub(2),
            );

            // Draw ": " separator
            let sep_x = area.x + label.len().min((label_w.saturating_sub(2)) as usize) as u16;
            draw_str(frame, sep_x, y, ": ", label_style, 2);

            // Draw field value
            let field_style = if is_focused {
                self.focused_style
            } else {
                self.style
            };

            self.render_field(
                frame,
                field,
                value_x,
                y,
                value_width,
                field_style,
                is_focused,
                state,
            );

            // Draw error indicator
            if let Some(msg) = error_msg {
                // Show error after value if space allows
                let err_x = value_x + value_width.saturating_sub(msg.len() as u16 + 2);
                if err_x > value_x {
                    draw_str(frame, err_x, y, msg, self.error_style, value_width);
                }
            }
        }
    }
}

impl Form {
    fn render_field(
        &self,
        frame: &mut Frame,
        field: &FormField,
        x: u16,
        y: u16,
        width: u16,
        style: Style,
        is_focused: bool,
        state: &FormState,
    ) {
        match field {
            FormField::Text {
                value, placeholder, ..
            } => {
                if value.is_empty() {
                    if let Some(ph) = placeholder {
                        draw_str(frame, x, y, ph, self.label_style, width);
                    }
                } else {
                    draw_str(frame, x, y, value, style, width);
                }
                // Draw cursor if focused
                if is_focused {
                    let buf = &mut frame.buffer;
                    let cursor_x = x + state.text_cursor.min(width as usize) as u16;
                    if cursor_x < x + width {
                        if let Some(cell) = buf.get_mut(cursor_x, y) {
                            use ftui_render::cell::StyleFlags;
                            let flags = cell.attrs.flags();
                            cell.attrs = cell.attrs.with_flags(flags ^ StyleFlags::REVERSE);
                        }
                    }
                }
            }
            FormField::Checkbox { checked, .. } => {
                let indicator = if *checked { "[x]" } else { "[ ]" };
                draw_str(frame, x, y, indicator, style, width);
            }
            FormField::Radio {
                options, selected, ..
            } => {
                if let Some(opt) = options.get(*selected) {
                    let display = format!("({}) {}", selected + 1, opt);
                    draw_str(frame, x, y, &display, style, width);
                }
            }
            FormField::Select {
                options, selected, ..
            } => {
                if let Some(opt) = options.get(*selected) {
                    let prefix = if is_focused { "< " } else { "  " };
                    let suffix = if is_focused { " >" } else { "  " };
                    let display = format!("{prefix}{opt}{suffix}");
                    draw_str(frame, x, y, &display, style, width);
                }
            }
            FormField::Number { value, .. } => {
                let display = if is_focused {
                    format!("< {value} >")
                } else {
                    format!("  {value}  ")
                };
                draw_str(frame, x, y, &display, style, width);
            }
        }
    }
}

// Implement Widget with default state for simple rendering
impl Widget for Form {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = FormState::default();
        StatefulWidget::render(self, area, frame, &mut state);
    }
}

// ---------------------------------------------------------------------------
// ConfirmDialog
// ---------------------------------------------------------------------------

/// A simple yes/no confirmation dialog.
#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    message: String,
    yes_label: String,
    no_label: String,
    style: Style,
    selected_style: Style,
}

impl ConfirmDialog {
    /// Create a confirm dialog with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            yes_label: "Yes".to_string(),
            no_label: "No".to_string(),
            style: Style::default(),
            selected_style: Style::default(),
        }
    }

    /// Set custom button labels.
    pub fn labels(mut self, yes: impl Into<String>, no: impl Into<String>) -> Self {
        self.yes_label = yes.into();
        self.no_label = no.into();
        self
    }

    /// Set base style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set selected button style.
    pub fn selected_style(mut self, style: Style) -> Self {
        self.selected_style = style;
        self
    }
}

/// State for ConfirmDialog.
#[derive(Debug, Clone, Default)]
pub struct ConfirmDialogState {
    /// `true` = "Yes" selected, `false` = "No" selected.
    pub selected_yes: bool,
    /// Whether a choice has been made.
    pub confirmed: Option<bool>,
}

impl ConfirmDialogState {
    /// Handle an event. Returns `true` if state changed.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        if self.confirmed.is_some() {
            return false;
        }
        if let Event::Key(key) = event
            && (key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat)
        {
            return self.handle_key(key);
        }
        false
    }

    fn handle_key(&mut self, key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Left | KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('h') => {
                self.selected_yes = !self.selected_yes;
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.selected_yes = !self.selected_yes;
                true
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.confirmed = Some(self.selected_yes);
                true
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.confirmed = Some(true);
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.confirmed = Some(false);
                true
            }
            KeyCode::Escape => {
                self.confirmed = Some(false);
                true
            }
            _ => false,
        }
    }
}

impl StatefulWidget for ConfirmDialog {
    type State = ConfirmDialogState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if area.is_empty() {
            return;
        }

        set_style_area(&mut frame.buffer, area, self.style);

        // Draw message on first row(s)
        let msg_y = area.y;
        draw_str(
            frame,
            area.x,
            msg_y,
            &self.message,
            self.style,
            area.width,
        );

        // Draw buttons on last row
        let btn_y = if area.height > 1 {
            area.y + area.height - 1
        } else {
            area.y
        };

        let yes_style = if state.selected_yes {
            self.selected_style
        } else {
            self.style
        };
        let no_style = if state.selected_yes {
            self.style
        } else {
            self.selected_style
        };

        let yes_str = format!("[ {} ]", self.yes_label);
        let no_str = format!("[ {} ]", self.no_label);
        let total_btn_width = yes_str.len() + 2 + no_str.len();
        let start_x = area
            .x
            .saturating_add(area.width.saturating_sub(total_btn_width as u16) / 2);

        draw_str(frame, start_x, btn_y, &yes_str, yes_style, area.width);
        let no_x = start_x + yes_str.len() as u16 + 2;
        draw_str(frame, no_x, btn_y, &no_str, no_style, area.width);
    }
}

impl Widget for ConfirmDialog {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = ConfirmDialogState::default();
        StatefulWidget::render(self, area, frame, &mut state);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Apply a style to a cell (fg, bg, attrs).
fn apply_style(cell: &mut Cell, style: Style) {
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg {
        cell.bg = bg;
    }
    if let Some(attrs) = style.attrs {
        let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
        cell.attrs = cell.attrs.with_flags(cell_flags);
    }
}

/// Apply a style to all cells in a rectangular area.
fn set_style_area(buf: &mut Buffer, area: Rect, style: Style) {
    if style.is_empty() {
        return;
    }
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.get_mut(x, y) {
                apply_style(cell, style);
            }
        }
    }
}

/// Draw a string into the frame, clamped to `max_width` visual columns.
fn draw_str(frame: &mut Frame, x: u16, y: u16, s: &str, style: Style, max_width: u16) {
    let mut col = 0u16;
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(s, true) {
        if col >= max_width {
            break;
        }
        let w = unicode_width::UnicodeWidthStr::width(grapheme) as u16;
        if w == 0 {
            continue;
        }
        if col + w > max_width {
            break;
        }

        // Intern grapheme if needed
        let cell_content = if w > 1 || grapheme.chars().count() > 1 {
            let id = frame.intern_with_width(grapheme, w as u8);
            ftui_render::cell::CellContent::from_grapheme(id)
        } else if let Some(c) = grapheme.chars().next() {
            ftui_render::cell::CellContent::from_char(c)
        } else {
            continue;
        };

        let mut cell = Cell::new(cell_content);
        apply_style(&mut cell, style);
        
        // Use set() which handles multi-width characters (atomic writes)
        frame.buffer.set(x + col, y, cell);
        
        col += w;
    }
}

/// Count grapheme clusters in a string.
fn grapheme_count(s: &str) -> usize {
    unicode_segmentation::UnicodeSegmentation::graphemes(s, true).count()
}

/// Get byte offset of the nth grapheme cluster.
fn grapheme_byte_offset(s: &str, grapheme_idx: usize) -> usize {
    unicode_segmentation::UnicodeSegmentation::grapheme_indices(s, true)
        .nth(grapheme_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyEvent, KeyEventKind};
    use ftui_render::grapheme_pool::GraphemePool;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    fn press_shift(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
        })
    }

    // -- FormField constructors --

    #[test]
    fn text_field_default() {
        let f = FormField::text("Name");
        assert_eq!(f.label(), "Name");
        if let FormField::Text {
            value, placeholder, ..
        } = &f
        {
            assert!(value.is_empty());
            assert!(placeholder.is_none());
        } else {
            panic!("expected Text");
        }
    }

    #[test]
    fn text_field_with_value() {
        let f = FormField::text_with_value("Name", "Alice");
        if let FormField::Text { value, .. } = &f {
            assert_eq!(value, "Alice");
        } else {
            panic!("expected Text");
        }
    }

    #[test]
    fn text_field_with_placeholder() {
        let f = FormField::text_with_placeholder("Name", "Enter name...");
        if let FormField::Text { placeholder, .. } = &f {
            assert_eq!(placeholder.as_deref(), Some("Enter name..."));
        } else {
            panic!("expected Text");
        }
    }

    #[test]
    fn checkbox_field() {
        let f = FormField::checkbox("Agree", false);
        if let FormField::Checkbox { checked, .. } = &f {
            assert!(!checked);
        } else {
            panic!("expected Checkbox");
        }
    }

    #[test]
    fn radio_field() {
        let f = FormField::radio("Color", vec!["Red".into(), "Blue".into()]);
        if let FormField::Radio {
            options, selected, ..
        } = &f
        {
            assert_eq!(options.len(), 2);
            assert_eq!(*selected, 0);
        } else {
            panic!("expected Radio");
        }
    }

    #[test]
    fn select_field() {
        let f = FormField::select("Size", vec!["S".into(), "M".into(), "L".into()]);
        if let FormField::Select {
            options, selected, ..
        } = &f
        {
            assert_eq!(options.len(), 3);
            assert_eq!(*selected, 0);
        } else {
            panic!("expected Select");
        }
    }

    #[test]
    fn number_field() {
        let f = FormField::number("Count", 42);
        if let FormField::Number {
            value, min, max, ..
        } = &f
        {
            assert_eq!(*value, 42);
            assert!(min.is_none());
            assert!(max.is_none());
        } else {
            panic!("expected Number");
        }
    }

    #[test]
    fn number_bounded_clamps() {
        let f = FormField::number_bounded("Age", 200, 0, 150);
        if let FormField::Number {
            value, min, max, ..
        } = &f
        {
            assert_eq!(*value, 150);
            assert_eq!(*min, Some(0));
            assert_eq!(*max, Some(150));
        } else {
            panic!("expected Number");
        }
    }

    // -- Form data collection --

    #[test]
    fn form_data_collection() {
        let form = Form::new(vec![
            FormField::text_with_value("Name", "Alice"),
            FormField::checkbox("Agree", true),
            FormField::number("Age", 30),
        ]);

        let data = form.data();
        assert_eq!(data.values.len(), 3);
        assert_eq!(data.get("Name"), Some(&FormValue::Text("Alice".into())));
        assert_eq!(data.get("Agree"), Some(&FormValue::Bool(true)));
        assert_eq!(data.get("Age"), Some(&FormValue::Number(30)));
    }

    #[test]
    fn form_data_radio_choice() {
        let form = Form::new(vec![FormField::radio(
            "Color",
            vec!["Red".into(), "Blue".into()],
        )]);

        let data = form.data();
        assert_eq!(
            data.get("Color"),
            Some(&FormValue::Choice {
                index: 0,
                label: "Red".into()
            })
        );
    }

    #[test]
    fn form_data_get_missing() {
        let form = Form::new(vec![FormField::text("Name")]);
        let data = form.data();
        assert!(data.get("Missing").is_none());
    }

    // -- Validation --

    #[test]
    fn validation_passes_when_no_validators() {
        let form = Form::new(vec![FormField::text("Name")]);
        assert!(form.validate_all().is_empty());
    }

    #[test]
    fn validation_catches_empty_required() {
        let form = Form::new(vec![FormField::text("Name")]).validate(
            0,
            Box::new(|f| {
                if let FormField::Text { value, .. } = f {
                    if value.is_empty() {
                        return Some("Required".into());
                    }
                }
                None
            }),
        );

        let errors = form.validate_all();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, 0);
        assert_eq!(errors[0].message, "Required");
    }

    #[test]
    fn validation_passes_when_filled() {
        let form = Form::new(vec![FormField::text_with_value("Name", "Alice")]).validate(
            0,
            Box::new(|f| {
                if let FormField::Text { value, .. } = f {
                    if value.is_empty() {
                        return Some("Required".into());
                    }
                }
                None
            }),
        );

        assert!(form.validate_all().is_empty());
    }

    // -- Navigation --

    #[test]
    fn tab_cycles_focus_forward() {
        let mut form = Form::new(vec![
            FormField::text("A"),
            FormField::text("B"),
            FormField::text("C"),
        ]);
        let mut state = FormState::default();
        assert_eq!(state.focused, 0);

        state.handle_event(&mut form, &press(KeyCode::Tab));
        assert_eq!(state.focused, 1);

        state.handle_event(&mut form, &press(KeyCode::Tab));
        assert_eq!(state.focused, 2);

        // Wraps around
        state.handle_event(&mut form, &press(KeyCode::Tab));
        assert_eq!(state.focused, 0);
    }

    #[test]
    fn backtab_cycles_focus_backward() {
        let mut form = Form::new(vec![
            FormField::text("A"),
            FormField::text("B"),
            FormField::text("C"),
        ]);
        let mut state = FormState::default();

        // Wraps from 0 to last
        state.handle_event(&mut form, &press(KeyCode::BackTab));
        assert_eq!(state.focused, 2);

        state.handle_event(&mut form, &press(KeyCode::BackTab));
        assert_eq!(state.focused, 1);
    }

    // -- Checkbox toggle --

    #[test]
    fn space_toggles_checkbox() {
        let mut form = Form::new(vec![FormField::checkbox("Agree", false)]);
        let mut state = FormState::default();

        state.handle_event(&mut form, &press(KeyCode::Char(' ')));
        if let FormField::Checkbox { checked, .. } = &form.fields[0] {
            assert!(checked);
        }

        state.handle_event(&mut form, &press(KeyCode::Char(' ')));
        if let FormField::Checkbox { checked, .. } = &form.fields[0] {
            assert!(!checked);
        }
    }

    // -- Radio cycling --

    #[test]
    fn up_down_cycles_radio() {
        let mut form = Form::new(vec![FormField::radio(
            "Color",
            vec!["Red".into(), "Green".into(), "Blue".into()],
        )]);
        let mut state = FormState::default();

        // Down cycles forward
        state.handle_event(&mut form, &press(KeyCode::Down));
        if let FormField::Radio { selected, .. } = &form.fields[0] {
            assert_eq!(*selected, 1);
        }

        state.handle_event(&mut form, &press(KeyCode::Down));
        if let FormField::Radio { selected, .. } = &form.fields[0] {
            assert_eq!(*selected, 2);
        }

        // Wraps around
        state.handle_event(&mut form, &press(KeyCode::Down));
        if let FormField::Radio { selected, .. } = &form.fields[0] {
            assert_eq!(*selected, 0);
        }

        // Up wraps from 0 to last
        state.handle_event(&mut form, &press(KeyCode::Up));
        if let FormField::Radio { selected, .. } = &form.fields[0] {
            assert_eq!(*selected, 2);
        }
    }

    // -- Select cycling --

    #[test]
    fn left_right_cycles_select() {
        let mut form = Form::new(vec![FormField::select(
            "Size",
            vec!["S".into(), "M".into(), "L".into()],
        )]);
        let mut state = FormState::default();

        state.handle_event(&mut form, &press(KeyCode::Right));
        if let FormField::Select { selected, .. } = &form.fields[0] {
            assert_eq!(*selected, 1);
        }

        state.handle_event(&mut form, &press(KeyCode::Left));
        if let FormField::Select { selected, .. } = &form.fields[0] {
            assert_eq!(*selected, 0);
        }

        // Wraps
        state.handle_event(&mut form, &press(KeyCode::Left));
        if let FormField::Select { selected, .. } = &form.fields[0] {
            assert_eq!(*selected, 2);
        }
    }

    // -- Number increment/decrement --

    #[test]
    fn up_down_changes_number() {
        let mut form = Form::new(vec![FormField::number("Count", 10)]);
        let mut state = FormState::default();

        state.handle_event(&mut form, &press(KeyCode::Up));
        if let FormField::Number { value, .. } = &form.fields[0] {
            assert_eq!(*value, 11);
        }

        state.handle_event(&mut form, &press(KeyCode::Down));
        if let FormField::Number { value, .. } = &form.fields[0] {
            assert_eq!(*value, 10);
        }
    }

    #[test]
    fn number_respects_bounds() {
        let mut form = Form::new(vec![FormField::number_bounded("Age", 0, 0, 5)]);
        let mut state = FormState::default();

        // Can't go below min
        state.handle_event(&mut form, &press(KeyCode::Down));
        if let FormField::Number { value, .. } = &form.fields[0] {
            assert_eq!(*value, 0);
        }

        // Go up to max
        for _ in 0..10 {
            state.handle_event(&mut form, &press(KeyCode::Up));
        }
        if let FormField::Number { value, .. } = &form.fields[0] {
            assert_eq!(*value, 5);
        }
    }

    // -- Text input --

    #[test]
    fn text_input_chars() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();

        state.handle_event(&mut form, &press(KeyCode::Char('A')));
        state.handle_event(&mut form, &press(KeyCode::Char('l')));
        state.handle_event(&mut form, &press(KeyCode::Char('i')));

        if let FormField::Text { value, .. } = &form.fields[0] {
            assert_eq!(value, "Ali");
        }
        assert_eq!(state.text_cursor, 3);
    }

    #[test]
    fn text_backspace() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "abc")]);
        let mut state = FormState::default();
        state.text_cursor = 3;

        state.handle_event(&mut form, &press(KeyCode::Backspace));
        if let FormField::Text { value, .. } = &form.fields[0] {
            assert_eq!(value, "ab");
        }
        assert_eq!(state.text_cursor, 2);
    }

    #[test]
    fn text_delete() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "abc")]);
        let mut state = FormState::default();
        state.text_cursor = 0;

        state.handle_event(&mut form, &press(KeyCode::Delete));
        if let FormField::Text { value, .. } = &form.fields[0] {
            assert_eq!(value, "bc");
        }
        assert_eq!(state.text_cursor, 0);
    }

    #[test]
    fn text_cursor_movement() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "hello")]);
        let mut state = FormState::default();
        state.text_cursor = 3;

        state.handle_event(&mut form, &press(KeyCode::Left));
        assert_eq!(state.text_cursor, 2);

        state.handle_event(&mut form, &press(KeyCode::Right));
        assert_eq!(state.text_cursor, 3);

        state.handle_event(&mut form, &press(KeyCode::Home));
        assert_eq!(state.text_cursor, 0);

        state.handle_event(&mut form, &press(KeyCode::End));
        assert_eq!(state.text_cursor, 5);
    }

    #[test]
    fn text_backspace_at_start_noop() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "abc")]);
        let mut state = FormState::default();
        state.text_cursor = 0;

        state.handle_event(&mut form, &press(KeyCode::Backspace));
        if let FormField::Text { value, .. } = &form.fields[0] {
            assert_eq!(value, "abc");
        }
    }

    #[test]
    fn text_delete_at_end_noop() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "abc")]);
        let mut state = FormState::default();
        state.text_cursor = 3;

        state.handle_event(&mut form, &press(KeyCode::Delete));
        if let FormField::Text { value, .. } = &form.fields[0] {
            assert_eq!(value, "abc");
        }
    }

    // -- Submit and cancel --

    #[test]
    fn enter_submits_form() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "Alice")]);
        let mut state = FormState::default();

        state.handle_event(&mut form, &press(KeyCode::Enter));
        assert!(state.submitted);
        assert!(!state.cancelled);
    }

    #[test]
    fn enter_blocks_submit_on_validation_error() {
        let mut form = Form::new(vec![FormField::text("Name")]).validate(
            0,
            Box::new(|f| {
                if let FormField::Text { value, .. } = f {
                    if value.is_empty() {
                        return Some("Required".into());
                    }
                }
                None
            }),
        );
        let mut state = FormState::default();

        state.handle_event(&mut form, &press(KeyCode::Enter));
        assert!(!state.submitted);
        assert_eq!(state.errors.len(), 1);
    }

    #[test]
    fn escape_cancels_form() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();

        state.handle_event(&mut form, &press(KeyCode::Escape));
        assert!(state.cancelled);
        assert!(!state.submitted);
    }

    #[test]
    fn events_ignored_after_submit() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "Alice")]);
        let mut state = FormState::default();
        state.submitted = true;

        let changed = state.handle_event(&mut form, &press(KeyCode::Tab));
        assert!(!changed);
    }

    #[test]
    fn events_ignored_after_cancel() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();
        state.cancelled = true;

        let changed = state.handle_event(&mut form, &press(KeyCode::Tab));
        assert!(!changed);
    }

    // -- Rendering --

    #[test]
    fn render_form_does_not_panic() {
        let form = Form::new(vec![
            FormField::text_with_value("Name", "Alice"),
            FormField::checkbox("Agree", true),
            FormField::number("Age", 25),
        ]);
        let area = Rect::new(0, 0, 40, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 5, &mut pool);
        let mut state = FormState::default();
        StatefulWidget::render(&form, area, &mut frame, &mut state);
    }

    #[test]
    fn render_form_zero_area() {
        let form = Form::new(vec![FormField::text("Name")]);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let mut state = FormState::default();
        StatefulWidget::render(&form, area, &mut frame, &mut state);
    }

    #[test]
    fn render_form_shows_label() {
        let form = Form::new(vec![FormField::text_with_value("Name", "Alice")]);
        let area = Rect::new(0, 0, 30, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 1, &mut pool);
        let mut state = FormState::default();
        StatefulWidget::render(&form, area, &mut frame, &mut state);

        // First cell should be 'N' from "Name"
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('N'));
    }

    #[test]
    fn render_checkbox_shows_indicator() {
        let form = Form::new(vec![FormField::checkbox("Accept", true)]);
        let area = Rect::new(0, 0, 30, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 1, &mut pool);
        let mut state = FormState::default();
        StatefulWidget::render(&form, area, &mut frame, &mut state);

        // After label "Accept: ", should show "[x]"
        let label_end = "Accept".len() + 2; // ": "
        assert_eq!(
            frame.buffer.get(label_end as u16, 0).unwrap().content.as_char(),
            Some('[')
        );
        assert_eq!(
            frame.buffer.get(label_end as u16 + 1, 0).unwrap().content.as_char(),
            Some('x')
        );
    }

    // -- ConfirmDialog --

    #[test]
    fn confirm_dialog_default_state() {
        let state = ConfirmDialogState::default();
        assert!(!state.selected_yes);
        assert!(state.confirmed.is_none());
    }

    #[test]
    fn confirm_dialog_toggle() {
        let mut state = ConfirmDialogState::default();
        state.handle_event(&press(KeyCode::Left));
        assert!(state.selected_yes);

        state.handle_event(&press(KeyCode::Right));
        assert!(!state.selected_yes);
    }

    #[test]
    fn confirm_dialog_enter_confirms() {
        let mut state = ConfirmDialogState::default();
        state.selected_yes = true;
        state.handle_event(&press(KeyCode::Enter));
        assert_eq!(state.confirmed, Some(true));
    }

    #[test]
    fn confirm_dialog_escape_denies() {
        let mut state = ConfirmDialogState::default();
        state.handle_event(&press(KeyCode::Escape));
        assert_eq!(state.confirmed, Some(false));
    }

    #[test]
    fn confirm_dialog_y_shortcut() {
        let mut state = ConfirmDialogState::default();
        state.handle_event(&press(KeyCode::Char('y')));
        assert_eq!(state.confirmed, Some(true));
    }

    #[test]
    fn confirm_dialog_n_shortcut() {
        let mut state = ConfirmDialogState::default();
        state.handle_event(&press(KeyCode::Char('n')));
        assert_eq!(state.confirmed, Some(false));
    }

    #[test]
    fn confirm_dialog_events_ignored_after_confirm() {
        let mut state = ConfirmDialogState::default();
        state.confirmed = Some(true);
        let changed = state.handle_event(&press(KeyCode::Left));
        assert!(!changed);
    }

    #[test]
    fn confirm_dialog_render_no_panic() {
        let dialog = ConfirmDialog::new("Are you sure?");
        let area = Rect::new(0, 0, 30, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 3, &mut pool);
        let mut state = ConfirmDialogState::default();
        StatefulWidget::render(&dialog, area, &mut frame, &mut state);
    }

    #[test]
    fn confirm_dialog_custom_labels() {
        let dialog = ConfirmDialog::new("Delete?").labels("Confirm", "Cancel");
        assert_eq!(dialog.yes_label, "Confirm");
        assert_eq!(dialog.no_label, "Cancel");
    }

    // -- Effective label width --

    #[test]
    fn effective_label_width_auto() {
        let form = Form::new(vec![
            FormField::text("Short"),
            FormField::text("Much Longer Label"),
        ]);
        // Should be max label len + 2
        assert_eq!(
            form.effective_label_width(),
            "Much Longer Label".len() as u16 + 2
        );
    }

    #[test]
    fn effective_label_width_fixed() {
        let form = Form::new(vec![FormField::text("Name")]).label_width(20);
        assert_eq!(form.effective_label_width(), 20);
    }

    // -- Field count and access --

    #[test]
    fn form_field_count() {
        let form = Form::new(vec![FormField::text("A"), FormField::text("B")]);
        assert_eq!(form.field_count(), 2);
    }

    #[test]
    fn form_field_access() {
        let form = Form::new(vec![FormField::text("Name")]);
        assert!(form.field(0).is_some());
        assert!(form.field(1).is_none());
    }

    #[test]
    fn form_field_mut_access() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        if let Some(FormField::Text { value, .. }) = form.field_mut(0) {
            *value = "Updated".into();
        }
        assert_eq!(
            form.data().get("Name"),
            Some(&FormValue::Text("Updated".into()))
        );
    }

    // -- Focus state edge cases --

    #[test]
    fn focus_on_empty_form() {
        let mut state = FormState::default();
        state.focus_next(0);
        assert_eq!(state.focused, 0);
        state.focus_prev(0);
        assert_eq!(state.focused, 0);
    }

    #[test]
    fn focus_single_field() {
        let mut state = FormState::default();
        state.focus_next(1);
        assert_eq!(state.focused, 0);
        state.focus_prev(1);
        assert_eq!(state.focused, 0);
    }

    // -- Scroll tracking --

    #[test]
    fn scroll_follows_focus() {
        let mut form = Form::new(vec![
            FormField::text("A"),
            FormField::text("B"),
            FormField::text("C"),
            FormField::text("D"),
            FormField::text("E"),
        ]);
        let mut state = FormState::default();

        // Viewport of 2 rows
        let area = Rect::new(0, 0, 30, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 2, &mut pool);

        // Focus on last field
        state.focused = 4;
        StatefulWidget::render(&form, area, &mut frame, &mut state);
        assert!(state.scroll >= 3); // Must scroll to show field 4
    }

    // -- Space inserts into text field --

    #[test]
    fn space_inserts_into_text() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "AB")]);
        let mut state = FormState::default();
        state.text_cursor = 1;

        state.handle_event(&mut form, &press(KeyCode::Char(' ')));
        if let FormField::Text { value, .. } = &form.fields[0] {
            assert_eq!(value, "A B");
        }
        assert_eq!(state.text_cursor, 2);
    }

    // -- Grapheme helpers --

    #[test]
    fn grapheme_count_ascii() {
        assert_eq!(grapheme_count("hello"), 5);
    }

    #[test]
    fn grapheme_count_unicode() {
        assert_eq!(grapheme_count("café"), 4);
    }

    #[test]
    fn grapheme_byte_offset_basic() {
        assert_eq!(grapheme_byte_offset("hello", 0), 0);
        assert_eq!(grapheme_byte_offset("hello", 3), 3);
        assert_eq!(grapheme_byte_offset("hello", 5), 5);
    }

    #[test]
    fn grapheme_byte_offset_past_end() {
        assert_eq!(grapheme_byte_offset("hi", 10), 2);
    }
}