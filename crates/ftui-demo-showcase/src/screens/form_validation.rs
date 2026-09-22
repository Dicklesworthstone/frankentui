#![forbid(unsafe_code)]

//! Form Validation Demo — comprehensive showcase of form validation features.
//!
//! Demonstrates:
//! - All validator types: required, email, min/max length, pattern, range
//! - Real-time vs on-submit validation mode toggle
//! - Error summary panel with all current validation errors
//! - Success feedback via toast notifications

use std::cell::{Cell, RefCell};
use std::time::Duration;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_extras::forms::{Form, FormField, FormState, ValidationError};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::grapheme_count;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::notification_queue::{
    NotificationPriority, NotificationQueue, NotificationStack, QueueConfig,
};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::toast::{Toast, ToastIcon, ToastPosition, ToastStyle};
use ftui_widgets::{StatefulWidget, Widget};

use super::{HelpEntry, Screen};
use crate::theme;

/// Validation mode determines when validation runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationMode {
    /// Validate fields in real-time as the user types/changes values.
    RealTime,
    /// Only validate when the user explicitly submits the form.
    OnSubmit,
}

impl ValidationMode {
    fn toggle(self) -> Self {
        match self {
            Self::RealTime => Self::OnSubmit,
            Self::OnSubmit => Self::RealTime,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::RealTime => "Real-time",
            Self::OnSubmit => "On Submit",
        }
    }
}

/// Form Validation demo screen state.
pub struct FormValidationDemo {
    /// The registration form with all field types and validators.
    pub form: Form,
    /// Mutable form state (RefCell for view access).
    pub form_state: RefCell<FormState>,
    /// Current validation mode.
    validation_mode: ValidationMode,
    /// Notification queue for success/error toasts.
    notifications: NotificationQueue,
    /// Status message shown at the bottom.
    status_text: String,
    /// Tick counter for animations.
    tick_count: u64,
    error_injection: bool,
    last_form_area: Cell<Rect>,
    last_error_area: Cell<Rect>,
}

impl Default for FormValidationDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl FormValidationDemo {
    /// Create a new form validation demo.
    pub fn new() -> Self {
        // Build the registration form with comprehensive field types
        let form = Form::new(vec![
            // Required text field
            FormField::text_with_placeholder("Username", "Enter username (required)"),
            // Email validation
            FormField::text_with_placeholder("Email", "user@example.com"),
            // Password with min length
            FormField::text_with_placeholder("Password", "Min 8 characters"),
            // Confirm password (pattern match)
            FormField::text_with_placeholder("Confirm Password", "Re-enter password"),
            // Age with range validation
            FormField::number_bounded("Age", 25, 13, 120),
            // Bio with max length
            FormField::text_with_placeholder("Bio", "Max 100 characters"),
            // Website with URL pattern
            FormField::text_with_placeholder("Website", "https://example.com"),
            // Role selection (required)
            FormField::select(
                "Role",
                vec![
                    "(Select a role)".into(),
                    "Developer".into(),
                    "Designer".into(),
                    "Manager".into(),
                    "Other".into(),
                ],
            ),
            // Terms checkbox (must be checked)
            FormField::checkbox("Accept Terms", false),
        ])
        // Attach validators to each field
        .validate(
            0,
            Box::new(|field| {
                // Required: username must not be empty
                if let FormField::Text { value, .. } = field {
                    if value.trim().is_empty() {
                        return Some("Username is required".into());
                    }
                    // Characters, not UTF-8 bytes: "éé" is two.
                    if grapheme_count(value) < 3 {
                        return Some("Username must be at least 3 characters".into());
                    }
                }
                None
            }),
        )
        .validate(
            1,
            Box::new(|field| {
                // Email validation
                if let FormField::Text { value, .. } = field {
                    if value.trim().is_empty() {
                        return Some("Email is required".into());
                    }
                    if !value.contains('@') || !value.contains('.') {
                        return Some("Please enter a valid email address".into());
                    }
                }
                None
            }),
        )
        .validate(
            2,
            Box::new(|field| {
                // Password min length
                if let FormField::Text { value, .. } = field {
                    if value.is_empty() {
                        return Some("Password is required".into());
                    }
                    if grapheme_count(value) < 8 {
                        return Some("Password must be at least 8 characters".into());
                    }
                }
                None
            }),
        )
        // Note: Confirm password validation is handled specially since it needs
        // access to the password field. We use a simple empty check here.
        .validate(
            3,
            Box::new(|field| {
                if let FormField::Text { value, .. } = field
                    && value.is_empty()
                {
                    return Some("Please confirm your password".into());
                }
                None
            }),
        )
        .validate(
            4,
            Box::new(|field| {
                // Age range validation (additional check beyond bounds)
                if let FormField::Number { value, .. } = field {
                    if *value < 13 {
                        return Some("Must be at least 13 years old".into());
                    }
                    if *value > 120 {
                        return Some("Please enter a valid age".into());
                    }
                }
                None
            }),
        )
        .validate(
            5,
            Box::new(|field| {
                // Bio max length
                if let FormField::Text { value, .. } = field {
                    let count = grapheme_count(value);
                    if count > 100 {
                        return Some(format!(
                            "Bio must be 100 characters or less ({count} entered)"
                        ));
                    }
                }
                None
            }),
        )
        .validate(
            6,
            Box::new(|field| {
                // Website URL pattern (optional but must be valid if provided)
                if let FormField::Text { value, .. } = field
                    && !value.is_empty()
                    && !value.starts_with("http://")
                    && !value.starts_with("https://")
                {
                    return Some("Website must start with http:// or https://".into());
                }
                None
            }),
        )
        .validate(
            7,
            Box::new(|field| {
                // Role selection required (not the placeholder)
                if let FormField::Select { selected, .. } = field
                    && *selected == 0
                {
                    return Some("Please select a role".into());
                }
                None
            }),
        )
        .validate(
            8,
            Box::new(|field| {
                // Terms must be accepted
                if let FormField::Checkbox { checked, .. } = field
                    && !*checked
                {
                    return Some("You must accept the terms".into());
                }
                None
            }),
        );

        let mut form_state = FormState::default();
        form_state.init_tracking(&form);

        let notifications = NotificationQueue::new(
            QueueConfig::new()
                .max_visible(3)
                .max_queued(10)
                .position(ToastPosition::TopRight),
        );

        Self {
            form,
            form_state: RefCell::new(form_state),
            validation_mode: ValidationMode::RealTime,
            notifications,
            // 46 columns: the whole slot in the app at 80x24.
            status_text: "Up/Down: fields | M/E/R/C: outside text fields".into(),
            tick_count: 0,
            error_injection: false,
            last_form_area: Cell::new(Rect::default()),
            last_error_area: Cell::new(Rect::default()),
        }
    }

    /// Validate password confirmation matches password.
    fn validate_password_match(&self) -> Option<ValidationError> {
        let pass_value = if let Some(FormField::Text { value, .. }) = self.form.field(2) {
            value.clone()
        } else {
            return None;
        };

        let confirm = if let Some(FormField::Text { value, .. }) = self.form.field(3) {
            value.clone()
        } else {
            return None;
        };

        if !confirm.is_empty() && pass_value != confirm {
            Some(ValidationError {
                field: 3,
                message: "Passwords do not match".into(),
            })
        } else {
            None
        }
    }

    /// Run validation (either real-time or on-submit based on mode).
    pub fn run_validation(&mut self) {
        let mut errors = self.form.validate_all();

        // Add password match validation
        if let Some(err) = self.validate_password_match() {
            // Replace any existing error for field 3 if passwords don't match
            errors.retain(|e| e.field != 3);
            errors.push(err);
        }

        self.form_state.borrow_mut().errors = errors;
    }

    /// Handle form submission.
    fn handle_submit(&mut self) {
        // Always run full validation on submit
        self.run_validation();

        let state = self.form_state.borrow();
        if state.errors.is_empty() {
            // Success!
            drop(state);

            let toast = Toast::new("Registration successful!")
                .icon(ToastIcon::Success)
                .title("Success")
                .style_variant(ToastStyle::Success)
                .duration(Duration::from_secs(5));
            self.notifications.push(toast, NotificationPriority::Normal);

            self.status_text = "Form submitted successfully!".into();
        } else {
            // Show error count
            let error_count = state.errors.len();
            drop(state);

            let toast = Toast::new(format!("{} validation error(s) found", error_count))
                .icon(ToastIcon::Error)
                .title("Validation Failed")
                .style_variant(ToastStyle::Error)
                .duration(Duration::from_secs(4));
            self.notifications.push(toast, NotificationPriority::High);

            self.status_text = format!("Please fix {} error(s) before submitting", error_count);
        }
    }

    /// Toggle between real-time and on-submit validation modes.
    fn toggle_validation_mode(&mut self) {
        self.validation_mode = self.validation_mode.toggle();

        // Clear errors when switching to on-submit mode
        if self.validation_mode == ValidationMode::OnSubmit {
            self.form_state.borrow_mut().errors.clear();
        } else {
            // Run validation immediately when switching to real-time mode
            self.run_validation();
        }

        let toast = Toast::new(format!("Validation mode: {}", self.validation_mode.label()))
            .icon(ToastIcon::Info)
            .style_variant(ToastStyle::Info)
            .duration(Duration::from_secs(2));
        self.notifications.push(toast, NotificationPriority::Normal);
    }

    /// Render the error summary panel.
    fn render_error_summary(&self, frame: &mut Frame, area: Rect) {
        let state = self.form_state.borrow();
        let errors = &state.errors;

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Error Summary")
            .title_alignment(Alignment::Center)
            .style(if errors.is_empty() {
                Style::new().fg(theme::accent::SUCCESS).bg(theme::bg::DEEP)
            } else {
                Style::new().fg(theme::accent::ERROR).bg(theme::bg::DEEP)
            });

        let inner = block.inner(area);
        block.render(area, frame);

        if errors.is_empty() {
            let text = if self.validation_mode == ValidationMode::OnSubmit {
                "Errors will appear here after submit"
            } else {
                "No validation errors"
            };
            Paragraph::new(text)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(inner, frame);
        } else {
            // Build error list
            let mut lines: Vec<String> = Vec::new();
            for error in errors.iter() {
                let field_name = self
                    .form
                    .field(error.field)
                    .map(|f| f.label())
                    .unwrap_or("Unknown");
                lines.push(format!("• {}: {}", field_name, error.message));
            }

            let text = lines.join("\n");
            Paragraph::new(text)
                .style(Style::new().fg(theme::accent::ERROR))
                .render(inner, frame);
        }
    }

    /// Render validation mode indicator.
    fn render_mode_indicator(&self, frame: &mut Frame, area: Rect) {
        let mode_style = match self.validation_mode {
            ValidationMode::RealTime => Style::new().fg(theme::accent::SUCCESS),
            ValidationMode::OnSubmit => Style::new().fg(theme::accent::INFO),
        };

        let indicator = format!("Mode: {} [M to toggle]", self.validation_mode.label());

        Paragraph::new(indicator)
            .style(mode_style)
            .render(area, frame);
    }

    fn inject_errors(&mut self) {
        self.error_injection = true;
        if let Some(FormField::Text { value, .. }) = self.form.field_mut(0) {
            *value = "ab".into();
        }
        if let Some(FormField::Text { value, .. }) = self.form.field_mut(1) {
            *value = "not-an-email".into();
        }
        if let Some(FormField::Text { value, .. }) = self.form.field_mut(2) {
            *value = "short".into();
        }
        if let Some(FormField::Text { value, .. }) = self.form.field_mut(3) {
            *value = "different".into();
        }
        if let Some(FormField::Text { value, .. }) = self.form.field_mut(5) {
            *value = "x".repeat(110);
        }
        if let Some(FormField::Text { value, .. }) = self.form.field_mut(6) {
            *value = "not-a-url".into();
        }
        if let Some(FormField::Select { selected, .. }) = self.form.field_mut(7) {
            *selected = 0;
        }
        if let Some(FormField::Checkbox { checked, .. }) = self.form.field_mut(8) {
            *checked = false;
        }
        self.run_validation();
        self.status_text = "Error injection active".into();
    }
    fn reset_form(&mut self) {
        *self = Self::new();
    }

    /// Up/Down on Age or Role as the form's Shift+Tab/Tab. The form spends
    /// Up/Down there on the value and relies on Tab to leave, but the app
    /// keeps Tab for switching screens, so keyboard focus stuck on those two
    /// and never reached Bio or Website. Left/Right still change the value.
    fn value_field_arrows_move_focus(&self, event: &Event) -> Option<Event> {
        let Event::Key(key) = event else {
            return None;
        };
        let code = match key.code {
            KeyCode::Up => KeyCode::BackTab,
            KeyCode::Down => KeyCode::Tab,
            _ => return None,
        };
        let focused = self.form_state.borrow().focused;
        matches!(
            self.form.field(focused),
            Some(FormField::Number { .. } | FormField::Select { .. })
        )
        .then_some(Event::Key(KeyEvent { code, ..*key }))
    }

    /// Whether the focused field takes typed characters. While it does, plain
    /// letters are text: the M/E/R/C controls and the app's single-key
    /// shortcuts stand aside, or typing "user" would inject errors and reset.
    fn text_field_focused(&self) -> bool {
        let state = self.form_state.borrow();
        !state.submitted
            && !state.cancelled
            && !self.form.is_disabled(state.focused)
            && matches!(self.form.field(state.focused), Some(FormField::Text { .. }))
    }
    fn handle_mouse(&mut self, event: &Event) {
        if let Event::Mouse(mouse) = event {
            let form_area = self.last_form_area.get();
            let error_area = self.last_error_area.get();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if error_area.contains(mouse.x, mouse.y) =>
                {
                    self.toggle_validation_mode();
                }
                // Leaving a field marks it touched, as Up/Down do, so its
                // error shows.
                MouseEventKind::ScrollDown if form_area.contains(mouse.x, mouse.y) => {
                    let mut state = self.form_state.borrow_mut();
                    let count = self.form.field_count();
                    if count > 0 {
                        let left = state.focused;
                        state.mark_touched(left);
                        state.focused = (left + 1) % count;
                    }
                }
                MouseEventKind::ScrollUp if form_area.contains(mouse.x, mouse.y) => {
                    let mut state = self.form_state.borrow_mut();
                    let count = self.form.field_count();
                    if count > 0 {
                        let left = state.focused;
                        state.mark_touched(left);
                        state.focused = (left + count - 1) % count;
                    }
                }
                _ => {}
            }
        }
    }
    /// Render dirty/touched state indicators.
    fn render_state_indicators(&self, frame: &mut Frame, area: Rect) {
        let state = self.form_state.borrow();

        let touched_count = state.touched_fields().len();
        let dirty_count = state.dirty_fields().len();
        let total_fields = self.form.field_count();

        let text = format!(
            "Touched: {}/{} | Dirty: {}/{}",
            touched_count, total_fields, dirty_count, total_fields
        );

        Paragraph::new(text)
            .style(Style::new().fg(theme::fg::MUTED))
            .render(area, frame);
    }
}

impl Screen for FormValidationDemo {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if matches!(event, Event::Mouse(_)) {
            self.handle_mouse(event);
            return Cmd::None;
        }
        let typing = self.text_field_focused();
        // Handle M key with either NONE or SHIFT modifiers for mode toggle
        if !typing
            && let Event::Key(KeyEvent {
                code: KeyCode::Char('m' | 'M'),
                kind: KeyEventKind::Press,
                modifiers,
                ..
            }) = event
            && matches!(*modifiers, Modifiers::NONE | Modifiers::SHIFT)
        {
            self.toggle_validation_mode();
            return Cmd::None;
        }
        if !typing
            && let Event::Key(KeyEvent {
                code,
                kind: KeyEventKind::Press,
                modifiers: Modifiers::NONE,
                ..
            }) = event
        {
            match code {
                KeyCode::Char('m' | 'M') => {
                    // Handled above with modifier check
                    return Cmd::None;
                }
                KeyCode::Char('e' | 'E') => {
                    self.inject_errors();
                    return Cmd::None;
                }
                KeyCode::Char('r' | 'R') => {
                    self.reset_form();
                    return Cmd::None;
                }
                KeyCode::Char('c' | 'C') => {
                    self.form_state.borrow_mut().errors.clear();
                    self.error_injection = false;
                    self.status_text = "Errors cleared".into();
                    return Cmd::None;
                }
                _ => {}
            }
        }

        // Handle form events
        let remapped = self.value_field_arrows_move_focus(event);
        let changed = {
            let mut state = self.form_state.borrow_mut();
            state.handle_event(&mut self.form, remapped.as_ref().unwrap_or(event))
        };

        // Check if form was submitted
        {
            let state = self.form_state.borrow();
            if state.submitted {
                drop(state);
                self.handle_submit();
                // Reset submitted flag
                self.form_state.borrow_mut().submitted = false;
            }
        }

        // Esc cancels the form, which then ignores every key until a reset.
        // Say so, or the form just looks frozen.
        if changed && self.form_state.borrow().cancelled {
            self.status_text = "Form cancelled | R: reset".into();
        }

        // Run real-time validation if enabled and something changed
        if changed && self.validation_mode == ValidationMode::RealTime {
            self.run_validation();
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        // Main layout: form (left) | error summary (right)
        let main_chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(area);

        // Left side: form + indicators
        let left_chunks = Flex::vertical()
            .constraints([
                Constraint::Fixed(1), // Mode indicator
                Constraint::Min(10),  // Form
                Constraint::Fixed(1), // State indicators
                Constraint::Fixed(1), // Status text
            ])
            .split(main_chunks[0]);

        // Mode indicator
        self.render_mode_indicator(frame, left_chunks[0]);

        // Form block
        let form_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Registration Form")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::DEEP));

        let form_inner = form_block.inner(left_chunks[1]);
        self.last_form_area.set(left_chunks[1]);
        form_block.render(left_chunks[1], frame);

        // Render form
        let mut state = self.form_state.borrow_mut();
        StatefulWidget::render(&self.form, form_inner, frame, &mut state);
        drop(state);

        // State indicators
        self.render_state_indicators(frame, left_chunks[2]);

        // Status text
        Paragraph::new(self.status_text.as_str())
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(left_chunks[3], frame);

        // Right side: error summary
        self.last_error_area.set(main_chunks[1]);
        self.render_error_summary(frame, main_chunks[1]);

        // Notification overlay
        NotificationStack::new(&self.notifications).render(area, frame);
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        self.notifications.tick(Duration::from_millis(100));
    }

    fn title(&self) -> &'static str {
        "Form Validation"
    }

    fn tab_label(&self) -> &'static str {
        "Validate"
    }

    fn consumes_text_input(&self) -> bool {
        self.text_field_focused()
    }

    // Tab never reaches the form: the app takes it to switch screens.
    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Up/Down",
                action: "Navigate fields",
            },
            HelpEntry {
                key: "Left/Right",
                action: "Change age / role",
            },
            HelpEntry {
                key: "Space",
                action: "Toggle checkbox",
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
                key: "M",
                action: "Toggle validation mode (off text fields)",
            },
            HelpEntry {
                key: "E",
                action: "Inject errors (off text fields)",
            },
            HelpEntry {
                key: "R",
                action: "Reset form (off text fields)",
            },
            HelpEntry {
                key: "C",
                action: "Clear errors (off text fields)",
            },
            HelpEntry {
                key: "Click",
                action: "Toggle mode (error panel)",
            },
            HelpEntry {
                key: "Scroll",
                action: "Navigate fields",
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn new_creates_valid_demo() {
        let demo = FormValidationDemo::new();
        assert_eq!(demo.form.field_count(), 9);
        assert_eq!(demo.validation_mode, ValidationMode::RealTime);
    }

    #[test]
    fn validation_mode_toggle() {
        let mut demo = FormValidationDemo::new();
        assert_eq!(demo.validation_mode, ValidationMode::RealTime);

        demo.toggle_validation_mode();
        assert_eq!(demo.validation_mode, ValidationMode::OnSubmit);

        demo.toggle_validation_mode();
        assert_eq!(demo.validation_mode, ValidationMode::RealTime);
    }

    #[test]
    fn initial_validation_has_errors() {
        let mut demo = FormValidationDemo::new();
        demo.run_validation();

        let state = demo.form_state.borrow();
        // Empty form should have multiple validation errors
        assert!(
            !state.errors.is_empty(),
            "Empty form should have validation errors"
        );
    }

    #[test]
    fn renders_without_panic() {
        let demo = FormValidationDemo::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        let area = Rect::new(0, 0, 120, 38);

        demo.view(&mut frame, area);
    }

    #[test]
    fn renders_at_small_size() {
        let demo = FormValidationDemo::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 15, &mut pool);
        let area = Rect::new(0, 0, 40, 15);

        demo.view(&mut frame, area);
    }

    #[test]
    fn password_match_validation() {
        let mut demo = FormValidationDemo::new();

        // Set password
        if let Some(FormField::Text { value, .. }) = demo.form.field_mut(2) {
            *value = "password123".into();
        }
        // Set different confirm password
        if let Some(FormField::Text { value, .. }) = demo.form.field_mut(3) {
            *value = "different".into();
        }

        let error = demo.validate_password_match();
        assert!(error.is_some());
        assert_eq!(error.unwrap().message, "Passwords do not match");
    }

    #[test]
    fn password_match_validation_success() {
        let mut demo = FormValidationDemo::new();

        // Set matching passwords
        if let Some(FormField::Text { value, .. }) = demo.form.field_mut(2) {
            *value = "password123".into();
        }
        if let Some(FormField::Text { value, .. }) = demo.form.field_mut(3) {
            *value = "password123".into();
        }

        let error = demo.validate_password_match();
        assert!(error.is_none());
    }
    #[test]
    fn error_injection_fills_bad_data() {
        let mut demo = FormValidationDemo::new();
        demo.inject_errors();
        assert!(demo.error_injection);
        assert!(demo.form_state.borrow().errors.len() >= 5);
    }
    #[test]
    fn reset_restores_initial_state() {
        let mut demo = FormValidationDemo::new();
        demo.inject_errors();
        demo.reset_form();
        assert!(!demo.error_injection);
        assert_eq!(demo.validation_mode, ValidationMode::RealTime);
    }
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers: Modifiers::NONE,
        })
    }

    /// Accept Terms, a checkbox: plain letters there are demo controls.
    const TERMS: usize = 8;

    #[test]
    fn clear_errors_removes_all() {
        let mut demo = FormValidationDemo::new();
        demo.inject_errors();
        demo.form_state.borrow_mut().focused = TERMS;
        demo.update(&key(KeyCode::Char('c')));
        assert!(demo.form_state.borrow().errors.is_empty());
    }
    #[test]
    fn e_key_injects_errors() {
        let mut demo = FormValidationDemo::new();
        demo.form_state.borrow_mut().focused = TERMS;
        demo.update(&key(KeyCode::Char('e')));
        assert!(demo.error_injection);
    }
    #[test]
    fn r_key_resets_form() {
        let mut demo = FormValidationDemo::new();
        demo.inject_errors();
        demo.form_state.borrow_mut().focused = TERMS;
        demo.update(&key(KeyCode::Char('r')));
        assert!(!demo.error_injection);
    }
    #[test]
    fn length_limits_count_characters_not_bytes() {
        let mut demo = FormValidationDemo::new();
        let set = |demo: &mut FormValidationDemo, idx, text: &str| {
            if let Some(FormField::Text { value, .. }) = demo.form.field_mut(idx) {
                *value = text.into();
            }
        };
        let has_error = |demo: &mut FormValidationDemo, idx| {
            demo.run_validation();
            demo.form_state
                .borrow()
                .errors
                .iter()
                .any(|e| e.field == idx)
        };
        // Two characters, four bytes.
        set(&mut demo, 0, "éé");
        assert!(has_error(&mut demo, 0));
        set(&mut demo, 0, "ééé");
        assert!(!has_error(&mut demo, 0));
        // Five characters, ten bytes.
        set(&mut demo, 2, "ééééé");
        assert!(has_error(&mut demo, 2));
        // Sixty characters, 240 bytes.
        set(&mut demo, 5, &"🦀".repeat(60));
        assert!(!has_error(&mut demo, 5));
    }
    #[test]
    fn letters_typed_into_a_text_field_are_text() {
        let mut demo = FormValidationDemo::new();
        demo.update(&key(KeyCode::Down));
        assert!(demo.consumes_text_input());
        // Every one of M/E/R/C appears in it.
        for ch in "user@example.com".chars() {
            demo.update(&key(KeyCode::Char(ch)));
        }
        let Some(FormField::Text { value, .. }) = demo.form.field(1) else {
            panic!("field 1 is the email text field");
        };
        assert_eq!(value, "user@example.com");
        assert!(!demo.error_injection);
        assert_eq!(demo.validation_mode, ValidationMode::RealTime);
        assert!(!demo.form_state.borrow().errors.iter().any(|e| e.field == 1));

        demo.form_state.borrow_mut().focused = TERMS;
        assert!(!demo.consumes_text_input());
    }
    #[test]
    fn down_reaches_every_field_without_changing_values() {
        // Tab never arrives from the app, so Down has to get past Age and Role.
        let mut demo = FormValidationDemo::new();
        for expected in 1..demo.form.field_count() {
            demo.update(&key(KeyCode::Down));
            assert_eq!(demo.form_state.borrow().focused, expected);
        }
        for _ in 0..demo.form.field_count() - 1 {
            demo.update(&key(KeyCode::Up));
        }
        assert_eq!(demo.form_state.borrow().focused, 0);
        assert!(matches!(
            demo.form.field(4),
            Some(FormField::Number { value: 25, .. })
        ));
        assert!(matches!(
            demo.form.field(7),
            Some(FormField::Select { selected: 0, .. })
        ));

        // Left/Right still change them.
        demo.form_state.borrow_mut().focused = 4;
        demo.update(&key(KeyCode::Right));
        assert!(matches!(
            demo.form.field(4),
            Some(FormField::Number { value: 26, .. })
        ));
    }
    #[test]
    fn escape_cancel_is_reported_and_frees_the_letter_keys() {
        let mut demo = FormValidationDemo::new();
        demo.update(&key(KeyCode::Escape));
        assert!(demo.form_state.borrow().cancelled);
        assert!(demo.status_text.contains("cancelled"));
        // The cancelled form takes no text, so R is a control again.
        assert!(!demo.consumes_text_input());
        demo.update(&key(KeyCode::Char('r')));
        assert!(!demo.form_state.borrow().cancelled);
    }
    #[test]
    fn wheel_focus_change_marks_the_left_field_touched() {
        use ftui_core::event::{MouseEvent, MouseEventKind};
        let mut demo = FormValidationDemo::new();
        demo.last_form_area.set(Rect::new(0, 0, 60, 30));
        demo.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            10,
            10,
        )));
        assert!(demo.form_state.borrow().is_touched(0));
    }
    #[test]
    fn mouse_scroll_navigates_fields() {
        use ftui_core::event::{MouseEvent, MouseEventKind};
        let mut demo = FormValidationDemo::new();
        demo.last_form_area.set(Rect::new(0, 0, 60, 30));
        let initial = demo.form_state.borrow().focused;
        demo.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            10,
            10,
        )));
        assert_ne!(initial, demo.form_state.borrow().focused);
    }
    #[test]
    fn mouse_click_toggles_mode() {
        use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut demo = FormValidationDemo::new();
        demo.last_error_area.set(Rect::new(60, 0, 40, 30));
        demo.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            70,
            10,
        )));
        assert_eq!(demo.validation_mode, ValidationMode::OnSubmit);
    }
    #[test]
    fn keybindings_has_mouse_entries() {
        let demo = FormValidationDemo::new();
        let bindings = demo.keybindings();
        assert!(bindings.len() >= 8);
        let keys: Vec<&str> = bindings.iter().map(|b| b.key).collect();
        assert!(keys.contains(&"Click"));
        assert!(keys.contains(&"Scroll"));
    }
}
