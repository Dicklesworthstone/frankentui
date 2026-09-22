#![forbid(unsafe_code)]

//! Integration tests for global keyboard shortcuts (bd-iuvb.17.5).
//!
//! Validates that all global shortcuts trigger the expected state changes
//! and that Esc correctly dismisses overlays in priority order.

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_demo_showcase::app::{AppModel, AppMsg, ScreenId};
use ftui_runtime::Model;

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::NONE,
        kind: KeyEventKind::Press,
    })
}

fn press_mod(code: KeyCode, modifiers: Modifiers) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
    })
}

#[test]
fn m_key_toggles_mouse_capture() {
    let mut app = AppModel::new();
    // Not the Dashboard: its own m cycles the markdown sample.
    app.current_screen = ScreenId::WidgetGallery;
    assert!(app.mouse_capture_enabled);

    app.update(AppMsg::from(press(KeyCode::Char('m'))));
    assert!(!app.mouse_capture_enabled, "m should disable mouse capture");

    app.update(AppMsg::from(press(KeyCode::Char('m'))));
    assert!(app.mouse_capture_enabled, "m again should re-enable");
}

#[test]
fn f6_toggles_mouse_capture() {
    let mut app = AppModel::new();
    assert!(app.mouse_capture_enabled);

    app.update(AppMsg::from(press(KeyCode::F(6))));
    assert!(
        !app.mouse_capture_enabled,
        "F6 should disable mouse capture"
    );
}

#[test]
fn shift_a_toggles_a11y_panel() {
    let mut app = AppModel::new();
    assert!(!app.a11y_panel_visible);

    app.update(AppMsg::from(press_mod(
        KeyCode::Char('A'),
        Modifiers::SHIFT,
    )));
    assert!(app.a11y_panel_visible, "Shift+A should open a11y panel");

    app.update(AppMsg::from(press_mod(
        KeyCode::Char('A'),
        Modifiers::SHIFT,
    )));
    assert!(!app.a11y_panel_visible, "Shift+A again should close it");
}

#[test]
fn f12_toggles_debug_overlay() {
    let mut app = AppModel::new();
    assert!(!app.debug_visible);

    app.update(AppMsg::from(press(KeyCode::F(12))));
    assert!(app.debug_visible, "F12 should open debug overlay");

    app.update(AppMsg::from(press(KeyCode::F(12))));
    assert!(!app.debug_visible, "F12 again should close it");
}

#[test]
fn ctrl_p_toggles_perf_hud() {
    let mut app = AppModel::new();
    assert!(!app.perf_hud_visible);

    app.update(AppMsg::from(press_mod(KeyCode::Char('p'), Modifiers::CTRL)));
    assert!(app.perf_hud_visible, "Ctrl+P should open perf HUD");

    app.update(AppMsg::from(press_mod(KeyCode::Char('p'), Modifiers::CTRL)));
    assert!(!app.perf_hud_visible, "Ctrl+P again should close it");
}

#[test]
fn shift_l_advances_screen() {
    let mut app = AppModel::new();
    assert_eq!(app.current_screen, ScreenId::Dashboard);

    app.update(AppMsg::from(press_mod(
        KeyCode::Char('L'),
        Modifiers::SHIFT,
    )));
    assert_eq!(
        app.current_screen,
        ScreenId::Shakespeare,
        "Shift+L should advance to next screen"
    );
}

#[test]
fn shift_h_goes_previous_screen() {
    let mut app = AppModel::new();
    app.current_screen = ScreenId::Shakespeare;

    app.update(AppMsg::from(press_mod(
        KeyCode::Char('H'),
        Modifiers::SHIFT,
    )));
    assert_eq!(
        app.current_screen,
        ScreenId::Dashboard,
        "Shift+H should go to previous screen"
    );
}

#[test]
fn esc_closes_a11y_panel() {
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;

    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(!app.a11y_panel_visible, "Esc should close the a11y panel");
}

#[test]
fn esc_closes_command_palette() {
    let mut app = AppModel::new();
    app.command_palette.open();
    assert!(app.command_palette.is_visible());

    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(
        !app.command_palette.is_visible(),
        "Esc should close the command palette"
    );
}

#[test]
fn ctrl_k_opens_command_palette() {
    let mut app = AppModel::new();
    assert!(!app.command_palette.is_visible());

    app.update(AppMsg::from(press_mod(KeyCode::Char('k'), Modifiers::CTRL)));
    assert!(
        app.command_palette.is_visible(),
        "Ctrl+K should open command palette"
    );
}

#[test]
fn question_mark_toggles_help() {
    let mut app = AppModel::new();
    assert!(!app.help_visible);

    app.update(AppMsg::from(press(KeyCode::Char('?'))));
    assert!(app.help_visible, "? should show help");

    app.update(AppMsg::from(press(KeyCode::Char('?'))));
    assert!(!app.help_visible, "? again should hide help");
}

#[test]
fn all_global_shortcuts_are_distinct() {
    // Verify no shortcut accidentally triggers two actions
    let mut app = AppModel::new();

    // Press Ctrl+P - only perf_hud should change
    app.update(AppMsg::from(press_mod(KeyCode::Char('p'), Modifiers::CTRL)));
    assert!(app.perf_hud_visible);
    assert!(!app.help_visible);
    assert!(!app.debug_visible);
    assert!(!app.a11y_panel_visible);
    assert!(!app.command_palette.is_visible());

    // Reset and press F12 - only debug should change
    let mut app = AppModel::new();
    app.update(AppMsg::from(press(KeyCode::F(12))));
    assert!(app.debug_visible);
    assert!(!app.perf_hud_visible);
    assert!(!app.help_visible);
    assert!(!app.a11y_panel_visible);
}

#[test]
fn help_visible_after_question_mark_survives_toggle_cycle() {
    // Verify that toggling help on/off/on results in help visible
    let mut app = AppModel::new();

    app.update(AppMsg::from(press(KeyCode::Char('?'))));
    assert!(app.help_visible);

    app.update(AppMsg::from(press(KeyCode::Char('?'))));
    assert!(!app.help_visible);

    app.update(AppMsg::from(press(KeyCode::Char('?'))));
    assert!(app.help_visible, "third toggle should show help again");
}

#[test]
fn esc_closes_help() {
    // Esc closes the topmost overlay
    let mut app = AppModel::new();
    app.help_visible = true;

    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(!app.help_visible, "Esc should close help overlay");
}

#[test]
fn esc_closes_debug() {
    // Esc closes the topmost overlay
    let mut app = AppModel::new();
    app.debug_visible = true;

    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(!app.debug_visible, "Esc should close debug overlay");
}

#[test]
fn esc_closes_perf_hud() {
    // Esc closes the topmost overlay
    let mut app = AppModel::new();
    app.perf_hud_visible = true;

    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(!app.perf_hud_visible, "Esc should close perf HUD overlay");
}

#[test]
fn esc_priority_palette_before_a11y() {
    // When both palette and a11y panel are open, Esc should close
    // only the palette (it consumes the event before a11y handler).
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;
    app.command_palette.open();
    assert!(app.command_palette.is_visible());

    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(
        !app.command_palette.is_visible(),
        "first Esc should close the command palette"
    );
    assert!(
        app.a11y_panel_visible,
        "a11y panel should remain open after first Esc"
    );

    // Second Esc closes the a11y panel
    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(
        !app.a11y_panel_visible,
        "second Esc should close the a11y panel"
    );
}

#[test]
fn esc_with_help_and_a11y_closes_help_first() {
    // Overlay priority: help is above a11y, so Esc closes help first
    let mut app = AppModel::new();
    app.help_visible = true;
    app.a11y_panel_visible = true;

    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(
        !app.help_visible,
        "first Esc should close help (higher priority)"
    );
    assert!(
        app.a11y_panel_visible,
        "a11y should remain visible after first Esc"
    );

    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(
        !app.a11y_panel_visible,
        "second Esc should close a11y panel"
    );
}

#[test]
fn shortcuts_ignored_while_palette_is_open() {
    // When the command palette is open, global shortcuts like ? and F12
    // should be consumed by the palette (not toggle overlays).
    let mut app = AppModel::new();
    app.command_palette.open();
    assert!(!app.help_visible);
    assert!(!app.debug_visible);

    app.update(AppMsg::from(press(KeyCode::Char('?'))));
    assert!(
        !app.help_visible,
        "? should not toggle help while palette is open"
    );

    app.update(AppMsg::from(press(KeyCode::F(12))));
    assert!(
        !app.debug_visible,
        "F12 should not toggle debug while palette is open"
    );
}

#[test]
fn number_keys_navigate_to_screens() {
    let mut app = AppModel::new();
    assert_eq!(app.current_screen, ScreenId::Dashboard);

    // Press '1' to go to screen 1 (Shakespeare is typically screen 1)
    app.update(AppMsg::from(press(KeyCode::Char('1'))));
    assert_ne!(
        app.current_screen,
        ScreenId::Dashboard,
        "number key should change screen"
    );
}

#[test]
fn tab_cycles_screens_forward() {
    let mut app = AppModel::new();
    let initial = app.current_screen;

    app.update(AppMsg::from(press(KeyCode::Tab)));
    assert_ne!(
        app.current_screen, initial,
        "Tab should advance to next screen"
    );
}

#[test]
fn backtab_cycles_screens_backward() {
    let mut app = AppModel::new();
    app.current_screen = ScreenId::Shakespeare;

    app.update(AppMsg::from(press_mod(KeyCode::BackTab, Modifiers::SHIFT)));
    assert_eq!(
        app.current_screen,
        ScreenId::Dashboard,
        "BackTab should go to previous screen"
    );
}

#[test]
fn ctrl_t_cycles_theme() {
    let mut app = AppModel::new();
    let initial_theme = app.base_theme;

    app.update(AppMsg::from(press_mod(KeyCode::Char('t'), Modifiers::CTRL)));
    assert_ne!(
        app.base_theme, initial_theme,
        "Ctrl+T should cycle to next theme"
    );
}

#[test]
fn q_key_is_quit_signal() {
    let mut app = AppModel::new();
    // q should return a quit command, but we can't easily test the return
    // value of update(). Instead, verify q doesn't toggle any overlays.
    let help_before = app.help_visible;
    let debug_before = app.debug_visible;
    let perf_before = app.perf_hud_visible;
    let screen_before = app.current_screen;

    app.update(AppMsg::from(press(KeyCode::Char('q'))));

    assert_eq!(app.help_visible, help_before, "q should not toggle help");
    assert_eq!(app.debug_visible, debug_before, "q should not toggle debug");
    assert_eq!(
        app.perf_hud_visible, perf_before,
        "q should not toggle perf"
    );
    assert_eq!(
        app.current_screen, screen_before,
        "q should not change screen"
    );
}

#[test]
fn multiple_overlays_coexist() {
    // Multiple overlays can be visible simultaneously
    let mut app = AppModel::new();

    // Open help, debug, and perf HUD
    app.update(AppMsg::from(press(KeyCode::Char('?'))));
    app.update(AppMsg::from(press(KeyCode::F(12))));
    app.update(AppMsg::from(press_mod(KeyCode::Char('p'), Modifiers::CTRL)));

    assert!(app.help_visible, "help should be on");
    assert!(app.debug_visible, "debug should be on");
    assert!(app.perf_hud_visible, "perf HUD should be on");

    // Close them individually
    app.update(AppMsg::from(press(KeyCode::Char('?'))));
    assert!(!app.help_visible, "help should be off");
    assert!(app.debug_visible, "debug still on");
    assert!(app.perf_hud_visible, "perf HUD still on");
}
