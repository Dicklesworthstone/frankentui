#![forbid(unsafe_code)]

//! UX and Accessibility Review Tests for Terminal Capability Explorer (bd-2sog.6)
//!
//! This module validates the UX/a11y surface for the Terminal Capability Explorer:
//!
//! # Keybindings Review
//! | Key | Action |
//! |-----|--------|
//! | Tab | Cycle view (matrix/evidence/simulation) |
//! | ↑/↓ or k/j | Select capability row |
//! | P | Cycle simulated profile |
//! | R | Reset to detected profile |
//!
//! # Focus Order Invariants
//! 1. **Keyboard-first**: all primary actions are keyboard-accessible.
//! 2. **Stable navigation**: selection moves deterministically and stays in bounds.
//!
//! # Contrast/Legibility Standards
//! - State is expressed in text ("yes"/"no") in addition to color.
//! - Detail panel uses explicit labels (Capability/Detected/Effective/Fallback/Reason).
//!
//! # Invariants (Alien Artifact)
//! 1. **View cycling is total**: Tab cycles through Matrix → Evidence → Simulation → Matrix.
//! 2. **Selection changes update the details panel**.
//! 3. **Profile override is visible in the summary line**.
//!
//! Run: `cargo test -p ftui-demo-showcase --test terminal_capabilities_ux_a11y`

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::TerminalProfile;
use ftui_demo_showcase::screens::Screen;
use ftui_demo_showcase::screens::terminal_capabilities::TerminalCapabilitiesScreen;
use ftui_demo_showcase::theme::{ScopedRenderLock, ThemeId};
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::Cmd;

// =============================================================================
// Test Utilities
// =============================================================================

fn log_jsonl(test: &str, check: &str, passed: bool, notes: &str) {
    eprintln!(
        "{{\"test\":\"{test}\",\"check\":\"{check}\",\"passed\":{passed},\"notes\":\"{notes}\"}}"
    );
}

fn key_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        modifiers: ftui_core::event::Modifiers::empty(),
    })
}

fn render_lines(screen: &TerminalCapabilitiesScreen, width: u16, height: u16) -> Vec<String> {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, width, height));

    let mut lines = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut line = String::new();
        let mut skip_continuations = 0usize;
        for x in 0..width {
            if skip_continuations > 0 {
                skip_continuations = skip_continuations.saturating_sub(1);
                continue;
            }
            if let Some(cell) = frame.buffer.get(x, y) {
                if let Some(ch) = cell.content.as_char() {
                    line.push(ch);
                    continue;
                }
                if let Some(id) = cell.content.grapheme_id()
                    && let Some(grapheme) = frame.pool.get(id)
                {
                    line.push_str(grapheme);
                    let width = id.width();
                    if width > 1 {
                        skip_continuations = width.saturating_sub(1);
                    }
                    continue;
                }
                if cell.content.is_continuation() {
                    continue;
                }
            }
            line.push(' ');
        }
        lines.push(line);
    }
    lines
}

fn find_line<'a>(lines: &'a [String], needle: &str) -> Option<&'a String> {
    lines.iter().find(|line| line.contains(needle))
}

// =============================================================================
// Keybinding Tests
// =============================================================================

#[test]
fn keybindings_documented() {
    let screen = TerminalCapabilitiesScreen::new();
    let bindings = screen.keybindings();

    let keys: Vec<_> = bindings.iter().map(|h| (h.key, h.action)).collect();
    log_jsonl(
        "keybindings",
        "count",
        !keys.is_empty(),
        &format!("bindings={}", keys.len()),
    );

    assert!(
        keys.iter()
            .any(|(k, a)| *k == "Tab" && a.contains("Cycle view")),
        "Tab should be documented for view cycling"
    );
    assert!(
        keys.iter()
            .any(|(k, a)| *k == "↑/↓" && a.contains("Select")),
        "Arrow keys should be documented for selection"
    );
    assert!(
        keys.iter()
            .any(|(k, a)| *k == "P" && a.contains("Cycle simulated profile")),
        "P should be documented for profile cycling"
    );
    assert!(
        keys.iter()
            .any(|(k, a)| *k == "R" && a.contains("Reset to detected profile")),
        "R should be documented for profile reset"
    );
}

#[test]
fn keybinding_tab_cycles_view() {
    // Acquire render lock to prevent race with parallel tests that mutate global theme state.
    let _render_guard = ScopedRenderLock::new(ThemeId::CyberpunkAurora, false, 1.0);
    let mut screen = TerminalCapabilitiesScreen::new();

    let lines = render_lines(&screen, 120, 40);
    let summary = find_line(&lines, "Profile:").expect("summary line should render");
    assert!(
        summary.contains("View: Matrix"),
        "Initial view should be Matrix"
    );

    let _ = screen.update(&key_press(KeyCode::Tab));
    let lines = render_lines(&screen, 120, 40);
    let summary = find_line(&lines, "Profile:").expect("summary line should render");
    assert!(
        summary.contains("View: Evidence"),
        "Tab should switch to Evidence"
    );

    let _ = screen.update(&key_press(KeyCode::Tab));
    let lines = render_lines(&screen, 120, 40);
    let summary = find_line(&lines, "Profile:").expect("summary line should render");
    assert!(
        summary.contains("View: Simulation"),
        "Tab should switch to Simulation"
    );

    let _ = screen.update(&key_press(KeyCode::Tab));
    let lines = render_lines(&screen, 120, 40);
    let summary = find_line(&lines, "Profile:").expect("summary line should render");
    assert!(
        summary.contains("View: Matrix"),
        "Tab should wrap to Matrix"
    );
}

#[test]
fn keybinding_profile_cycle_and_reset() {
    // Acquire render lock to prevent race with parallel tests that mutate global theme state.
    let _render_guard = ScopedRenderLock::new(ThemeId::CyberpunkAurora, false, 1.0);
    let mut screen = TerminalCapabilitiesScreen::with_profile(TerminalProfile::Modern);

    // When profile_override is set, summary shows "Detected:" instead of "Profile:"
    let lines = render_lines(&screen, 120, 40);
    let before =
        find_line(&lines, "Detected:").expect("summary line should render (with override)");

    let _ = screen.update(&key_press(KeyCode::Char('p')));
    let lines = render_lines(&screen, 120, 40);
    // After cycling from Modern, the override is still set so still shows "Detected:"
    let after_cycle = find_line(&lines, "Detected:").expect("summary line should render");
    assert_ne!(
        before, after_cycle,
        "Profile cycle should change summary line"
    );

    let _ = screen.update(&key_press(KeyCode::Char('r')));
    let lines = render_lines(&screen, 120, 40);
    // After reset, override is cleared so shows "Profile:" instead of "Detected:"
    let after_reset =
        find_line(&lines, "Profile:").expect("summary line should render (after reset)");
    // Compare with previous state to ensure change happened
    assert!(
        !after_reset.contains("Simulated:"),
        "Profile reset should clear the simulation override"
    );
}

// =============================================================================
// Focus / Selection Tests
// =============================================================================

#[test]
fn selection_updates_details_panel() {
    // Acquire render lock to prevent race with parallel tests that mutate global theme state.
    let _render_guard = ScopedRenderLock::new(ThemeId::CyberpunkAurora, false, 1.0);
    let mut screen = TerminalCapabilitiesScreen::new();

    let lines = render_lines(&screen, 120, 40);
    let before = find_line(&lines, "Capability:")
        .expect("details panel should render capability line")
        .to_string();

    let _ = screen.update(&key_press(KeyCode::Down));
    let lines = render_lines(&screen, 120, 40);
    let after = find_line(&lines, "Capability:")
        .expect("details panel should render capability line")
        .to_string();

    log_jsonl(
        "selection",
        "details_update",
        before != after,
        "Capability line should update on selection change",
    );
    assert_ne!(
        before, after,
        "Selection change should update details panel"
    );
}

// =============================================================================
// Legibility Tests
// =============================================================================

#[test]
fn legibility_state_labels_present() {
    // Acquire render lock to prevent race with parallel tests that mutate global theme state.
    let _render_guard = ScopedRenderLock::new(ThemeId::CyberpunkAurora, false, 1.0);
    let screen = TerminalCapabilitiesScreen::new();
    let lines = render_lines(&screen, 120, 40);

    let has_detected = lines.iter().any(|l| l.contains("Detected:"));
    let has_effective = lines.iter().any(|l| l.contains("Effective:"));
    let has_fallback = lines.iter().any(|l| l.contains("Fallback:"));
    let has_reason = lines.iter().any(|l| l.contains("Reason:"));
    let has_yes_no = lines.iter().any(|l| l.contains("yes") || l.contains("no"));

    log_jsonl("legibility", "detected_label", has_detected, "");
    log_jsonl("legibility", "effective_label", has_effective, "");
    log_jsonl("legibility", "fallback_label", has_fallback, "");
    log_jsonl("legibility", "reason_label", has_reason, "");
    log_jsonl(
        "legibility",
        "yes_no_text",
        has_yes_no,
        "state should be text, not color-only",
    );

    assert!(has_detected, "Detected label should render");
    assert!(has_effective, "Effective label should render");
    assert!(has_fallback, "Fallback label should render");
    assert!(has_reason, "Reason label should render");
    assert!(has_yes_no, "State should be visible as yes/no text");
}

// =============================================================================
// Compile-time sanity (clippy-friendly)
// =============================================================================

#[test]
fn update_returns_cmd_none() {
    let mut screen = TerminalCapabilitiesScreen::new();
    let cmd = screen.update(&key_press(KeyCode::Tab));
    assert!(matches!(cmd, Cmd::None));
}
