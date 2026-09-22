#![forbid(unsafe_code)]

//! Form and picker widgets for interactive data entry.
//!
//! Provides a `Form` widget with field types (text, checkbox, radio, select, number),
//! validation, tab navigation, and submit/cancel actions. Also includes a `ConfirmDialog`
//! for simple yes/no prompts.
//!
//! Feature-gated under `forms`.

use ftui_a11y::node::{A11yNodeInfo, A11yRole, LiveRegion};
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
    /// Optional stable accessibility identity for the form container.
    ///
    /// Set this when the same logical form can move or resize between frames.
    /// Without it the fallback identity is derived from the rendered area.
    accessibility_id: Option<u64>,
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
            accessibility_id: None,
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

    /// Give this form a stable accessibility identity.
    ///
    /// Child field identities are derived from this value and their field
    /// index, so values, validation messages, scrolling, and geometry changes
    /// do not replace the focused accessibility node.
    #[must_use]
    pub fn accessibility_id(mut self, id: u64) -> Self {
        self.accessibility_id = Some(id);
        self
    }

    /// Update the stable accessibility identity in place.
    pub fn set_accessibility_id(&mut self, id: Option<u64>) {
        self.accessibility_id = id;
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

    /// Move forward to the next enabled field, wrapping once.
    fn focus_next_enabled(&mut self, form: &Form) {
        let count = form.field_count();
        if count == 0 {
            return;
        }
        self.focused = self.focused.min(count - 1);
        for offset in 1..=count {
            let candidate = (self.focused + offset) % count;
            if !form.is_disabled(candidate) {
                self.focused = candidate;
                return;
            }
        }
    }

    /// Move backward to the previous enabled field, wrapping once.
    fn focus_prev_enabled(&mut self, form: &Form) {
        let count = form.field_count();
        if count == 0 {
            return;
        }
        self.focused = self.focused.min(count - 1);
        for offset in 1..=count {
            let candidate = (self.focused + count - (offset % count)) % count;
            if !form.is_disabled(candidate) {
                self.focused = candidate;
                return;
            }
        }
    }

    /// Repair externally retained focus before rendering.
    ///
    /// Disabled fields are not keyboard focus stops. If every field is disabled
    /// the numeric state remains clamped but no accessibility child is focused.
    fn normalize_focus(&mut self, form: &Form) {
        let count = form.field_count();
        if count == 0 {
            self.focused = 0;
            return;
        }
        self.focused = self.focused.min(count - 1);
        if form.is_disabled(self.focused) {
            for offset in 1..=count {
                let candidate = (self.focused + offset) % count;
                if !form.is_disabled(candidate) {
                    self.focused = candidate;
                    break;
                }
            }
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
                    self.focus_next_enabled(form);
                    self.sync_text_cursor(form);
                    return true;
                }
                KeyCode::BackTab => {
                    self.focus_prev_enabled(form);
                    self.sync_text_cursor(form);
                    return true;
                }
                KeyCode::Up => {
                    self.focus_prev_enabled(form);
                    self.sync_text_cursor(form);
                    return true;
                }
                KeyCode::Down => {
                    self.focus_next_enabled(form);
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
                // Mark current field as touched before moving focus.
                self.mark_touched(self.focused);
                self.focus_next_enabled(form);
                self.sync_text_cursor(form);
                true
            }
            KeyCode::BackTab => {
                // Mark current field as touched before moving focus.
                self.mark_touched(self.focused);
                self.focus_prev_enabled(form);
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
        // Default: move focus up (mark touched before moving).
        self.mark_touched(self.focused);
        self.focus_prev_enabled(form);
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
        // Default: move focus down (mark touched before moving).
        self.mark_touched(self.focused);
        self.focus_next_enabled(form);
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
        state.normalize_focus(self);

        let a11y_root = if frame.a11y_enabled() {
            let bounds = area.intersection(&frame.buffer.current_scissor());
            if bounds.is_empty() {
                None
            } else {
                let id = self.accessibility_root_id(area);
                let root = A11yNodeInfo::new(id, A11yRole::Group, bounds)
                    .with_name("Form")
                    .with_description(format!("{} fields", self.fields.len()));
                frame.push_a11y(root);
                Some(id)
            }
        } else {
            None
        };

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

                if let Some(parent) = a11y_root {
                    let bounds = Rect::new(area.x, y, area.width, 1)
                        .intersection(&frame.buffer.current_scissor());
                    if !bounds.is_empty() {
                        frame.push_a11y(self.accessibility_field_node(
                            parent,
                            i,
                            field,
                            bounds,
                            focus_for_field,
                            error_msg,
                        ));
                    }
                }
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
                    // The field's own node already carries the error, and
                    // is the live region only while the field has focus.
                    let display = ValidationErrorDisplay::new(msg)
                        .with_style(self.error_style)
                        .with_icon_style(self.error_style)
                        .without_accessibility_node();
                    StatefulWidget::render(&display, error_area, frame, error_state);
                }
            }
        }
    }
}

impl Form {
    fn accessibility_root_id(&self, area: Rect) -> u64 {
        self.accessibility_id
            .unwrap_or_else(|| A11yNodeInfo::stable_id_for(A11yRole::Group, area))
    }

    fn accessibility_field_id(parent: u64, index: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        ("ftui.form.field", parent, index).hash(&mut hasher);
        hasher.finish()
    }

    fn accessibility_field_node(
        &self,
        parent: u64,
        index: usize,
        field: &FormField,
        bounds: Rect,
        focused: bool,
        error: Option<&str>,
    ) -> A11yNodeInfo {
        let role = match field {
            FormField::Text { .. } => A11yRole::TextInput,
            FormField::Checkbox { .. } => A11yRole::Checkbox,
            FormField::Radio { .. } => A11yRole::RadioButton,
            FormField::Select { .. } => A11yRole::MenuItem,
            FormField::Number { .. } => A11yRole::Slider,
        };
        let mut node = A11yNodeInfo::new(Self::accessibility_field_id(parent, index), role, bounds)
            .with_parent(parent)
            .with_name(field.label());
        node.state.focused = focused;
        node.state.disabled = self.is_disabled(index);
        node.state.required = self.is_required(index);
        match field {
            FormField::Text {
                value, placeholder, ..
            } => {
                node.state.value_text = Some(value.clone());
                if error.is_none() && value.is_empty() {
                    node.description = placeholder
                        .as_deref()
                        .filter(|text| !text.trim().is_empty())
                        .map(|text| format!("Placeholder: {text}"));
                }
            }
            FormField::Checkbox { checked, .. } => node.state.checked = Some(*checked),
            FormField::Radio {
                options, selected, ..
            } => {
                node.state.checked = Some(true);
                node.state.value_now = Some(*selected as f64);
                node.state.value_min = Some(0.0);
                node.state.value_max = options.len().checked_sub(1).map(|last| last as f64);
                node.state.value_text = options.get(*selected).cloned();
            }
            FormField::Select {
                options, selected, ..
            } => {
                node.state.selected = true;
                node.state.value_now = Some(*selected as f64);
                node.state.value_min = Some(0.0);
                node.state.value_max = options.len().checked_sub(1).map(|last| last as f64);
                node.state.value_text = options.get(*selected).cloned();
            }
            FormField::Number {
                value, min, max, ..
            } => {
                node.state.value_now = Some(*value as f64);
                node.state.value_min = min.map(|value| value as f64);
                node.state.value_max = max.map(|value| value as f64);
            }
        }
        if let Some(error) = error {
            node.description = Some(format!("Error: {error}"));
            if focused {
                node.live_region = Some(LiveRegion::Polite);
            }
        }
        node
    }

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
        if total_btn_width as u16 <= area.width {
            let start_x = area
                .x
                .saturating_add(area.width.saturating_sub(total_btn_width as u16) / 2);

            let yes_width = area.right().saturating_sub(start_x);
            draw_str(frame, start_x, btn_y, &yes_str, yes_style, yes_width);
            let no_x = start_x.saturating_add(yes_w as u16).saturating_add(2);
            let no_width = area.right().saturating_sub(no_x);
            draw_str(frame, no_x, btn_y, &no_str, no_style, no_width);
        } else {
            // Under severe width pressure, keep the selected action visible
            // instead of letting it fall completely outside the dialog area.
            let (selected_label, selected_style, selected_width) = if state.selected_yes {
                (&yes_str, yes_style, yes_w as u16)
            } else {
                (&no_str, no_style, no_w as u16)
            };
            let selected_width = area.width.min(selected_width);
            let selected_x = area
                .x
                .saturating_add(area.width.saturating_sub(selected_width) / 2);
            draw_str(
                frame,
                selected_x,
                btn_y,
                selected_label,
                selected_style,
                selected_width,
            );
        }
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
        match bg.a() {
            0 => {}                          // Fully transparent: no-op
            255 => cell.bg = bg,             // Fully opaque: replace
            _ => cell.bg = bg.over(cell.bg), // Composite src-over-dst
        }
    }
    if let Some(attrs) = style.attrs {
        let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
        cell.attrs = cell.attrs.merged_flags(cell_flags);
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

/// Apply a style to a cell that will be written back through `Buffer::set`.
///
/// Translucent backgrounds must remain as source colors here so the buffer can
/// composite them exactly once against the current destination cell.
fn apply_write_style(cell: &mut Cell, style: Style) {
    // Rewritten text must not inherit stale hyperlink metadata from the
    // background cell it is painting over.
    cell.attrs = ftui_render::cell::CellAttrs::new(cell.attrs.flags(), 0);
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg
        && bg.a() != 0
    {
        cell.bg = bg;
    }
    if let Some(attrs) = style.attrs {
        let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
        cell.attrs = cell.attrs.merged_flags(cell_flags);
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

        let mut cell = frame
            .buffer
            .get(x.saturating_add(col), y)
            .copied()
            .unwrap_or_else(|| Cell::new(cell_content));
        cell.content = cell_content;
        apply_write_style(&mut cell, style);

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

// Measure as the buffer draws: see the note on the same import in charts.rs.
use ftui_core::text_width::{display_width, grapheme_width};

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
    use ftui_a11y::tree::{A11yTree, A11yTreeBuilder, AnnouncementReason, ScreenReaderPolicy};
    use ftui_core::event::{KeyEvent, KeyEventKind};
    use ftui_render::cell::PackedRgba;
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

    fn render_form_a11y(form: &Form, state: &mut FormState, area: Rect) -> A11yTree {
        let mut builder = A11yTreeBuilder::new();
        {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(80, 24, &mut pool);
            frame.set_a11y(&mut builder);
            StatefulWidget::render(form, area, &mut frame, state);
            frame.finish_a11y();
        }
        builder.build()
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
    fn confirm_dialog_selected_style_preserves_base_background() {
        let base_bg = PackedRgba::rgb(12, 34, 56);
        let selected_fg = PackedRgba::rgb(250, 240, 10);
        let dialog = ConfirmDialog::new("Proceed?")
            .style(Style::new().bg(base_bg))
            .selected_style(Style::new().fg(selected_fg));
        let area = Rect::new(0, 0, 24, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(area.width, area.height, &mut pool);
        let mut state = ConfirmDialogState {
            selected_yes: true,
            ..Default::default()
        };

        StatefulWidget::render(&dialog, area, &mut frame, &mut state);

        let selected_cell = frame
            .buffer
            .get(8, 2)
            .copied()
            .expect("selected button cell should exist");
        assert_eq!(selected_cell.bg, base_bg);
        assert_eq!(selected_cell.fg, selected_fg);
    }

    #[test]
    fn draw_str_translucent_background_composites_once() {
        let base_bg = PackedRgba::rgb(0, 0, 255);
        let overlay_bg = PackedRgba::rgba(255, 0, 0, 128);
        let area = Rect::new(0, 0, 4, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(area.width, area.height, &mut pool);

        set_style_area(&mut frame.buffer, area, Style::new().bg(base_bg));
        draw_str(&mut frame, 0, 0, "A", Style::new().bg(overlay_bg), 1);

        let cell = frame
            .buffer
            .get(0, 0)
            .copied()
            .expect("drawn cell should exist");
        assert_eq!(cell.bg, overlay_bg.over(base_bg));
    }

    #[test]
    fn draw_str_clears_stale_link_metadata() {
        let area = Rect::new(0, 0, 4, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(area.width, area.height, &mut pool);
        let mut cell = Cell::from_char('X');
        cell.attrs =
            ftui_render::cell::CellAttrs::new(ftui_render::cell::StyleFlags::UNDERLINE, 42);
        frame.buffer.set_fast(0, 0, cell);

        draw_str(&mut frame, 0, 0, "A", Style::new(), 1);

        let cell = frame
            .buffer
            .get(0, 0)
            .copied()
            .expect("drawn cell should exist");
        assert_eq!(cell.attrs.link_id(), 0);
        assert!(
            cell.attrs
                .flags()
                .contains(ftui_render::cell::StyleFlags::UNDERLINE)
        );
    }

    #[test]
    fn confirm_dialog_buttons_stay_within_area_bounds() {
        let dialog = ConfirmDialog::new("Proceed?");
        let area = Rect::new(10, 0, 15, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, area.height, &mut pool);
        let mut state = ConfirmDialogState {
            selected_yes: true,
            ..Default::default()
        };

        StatefulWidget::render(&dialog, area, &mut frame, &mut state);

        for x in area.right()..30 {
            let cell = frame
                .buffer
                .get(x, area.bottom().saturating_sub(1))
                .copied()
                .expect("cell outside dialog area should exist");
            assert!(
                cell.content.is_empty(),
                "confirm dialog must not render outside its area at x={x}"
            );
        }
    }

    #[test]
    fn confirm_dialog_narrow_layout_keeps_selected_button_visible() {
        let selected_fg = PackedRgba::rgb(250, 240, 10);
        let dialog = ConfirmDialog::new("Proceed?")
            .style(Style::new().fg(PackedRgba::rgb(40, 40, 40)))
            .selected_style(Style::new().fg(selected_fg));
        let area = Rect::new(10, 0, 8, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, area.height, &mut pool);
        let mut state = ConfirmDialogState::default();

        StatefulWidget::render(&dialog, area, &mut frame, &mut state);

        let selected_visible = (area.x..area.right()).any(|x| {
            frame
                .buffer
                .get(x, area.bottom().saturating_sub(1))
                .is_some_and(|cell| !cell.content.is_empty() && cell.fg == selected_fg)
        });
        assert!(
            selected_visible,
            "narrow confirm dialog should keep the selected button visible"
        );
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

    // -----------------------------------------------------------------------
    // Helper function tests
    // -----------------------------------------------------------------------

    #[test]
    fn display_width_counts_whitespace_and_skips_other_controls() {
        // Tab, newline, CR each count as 1; other control bytes as 0.
        assert_eq!(super::display_width("\t\n\r"), 3);
        assert_eq!(super::display_width("\x01\x02\x03"), 0);
        assert_eq!(super::display_width("a\x01b"), 2);
    }

    #[test]
    fn zero_width_codepoints_measure_nothing() {
        for c in [
            '\x00', '\x1F', '\x7F', '\u{0300}', '\u{036F}', '\u{FE00}', '\u{FE0F}', '\u{200B}',
            '\u{200D}', '\u{FEFF}', '\u{2060}', '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}',
        ] {
            assert_eq!(super::grapheme_width(&c.to_string()), 0, "{c:?}");
        }
        for c in ['a', ' ', 'Z'] {
            assert_eq!(super::grapheme_width(&c.to_string()), 1, "{c:?}");
        }
    }

    #[test]
    fn display_width_pure_ascii_printable() {
        // Fast path: all bytes in 0x20..=0x7E
        assert_eq!(super::display_width("hello world"), 11);
        assert_eq!(super::display_width(""), 0);
    }

    #[test]
    fn display_width_ascii_with_control_chars() {
        // ASCII but has control chars → ascii_display_width path
        assert_eq!(super::display_width("a\tb"), 3);
        assert_eq!(super::display_width("\n"), 1);
    }

    #[test]
    fn display_width_cjk_wide_chars() {
        // CJK ideographs are 2 cells wide
        assert_eq!(super::display_width("\u{4E16}\u{754C}"), 4); // 世界
    }

    #[test]
    fn display_width_mixed_with_zero_width() {
        // Combining accent after 'a' → grapheme cluster "a\u{0300}" still 1 cell
        let s = "a\u{0300}";
        assert_eq!(super::display_width(s), 1);
    }

    #[test]
    fn grapheme_width_ascii() {
        assert_eq!(super::grapheme_width("a"), 1);
        assert_eq!(super::grapheme_width(" "), 1);
    }

    #[test]
    fn grapheme_width_combining_only() {
        // A grapheme that is only combining marks → zero width
        assert_eq!(super::grapheme_width("\u{0300}"), 0);
    }

    #[test]
    fn grapheme_display_width_counts_first_n() {
        let s = "abcdef";
        assert_eq!(super::grapheme_display_width(s, 3), 3);
        assert_eq!(super::grapheme_display_width(s, 0), 0);
        assert_eq!(super::grapheme_display_width(s, 100), 6); // clamps
    }

    #[test]
    fn grapheme_display_width_wide_chars() {
        // Two CJK chars, take 1 grapheme → width 2
        let s = "\u{4E16}\u{754C}";
        assert_eq!(super::grapheme_display_width(s, 1), 2);
        assert_eq!(super::grapheme_display_width(s, 2), 4);
    }

    // -----------------------------------------------------------------------
    // Builder method tests
    // -----------------------------------------------------------------------

    #[test]
    fn style_builder_sets_base_style() {
        let s = Style::default().fg(PackedRgba::rgb(255, 0, 0));
        let form = Form::new(vec![FormField::text("X")]).style(s);
        assert_eq!(form.style, s);
    }

    #[test]
    fn label_style_builder() {
        let s = Style::default().fg(PackedRgba::rgb(0, 0, 255));
        let form = Form::new(vec![FormField::text("X")]).label_style(s);
        assert_eq!(form.label_style, s);
    }

    #[test]
    fn focused_style_builder() {
        let s = Style::default().fg(PackedRgba::rgb(0, 255, 0));
        let form = Form::new(vec![FormField::text("X")]).focused_style(s);
        assert_eq!(form.focused_style, s);
    }

    #[test]
    fn error_style_builder() {
        let s = Style::default().fg(PackedRgba::rgb(255, 0, 0));
        let form = Form::new(vec![FormField::text("X")]).error_style(s);
        assert_eq!(form.error_style, s);
    }

    #[test]
    fn success_style_builder() {
        let s = Style::default().fg(PackedRgba::rgb(0, 255, 0));
        let form = Form::new(vec![FormField::text("X")]).success_style(s);
        assert_eq!(form.success_style, s);
    }

    #[test]
    fn disabled_style_builder() {
        let s = Style::default().fg(PackedRgba::rgb(128, 128, 128));
        let form = Form::new(vec![FormField::text("X")]).disabled_style(s);
        assert_eq!(form.disabled_style, s);
    }

    #[test]
    fn required_style_builder() {
        let s = Style::default().fg(PackedRgba::rgb(255, 255, 0));
        let form = Form::new(vec![FormField::text("X")]).required_style(s);
        assert_eq!(form.required_style, s);
    }

    #[test]
    fn set_style_in_place() {
        let s = Style::default().fg(PackedRgba::rgb(255, 0, 0));
        let mut form = Form::new(vec![FormField::text("X")]);
        form.set_style(s);
        assert_eq!(form.style, s);
    }

    #[test]
    fn set_label_style_in_place() {
        let s = Style::default().fg(PackedRgba::rgb(0, 0, 255));
        let mut form = Form::new(vec![FormField::text("X")]);
        form.set_label_style(s);
        assert_eq!(form.label_style, s);
    }

    #[test]
    fn set_focused_style_in_place() {
        let s = Style::default().fg(PackedRgba::rgb(0, 255, 0));
        let mut form = Form::new(vec![FormField::text("X")]);
        form.set_focused_style(s);
        assert_eq!(form.focused_style, s);
    }

    #[test]
    fn set_error_style_in_place() {
        let s = Style::default().fg(PackedRgba::rgb(255, 0, 0));
        let mut form = Form::new(vec![FormField::text("X")]);
        form.set_error_style(s);
        assert_eq!(form.error_style, s);
    }

    #[test]
    fn set_success_style_in_place() {
        let s = Style::default().fg(PackedRgba::rgb(0, 255, 0));
        let mut form = Form::new(vec![FormField::text("X")]);
        form.set_success_style(s);
        assert_eq!(form.success_style, s);
    }

    #[test]
    fn set_disabled_style_in_place() {
        let s = Style::default().fg(PackedRgba::rgb(128, 128, 128));
        let mut form = Form::new(vec![FormField::text("X")]);
        form.set_disabled_style(s);
        assert_eq!(form.disabled_style, s);
    }

    #[test]
    fn set_required_style_in_place() {
        let s = Style::default().fg(PackedRgba::rgb(255, 255, 0));
        let mut form = Form::new(vec![FormField::text("X")]);
        form.set_required_style(s);
        assert_eq!(form.required_style, s);
    }

    #[test]
    fn label_width_builder() {
        let form = Form::new(vec![FormField::text("X")]).label_width(20);
        assert_eq!(form.label_width, 20);
    }

    #[test]
    fn disabled_builder_chain() {
        let form = Form::new(vec![FormField::text("A"), FormField::text("B")]).disabled(1, true);
        assert!(!form.is_disabled(0));
        assert!(form.is_disabled(1));
    }

    #[test]
    fn required_builder_chain() {
        let form = Form::new(vec![FormField::text("A"), FormField::text("B")]).required(0, true);
        assert!(form.is_required(0));
        assert!(!form.is_required(1));
    }

    #[test]
    fn set_required_out_of_bounds_noop() {
        let mut form = Form::new(vec![FormField::text("A")]);
        form.set_required(99, true); // should not panic
        assert!(!form.is_required(99));
    }

    #[test]
    fn set_disabled_out_of_bounds_noop() {
        let mut form = Form::new(vec![FormField::text("A")]);
        form.set_disabled(99, true); // should not panic
        assert!(!form.is_disabled(99));
    }

    #[test]
    fn validate_all_skips_disabled_field() {
        let form = Form::new(vec![FormField::text("Name")])
            .validate(0, Box::new(|_| Some("required".into())))
            .disabled(0, true);
        assert!(form.validate_all().is_empty());
    }

    #[test]
    fn validate_builder_out_of_bounds_noop() {
        let form =
            Form::new(vec![FormField::text("A")]).validate(99, Box::new(|_| Some("err".into())));
        // No panic, and validate_all only checks existing fields
        assert!(form.validate_all().is_empty());
    }

    #[test]
    fn form_data_select_field() {
        let form = Form::new(vec![FormField::select(
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
    fn form_data_number_field() {
        let form = Form::new(vec![FormField::number("Count", 42)]);
        let data = form.data();
        assert_eq!(data.get("Count"), Some(&FormValue::Number(42)));
    }

    #[test]
    fn form_data_checkbox_field() {
        let form = Form::new(vec![FormField::checkbox("Agree", true)]);
        let data = form.data();
        assert_eq!(data.get("Agree"), Some(&FormValue::Bool(true)));
    }

    #[test]
    fn field_label_accessor() {
        assert_eq!(FormField::text("Name").label(), "Name");
        assert_eq!(FormField::checkbox("Accept", false).label(), "Accept");
        assert_eq!(
            FormField::radio("Size", vec!["S".into(), "M".into()]).label(),
            "Size"
        );
        assert_eq!(
            FormField::select("Color", vec!["R".into()]).label(),
            "Color"
        );
        assert_eq!(FormField::number("Count", 0).label(), "Count");
    }

    #[test]
    fn number_bounded_step_default() {
        if let FormField::Number { step, min, max, .. } = FormField::number_bounded("N", 5, 0, 10) {
            assert_eq!(step, 1);
            assert_eq!(min, Some(0));
            assert_eq!(max, Some(10));
        } else {
            panic!("expected Number variant");
        }
    }

    #[test]
    fn effective_label_width_with_required() {
        let form = Form::new(vec![FormField::text("AB")]).required(0, true);
        // "AB" = 2, + " *" = 2, + ": " = 2 → 6
        assert_eq!(form.effective_label_width(), 6);
    }

    #[test]
    fn form_accessibility_exposes_real_field_semantics_and_states() {
        let form = Form::new(vec![
            FormField::text_with_placeholder("Name", "Enter name"),
            FormField::checkbox("Agree", true),
            FormField::radio("Color", vec!["Red".into(), "Blue".into()]),
            FormField::select("Size", vec!["Small".into(), "Large".into()]),
            FormField::number_bounded("Count", 4, 1, 9),
        ])
        .required(0, true)
        .disabled(1, true)
        .accessibility_id(77);
        let mut state = FormState::default();
        let tree = render_form_a11y(&form, &mut state, Rect::new(2, 3, 50, 8));
        assert_eq!(tree.root_id(), Some(77));
        let root = tree.root().unwrap();
        assert_eq!(root.children.len(), 5);
        let fields: Vec<_> = root
            .children
            .iter()
            .map(|id| tree.node(*id).unwrap())
            .collect();
        assert_eq!(
            fields.iter().map(|node| node.role).collect::<Vec<_>>(),
            vec![
                A11yRole::TextInput,
                A11yRole::Checkbox,
                A11yRole::RadioButton,
                A11yRole::MenuItem,
                A11yRole::Slider,
            ]
        );
        assert!(fields[0].state.focused && fields[0].state.required);
        assert_eq!(
            fields[0].description.as_deref(),
            Some("Placeholder: Enter name")
        );
        assert!(fields[1].state.disabled && !fields[1].state.focused);
        assert_eq!(fields[1].state.checked, Some(true));
        assert_eq!(fields[2].state.value_text.as_deref(), Some("Red"));
        assert_eq!(fields[3].state.value_text.as_deref(), Some("Small"));
        assert_eq!(fields[4].state.value_now, Some(4.0));
        assert_eq!(tree.focused_id(), Some(fields[0].id));
    }

    #[test]
    fn form_keyboard_focus_skips_disabled_fields_and_announces_one_destination() {
        let mut form = Form::new(vec![
            FormField::text("A"),
            FormField::text("Disabled"),
            FormField::text("C"),
        ])
        .disabled(1, true)
        .accessibility_id(88);
        let mut state = FormState::default();
        let before = render_form_a11y(&form, &mut state, Rect::new(0, 0, 30, 3));
        assert!(state.handle_event(&mut form, &press(KeyCode::Tab)));
        assert_eq!(state.focused, 2);
        let after = render_form_a11y(&form, &mut state, Rect::new(0, 0, 30, 3));
        let batch = after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
        assert_eq!(batch.announcements.len(), 1);
        assert_eq!(batch.dropped_count, 0);
        assert_eq!(
            batch.announcements[0].reason,
            AnnouncementReason::FocusChanged
        );
        assert_eq!(
            after.focused().and_then(|node| node.name.as_deref()),
            Some("C")
        );
        assert!(state.handle_event(&mut form, &press(KeyCode::BackTab)));
        assert_eq!(state.focused, 0);
    }

    #[test]
    fn all_disabled_form_has_no_phantom_accessibility_focus() {
        let form = Form::new(vec![FormField::text("A"), FormField::checkbox("B", false)])
            .disabled(0, true)
            .disabled(1, true)
            .accessibility_id(89);
        let mut state = FormState {
            focused: usize::MAX,
            ..Default::default()
        };
        let tree = render_form_a11y(&form, &mut state, Rect::new(0, 0, 30, 2));
        assert!(tree.focused().is_none());
        assert!(!tree.nodes().any(|node| node.state.focused));
    }

    #[test]
    fn form_choice_value_changes_announce_without_live_region_opt_in() {
        for mut form in [
            Form::new(vec![FormField::radio(
                "Color",
                vec!["Red".into(), "Blue".into()],
            )])
            .accessibility_id(90),
            Form::new(vec![FormField::select(
                "Size",
                vec!["Small".into(), "Large".into()],
            )])
            .accessibility_id(91),
        ] {
            let mut state = FormState::default();
            let before = render_form_a11y(&form, &mut state, Rect::new(0, 0, 30, 1));
            let code = if matches!(form.field(0), Some(FormField::Radio { .. })) {
                KeyCode::Down
            } else {
                KeyCode::Right
            };
            assert!(state.handle_event(&mut form, &press(code)));
            let after = render_form_a11y(&form, &mut state, Rect::new(0, 0, 30, 1));
            let batch =
                after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
            assert_eq!(batch.announcements.len(), 1);
            assert_eq!(
                batch.announcements[0].reason,
                AnnouncementReason::FocusedStateChanged
            );
            assert!(batch.announcements[0].text.contains("value "));
        }
    }

    #[test]
    fn validation_announces_only_the_focused_new_error() {
        let mut form = Form::new(vec![FormField::text("Name"), FormField::text("Email")])
            .validate(0, Box::new(|_| Some("Name is required".into())))
            .validate(1, Box::new(|_| Some("Email is required".into())))
            .accessibility_id(92);
        let mut state = FormState::default();
        let before = render_form_a11y(&form, &mut state, Rect::new(0, 0, 40, 4));
        assert!(state.handle_event(&mut form, &press(KeyCode::Enter)));
        let after = render_form_a11y(&form, &mut state, Rect::new(0, 0, 40, 4));
        let batch = after.screen_reader_announcements_since(&before, ScreenReaderPolicy::default());
        assert_eq!(batch.announcements.len(), 1);
        assert_eq!(
            batch.announcements[0].reason,
            AnnouncementReason::LiveRegionChanged
        );
        assert!(batch.announcements[0].text.contains("Name is required"));
        assert!(!batch.announcements[0].text.contains("Email is required"));
        let root = after.root().unwrap();
        assert_eq!(
            after.node(root.children[0]).unwrap().live_region,
            Some(LiveRegion::Polite)
        );
        assert_eq!(after.node(root.children[1]).unwrap().live_region, None);
    }

    #[test]
    fn explicit_form_accessibility_id_survives_geometry_and_validation_layout_changes() {
        let form =
            Form::new(vec![FormField::text("Name"), FormField::text("Email")]).accessibility_id(93);
        let mut state = FormState::default();
        let before = render_form_a11y(&form, &mut state, Rect::new(0, 0, 40, 2));
        let mut before_ids: Vec<_> = before.nodes().map(|node| node.id).collect();
        state.errors.push(ValidationError {
            field: 0,
            message: "Bad name".into(),
        });
        let after = render_form_a11y(&form, &mut state, Rect::new(5, 4, 40, 4));
        let mut after_ids: Vec<_> = after.nodes().map(|node| node.id).collect();
        before_ids.sort_unstable();
        after_ids.sort_unstable();
        assert_eq!(before_ids, after_ids);
        assert_eq!(before.focused_id(), after.focused_id());
    }
}
