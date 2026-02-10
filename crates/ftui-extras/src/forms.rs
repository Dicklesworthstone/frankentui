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
use ftui_widgets::{StatefulWidget, ValidationErrorDisplay, ValidationErrorState, Widget};

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
    success_style: Style,
    disabled_style: Style,
    required_style: Style,
    label_width: u16,
    required: Vec<bool>,
    disabled: Vec<bool>,
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
            success_style: Style::default(),
            disabled_style: Style::default(),
            required_style: Style::default(),
            label_width: 0, // auto-detect
            required: vec![false; count],
            disabled: vec![false; count],
        }
    }

    /// Set base style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set label style.
    #[must_use]
    pub fn label_style(mut self, style: Style) -> Self {
        self.label_style = style;
        self
    }

    /// Set focused field style.
    #[must_use]
    pub fn focused_style(mut self, style: Style) -> Self {
        self.focused_style = style;
        self
    }

    /// Set error message style.
    #[must_use]
    pub fn error_style(mut self, style: Style) -> Self {
        self.error_style = style;
        self
    }

    /// Set success style (valid field state).
    #[must_use]
    pub fn success_style(mut self, style: Style) -> Self {
        self.success_style = style;
        self
    }

    /// Set disabled field style.
    #[must_use]
    pub fn disabled_style(mut self, style: Style) -> Self {
        self.disabled_style = style;
        self
    }

    /// Set required indicator style.
    #[must_use]
    pub fn required_style(mut self, style: Style) -> Self {
        self.required_style = style;
        self
    }

    /// Update base style in place.
    pub fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    /// Update label style in place.
    pub fn set_label_style(&mut self, style: Style) {
        self.label_style = style;
    }

    /// Update focused field style in place.
    pub fn set_focused_style(&mut self, style: Style) {
        self.focused_style = style;
    }

    /// Update error message style in place.
    pub fn set_error_style(&mut self, style: Style) {
        self.error_style = style;
    }

    /// Update success style in place.
    pub fn set_success_style(&mut self, style: Style) {
        self.success_style = style;
    }

    /// Update disabled style in place.
    pub fn set_disabled_style(&mut self, style: Style) {
        self.disabled_style = style;
    }

    /// Update required indicator style in place.
    pub fn set_required_style(&mut self, style: Style) {
        self.required_style = style;
    }

    /// Mark a field as required (adds indicator).
    #[must_use]
    pub fn required(mut self, field_index: usize, required: bool) -> Self {
        self.set_required(field_index, required);
        self
    }

    /// Mark a field as disabled (non-interactive).
    #[must_use]
    pub fn disabled(mut self, field_index: usize, disabled: bool) -> Self {
        self.set_disabled(field_index, disabled);
        self
    }

    /// Set required flag for a field by index.
    pub fn set_required(&mut self, field_index: usize, required: bool) {
        if field_index < self.required.len() {
            self.required[field_index] = required;
        }
    }

    /// Set disabled flag for a field by index.
    pub fn set_disabled(&mut self, field_index: usize, disabled: bool) {
        if field_index < self.disabled.len() {
            self.disabled[field_index] = disabled;
        }
    }

    /// Check if a field is required.
    pub fn is_required(&self, field_index: usize) -> bool {
        self.required.get(field_index).copied().unwrap_or(false)
    }

    /// Check if a field is disabled.
    pub fn is_disabled(&self, field_index: usize) -> bool {
        self.disabled.get(field_index).copied().unwrap_or(false)
    }

    /// Set fixed label width (0 = auto-detect from longest label).
    #[must_use]
    pub fn label_width(mut self, width: u16) -> Self {
        self.label_width = width;
        self
    }

    /// Attach a validator to a field by index.
    #[must_use]
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
            if self.is_disabled(i) {
                continue;
            }
            if let Some(vf) = validator
                && let Some(msg) = vf(field)
            {
                errors.push(ValidationError {
                    field: i,
                    message: msg,
                });
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
            .enumerate()
            .map(|(i, f)| {
                let mut width = display_width(f.label()) as u16;
                if self.is_required(i) {
                    width = width.saturating_add(2); // " *"
                }
                width
            })
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
    /// Per-field touched state (true if field was focused then blurred).
    touched: Vec<bool>,
    /// Per-field dirty state (true if field value differs from initial).
    dirty: Vec<bool>,
    /// Initial field values for dirty tracking (set via `init_tracking`).
    initial_values: Option<Vec<FormValue>>,
    /// Per-field validation error display state (for animation/accessibility).
    error_states: Vec<ValidationErrorState>,
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

    // -------------------------------------------------------------------------
    // Touched / Dirty State Tracking
    // -------------------------------------------------------------------------

    /// Initialize tracking for a form's fields.
    ///
    /// This captures the current field values as the "initial" state for dirty
    /// tracking and ensures the touched/dirty vectors are sized correctly.
    /// Should be called once when the form is first displayed.
    pub fn init_tracking(&mut self, form: &Form) {
        let count = form.field_count();
        self.touched = vec![false; count];
        self.dirty = vec![false; count];
        self.ensure_error_states(count);
        self.initial_values = Some(
            form.fields
                .iter()
                .map(|f| match f {
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
                })
                .collect(),
        );
    }

    /// Check if a specific field has been touched (focused then blurred).
    pub fn is_touched(&self, field_idx: usize) -> bool {
        self.touched.get(field_idx).copied().unwrap_or(false)
    }

    /// Check if any field has been touched.
    pub fn any_touched(&self) -> bool {
        self.touched.iter().any(|&t| t)
    }

    /// Mark a specific field as touched.
    pub fn mark_touched(&mut self, field_idx: usize) {
        if field_idx < self.touched.len() {
            self.touched[field_idx] = true;
        }
    }

    /// Check if a specific field is dirty (value differs from initial).
    ///
    /// Returns `false` if tracking was not initialized or the field doesn't exist.
    pub fn is_dirty(&self, field_idx: usize) -> bool {
        self.dirty.get(field_idx).copied().unwrap_or(false)
    }

    /// Check if any field is dirty.
    pub fn any_dirty(&self) -> bool {
        self.dirty.iter().any(|&d| d)
    }

    /// Update dirty state for a field by comparing current value to initial.
    ///
    /// Call this after any value change to keep dirty state accurate.
    pub fn update_dirty(&mut self, form: &Form, field_idx: usize) {
        let Some(initial_values) = &self.initial_values else {
            return;
        };
        let Some(initial) = initial_values.get(field_idx) else {
            return;
        };
        let Some(field) = form.fields.get(field_idx) else {
            return;
        };

        let current = match field {
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

        if field_idx < self.dirty.len() {
            self.dirty[field_idx] = current != *initial;
        }
    }

    /// Get list of touched field indices.
    pub fn touched_fields(&self) -> Vec<usize> {
        self.touched
            .iter()
            .enumerate()
            .filter_map(|(i, &t)| if t { Some(i) } else { None })
            .collect()
    }

    /// Get list of dirty field indices.
    pub fn dirty_fields(&self) -> Vec<usize> {
        self.dirty
            .iter()
            .enumerate()
            .filter_map(|(i, &d)| if d { Some(i) } else { None })
            .collect()
    }

    /// Reset touched state for all fields.
    pub fn reset_touched(&mut self) {
        self.touched.iter_mut().for_each(|t| *t = false);
    }

    /// Reset dirty state by re-capturing current values as initial.
    pub fn reset_dirty(&mut self, form: &Form) {
        self.init_tracking(form);
    }

    /// Check if form is pristine (no fields touched or dirty).
    pub fn is_pristine(&self) -> bool {
        !self.any_touched() && !self.any_dirty()
    }

    fn ensure_error_states(&mut self, count: usize) {
        if self.error_states.len() != count {
            self.error_states = (0..count)
                .map(|i| ValidationErrorState::default().with_aria_id(i as u32 + 1))
                .collect();
        }
    }

    // -------------------------------------------------------------------------
    // Event Handling
    // -------------------------------------------------------------------------

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
        if form.is_disabled(self.focused) {
            match key.code {
                KeyCode::Tab => {
                    self.focus_next(form.field_count());
                    self.sync_text_cursor(form);
                    return true;
                }
                KeyCode::BackTab => {
                    self.focus_prev(form.field_count());
                    self.sync_text_cursor(form);
                    return true;
                }
                KeyCode::Up => {
                    self.focus_prev(form.field_count());
                    self.sync_text_cursor(form);
                    return true;
                }
                KeyCode::Down => {
                    self.focus_next(form.field_count());
                    self.sync_text_cursor(form);
                    return true;
                }
                KeyCode::Enter | KeyCode::Escape => {}
                _ => return false,
            }
        }
        match key.code {
            // Tab / Shift+Tab: navigate fields
            KeyCode::Tab => {
                // Mark current field as touched before moving focus
                self.mark_touched(self.focused);
                self.focus_next(form.field_count());
                self.sync_text_cursor(form);
                true
            }
            KeyCode::BackTab => {
                // Mark current field as touched before moving focus
                self.mark_touched(self.focused);
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
                    self.update_dirty(form, self.focused);
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
                    self.update_dirty(form, self.focused);
                    return true;
                }
                FormField::Number {
                    value, max, step, ..
                } => {
                    let new_val = value.saturating_add(*step);
                    *value = max.map_or(new_val, |m| new_val.min(m));
                    self.update_dirty(form, self.focused);
                    return true;
                }
                _ => {}
            }
        }
        // Default: move focus up (mark touched before moving)
        self.mark_touched(self.focused);
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
                    self.update_dirty(form, self.focused);
                    return true;
                }
                FormField::Select {
                    options, selected, ..
                } => {
                    if !options.is_empty() {
                        *selected = (*selected + 1) % options.len();
                    }
                    self.update_dirty(form, self.focused);
                    return true;
                }
                FormField::Number {
                    value, min, step, ..
                } => {
                    let new_val = value.saturating_sub(*step);
                    *value = min.map_or(new_val, |m| new_val.max(m));
                    self.update_dirty(form, self.focused);
                    return true;
                }
                _ => {}
            }
        }
        // Default: move focus down (mark touched before moving)
        self.mark_touched(self.focused);
        self.focus_next(form.field_count());
        self.sync_text_cursor(form);
        true
    }

    fn handle_space(&mut self, form: &mut Form) -> bool {
        if let Some(field) = form.fields.get_mut(self.focused) {
            match field {
                FormField::Checkbox { checked, .. } => {
                    *checked = !*checked;
                    self.update_dirty(form, self.focused);
                    return true;
                }
                FormField::Text { value, .. } => {
                    let byte_offset = grapheme_byte_offset(value, self.text_cursor);
                    value.insert(byte_offset, ' ');
                    self.text_cursor += 1;
                    self.update_dirty(form, self.focused);
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
                    self.update_dirty(form, self.focused);
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
                    self.update_dirty(form, self.focused);
                    return true;
                }
                FormField::Text { .. } => {
                    // Cursor movement doesn't change value, no dirty update needed
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
                    self.update_dirty(form, self.focused);
                    return true;
                }
                FormField::Select {
                    options, selected, ..
                } => {
                    if !options.is_empty() {
                        *selected = (*selected + 1) % options.len();
                    }
                    self.update_dirty(form, self.focused);
                    return true;
                }
                FormField::Text { value, .. } => {
                    // Cursor movement doesn't change value, no dirty update needed
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
            let before_count = grapheme_count(value);
            let byte_offset = grapheme_byte_offset(value, self.text_cursor);
            value.insert(byte_offset, c);
            let after_count = grapheme_count(value);
            if after_count > before_count {
                self.text_cursor += 1;
            } else {
                self.text_cursor = self.text_cursor.min(after_count);
            }
            self.update_dirty(form, self.focused);
            return true;
        }
        false
    }

    fn handle_text_backspace(&mut self, form: &mut Form) -> bool {
        if let Some(FormField::Text { value, .. }) = form.fields.get_mut(self.focused)
            && self.text_cursor > 0
        {
            let byte_start = grapheme_byte_offset(value, self.text_cursor - 1);
            let byte_end = grapheme_byte_offset(value, self.text_cursor);
            value.drain(byte_start..byte_end);
            self.text_cursor -= 1;
            self.update_dirty(form, self.focused);
            return true;
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
                self.update_dirty(form, self.focused);
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
        state.ensure_error_states(self.fields.len());

        // Compute row heights (1 line for field + optional error line).
        let mut row_heights = Vec::with_capacity(self.fields.len());
        let mut total_rows = 0usize;
        for i in 0..self.fields.len() {
            let has_error = state.errors.iter().any(|e| e.field == i);
            let height = if has_error { 2 } else { 1 };
            row_heights.push(height);
            total_rows = total_rows.saturating_add(height);
        }

        // Clamp focus
        if state.focused >= self.fields.len() {
            state.focused = self.fields.len().saturating_sub(1);
        }

        let focus_row_start: usize = row_heights.iter().take(state.focused).sum();
        let focus_row_end =
            focus_row_start.saturating_add(row_heights[state.focused].saturating_sub(1));

        // Ensure focused field is visible
        if focus_row_end >= state.scroll.saturating_add(visible_rows) {
            state.scroll = focus_row_end.saturating_sub(visible_rows.saturating_sub(1));
        } else if focus_row_start < state.scroll {
            state.scroll = focus_row_start;
        }
        let max_scroll = total_rows.saturating_sub(visible_rows);
        state.scroll = state.scroll.min(max_scroll);

        let mut row_cursor = 0usize;
        for (i, field) in self.fields.iter().enumerate() {
            let row_start = row_cursor;
            let row_height = row_heights[i];
            row_cursor = row_cursor.saturating_add(row_height);

            if row_start >= state.scroll.saturating_add(visible_rows) {
                break;
            }
            if row_start.saturating_add(row_height) <= state.scroll {
                continue;
            }

            let is_focused = i == state.focused;
            let is_disabled = self.is_disabled(i);

            // Find error for this field
            let error_msg = state
                .errors
                .iter()
                .find(|e| e.field == i)
                .map(|e| e.message.as_str());
            let has_error = error_msg.is_some();
            let is_success = !has_error
                && !is_disabled
                && (state.is_touched(i) || state.is_dirty(i) || state.submitted);

            // Sync error display state
            if let Some(error_state) = state.error_states.get_mut(i) {
                if error_msg.is_some() {
                    error_state.show();
                } else {
                    error_state.hide();
                }
            }

            // Draw label + value on the primary line if visible
            if row_start >= state.scroll {
                let y = area.y.saturating_add((row_start - state.scroll) as u16);

                let label_style = if is_disabled {
                    self.disabled_style
                } else if has_error {
                    self.error_style
                } else if is_focused {
                    self.focused_style
                } else if is_success {
                    self.success_style
                } else {
                    self.label_style
                };

                let label = field.label();
                let label_space = label_w.saturating_sub(2);
                let label_width = display_width(label).min(label_space as usize) as u16;
                let can_show_required =
                    self.is_required(i) && label_width.saturating_add(2) <= label_space;
                draw_str(frame, area.x, y, label, label_style, label_space);
                if can_show_required {
                    let star_x = area.x.saturating_add(label_width);
                    draw_str(
                        frame,
                        star_x,
                        y,
                        " *",
                        self.required_style,
                        label_space.saturating_sub(label_width),
                    );
                }

                // Draw ": " separator
                let label_render_width = if can_show_required {
                    label_width.saturating_add(2)
                } else {
                    label_width
                };
                let sep_x = area.x.saturating_add(label_render_width);
                draw_str(frame, sep_x, y, ": ", label_style, 2);

                // Draw field value
                let field_style = if is_disabled {
                    self.disabled_style
                } else if has_error {
                    self.error_style
                } else if is_focused {
                    self.focused_style
                } else if is_success {
                    self.success_style
                } else {
                    self.style
                };
                let placeholder_style = if is_disabled {
                    self.disabled_style
                } else if has_error {
                    self.error_style
                } else {
                    self.label_style
                };

                let focus_for_field = is_focused && !is_disabled;
                self.render_field(
                    frame,
                    field,
                    value_x,
                    y,
                    value_width,
                    field_style,
                    placeholder_style,
                    focus_for_field,
                    state,
                );
            }

            // Draw error on the line below if visible
            if let (Some(msg), Some(error_state)) = (error_msg, state.error_states.get_mut(i)) {
                let error_row = row_start.saturating_add(1);
                if error_row >= state.scroll
                    && error_row < state.scroll.saturating_add(visible_rows)
                    && value_width > 0
                {
                    let y = area.y.saturating_add((error_row - state.scroll) as u16);
                    let error_area = Rect::new(value_x, y, value_width, 1);
                    let display = ValidationErrorDisplay::new(msg)
                        .with_style(self.error_style)
                        .with_icon_style(self.error_style);
                    StatefulWidget::render(&display, error_area, frame, error_state);
                }
            }
        }
    }
}

impl Form {
    #[allow(clippy::too_many_arguments)]
    fn render_field(
        &self,
        frame: &mut Frame,
        field: &FormField,
        x: u16,
        y: u16,
        width: u16,
        style: Style,
        placeholder_style: Style,
        is_focused: bool,
        state: &FormState,
    ) {
        match field {
            FormField::Text {
                value, placeholder, ..
            } => {
                if value.is_empty() {
                    if let Some(ph) = placeholder {
                        draw_str(frame, x, y, ph, placeholder_style, width);
                    }
                } else {
                    draw_str(frame, x, y, value, style, width);
                }
                // Draw cursor if focused
                if is_focused {
                    let buf = &mut frame.buffer;
                    let cursor_col = grapheme_display_width(value, state.text_cursor);
                    let cursor_x = x.saturating_add(cursor_col.min(width as usize) as u16);
                    if cursor_x < x.saturating_add(width)
                        && let Some(cell) = buf.get_mut(cursor_x, y)
                    {
                        use ftui_render::cell::StyleFlags;
                        let flags = cell.attrs.flags();
                        cell.attrs = cell.attrs.with_flags(flags ^ StyleFlags::REVERSE);
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
    #[must_use]
    pub fn labels(mut self, yes: impl Into<String>, no: impl Into<String>) -> Self {
        self.yes_label = yes.into();
        self.no_label = no.into();
        self
    }

    /// Set base style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set selected button style.
    #[must_use]
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
        draw_str(frame, area.x, msg_y, &self.message, self.style, area.width);

        // Draw buttons on last row
        let btn_y = if area.height > 1 {
            area.bottom().saturating_sub(1)
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
        let yes_w = display_width(yes_str.as_str());
        let no_w = display_width(no_str.as_str());
        let total_btn_width = yes_w + 2 + no_w;
        let start_x = area
            .x
            .saturating_add(area.width.saturating_sub(total_btn_width as u16) / 2);

        draw_str(frame, start_x, btn_y, &yes_str, yes_style, area.width);
        let no_x = start_x.saturating_add(yes_w as u16).saturating_add(2);
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
        let w = grapheme_width(grapheme) as u16;
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

        // set_fast() skips scissor/opacity/compositing checks for common
        // single-width opaque cells; falls back to set() otherwise.
        frame.buffer.set_fast(x.saturating_add(col), y, cell);

        col = col.saturating_add(w);
    }
}

/// Count grapheme clusters in a string.
fn grapheme_count(s: &str) -> usize {
    unicode_segmentation::UnicodeSegmentation::graphemes(s, true).count()
}

/// Display width of a single grapheme cluster.
fn grapheme_width(grapheme: &str) -> usize {
    if grapheme.is_ascii() {
        return ascii_display_width(grapheme);
    }
    if grapheme.chars().all(is_zero_width_codepoint) {
        return 0;
    }
    usize::try_from(unicode_display_width::width(grapheme))
        .expect("unicode display width should fit in usize")
}

/// Display width of a string in terminal cells.
fn display_width(s: &str) -> usize {
    if s.is_ascii() && s.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return s.len();
    }
    if s.is_ascii() {
        return ascii_display_width(s);
    }
    if !s.chars().any(is_zero_width_codepoint) {
        return usize::try_from(unicode_display_width::width(s))
            .expect("unicode display width should fit in usize");
    }
    unicode_segmentation::UnicodeSegmentation::graphemes(s, true)
        .map(grapheme_width)
        .sum()
}

#[inline]
fn ascii_display_width(text: &str) -> usize {
    let mut width = 0;
    for b in text.bytes() {
        match b {
            b'\t' | b'\n' | b'\r' => width += 1,
            0x20..=0x7E => width += 1,
            _ => {}
        }
    }
    width
}

#[inline]
fn is_zero_width_codepoint(c: char) -> bool {
    let u = c as u32;
    matches!(u, 0x0000..=0x001F | 0x007F..=0x009F)
        || matches!(u, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF)
        || matches!(u, 0xFE20..=0xFE2F)
        || matches!(u, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
        || matches!(
            u,
            0x00AD | 0x034F | 0x180E | 0x200B | 0x200C | 0x200D | 0x200E | 0x200F | 0x2060 | 0xFEFF
        )
        || matches!(u, 0x202A..=0x202E | 0x2066..=0x2069 | 0x206A..=0x206F)
}

/// Compute the display width (cells) of the first `grapheme_count` graphemes.
fn grapheme_display_width(s: &str, grapheme_count: usize) -> usize {
    unicode_segmentation::UnicodeSegmentation::graphemes(s, true)
        .take(grapheme_count)
        .map(grapheme_width)
        .sum()
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

    fn row_to_string(buffer: &Buffer, y: u16, width: u16) -> String {
        let mut out = String::with_capacity(width as usize);
        for x in 0..width {
            let ch = buffer
                .get(x, y)
                .and_then(|cell| cell.content.as_char())
                .unwrap_or(' ');
            out.push(ch);
        }
        out
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    #[allow(dead_code)]
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
            unreachable!("expected Text");
        }
    }

    #[test]
    fn text_field_with_value() {
        let f = FormField::text_with_value("Name", "Alice");
        if let FormField::Text { value, .. } = &f {
            assert_eq!(value, "Alice");
        } else {
            unreachable!("expected Text");
        }
    }

    #[test]
    fn text_field_with_placeholder() {
        let f = FormField::text_with_placeholder("Name", "Enter name...");
        if let FormField::Text { placeholder, .. } = &f {
            assert_eq!(placeholder.as_deref(), Some("Enter name..."));
        } else {
            unreachable!("expected Text");
        }
    }

    #[test]
    fn checkbox_field() {
        let f = FormField::checkbox("Agree", false);
        if let FormField::Checkbox { checked, .. } = &f {
            assert!(!checked);
        } else {
            unreachable!("expected Checkbox");
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
            unreachable!("expected Radio");
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
            unreachable!("expected Select");
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
            unreachable!("expected Number");
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
            unreachable!("expected Number");
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
                if let FormField::Text { value, .. } = f
                    && value.is_empty()
                {
                    return Some("Required".into());
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
                if let FormField::Text { value, .. } = f
                    && value.is_empty()
                {
                    return Some("Required".into());
                }
                None
            }),
        );

        assert!(form.validate_all().is_empty());
    }

    #[test]
    fn required_flag_sets_indicator() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        assert!(!form.is_required(0));
        form.set_required(0, true);
        assert!(form.is_required(0));
    }

    #[test]
    fn disabled_field_ignores_text_input() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        form.set_disabled(0, true);
        let mut state = FormState::default();
        state.init_tracking(&form);

        let changed = state.handle_event(&mut form, &press(KeyCode::Char('a')));
        assert!(!changed, "disabled field should ignore input");

        if let Some(FormField::Text { value, .. }) = form.field(0) {
            assert!(value.is_empty());
        }
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
        let mut state = FormState {
            text_cursor: 3,
            ..Default::default()
        };

        state.handle_event(&mut form, &press(KeyCode::Backspace));
        if let FormField::Text { value, .. } = &form.fields[0] {
            assert_eq!(value, "ab");
        }
        assert_eq!(state.text_cursor, 2);
    }

    #[test]
    fn text_delete() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "abc")]);
        let mut state = FormState {
            text_cursor: 0,
            ..Default::default()
        };

        state.handle_event(&mut form, &press(KeyCode::Delete));
        if let FormField::Text { value, .. } = &form.fields[0] {
            assert_eq!(value, "bc");
        }
        assert_eq!(state.text_cursor, 0);
    }

    #[test]
    fn text_cursor_movement() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "hello")]);
        let mut state = FormState {
            text_cursor: 3,
            ..Default::default()
        };

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
        let mut state = FormState {
            text_cursor: 0,
            ..Default::default()
        };

        state.handle_event(&mut form, &press(KeyCode::Backspace));
        if let FormField::Text { value, .. } = &form.fields[0] {
            assert_eq!(value, "abc");
        }
    }

    #[test]
    fn text_delete_at_end_noop() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "abc")]);
        let mut state = FormState {
            text_cursor: 3,
            ..Default::default()
        };

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
                if let FormField::Text { value, .. } = f
                    && value.is_empty()
                {
                    return Some("Required".into());
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
        let mut state = FormState {
            submitted: true,
            ..Default::default()
        };

        let changed = state.handle_event(&mut form, &press(KeyCode::Tab));
        assert!(!changed);
    }

    #[test]
    fn events_ignored_after_cancel() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState {
            cancelled: true,
            ..Default::default()
        };

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
            frame
                .buffer
                .get(label_end as u16, 0)
                .unwrap()
                .content
                .as_char(),
            Some('[')
        );
        assert_eq!(
            frame
                .buffer
                .get(label_end as u16 + 1, 0)
                .unwrap()
                .content
                .as_char(),
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
        let mut state = ConfirmDialogState {
            selected_yes: true,
            ..Default::default()
        };
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
        let mut state = ConfirmDialogState {
            confirmed: Some(true),
            ..Default::default()
        };
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
            display_width("Much Longer Label") as u16 + 2
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
        let form = Form::new(vec![
            FormField::text("A"),
            FormField::text("B"),
            FormField::text("C"),
            FormField::text("D"),
            FormField::text("E"),
        ]);
        let mut state = FormState {
            focused: 4,
            ..Default::default()
        };

        // Viewport of 2 rows
        let area = Rect::new(0, 0, 30, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 2, &mut pool);
        StatefulWidget::render(&form, area, &mut frame, &mut state);
        assert!(state.scroll >= 3); // Must scroll to show field 4
    }

    #[test]
    fn error_renders_below_field() {
        let form = Form::new(vec![FormField::text("Name")])
            .validate(0, Box::new(|_| Some("Required".to_string())));
        let mut state = FormState {
            errors: form.validate_all(),
            ..Default::default()
        };

        let area = Rect::new(0, 0, 30, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 2, &mut pool);
        StatefulWidget::render(&form, area, &mut frame, &mut state);

        let row0 = row_to_string(&frame.buffer, 0, 30);
        let row1 = row_to_string(&frame.buffer, 1, 30);
        assert!(!row0.contains('⚠'));
        assert!(row1.contains('⚠'));
        assert!(row1.contains("Required"));
    }

    // -- Space inserts into text field --

    #[test]
    fn space_inserts_into_text() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "AB")]);
        let mut state = FormState {
            text_cursor: 1,
            ..Default::default()
        };

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

    // -- Touched / Dirty state tracking --

    #[test]
    fn init_tracking_sets_up_vectors() {
        let form = Form::new(vec![
            FormField::text("Name"),
            FormField::checkbox("Agree", false),
            FormField::number("Age", 25),
        ]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert_eq!(state.touched.len(), 3);
        assert_eq!(state.dirty.len(), 3);
        assert!(state.initial_values.is_some());
        assert!(state.is_pristine());
    }

    #[test]
    fn tab_marks_field_as_touched() {
        let mut form = Form::new(vec![FormField::text("A"), FormField::text("B")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.is_touched(0));
        state.handle_event(&mut form, &press(KeyCode::Tab));
        assert!(state.is_touched(0));
        assert!(!state.is_touched(1));
    }

    #[test]
    fn backtab_marks_field_as_touched() {
        let mut form = Form::new(vec![FormField::text("A"), FormField::text("B")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::BackTab));
        assert!(state.is_touched(0));
    }

    #[test]
    fn text_input_marks_dirty() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.is_dirty(0));
        state.handle_event(&mut form, &press(KeyCode::Char('A')));
        assert!(state.is_dirty(0));
    }

    #[test]
    fn checkbox_toggle_marks_dirty() {
        let mut form = Form::new(vec![FormField::checkbox("Agree", false)]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.is_dirty(0));
        state.handle_event(&mut form, &press(KeyCode::Char(' ')));
        assert!(state.is_dirty(0));
    }

    #[test]
    fn number_change_marks_dirty() {
        let mut form = Form::new(vec![FormField::number("Count", 10)]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.is_dirty(0));
        state.handle_event(&mut form, &press(KeyCode::Up));
        assert!(state.is_dirty(0));
    }

    #[test]
    fn radio_change_marks_dirty() {
        let mut form = Form::new(vec![FormField::radio(
            "Color",
            vec!["Red".into(), "Green".into()],
        )]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.is_dirty(0));
        state.handle_event(&mut form, &press(KeyCode::Down));
        assert!(state.is_dirty(0));
    }

    #[test]
    fn select_change_marks_dirty() {
        let mut form = Form::new(vec![FormField::select(
            "Size",
            vec!["S".into(), "M".into()],
        )]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.is_dirty(0));
        state.handle_event(&mut form, &press(KeyCode::Right));
        assert!(state.is_dirty(0));
    }

    #[test]
    fn any_touched_returns_true_when_one_touched() {
        let mut form = Form::new(vec![FormField::text("A"), FormField::text("B")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.any_touched());
        state.handle_event(&mut form, &press(KeyCode::Tab));
        assert!(state.any_touched());
    }

    #[test]
    fn any_dirty_returns_true_when_one_dirty() {
        let mut form = Form::new(vec![
            FormField::text("A"),
            FormField::text_with_value("B", "Hello"),
        ]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.any_dirty());
        state.handle_event(&mut form, &press(KeyCode::Char('X')));
        assert!(state.any_dirty());
    }

    #[test]
    fn touched_fields_returns_indices() {
        let mut form = Form::new(vec![
            FormField::text("A"),
            FormField::text("B"),
            FormField::text("C"),
        ]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::Tab));
        state.handle_event(&mut form, &press(KeyCode::Tab));
        // Touched: 0, 1 (current field 2 not yet touched since we haven't left it)
        assert_eq!(state.touched_fields(), vec![0, 1]);
    }

    #[test]
    fn dirty_fields_returns_indices() {
        let mut form = Form::new(vec![
            FormField::text("A"),
            FormField::text("B"),
            FormField::text("C"),
        ]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::Char('X')));
        state.handle_event(&mut form, &press(KeyCode::Tab));
        state.handle_event(&mut form, &press(KeyCode::Tab));
        state.handle_event(&mut form, &press(KeyCode::Char('Y')));
        // Dirty: 0 (typed X), 2 (typed Y)
        assert_eq!(state.dirty_fields(), vec![0, 2]);
    }

    #[test]
    fn reset_touched_clears_all() {
        let mut form = Form::new(vec![FormField::text("A"), FormField::text("B")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::Tab));
        assert!(state.any_touched());

        state.reset_touched();
        assert!(!state.any_touched());
    }

    #[test]
    fn reset_dirty_re_initializes() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::Char('A')));
        assert!(state.is_dirty(0));

        state.reset_dirty(&form);
        // After reset, "A" is now the initial value, so not dirty
        assert!(!state.is_dirty(0));
    }

    #[test]
    fn is_pristine_initially_true() {
        let form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(state.is_pristine());
    }

    #[test]
    fn is_pristine_false_after_touched() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::Tab));
        assert!(!state.is_pristine());
    }

    #[test]
    fn is_pristine_false_after_dirty() {
        let mut form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::Char('X')));
        assert!(!state.is_pristine());
    }

    #[test]
    fn dirty_becomes_false_when_value_reverts() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "A")]);
        let mut state = FormState {
            text_cursor: 1,
            ..Default::default()
        };
        state.init_tracking(&form);

        // Type a character
        state.handle_event(&mut form, &press(KeyCode::Char('B')));
        assert!(state.is_dirty(0));

        // Delete it
        state.handle_event(&mut form, &press(KeyCode::Backspace));
        assert!(!state.is_dirty(0));
    }

    #[test]
    fn backspace_updates_dirty() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "AB")]);
        let mut state = FormState {
            text_cursor: 2,
            ..Default::default()
        };
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::Backspace));
        assert!(state.is_dirty(0));
    }

    #[test]
    fn delete_updates_dirty() {
        let mut form = Form::new(vec![FormField::text_with_value("Name", "AB")]);
        let mut state = FormState {
            text_cursor: 0,
            ..Default::default()
        };
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::Delete));
        assert!(state.is_dirty(0));
    }

    #[test]
    fn is_touched_returns_false_for_invalid_index() {
        let form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.is_touched(100));
    }

    #[test]
    fn is_dirty_returns_false_for_invalid_index() {
        let form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        assert!(!state.is_dirty(100));
    }

    #[test]
    fn is_touched_false_without_init() {
        let state = FormState::default();
        assert!(!state.is_touched(0));
    }

    #[test]
    fn is_dirty_false_without_init() {
        let state = FormState::default();
        assert!(!state.is_dirty(0));
    }

    #[test]
    fn mark_touched_noop_for_invalid_index() {
        let form = Form::new(vec![FormField::text("Name")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        // Should not panic
        state.mark_touched(100);
        assert!(!state.is_touched(100));
    }

    #[test]
    fn up_on_text_field_marks_touched() {
        let mut form = Form::new(vec![FormField::text("A"), FormField::text("B")]);
        let mut state = FormState {
            focused: 1,
            ..Default::default()
        };
        state.init_tracking(&form);

        // Up on text field moves focus (doesn't change value)
        state.handle_event(&mut form, &press(KeyCode::Up));
        assert!(state.is_touched(1));
        assert_eq!(state.focused, 0);
    }

    #[test]
    fn down_on_text_field_marks_touched() {
        let mut form = Form::new(vec![FormField::text("A"), FormField::text("B")]);
        let mut state = FormState::default();
        state.init_tracking(&form);

        state.handle_event(&mut form, &press(KeyCode::Down));
        assert!(state.is_touched(0));
        assert_eq!(state.focused, 1);
    }
}
