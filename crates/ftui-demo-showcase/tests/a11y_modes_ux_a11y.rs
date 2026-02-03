#![forbid(unsafe_code)]

//! UX and Accessibility Review Tests for Accessibility Modes (bd-2o55.6)
//!
//! This suite validates the Accessibility Modes UX surface:
//!
//! # Keybindings Review
//!
//! | Key | Action |
//! |-----|--------|
//! | Shift+A | Toggle A11y panel |
//! | Shift+H | Toggle high contrast (panel open) |
//! | Shift+M | Toggle reduced motion (panel open) |
//! | Shift+L | Toggle large text (panel open) |
//! | Esc | Close A11y panel |
//!
//! # Focus Order Invariants
//!
//! 1. **Non-modal panel**: A11y overlay does not trap navigation keys.
//! 2. **Overlay coexistence**: Global overlays (help/debug) remain accessible.
//!
//! # Contrast/Legibility Standards
//!
//! - Panel labels are rendered as text (not color-only).
//! - Toggle state is visible as text ("ON"/"OFF").
//!
//! # Failure Modes
//!
//! | Scenario | Expected | Verified |
//! |----------|----------|----------|
//! | Shift+A fails | Panel stays hidden | ✓ |
//! | Panel traps keys | Tab can't change screens | ✓ |
//! | State not visible | Missing labels/ON/OFF | ✓ |
//!
//! # Invariants (Alien Artifact)
//!
//! 1. **Toggle idempotent**: double-toggle returns to original state.
//! 2. **Non-interference**: overlay doesn't block global navigation.
//! 3. **Text-first**: mode states are readable without relying on color.
//!
//! Run: `cargo test -p ftui-demo-showcase --test a11y_modes_ux_a11y`

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_demo_showcase::app::{AppModel, AppMsg};
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::program::Model;

// =============================================================================
// Test Utilities
// =============================================================================

/// Emit a JSONL log entry (for CI artifact review).
fn log_jsonl(test: &str, check: &str, passed: bool, notes: &str) {
    eprintln!(
        "{{\"test\":\"{test}\",\"check\":\"{check}\",\"passed\":{passed},\"notes\":\"{notes}\"}}"
    );
}

fn key_event(code: KeyCode, modifiers: Modifiers) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
    })
}

fn shift_char(ch: char) -> Event {
    key_event(KeyCode::Char(ch), Modifiers::SHIFT)
}

fn key_press(code: KeyCode) -> Event {
    key_event(code, Modifiers::empty())
}

/// Render the app to a frame and return the text content.
fn frame_text(app: &AppModel, width: u16, height: u16) -> String {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    app.view(&mut frame);

    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                text.push(ch);
            }
        }
    }
    text
}

// =============================================================================
// Keybinding Tests
// =============================================================================

#[test]
fn keybinding_shift_a_toggles_panel() {
    let mut app = AppModel::new();
    assert!(!app.a11y_panel_visible, "A11y panel should start hidden");

    let _ = app.update(AppMsg::ScreenEvent(shift_char('A')));
    log_jsonl("keybinding_shift_a", "toggle_on", app.a11y_panel_visible, "");
    assert!(app.a11y_panel_visible, "Shift+A should open the A11y panel");

    let _ = app.update(AppMsg::ScreenEvent(shift_char('A')));
    log_jsonl("keybinding_shift_a", "toggle_off", !app.a11y_panel_visible, "");
    assert!(
        !app.a11y_panel_visible,
        "Shift+A should close the A11y panel"
    );
}

#[test]
fn keybinding_panel_shortcuts_toggle_modes() {
    let mut app = AppModel::new();
    let _ = app.update(AppMsg::ScreenEvent(shift_char('A')));
    assert!(app.a11y_panel_visible);

    let _ = app.update(AppMsg::ScreenEvent(shift_char('H')));
    log_jsonl("keybinding_panel", "high_contrast", app.a11y.high_contrast, "");
    assert!(app.a11y.high_contrast, "Shift+H toggles high contrast");

    let _ = app.update(AppMsg::ScreenEvent(shift_char('M')));
    log_jsonl("keybinding_panel", "reduced_motion", app.a11y.reduced_motion, "");
    assert!(app.a11y.reduced_motion, "Shift+M toggles reduced motion");

    let _ = app.update(AppMsg::ScreenEvent(shift_char('L')));
    log_jsonl("keybinding_panel", "large_text", app.a11y.large_text, "");
    assert!(app.a11y.large_text, "Shift+L toggles large text");
}

#[test]
fn keybinding_escape_closes_panel() {
    let mut app = AppModel::new();
    let _ = app.update(AppMsg::ScreenEvent(shift_char('A')));
    assert!(app.a11y_panel_visible);

    let _ = app.update(AppMsg::ScreenEvent(key_press(KeyCode::Escape)));
    log_jsonl("keybinding_escape", "panel_closed", !app.a11y_panel_visible, "");
    assert!(
        !app.a11y_panel_visible,
        "Escape should close the A11y panel"
    );
}

// =============================================================================
// Focus Order Tests
// =============================================================================

#[test]
fn focus_panel_non_modal_navigation() {
    let mut app = AppModel::new();
    let initial_screen = app.current_screen;

    let _ = app.update(AppMsg::ScreenEvent(shift_char('A')));
    assert!(app.a11y_panel_visible);

    let _ = app.update(AppMsg::ScreenEvent(key_press(KeyCode::Tab)));
    log_jsonl(
        "focus",
        "tab_with_panel",
        app.current_screen != initial_screen,
        "Tab should still change screens while panel is visible",
    );
    assert_ne!(
        app.current_screen, initial_screen,
        "Panel should not trap screen navigation"
    );
}

#[test]
fn focus_help_overlay_accessible_with_panel() {
    let mut app = AppModel::new();
    let _ = app.update(AppMsg::ScreenEvent(shift_char('A')));
    assert!(app.a11y_panel_visible);

    let _ = app.update(AppMsg::ScreenEvent(key_press(KeyCode::Char('?'))));
    log_jsonl("focus", "help_visible", app.help_visible, "");
    assert!(app.help_visible, "Help overlay should open while panel is visible");
}

// =============================================================================
// Contrast / Legibility Tests
// =============================================================================

#[test]
fn legibility_panel_text_and_states_rendered() {
    let mut app = AppModel::new();
    app.terminal_width = 120;
    app.terminal_height = 40;

    let _ = app.update(AppMsg::ScreenEvent(shift_char('A')));
    assert!(app.a11y_panel_visible);

    let text = frame_text(&app, 120, 40);
    let has_title = text.contains("A11y");
    let has_high_contrast = text.contains("High Contrast");
    let has_reduced_motion = text.contains("Reduced Motion");
    let has_large_text = text.contains("Large Text");
    let has_off = text.contains("OFF");

    log_jsonl("legibility", "title", has_title, "");
    log_jsonl("legibility", "high_contrast_label", has_high_contrast, "");
    log_jsonl("legibility", "reduced_motion_label", has_reduced_motion, "");
    log_jsonl("legibility", "large_text_label", has_large_text, "");
    log_jsonl("legibility", "state_text_off", has_off, "Expect OFF for default modes");

    assert!(has_title, "Panel title should render");
    assert!(has_high_contrast, "High Contrast label should render");
    assert!(has_reduced_motion, "Reduced Motion label should render");
    assert!(has_large_text, "Large Text label should render");
    assert!(has_off, "OFF state should be visible as text");
}

#[test]
fn legibility_state_text_on_visible() {
    let mut app = AppModel::new();
    app.terminal_width = 120;
    app.terminal_height = 40;

    let _ = app.update(AppMsg::ScreenEvent(shift_char('A')));
    let _ = app.update(AppMsg::ScreenEvent(shift_char('H')));
    assert!(app.a11y.high_contrast);

    let text = frame_text(&app, 120, 40);
    let has_on = text.contains("ON");
    log_jsonl("legibility", "state_text_on", has_on, "High contrast should show ON");
    assert!(has_on, "ON state should be visible as text");
}
