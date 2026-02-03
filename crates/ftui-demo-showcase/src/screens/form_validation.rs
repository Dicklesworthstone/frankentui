#![forbid(unsafe_code)]

//! Form Validation Demo — comprehensive showcase of form validation features.
//!
//! Demonstrates:
//! - All validator types: required, email, min/max length, pattern, range
//! - Real-time vs on-submit validation mode toggle
//! - Error summary panel with all current validation errors
//! - Success feedback via toast notifications

use std::cell::RefCell;
use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_extras::forms::{Form, FormField, FormState, ValidationError};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
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
    form: Form,
    /// Mutable form state (RefCell for view access).
    form_state: RefCell<FormState>,
    /// Current validation mode.
    validation_mode: ValidationMode,
    /// Notification queue for success/error toasts.
    notifications: NotificationQueue,
    /// Status message shown at the bottom.
    status_text: String,
    /// Tick counter for animations.
    tick_count: u64,
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
                    if value.len() < 3 {
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
                    if value.len() < 8 {
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
                if let FormField::Text { value, .. } = field
                    && value.len() > 100
                {
                    return Some(format!(
                        "Bio must be 100 characters or less ({} entered)",
                        value.len()
                    ));
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
            status_text: "Tab/Arrow: navigate | Space: toggle | Enter: submit | M: mode toggle"
                .into(),
            tick_count: 0,
        }
    }

    /// Validate password confirmation matches password.
    fn validate_password_match(&self) -> Option<ValidationError> {
        let password = if let Some(FormField::Text { value, .. }) = self.form.field(2) {
            value.clone()
        } else {
            return None;
        };

        let confirm = if let Some(FormField::Text { value, .. }) = self.form.field(3) {
            value.clone()
        } else {
            return None;
        };

        if !confirm.is_empty() && password != confirm {
            Some(ValidationError {
                field: 3,
                message: "Passwords do not match".into(),
            })
        } else {
            None
        }
    }

    /// Run validation (either real-time or on-submit based on mode).
    fn run_validation(&mut self) {
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
        // Check for mode toggle key
        if let Event::Key(KeyEvent {
            code: KeyCode::Char('m' | 'M'),
            kind: KeyEventKind::Press,
            modifiers: Modifiers::NONE,
            ..
        }) = event
        {
            self.toggle_validation_mode();
            return Cmd::None;
        }

        // Handle form events
        let changed = {
            let mut state = self.form_state.borrow_mut();
            state.handle_event(&mut self.form, event)
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

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Tab/S-Tab",
                action: "Navigate fields",
            },
            HelpEntry {
                key: "Up/Down",
                action: "Change value / navigate",
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
                key: "M",
                action: "Toggle validation mode",
            },
            HelpEntry {
                key: "Esc",
                action: "Cancel / reset",
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
}
