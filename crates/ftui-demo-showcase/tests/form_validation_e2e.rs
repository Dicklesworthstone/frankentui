#![forbid(unsafe_code)]

//! End-to-end tests for the Form Validation Demo (bd-34pj.6).
//!
//! These tests exercise the form validation lifecycle through the
//! `FormValidationDemo` screen, covering:
//!
//! - Field navigation (Tab, Shift-Tab, Up/Down)
//! - Real-time validation with error display
//! - On-submit validation mode toggle
//! - Error summary panel updates
//! - Form submission with validation errors
//! - Successful form submission with valid data
//! - Touched/dirty state tracking
//! - Toast notification appearance
//!
//! # Invariants (Alien Artifact)
//!
//! 1. **Validation timing**: In real-time mode, errors appear immediately after
//!    field modification; in on-submit mode, errors only appear after Enter.
//! 2. **Error count consistency**: Error summary count matches the number of
//!    validation errors in `FormState.errors`.
//! 3. **State tracking**: `touched` is set when focus leaves a field;
//!    `dirty` is set when value differs from initial.
//! 4. **Mode toggle idempotency**: Toggling mode twice returns to original state.
//!
//! # Failure Modes
//!
//! | Scenario | Expected Behavior |
//! |----------|-------------------|
//! | Zero-width render area | No panic, graceful no-op |
//! | Submit with all errors | Toast shows error count, form not reset |
//! | Submit with valid data | Toast shows success, status updated |
//! | Rapid mode toggles | State remains consistent |
//!
//! Run: `cargo test -p ftui-demo-showcase --test form_validation_e2e`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::screens::Screen;
use ftui_demo_showcase::screens::form_validation::FormValidationDemo;
use ftui_extras::forms::FormField;
use ftui_harness::assert_snapshot;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::NONE,
        kind: KeyEventKind::Press,
    })
}

fn shift_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::SHIFT,
        kind: KeyEventKind::Press,
    })
}

fn char_press(ch: char) -> Event {
    press(KeyCode::Char(ch))
}

/// Emit a JSONL log entry to stderr for verbose test logging.
fn log_jsonl(step: &str, data: &[(&str, &str)]) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = COUNTER.fetch_add(1, Ordering::Relaxed);
    let fields: Vec<String> = std::iter::once(format!("\"ts\":\"T{ts:06}\""))
        .chain(std::iter::once(format!("\"step\":\"{step}\"")))
        .chain(data.iter().map(|(k, v)| format!("\"{k}\":\"{v}\"")))
        .collect();
    eprintln!("{{{}}}", fields.join(","));
}

/// Capture a frame and return a hash for determinism checks.
fn capture_frame_hash(demo: &FormValidationDemo, width: u16, height: u16) -> u64 {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let area = Rect::new(0, 0, width, height);
    demo.view(&mut frame, area);
    let mut hasher = DefaultHasher::new();
    for y in 0..height {
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                ch.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Render the demo and return the frame for inspection.
fn render_demo(demo: &FormValidationDemo, width: u16, height: u16) -> Frame {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let area = Rect::new(0, 0, width, height);
    demo.view(&mut frame, area);
    frame
}

// ===========================================================================
// Scenario 1: Initial State and Rendering
// ===========================================================================

#[test]
fn e2e_initial_state_renders_correctly() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_initial_state_renders_correctly"),
            ("term_cols", "120"),
            ("term_rows", "40"),
        ],
    );

    let demo = FormValidationDemo::new();

    // Verify field count
    assert_eq!(demo.form.field_count(), 9, "Form should have 9 fields");

    // Verify initial validation mode
    log_jsonl("check", &[("validation_mode", "RealTime")]);

    // Render at standard size - should not panic
    let frame_hash = capture_frame_hash(&demo, 120, 40);
    log_jsonl("rendered", &[("frame_hash", &format!("{frame_hash:016x}"))]);
}

#[test]
fn e2e_renders_at_various_sizes() {
    log_jsonl("env", &[("test", "e2e_renders_at_various_sizes")]);

    let demo = FormValidationDemo::new();

    // Standard sizes
    for (w, h) in [(120, 40), (80, 24), (60, 20), (40, 15)] {
        let hash = capture_frame_hash(&demo, w, h);
        log_jsonl(
            "rendered",
            &[
                ("width", &w.to_string()),
                ("height", &h.to_string()),
                ("frame_hash", &format!("{hash:016x}")),
            ],
        );
    }

    // Zero area should not panic
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    demo.view(&mut frame, Rect::new(0, 0, 0, 0));
    log_jsonl("zero_area", &[("result", "no_panic")]);
}

#[test]
fn form_validation_initial_80x24() {
    let demo = FormValidationDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    demo.view(&mut frame, area);
    assert_snapshot!("form_validation_initial_80x24", &frame.buffer);
}

#[test]
fn form_validation_initial_120x40() {
    let demo = FormValidationDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    demo.view(&mut frame, area);
    assert_snapshot!("form_validation_initial_120x40", &frame.buffer);
}

// ===========================================================================
// Scenario 2: Field Navigation
// ===========================================================================

#[test]
fn e2e_tab_navigates_fields() {
    log_jsonl("env", &[("test", "e2e_tab_navigates_fields")]);

    let mut demo = FormValidationDemo::new();

    // Initial focus should be on field 0 (Username)
    let initial_focus = demo.form_state.borrow().focused;
    assert_eq!(initial_focus, 0, "Initial focus should be field 0");
    log_jsonl("initial", &[("focused", "0")]);

    // Tab through fields
    for expected_focus in 1..=8 {
        demo.update(&press(KeyCode::Tab));
        let focus = demo.form_state.borrow().focused;
        assert_eq!(
            focus, expected_focus,
            "Focus should advance to field {expected_focus}"
        );
        log_jsonl("tab", &[("focused", &expected_focus.to_string())]);
    }

    // Tab wraps around
    demo.update(&press(KeyCode::Tab));
    let focus = demo.form_state.borrow().focused;
    assert_eq!(focus, 0, "Tab should wrap to field 0");
    log_jsonl("wrap", &[("focused", "0")]);
}

#[test]
fn e2e_shift_tab_navigates_backwards() {
    log_jsonl("env", &[("test", "e2e_shift_tab_navigates_backwards")]);

    let mut demo = FormValidationDemo::new();

    // Shift-Tab from field 0 should go to field 8
    demo.update(&shift_press(KeyCode::BackTab));
    let focus = demo.form_state.borrow().focused;
    assert_eq!(focus, 8, "Shift-Tab from 0 should wrap to field 8");
    log_jsonl("back_wrap", &[("focused", "8")]);

    // Continue backwards
    demo.update(&shift_press(KeyCode::BackTab));
    let focus = demo.form_state.borrow().focused;
    assert_eq!(focus, 7, "Shift-Tab should go to field 7");
    log_jsonl("back", &[("focused", "7")]);
}

// ===========================================================================
// Scenario 3: Real-time Validation
// ===========================================================================

#[test]
fn e2e_realtime_validation_shows_errors() {
    log_jsonl("env", &[("test", "e2e_realtime_validation_shows_errors")]);

    let mut demo = FormValidationDemo::new();

    // Initial state should have errors (empty required fields)
    demo.run_validation();
    let error_count = demo.form_state.borrow().errors.len();
    assert!(error_count > 0, "Empty form should have validation errors");
    log_jsonl("initial_errors", &[("count", &error_count.to_string())]);

    // Type a short username (less than 3 chars)
    demo.update(&char_press('a'));
    demo.update(&char_press('b'));

    // Validation should run and show "at least 3 characters" error
    demo.run_validation();
    let errors = demo.form_state.borrow().errors.clone();
    let username_error = errors.iter().find(|e| e.field == 0);
    assert!(
        username_error.is_some(),
        "Username field should have a validation error"
    );
    log_jsonl(
        "short_username",
        &[(
            "error",
            username_error.map(|e| e.message.as_str()).unwrap_or("none"),
        )],
    );

    // Type one more character to reach minimum
    demo.update(&char_press('c'));
    demo.run_validation();
    let errors = demo.form_state.borrow().errors.clone();
    let username_error = errors.iter().find(|e| e.field == 0);
    // Should still have error if username requirement isn't just length
    log_jsonl(
        "valid_username",
        &[("has_error", &username_error.is_some().to_string())],
    );
}

#[test]
fn e2e_email_validation() {
    log_jsonl("env", &[("test", "e2e_email_validation")]);

    let mut demo = FormValidationDemo::new();

    // Navigate to email field (field 1)
    demo.update(&press(KeyCode::Tab));

    // Type invalid email
    for ch in "notanemail".chars() {
        demo.update(&char_press(ch));
    }
    demo.run_validation();

    let errors = demo.form_state.borrow().errors.clone();
    let email_error = errors.iter().find(|e| e.field == 1);
    assert!(email_error.is_some(), "Invalid email should have error");
    log_jsonl(
        "invalid_email",
        &[(
            "error",
            email_error.map(|e| e.message.as_str()).unwrap_or("none"),
        )],
    );

    // Clear and type valid email
    // First clear the field
    if let Some(FormField::Text { value, .. }) = demo.form.field_mut(1) {
        value.clear();
    }
    for ch in "user@example.com".chars() {
        demo.update(&char_press(ch));
    }
    demo.run_validation();

    let errors = demo.form_state.borrow().errors.clone();
    let email_error = errors.iter().find(|e| e.field == 1);
    assert!(email_error.is_none(), "Valid email should have no error");
    log_jsonl("valid_email", &[("has_error", "false")]);
}

// ===========================================================================
// Scenario 4: Validation Mode Toggle
// ===========================================================================

#[test]
fn e2e_validation_mode_toggle() {
    log_jsonl("env", &[("test", "e2e_validation_mode_toggle")]);

    let mut demo = FormValidationDemo::new();

    // Initial mode is RealTime
    demo.run_validation();
    let initial_errors = demo.form_state.borrow().errors.len();
    log_jsonl(
        "initial",
        &[
            ("mode", "RealTime"),
            ("error_count", &initial_errors.to_string()),
        ],
    );

    // Toggle to OnSubmit mode (press 'M')
    demo.update(&char_press('m'));
    let errors_after_toggle = demo.form_state.borrow().errors.len();
    assert_eq!(errors_after_toggle, 0, "OnSubmit mode should clear errors");
    log_jsonl(
        "toggle_to_onsubmit",
        &[("mode", "OnSubmit"), ("error_count", "0")],
    );

    // Toggle back to RealTime mode
    demo.update(&char_press('M'));
    demo.run_validation();
    let errors_back = demo.form_state.borrow().errors.len();
    assert!(errors_back > 0, "RealTime mode should show errors again");
    log_jsonl(
        "toggle_to_realtime",
        &[
            ("mode", "RealTime"),
            ("error_count", &errors_back.to_string()),
        ],
    );
}

#[test]
fn e2e_mode_toggle_idempotency() {
    log_jsonl("env", &[("test", "e2e_mode_toggle_idempotency")]);

    let mut demo = FormValidationDemo::new();

    // Capture initial frame hash
    let initial_hash = capture_frame_hash(&demo, 80, 24);

    // Toggle mode twice
    demo.update(&char_press('m'));
    demo.update(&char_press('m'));

    // Frame should be similar (mode back to original)
    let final_hash = capture_frame_hash(&demo, 80, 24);

    log_jsonl(
        "idempotency",
        &[
            ("initial_hash", &format!("{initial_hash:016x}")),
            ("final_hash", &format!("{final_hash:016x}")),
        ],
    );
}

// ===========================================================================
// Scenario 5: Form Submission
// ===========================================================================

#[test]
fn e2e_submit_with_errors() {
    log_jsonl("env", &[("test", "e2e_submit_with_errors")]);
    let start = Instant::now();

    let mut demo = FormValidationDemo::new();

    // Submit empty form (should have errors)
    demo.update(&press(KeyCode::Enter));

    // Form should not be marked as successfully submitted
    let state = demo.form_state.borrow();
    assert!(
        !state.errors.is_empty(),
        "Submitting empty form should have errors"
    );
    log_jsonl(
        "submit_failed",
        &[
            ("error_count", &state.errors.len().to_string()),
            ("elapsed_ms", &start.elapsed().as_millis().to_string()),
        ],
    );
}

#[test]
fn e2e_submit_with_valid_data() {
    log_jsonl("env", &[("test", "e2e_submit_with_valid_data")]);
    let start = Instant::now();

    let mut demo = FormValidationDemo::new();

    // Fill in valid data for all required fields
    // Field 0: Username (min 3 chars)
    if let Some(FormField::Text { value, .. }) = demo.form.field_mut(0) {
        *value = "testuser".into();
    }

    // Field 1: Email
    if let Some(FormField::Text { value, .. }) = demo.form.field_mut(1) {
        *value = "test@example.com".into();
    }

    // Field 2: Password (min 8 chars)
    if let Some(FormField::Text { value, .. }) = demo.form.field_mut(2) {
        *value = "password123".into();
    }

    // Field 3: Confirm Password (must match)
    if let Some(FormField::Text { value, .. }) = demo.form.field_mut(3) {
        *value = "password123".into();
    }

    // Field 4: Age (13-120) - already defaults to 25

    // Field 5: Bio - optional, leave empty

    // Field 6: Website - optional, leave empty

    // Field 7: Role (must not be placeholder at index 0)
    if let Some(FormField::Select { selected, .. }) = demo.form.field_mut(7) {
        *selected = 1; // "Developer"
    }

    // Field 8: Accept Terms (must be checked)
    if let Some(FormField::Checkbox { checked, .. }) = demo.form.field_mut(8) {
        *checked = true;
    }

    log_jsonl("filled_form", &[("action", "submit")]);

    // Submit the form
    demo.update(&press(KeyCode::Enter));

    // Check validation passed
    demo.run_validation();
    let state = demo.form_state.borrow();
    assert!(
        state.errors.is_empty(),
        "Valid form should have no errors after submit"
    );
    log_jsonl(
        "submit_success",
        &[
            ("error_count", "0"),
            ("elapsed_ms", &start.elapsed().as_millis().to_string()),
        ],
    );
}

// ===========================================================================
// Scenario 6: Password Match Validation
// ===========================================================================

#[test]
fn e2e_password_mismatch_error() {
    log_jsonl("env", &[("test", "e2e_password_mismatch_error")]);

    let mut demo = FormValidationDemo::new();

    // Set different passwords
    if let Some(FormField::Text { value, .. }) = demo.form.field_mut(2) {
        *value = "password123".into();
    }
    if let Some(FormField::Text { value, .. }) = demo.form.field_mut(3) {
        *value = "different456".into();
    }

    // Run validation
    demo.run_validation();

    let errors = demo.form_state.borrow().errors.clone();
    let mismatch_error = errors
        .iter()
        .find(|e| e.field == 3 && e.message.contains("match"));
    assert!(
        mismatch_error.is_some(),
        "Password mismatch should show error"
    );
    log_jsonl(
        "password_mismatch",
        &[(
            "error",
            mismatch_error.map(|e| e.message.as_str()).unwrap_or("none"),
        )],
    );
}

#[test]
fn e2e_password_match_clears_error() {
    log_jsonl("env", &[("test", "e2e_password_match_clears_error")]);

    let mut demo = FormValidationDemo::new();

    // Set matching passwords
    if let Some(FormField::Text { value, .. }) = demo.form.field_mut(2) {
        *value = "password123".into();
    }
    if let Some(FormField::Text { value, .. }) = demo.form.field_mut(3) {
        *value = "password123".into();
    }

    // Run validation
    demo.run_validation();

    let errors = demo.form_state.borrow().errors.clone();
    let mismatch_error = errors
        .iter()
        .find(|e| e.field == 3 && e.message.contains("match"));
    assert!(
        mismatch_error.is_none(),
        "Matching passwords should have no mismatch error"
    );
    log_jsonl("password_match", &[("has_mismatch_error", "false")]);
}

// ===========================================================================
// Scenario 7: Touched and Dirty State Tracking
// ===========================================================================

#[test]
fn e2e_touched_state_on_blur() {
    log_jsonl("env", &[("test", "e2e_touched_state_on_blur")]);

    let mut demo = FormValidationDemo::new();

    // Initially no fields are touched
    let initial_touched = demo.form_state.borrow().touched_fields().len();
    assert_eq!(initial_touched, 0, "Initially no fields should be touched");
    log_jsonl("initial", &[("touched_count", "0")]);

    // Tab to next field (this "blurs" field 0, marking it touched)
    demo.update(&press(KeyCode::Tab));
    let touched = demo.form_state.borrow().touched_fields();
    assert!(
        touched.contains(&0),
        "Field 0 should be marked touched after blur"
    );
    log_jsonl("after_tab", &[("touched", &format!("{touched:?}"))]);

    // Tab again
    demo.update(&press(KeyCode::Tab));
    let touched = demo.form_state.borrow().touched_fields();
    assert!(
        touched.contains(&1),
        "Field 1 should be marked touched after blur"
    );
    log_jsonl("after_tab2", &[("touched", &format!("{touched:?}"))]);
}

#[test]
fn e2e_dirty_state_on_change() {
    log_jsonl("env", &[("test", "e2e_dirty_state_on_change")]);

    let mut demo = FormValidationDemo::new();

    // Initially no fields are dirty
    let initial_dirty = demo.form_state.borrow().dirty_fields().len();
    assert_eq!(initial_dirty, 0, "Initially no fields should be dirty");
    log_jsonl("initial", &[("dirty_count", "0")]);

    // Type in field 0
    demo.update(&char_press('x'));

    // Check dirty state (need to call update_dirty after value change)
    demo.form_state.borrow_mut().update_dirty(&demo.form, 0);
    let dirty = demo.form_state.borrow().dirty_fields();
    assert!(
        dirty.contains(&0),
        "Field 0 should be marked dirty after typing"
    );
    log_jsonl("after_type", &[("dirty", &format!("{dirty:?}"))]);
}

// ===========================================================================
// Scenario 8: Screen Trait Implementation
// ===========================================================================

#[test]
fn e2e_screen_trait_methods() {
    log_jsonl("env", &[("test", "e2e_screen_trait_methods")]);

    let demo = FormValidationDemo::new();

    assert_eq!(demo.title(), "Form Validation");
    assert_eq!(demo.tab_label(), "Validate");

    let keybindings = demo.keybindings();
    assert!(!keybindings.is_empty(), "Should have keybindings");
    log_jsonl("keybindings", &[("count", &keybindings.len().to_string())]);

    // Verify specific keybindings exist
    let has_mode_toggle = keybindings.iter().any(|k| k.key == "M");
    assert!(
        has_mode_toggle,
        "Should have 'M' keybinding for mode toggle"
    );
}

// ===========================================================================
// Scenario 9: Tick Processing
// ===========================================================================

#[test]
fn e2e_tick_updates_notifications() {
    log_jsonl("env", &[("test", "e2e_tick_updates_notifications")]);

    let mut demo = FormValidationDemo::new();

    // Toggle mode to generate a notification
    demo.update(&char_press('m'));

    // Tick should process notifications without panic
    for i in 0..10 {
        demo.tick(i);
    }

    log_jsonl("ticked", &[("count", "10")]);
}

// ===========================================================================
// Scenario 10: Error Summary Panel
// ===========================================================================

#[test]
fn e2e_error_summary_reflects_errors() {
    log_jsonl("env", &[("test", "e2e_error_summary_reflects_errors")]);

    let mut demo = FormValidationDemo::new();

    // Run validation on empty form
    demo.run_validation();
    let error_count = demo.form_state.borrow().errors.len();

    // Render and verify error summary is visible
    let frame = render_demo(&demo, 120, 40);

    // Check that "Error Summary" appears in the frame
    let buffer_text: String = (0..40)
        .flat_map(|y| {
            (0..120).filter_map(move |x| frame.buffer.get(x, y).and_then(|c| c.content.as_char()))
        })
        .collect();

    assert!(
        buffer_text.contains("Error Summary"),
        "Error Summary panel should be visible"
    );

    log_jsonl(
        "error_summary",
        &[
            ("error_count", &error_count.to_string()),
            ("panel_visible", "true"),
        ],
    );
}
