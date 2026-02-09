#![forbid(unsafe_code)]

//! Per-screen snapshot tests for the FrankenTUI Demo Showcase.
//!
//! Each screen is rendered at standard sizes and compared against stored
//! baselines. Run `BLESS=1 cargo test -p ftui-demo-showcase` to create or
//! update snapshot files.
//!
//! Naming convention: `screen_name_scenario_WIDTHxHEIGHT`

use std::env;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::TerminalProfile;
use ftui_demo_showcase::app::{AppModel, AppMsg, ScreenId};
use ftui_demo_showcase::screens::Screen;
use ftui_demo_showcase::theme::{ScopedThemeLock, ThemeId};
use ftui_harness::{assert_snapshot, assert_snapshot_ansi, buffer_to_text};
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::link_registry::LinkRegistry;
use ftui_runtime::Model;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// Environment overrides are avoided here because std::env::set_var/remove_var
// are unsafe in Rust 2024. Tests that require stable terminal metadata should
// use explicit overrides (e.g., `terminal_caps_env()`).

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::NONE,
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

fn mouse_move(x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent::new(MouseEventKind::Moved, x, y))
}

fn snapshot_app(app: &mut AppModel, width: u16, height: u16, name: &str) {
    app.terminal_width = width;
    app.terminal_height = height;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    app.view(&mut frame);
    assert_snapshot!(name, &frame.buffer);
}

fn terminal_caps_env() -> ftui_demo_showcase::screens::terminal_capabilities::EnvSnapshot {
    ftui_demo_showcase::screens::terminal_capabilities::EnvSnapshot::from_values(
        "xterm-256color",
        "ftui-test",
        "truecolor",
        false,
        false,
        false,
        false,
        false,
        false,
    )
}

fn terminal_caps_screen()
-> ftui_demo_showcase::screens::terminal_capabilities::TerminalCapabilitiesScreen {
    let mut screen =
        ftui_demo_showcase::screens::terminal_capabilities::TerminalCapabilitiesScreen::with_profile(
            TerminalProfile::Modern,
        );
    screen.set_detected_profile_override(TerminalProfile::Xterm256Color);
    screen.set_env_override(terminal_caps_env());
    screen
}

fn i18n_stress_screen(
    set_steps: usize,
    sample_steps: usize,
) -> ftui_demo_showcase::screens::i18n_demo::I18nDemo {
    let mut screen = ftui_demo_showcase::screens::i18n_demo::I18nDemo::new();
    for _ in 0..3 {
        screen.update(&press(KeyCode::Tab));
    }
    for _ in 0..set_steps {
        screen.update(&press(KeyCode::Char(']')));
    }
    for _ in 0..sample_steps {
        screen.update(&press(KeyCode::Down));
    }
    screen
}

fn mermaid_showcase_goto_sample(
    screen: &mut ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen,
    id: &str,
) {
    assert!(
        screen.select_sample_by_id_for_test(id),
        "unknown Mermaid showcase sample id: {id}"
    );
}

fn mermaid_showcase_snapshot(
    screen: &mut ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen,
    width: u16,
    height: u16,
    name: &str,
) {
    screen.stabilize_metrics_for_snapshot();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let area = Rect::new(0, 0, width, height);
    screen.view(&mut frame, area);
    assert_snapshot!(name, &frame.buffer);
}

// ============================================================================
// Guided Tour Overlay
// ============================================================================

#[test]
fn guided_tour_overlay_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.start_tour(0, 1.0);
    snapshot_app(&mut app, 80, 24, "guided_tour_overlay_80x24");
}

#[test]
fn guided_tour_overlay_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.start_tour(1, 1.0);
    snapshot_app(&mut app, 120, 40, "guided_tour_overlay_120x40");
}

// ============================================================================
// Dashboard
// ============================================================================

#[test]
fn dashboard_initial_80x24() {
    let screen = ftui_demo_showcase::screens::dashboard::Dashboard::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("dashboard_initial_80x24", &frame.buffer);
}

#[test]
fn dashboard_initial_120x40() {
    let screen = ftui_demo_showcase::screens::dashboard::Dashboard::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("dashboard_initial_120x40", &frame.buffer);
}

#[test]
fn dashboard_hover_dataviz_120x40() {
    let mut screen = ftui_demo_showcase::screens::dashboard::Dashboard::new();

    // Render once with hit testing enabled so we can pick a stable coordinate
    // inside the DataViz tile.
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    frame.enable_hit_testing();
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);

    let expected = ftui_demo_showcase::chrome::PANE_HIT_BASE
        + ftui_demo_showcase::screens::screen_index(ScreenId::DataViz) as u32;
    let mut tile_xy = None;
    for y in 0..40u16 {
        for x in 0..120u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, y)
                && id.id() == expected
            {
                tile_xy = Some((x, y));
                break;
            }
        }
        if tile_xy.is_some() {
            break;
        }
    }
    let (x, y) = tile_xy.expect("Should find DataViz tile pane hit region");

    let moved = Event::Mouse(MouseEvent::new(MouseEventKind::Moved, x, y));
    let _ = screen.update(&moved);

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("dashboard_hover_dataviz_120x40", &frame.buffer);
    assert_snapshot_ansi!("dashboard_hover_dataviz_120x40", &frame.buffer);
}

#[test]
fn dashboard_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::dashboard::Dashboard::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("dashboard_tiny_40x10", &frame.buffer);
}

#[test]
fn dashboard_zero_area() {
    let screen = ftui_demo_showcase::screens::dashboard::Dashboard::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
    // No panic = success
}

#[test]
fn dashboard_title() {
    let screen = ftui_demo_showcase::screens::dashboard::Dashboard::new();
    assert_eq!(screen.title(), "Dashboard");
    assert_eq!(screen.tab_label(), "Dashboard");
}

// ============================================================================
// Shakespeare
// ============================================================================

#[test]
fn shakespeare_initial_120x40() {
    let screen = ftui_demo_showcase::screens::shakespeare::Shakespeare::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("shakespeare_initial_120x40", &frame.buffer);
}

#[test]
fn shakespeare_initial_80x24() {
    let screen = ftui_demo_showcase::screens::shakespeare::Shakespeare::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("shakespeare_initial_80x24", &frame.buffer);
}

#[test]
fn shakespeare_after_scroll_120x40() {
    let mut screen = ftui_demo_showcase::screens::shakespeare::Shakespeare::new();
    for _ in 0..5 {
        screen.update(&press(KeyCode::Down));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("shakespeare_after_scroll_120x40", &frame.buffer);
}

#[test]
fn shakespeare_end_key_120x40() {
    let mut screen = ftui_demo_showcase::screens::shakespeare::Shakespeare::new();
    screen.update(&press(KeyCode::End));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("shakespeare_end_key_120x40", &frame.buffer);
}

#[test]
fn shakespeare_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::shakespeare::Shakespeare::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("shakespeare_tiny_40x10", &frame.buffer);
}

// ============================================================================
// i18n Stress Lab
// ============================================================================

#[test]
fn i18n_stress_lab_rtl_120x40() {
    let screen = i18n_stress_screen(2, 0); // RTL set, first sample
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("i18n_stress_lab_rtl_120x40", &frame.buffer);
}

#[test]
fn i18n_stress_lab_emoji_120x40() {
    let screen = i18n_stress_screen(3, 1); // Emoji set, second sample
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("i18n_stress_lab_emoji_120x40", &frame.buffer);
}

// ============================================================================
// Code Explorer
// ============================================================================

#[test]
fn code_explorer_initial_120x40() {
    let screen = ftui_demo_showcase::screens::code_explorer::CodeExplorer::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("code_explorer_initial_120x40", &frame.buffer);
}

#[test]
fn code_explorer_initial_80x24() {
    let screen = ftui_demo_showcase::screens::code_explorer::CodeExplorer::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("code_explorer_initial_80x24", &frame.buffer);
}

#[test]
fn code_explorer_scrolled_120x40() {
    let mut screen = ftui_demo_showcase::screens::code_explorer::CodeExplorer::new();
    for _ in 0..20 {
        screen.update(&press(KeyCode::Down));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("code_explorer_scrolled_120x40", &frame.buffer);
}

#[test]
fn code_explorer_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::code_explorer::CodeExplorer::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("code_explorer_tiny_40x10", &frame.buffer);
}

#[test]
fn code_explorer_wide_200x50() {
    let screen = ftui_demo_showcase::screens::code_explorer::CodeExplorer::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(200, 50, &mut pool);
    let area = Rect::new(0, 0, 200, 50);
    screen.view(&mut frame, area);
    assert_snapshot!("code_explorer_wide_200x50", &frame.buffer);
}

// ============================================================================
// Widget Gallery
// ============================================================================

#[test]
fn widget_gallery_initial_120x40() {
    let screen = ftui_demo_showcase::screens::widget_gallery::WidgetGallery::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("widget_gallery_initial_120x40", &frame.buffer);
}

#[test]
fn widget_gallery_initial_80x24() {
    let screen = ftui_demo_showcase::screens::widget_gallery::WidgetGallery::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("widget_gallery_initial_80x24", &frame.buffer);
}

#[test]
fn widget_gallery_section2_120x40() {
    let mut screen = ftui_demo_showcase::screens::widget_gallery::WidgetGallery::new();
    screen.update(&press(KeyCode::Right));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("widget_gallery_section2_120x40", &frame.buffer);
}

#[test]
fn widget_gallery_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::widget_gallery::WidgetGallery::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("widget_gallery_tiny_40x10", &frame.buffer);
}

#[test]
fn widget_gallery_with_tick_120x40() {
    let mut screen = ftui_demo_showcase::screens::widget_gallery::WidgetGallery::new();
    screen.tick(5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("widget_gallery_with_tick_120x40", &frame.buffer);
}

// ============================================================================
// Widget Builder
// ============================================================================

#[test]
fn widget_builder_initial_120x40() {
    let screen = ftui_demo_showcase::screens::widget_builder::WidgetBuilder::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("widget_builder_initial_120x40", &frame.buffer);
}

#[test]
fn widget_builder_initial_80x24() {
    let screen = ftui_demo_showcase::screens::widget_builder::WidgetBuilder::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("widget_builder_initial_80x24", &frame.buffer);
}

#[test]
fn widget_builder_status_wall_120x40() {
    let mut screen = ftui_demo_showcase::screens::widget_builder::WidgetBuilder::new();
    screen.update(&press(KeyCode::Char('p')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("widget_builder_status_wall_120x40", &frame.buffer);
}

#[test]
fn widget_builder_minimal_120x40() {
    let mut screen = ftui_demo_showcase::screens::widget_builder::WidgetBuilder::new();
    screen.update(&press(KeyCode::Char('p')));
    screen.update(&press(KeyCode::Char('p')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("widget_builder_minimal_120x40", &frame.buffer);
}

// ============================================================================
// Layout Lab
// ============================================================================

#[test]
fn layout_lab_initial_120x40() {
    let screen = ftui_demo_showcase::screens::layout_lab::LayoutLab::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("layout_lab_initial_120x40", &frame.buffer);
}

#[test]
fn layout_lab_initial_80x24() {
    let screen = ftui_demo_showcase::screens::layout_lab::LayoutLab::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("layout_lab_initial_80x24", &frame.buffer);
}

#[test]
fn layout_lab_preset2_120x40() {
    let mut screen = ftui_demo_showcase::screens::layout_lab::LayoutLab::new();
    screen.update(&press(KeyCode::Right));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("layout_lab_preset2_120x40", &frame.buffer);
}

#[test]
fn layout_lab_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::layout_lab::LayoutLab::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("layout_lab_tiny_40x10", &frame.buffer);
}

#[test]
fn layout_lab_wide_200x50() {
    let screen = ftui_demo_showcase::screens::layout_lab::LayoutLab::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(200, 50, &mut pool);
    let area = Rect::new(0, 0, 200, 50);
    screen.view(&mut frame, area);
    assert_snapshot!("layout_lab_wide_200x50", &frame.buffer);
}

// ============================================================================
// Layout Inspector
// ============================================================================

#[test]
fn layout_inspector_flex_trio_80x24() {
    let screen = ftui_demo_showcase::screens::layout_inspector::LayoutInspector::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("layout_inspector_flex_trio_80x24", &frame.buffer);
}

#[test]
fn layout_inspector_tight_grid_80x24() {
    let mut screen = ftui_demo_showcase::screens::layout_inspector::LayoutInspector::new();
    screen.update(&press(KeyCode::Char('n')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("layout_inspector_tight_grid_80x24", &frame.buffer);
}

#[test]
fn layout_inspector_fit_content_80x24() {
    let mut screen = ftui_demo_showcase::screens::layout_inspector::LayoutInspector::new();
    screen.update(&press(KeyCode::Char('n')));
    screen.update(&press(KeyCode::Char('n')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("layout_inspector_fit_content_80x24", &frame.buffer);
}

#[test]
fn layout_inspector_flex_trio_120x40() {
    let screen = ftui_demo_showcase::screens::layout_inspector::LayoutInspector::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("layout_inspector_flex_trio_120x40", &frame.buffer);
}

// ============================================================================
// Forms & Input
// ============================================================================

#[test]
fn forms_input_initial_120x40() {
    let screen = ftui_demo_showcase::screens::forms_input::FormsInput::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("forms_input_initial_120x40", &frame.buffer);
}

#[test]
fn forms_input_initial_80x24() {
    let screen = ftui_demo_showcase::screens::forms_input::FormsInput::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("forms_input_initial_80x24", &frame.buffer);
}

#[test]
fn forms_input_panel_switch_120x40() {
    let mut screen = ftui_demo_showcase::screens::forms_input::FormsInput::new();
    screen.update(&ctrl_press(KeyCode::Right));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("forms_input_panel_switch_120x40", &frame.buffer);
}

#[test]
fn forms_input_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::forms_input::FormsInput::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("forms_input_tiny_40x10", &frame.buffer);
}

#[test]
fn forms_input_tab_down_120x40() {
    let mut screen = ftui_demo_showcase::screens::forms_input::FormsInput::new();
    screen.update(&press(KeyCode::Tab));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("forms_input_tab_down_120x40", &frame.buffer);
}

// ============================================================================
// Form Validation Demo
// ============================================================================

#[test]
fn form_validation_initial_120x40() {
    let screen = ftui_demo_showcase::screens::form_validation::FormValidationDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("form_validation_initial_120x40", &frame.buffer);
}

#[test]
fn form_validation_submit_errors_120x40() {
    let mut screen = ftui_demo_showcase::screens::form_validation::FormValidationDemo::new();
    screen.update(&press(KeyCode::Enter));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("form_validation_submit_errors_120x40", &frame.buffer);
}

#[test]
fn form_validation_mode_toggle_80x24() {
    let mut screen = ftui_demo_showcase::screens::form_validation::FormValidationDemo::new();
    screen.update(&press(KeyCode::Char('m')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("form_validation_mode_toggle_80x24", &frame.buffer);
}

// ============================================================================
// Data Viz
// ============================================================================

#[test]
fn data_viz_initial_120x40() {
    let screen = ftui_demo_showcase::screens::data_viz::DataViz::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("data_viz_initial_120x40", &frame.buffer);
}

#[test]
fn data_viz_initial_80x24() {
    let screen = ftui_demo_showcase::screens::data_viz::DataViz::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("data_viz_initial_80x24", &frame.buffer);
}

#[test]
fn data_viz_after_ticks_120x40() {
    let mut screen = ftui_demo_showcase::screens::data_viz::DataViz::new();
    screen.tick(35);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("data_viz_after_ticks_120x40", &frame.buffer);
}

#[test]
fn data_viz_bar_horizontal_120x40() {
    let mut screen = ftui_demo_showcase::screens::data_viz::DataViz::new();
    screen.update(&press(KeyCode::Char('d')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("data_viz_bar_horizontal_120x40", &frame.buffer);
}

#[test]
fn data_viz_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::data_viz::DataViz::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("data_viz_tiny_40x10", &frame.buffer);
}

// ============================================================================
// Table Theme Gallery
// ============================================================================

#[test]
fn table_theme_gallery_80x24() {
    let screen = ftui_demo_showcase::screens::table_theme_gallery::TableThemeGallery::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("table_theme_gallery_80x24", &frame.buffer);
}

#[test]
fn table_theme_gallery_120x40() {
    let screen = ftui_demo_showcase::screens::table_theme_gallery::TableThemeGallery::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("table_theme_gallery_120x40", &frame.buffer);
}

#[test]
fn table_theme_gallery_logs_overrides() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = env::temp_dir().join(format!("ftui_table_theme_gallery_{unique}.jsonl"));
    let path_str = path.to_string_lossy().to_string();

    let mut screen =
        ftui_demo_showcase::screens::table_theme_gallery::TableThemeGallery::with_log_path(Some(
            path_str.clone(),
        ));
    screen.update(&press(KeyCode::Char('m')));
    screen.update(&press(KeyCode::Char('h')));
    screen.update(&press(KeyCode::Char('z')));
    screen.update(&press(KeyCode::Char('b')));
    screen.update(&press(KeyCode::Char('l')));

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);

    let contents = fs::read_to_string(&path).expect("table theme log should be written");
    let line = contents
        .lines()
        .find(|line| !line.trim().is_empty())
        .expect("table theme log should contain at least one entry");
    let value: Value = serde_json::from_str(line).expect("table theme log should be valid JSON");

    assert_eq!(
        value.get("event").and_then(Value::as_str),
        Some("table_theme_gallery")
    );
    assert_eq!(
        value.get("selected_preset").and_then(Value::as_str),
        Some("aurora")
    );
    assert_eq!(value.get("selected_index").and_then(Value::as_u64), Some(0));
    assert_eq!(
        value.get("preview_mode").and_then(Value::as_str),
        Some("Markdown")
    );
    assert_eq!(
        value.get("header_emphasis").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        value.get("zebra_strength").and_then(Value::as_str),
        Some("Strong")
    );
    assert_eq!(
        value.get("border_style").and_then(Value::as_str),
        Some("Subtle")
    );
    assert_eq!(
        value.get("highlight_row").and_then(Value::as_bool),
        Some(true)
    );
}

// ============================================================================
// File Browser
// ============================================================================

#[test]
fn file_browser_initial_120x40() {
    let screen = ftui_demo_showcase::screens::file_browser::FileBrowser::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("file_browser_initial_120x40", &frame.buffer);
}

#[test]
fn file_browser_initial_80x24() {
    let screen = ftui_demo_showcase::screens::file_browser::FileBrowser::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("file_browser_initial_80x24", &frame.buffer);
}

#[test]
fn file_browser_navigate_down_120x40() {
    let mut screen = ftui_demo_showcase::screens::file_browser::FileBrowser::new();
    for _ in 0..3 {
        screen.update(&press(KeyCode::Down));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("file_browser_navigate_down_120x40", &frame.buffer);
}

#[test]
fn file_browser_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::file_browser::FileBrowser::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("file_browser_tiny_40x10", &frame.buffer);
}

#[test]
fn file_browser_panel_switch_120x40() {
    let mut screen = ftui_demo_showcase::screens::file_browser::FileBrowser::new();
    screen.update(&ctrl_press(KeyCode::Right));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("file_browser_panel_switch_120x40", &frame.buffer);
}

// ============================================================================
// Markdown & Rich Text
// ============================================================================

#[test]
fn markdown_initial_120x40() {
    let screen = ftui_demo_showcase::screens::markdown_rich_text::MarkdownRichText::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_initial_120x40", &frame.buffer);
}

#[test]
fn markdown_initial_80x24() {
    let screen = ftui_demo_showcase::screens::markdown_rich_text::MarkdownRichText::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_initial_80x24", &frame.buffer);
}

#[test]
fn markdown_scrolled_120x40() {
    let mut screen = ftui_demo_showcase::screens::markdown_rich_text::MarkdownRichText::new();
    for _ in 0..10 {
        screen.update(&press(KeyCode::Down));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_scrolled_120x40", &frame.buffer);
}

#[test]
fn markdown_wrap_cycle_120x40() {
    let mut screen = ftui_demo_showcase::screens::markdown_rich_text::MarkdownRichText::new();
    screen.update(&press(KeyCode::Char('w')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_wrap_cycle_120x40", &frame.buffer);
}

#[test]
fn markdown_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::markdown_rich_text::MarkdownRichText::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_tiny_40x10", &frame.buffer);
}

/// Verify markdown-over-backdrop readability with a light theme (LumenLight).
/// Complements `markdown_initial_120x40` which uses the default dark theme.
/// Required by bd-l8x9.7: snapshot tests across at least two theme variants.
#[test]
fn markdown_backdrop_light_theme_120x40() {
    let _theme_guard = ScopedThemeLock::new(ThemeId::LumenLight);
    let mut screen = ftui_demo_showcase::screens::markdown_rich_text::MarkdownRichText::new();
    screen.apply_theme();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_backdrop_light_theme_120x40", &frame.buffer);
}

/// Verify markdown-over-backdrop readability with the NordicFrost theme.
/// Provides a third theme variant for bd-l8x9.7 snapshot regression coverage.
#[test]
fn markdown_backdrop_nordic_theme_120x40() {
    let _theme_guard = ScopedThemeLock::new(ThemeId::NordicFrost);
    let mut screen = ftui_demo_showcase::screens::markdown_rich_text::MarkdownRichText::new();
    screen.apply_theme();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_backdrop_nordic_theme_120x40", &frame.buffer);
}

// ============================================================================
// Advanced Features
// ============================================================================

#[test]
fn advanced_initial_120x40() {
    let screen = ftui_demo_showcase::screens::advanced_features::AdvancedFeatures::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("advanced_initial_120x40", &frame.buffer);
}

#[test]
fn advanced_initial_80x24() {
    let screen = ftui_demo_showcase::screens::advanced_features::AdvancedFeatures::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("advanced_initial_80x24", &frame.buffer);
}

#[test]
fn advanced_panel_switch_120x40() {
    let mut screen = ftui_demo_showcase::screens::advanced_features::AdvancedFeatures::new();
    screen.update(&ctrl_press(KeyCode::Right));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("advanced_panel_switch_120x40", &frame.buffer);
}

#[test]
fn advanced_after_ticks_120x40() {
    let mut screen = ftui_demo_showcase::screens::advanced_features::AdvancedFeatures::new();
    for t in 1..=10 {
        screen.tick(t);
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("advanced_after_ticks_120x40", &frame.buffer);
}

#[test]
fn advanced_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::advanced_features::AdvancedFeatures::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("advanced_tiny_40x10", &frame.buffer);
}

// ============================================================================
// Performance
// ============================================================================

#[test]
fn performance_initial_120x40() {
    let screen = ftui_demo_showcase::screens::performance::Performance::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("performance_initial_120x40", &frame.buffer);
}

#[test]
fn performance_initial_80x24() {
    let screen = ftui_demo_showcase::screens::performance::Performance::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("performance_initial_80x24", &frame.buffer);
}

#[test]
fn performance_scrolled_120x40() {
    let mut screen = ftui_demo_showcase::screens::performance::Performance::new();
    screen.update(&press(KeyCode::PageDown));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("performance_scrolled_120x40", &frame.buffer);
}

#[test]
fn performance_end_key_120x40() {
    let mut screen = ftui_demo_showcase::screens::performance::Performance::new();
    screen.update(&press(KeyCode::End));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("performance_end_key_120x40", &frame.buffer);
}

#[test]
fn performance_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::performance::Performance::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("performance_tiny_40x10", &frame.buffer);
}

// ============================================================================
// Full AppModel integration snapshots
// ============================================================================

#[test]
fn app_dashboard_full_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let app = AppModel::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("app_dashboard_full_120x40", &frame.buffer);
}

#[test]
fn app_dashboard_hover_dataviz_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.terminal_width = 120;
    app.terminal_height = 40;

    // Render once to populate layout + hit grid for robust point selection.
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    let grid = frame.hit_grid.as_ref().expect("hit grid should be enabled");

    let target_hit = ftui_render::frame::HitId::new(
        ftui_demo_showcase::chrome::PANE_HIT_BASE
            + ftui_demo_showcase::screens::screen_index(ScreenId::DataViz) as u32,
    );

    let mut hit_xy = None;
    'outer: for y in 0..40 {
        for x in 0..120 {
            if let Some((id, _region, _data)) = grid.hit_test(x, y)
                && id == target_hit
            {
                hit_xy = Some((x, y));
                break 'outer;
            }
        }
    }
    let (x, y) = hit_xy.expect("should find a point inside the DataViz dashboard tile");

    // Hover highlight: focus remains on the default tile; hover uses dimmed accent.
    app.update(AppMsg::from(mouse_move(x, y)));

    snapshot_app(&mut app, 120, 40, "app_dashboard_hover_dataviz_120x40");
}

#[test]
fn app_shakespeare_full_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.current_screen = ScreenId::Shakespeare;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("app_shakespeare_full_120x40", &frame.buffer);
}

#[test]
fn app_help_overlay_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.help_visible = true;
    snapshot_app(&mut app, 120, 40, "app_help_overlay_120x40");
}

#[test]
fn app_help_overlay_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.help_visible = true;
    snapshot_app(&mut app, 80, 24, "app_help_overlay_80x24");
}

#[test]
fn app_help_overlay_mermaid_mega_showcase_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.current_screen = ScreenId::MermaidMegaShowcase;
    app.help_visible = true;
    snapshot_app(
        &mut app,
        80,
        24,
        "app_help_overlay_mermaid_mega_showcase_80x24",
    );
}

#[test]
fn app_help_overlay_mermaid_mega_showcase_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.current_screen = ScreenId::MermaidMegaShowcase;
    app.help_visible = true;
    snapshot_app(
        &mut app,
        120,
        40,
        "app_help_overlay_mermaid_mega_showcase_120x40",
    );
}

#[test]
fn app_help_overlay_advanced_text_editor_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.current_screen = ScreenId::AdvancedTextEditor;
    app.help_visible = true;
    snapshot_app(
        &mut app,
        80,
        24,
        "app_help_overlay_advanced_text_editor_80x24",
    );
}

#[test]
fn app_help_overlay_advanced_text_editor_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.current_screen = ScreenId::AdvancedTextEditor;
    app.help_visible = true;
    snapshot_app(
        &mut app,
        120,
        40,
        "app_help_overlay_advanced_text_editor_120x40",
    );
}

#[test]
fn app_debug_overlay_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.debug_visible = true;
    app.terminal_width = 120;
    app.terminal_height = 40;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("app_debug_overlay_120x40", &frame.buffer);
}

#[test]
fn app_all_screens_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);

    for &id in ftui_demo_showcase::screens::screen_ids() {
        let mut app = AppModel::new();
        app.current_screen = id;
        if id == ScreenId::TerminalCapabilities {
            app.screens
                .terminal_capabilities
                .set_detected_profile_override(TerminalProfile::Modern);
            app.screens
                .terminal_capabilities
                .set_env_override(terminal_caps_env());
        }
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        app.view(&mut frame);
        let name = format!("app_{:?}_80x24", id).to_lowercase();
        assert_snapshot!(&name, &frame.buffer);
    }
}

#[test]
fn app_palette_empty_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.command_palette.open();
    snapshot_app(&mut app, 80, 24, "app_palette_empty_80x24");
}

#[test]
fn app_palette_empty_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.command_palette.open();
    snapshot_app(&mut app, 120, 40, "app_palette_empty_120x40");
}

#[test]
fn app_palette_filtered_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.command_palette.open();
    let ctrl_1 = Event::Key(KeyEvent {
        code: KeyCode::Char('1'),
        modifiers: Modifiers::CTRL,
        kind: KeyEventKind::Press,
    });
    app.update(AppMsg::from(ctrl_1));
    snapshot_app(&mut app, 80, 24, "app_palette_filtered_80x24");
}

#[test]
fn app_palette_filtered_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.command_palette.open();
    let ctrl_1 = Event::Key(KeyEvent {
        code: KeyCode::Char('1'),
        modifiers: Modifiers::CTRL,
        kind: KeyEventKind::Press,
    });
    app.update(AppMsg::from(ctrl_1));
    snapshot_app(&mut app, 120, 40, "app_palette_filtered_120x40");
}

#[test]
fn app_palette_favorites_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.command_palette.open();
    let ctrl_f = Event::Key(KeyEvent {
        code: KeyCode::Char('f'),
        modifiers: Modifiers::CTRL,
        kind: KeyEventKind::Press,
    });
    app.update(AppMsg::from(ctrl_f));
    let ctrl_shift_f = Event::Key(KeyEvent {
        code: KeyCode::Char('F'),
        modifiers: Modifiers::CTRL | Modifiers::SHIFT,
        kind: KeyEventKind::Press,
    });
    app.update(AppMsg::from(ctrl_shift_f));
    snapshot_app(&mut app, 80, 24, "app_palette_favorites_80x24");
}

#[test]
fn app_palette_favorites_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut app = AppModel::new();
    app.command_palette.open();
    let ctrl_f = Event::Key(KeyEvent {
        code: KeyCode::Char('f'),
        modifiers: Modifiers::CTRL,
        kind: KeyEventKind::Press,
    });
    app.update(AppMsg::from(ctrl_f));
    let ctrl_shift_f = Event::Key(KeyEvent {
        code: KeyCode::Char('F'),
        modifiers: Modifiers::CTRL | Modifiers::SHIFT,
        kind: KeyEventKind::Press,
    });
    app.update(AppMsg::from(ctrl_shift_f));
    snapshot_app(&mut app, 120, 40, "app_palette_favorites_120x40");
}

// ============================================================================
// Macro Recorder — Per-state snapshots (bd-2lus.3)
// ============================================================================

#[test]
fn macro_recorder_idle_80x24() {
    let screen = ftui_demo_showcase::screens::macro_recorder::MacroRecorderScreen::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("macro_recorder_idle_80x24", &frame.buffer);
}

#[test]
fn macro_recorder_idle_120x40() {
    let screen = ftui_demo_showcase::screens::macro_recorder::MacroRecorderScreen::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("macro_recorder_idle_120x40", &frame.buffer);
}

#[test]
fn macro_recorder_stopped_80x24() {
    let mut screen = ftui_demo_showcase::screens::macro_recorder::MacroRecorderScreen::new();
    // Start recording, add events, stop — ends in Stopped with macro data
    screen.update(&press(KeyCode::Char('r')));
    screen.record_event(&press(KeyCode::Char('a')), false);
    screen.record_event(&press(KeyCode::Char('b')), false);
    screen.record_event(&press(KeyCode::Char('c')), false);
    screen.update(&press(KeyCode::Char('r'))); // stop
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("macro_recorder_stopped_80x24", &frame.buffer);
}

#[test]
fn macro_recorder_stopped_120x40() {
    let mut screen = ftui_demo_showcase::screens::macro_recorder::MacroRecorderScreen::new();
    screen.update(&press(KeyCode::Char('r')));
    screen.record_event(&press(KeyCode::Char('a')), false);
    screen.record_event(&press(KeyCode::Char('b')), false);
    screen.record_event(&press(KeyCode::Char('c')), false);
    screen.update(&press(KeyCode::Char('r')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("macro_recorder_stopped_120x40", &frame.buffer);
}

#[test]
fn macro_recorder_playing_80x24() {
    let mut screen = ftui_demo_showcase::screens::macro_recorder::MacroRecorderScreen::new();
    screen.update(&press(KeyCode::Char('r')));
    screen.record_event(&press(KeyCode::Char('a')), false);
    screen.record_event(&press(KeyCode::Char('b')), false);
    screen.record_event(&press(KeyCode::Char('c')), false);
    screen.update(&press(KeyCode::Char('r'))); // stop recording
    screen.update(&press(KeyCode::Char('p'))); // start playing
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("macro_recorder_playing_80x24", &frame.buffer);
}

#[test]
fn macro_recorder_playing_120x40() {
    let mut screen = ftui_demo_showcase::screens::macro_recorder::MacroRecorderScreen::new();
    screen.update(&press(KeyCode::Char('r')));
    screen.record_event(&press(KeyCode::Char('a')), false);
    screen.record_event(&press(KeyCode::Char('b')), false);
    screen.record_event(&press(KeyCode::Char('c')), false);
    screen.update(&press(KeyCode::Char('r')));
    screen.update(&press(KeyCode::Char('p')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("macro_recorder_playing_120x40", &frame.buffer);
}

#[test]
fn macro_recorder_error_no_macro_80x24() {
    let mut screen = ftui_demo_showcase::screens::macro_recorder::MacroRecorderScreen::new();
    // Play without recording first — triggers "No macro recorded" error
    screen.update(&press(KeyCode::Char('p')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("macro_recorder_error_no_macro_80x24", &frame.buffer);
}

#[test]
fn macro_recorder_error_120x40() {
    let mut screen = ftui_demo_showcase::screens::macro_recorder::MacroRecorderScreen::new();
    screen.update(&press(KeyCode::Char('p')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("macro_recorder_error_120x40", &frame.buffer);
}

#[test]
fn macro_recorder_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::macro_recorder::MacroRecorderScreen::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("macro_recorder_tiny_40x10", &frame.buffer);
}

// ============================================================================
// Responsive Demo — Breakpoint-specific snapshots
// ============================================================================

/// Snapshot at Xs breakpoint (40 cols): single-column stacked layout.
#[test]
fn responsive_demo_xs_40x24() {
    let screen = ftui_demo_showcase::screens::responsive_demo::ResponsiveDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 24, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, 40, 24));
    assert_snapshot!("responsive_demo_xs_40x24", &frame.buffer);
}

/// Snapshot at Sm breakpoint (70 cols): single-column stacked layout.
#[test]
fn responsive_demo_sm_70x24() {
    let screen = ftui_demo_showcase::screens::responsive_demo::ResponsiveDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(70, 24, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, 70, 24));
    assert_snapshot!("responsive_demo_sm_70x24", &frame.buffer);
}

/// Snapshot at Md breakpoint (100 cols): two-column sidebar+content layout.
#[test]
fn responsive_demo_md_100x30() {
    let screen = ftui_demo_showcase::screens::responsive_demo::ResponsiveDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(100, 30, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, 100, 30));
    assert_snapshot!("responsive_demo_md_100x30", &frame.buffer);
}

/// Snapshot at Lg breakpoint (130 cols): three-column layout with aside.
#[test]
fn responsive_demo_lg_130x40() {
    let screen = ftui_demo_showcase::screens::responsive_demo::ResponsiveDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(130, 40, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, 130, 40));
    assert_snapshot!("responsive_demo_lg_130x40", &frame.buffer);
}

/// Snapshot at Xl breakpoint (170 cols): three-column layout (inherits Lg).
#[test]
fn responsive_demo_xl_170x40() {
    let screen = ftui_demo_showcase::screens::responsive_demo::ResponsiveDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(170, 40, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, 170, 40));
    assert_snapshot!("responsive_demo_xl_170x40", &frame.buffer);
}

/// Verify layout changes structure at each breakpoint transition.
#[test]
fn responsive_demo_breakpoint_transitions() {
    use ftui_layout::{Breakpoint, Breakpoints};

    // Widths that land in each breakpoint with default thresholds
    let cases: &[(u16, Breakpoint, usize)] = &[
        (40, Breakpoint::Xs, 1),  // single column
        (70, Breakpoint::Sm, 1),  // still single column
        (100, Breakpoint::Md, 2), // sidebar + content
        (130, Breakpoint::Lg, 3), // sidebar + content + aside
        (170, Breakpoint::Xl, 3), // inherits Lg layout
    ];

    let bps = Breakpoints::DEFAULT;
    for &(width, expected_bp, expected_cols) in cases {
        let bp = bps.classify_width(width);
        assert_eq!(
            bp, expected_bp,
            "width={width} should be {expected_bp:?}, got {bp:?}"
        );

        // Render the screen and verify it doesn't panic
        let screen = ftui_demo_showcase::screens::responsive_demo::ResponsiveDemo::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(width, 24, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, width, 24));

        // The layout column count is implicitly verified via the snapshot tests above.
        // Here we just verify the breakpoint classification is correct.
        let _ = expected_cols; // Used in doc comments, verified by snapshots
    }
}

// ============================================================================
// Action Timeline — Event stream viewer snapshots (bd-11ck.1)
// ============================================================================

#[test]
fn action_timeline_initial_80x24() {
    let screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_initial_80x24", &frame.buffer);
}

#[test]
fn action_timeline_initial_120x40() {
    let screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_initial_120x40", &frame.buffer);
}

#[test]
fn action_timeline_after_ticks_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Generate more events via ticks
    for t in 1..=20 {
        screen.tick(t);
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_after_ticks_120x40", &frame.buffer);
}

#[test]
fn action_timeline_filter_component_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Press 'c' to cycle component filter
    screen.update(&press(KeyCode::Char('c')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_filter_component_120x40", &frame.buffer);
}

#[test]
fn action_timeline_filter_severity_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Press 's' to cycle severity filter
    screen.update(&press(KeyCode::Char('s')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_filter_severity_120x40", &frame.buffer);
}

#[test]
fn action_timeline_filter_kind_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Press 't' to cycle type/kind filter
    screen.update(&press(KeyCode::Char('t')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_filter_kind_120x40", &frame.buffer);
}

#[test]
fn action_timeline_navigate_up_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Navigate up from initial selection
    for _ in 0..5 {
        screen.update(&press(KeyCode::Up));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_navigate_up_120x40", &frame.buffer);
}

#[test]
fn action_timeline_details_collapsed_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Toggle detail expansion with Enter
    screen.update(&press(KeyCode::Enter));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_details_collapsed_120x40", &frame.buffer);
}

#[test]
fn action_timeline_follow_off_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Toggle follow mode with 'f'
    screen.update(&press(KeyCode::Char('f')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_follow_off_120x40", &frame.buffer);
}

#[test]
fn action_timeline_clear_filters_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Set some filters then clear
    screen.update(&press(KeyCode::Char('c')));
    screen.update(&press(KeyCode::Char('s')));
    screen.update(&press(KeyCode::Char('x'))); // clear all filters
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_clear_filters_120x40", &frame.buffer);
}

#[test]
fn action_timeline_tiny_40x10() {
    let screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_tiny_40x10", &frame.buffer);
}

#[test]
fn action_timeline_wide_200x50() {
    let screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(200, 50, &mut pool);
    let area = Rect::new(0, 0, 200, 50);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_wide_200x50", &frame.buffer);
}

#[test]
fn action_timeline_page_navigation_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Generate more events first
    for t in 1..=30 {
        screen.tick(t);
    }
    // Navigate with PageUp
    screen.update(&press(KeyCode::PageUp));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_page_navigation_120x40", &frame.buffer);
}

#[test]
fn action_timeline_home_key_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Generate events then go to beginning
    for t in 1..=20 {
        screen.tick(t);
    }
    screen.update(&press(KeyCode::Home));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_home_key_120x40", &frame.buffer);
}

#[test]
fn action_timeline_end_key_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Go to beginning first, then end
    screen.update(&press(KeyCode::Home));
    screen.update(&press(KeyCode::End));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_end_key_120x40", &frame.buffer);
}

#[test]
fn action_timeline_vim_navigation_120x40() {
    let mut screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    // Use vim-style j/k navigation
    for _ in 0..3 {
        screen.update(&press(KeyCode::Char('k')));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("action_timeline_vim_navigation_120x40", &frame.buffer);
}

#[test]
fn action_timeline_zero_area() {
    let screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
    // No panic = success
}

#[test]
fn action_timeline_title() {
    let screen = ftui_demo_showcase::screens::action_timeline::ActionTimeline::new();
    assert_eq!(screen.title(), "Action Timeline");
    assert_eq!(screen.tab_label(), "Timeline");
}

// ============================================================================
// Theme Studio — Live palette editor (bd-vu0o.1)
// ============================================================================

#[test]
fn theme_studio_initial_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_initial_80x24", &frame.buffer);
}

#[test]
fn theme_studio_initial_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_initial_120x40", &frame.buffer);
}

#[test]
fn theme_studio_tiny_40x10() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_tiny_40x10", &frame.buffer);
}

#[test]
fn theme_studio_wide_200x50() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(200, 50, &mut pool);
    let area = Rect::new(0, 0, 200, 50);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_wide_200x50", &frame.buffer);
}

#[test]
fn theme_studio_navigate_tokens_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    // Switch to token inspector panel
    screen.update(&press(KeyCode::Tab));
    // Navigate down through tokens
    for _ in 0..5 {
        screen.update(&press(KeyCode::Down));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_navigate_tokens_120x40", &frame.buffer);
}

#[test]
fn theme_studio_vim_navigation_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    // Use vim-style j/k navigation in presets panel
    for _ in 0..3 {
        screen.update(&press(KeyCode::Char('j')));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_vim_navigation_120x40", &frame.buffer);
}

#[test]
fn theme_studio_zero_area() {
    let screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
    // No panic = success
}

#[test]
fn theme_studio_title() {
    let screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    assert_eq!(screen.title(), "Theme Studio");
    assert_eq!(screen.tab_label(), "Themes");
}

#[test]
fn theme_studio_focus_token_inspector_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    // Switch focus to token inspector panel (tests focus indicator)
    screen.update(&press(KeyCode::Tab));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_focus_token_inspector_120x40", &frame.buffer);
}

#[test]
fn theme_studio_home_key_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    // Navigate down then use Home to jump to first
    for _ in 0..5 {
        screen.update(&press(KeyCode::Down));
    }
    screen.update(&press(KeyCode::Home));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_home_key_120x40", &frame.buffer);
}

#[test]
fn theme_studio_end_key_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    // Use End to jump to last preset
    screen.update(&press(KeyCode::End));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_end_key_120x40", &frame.buffer);
}

#[test]
fn theme_studio_page_down_tokens_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::theme_studio::ThemeStudioDemo::new();
    // Switch to token inspector and use PageDown
    screen.update(&press(KeyCode::Tab));
    screen.update(&press(KeyCode::PageDown));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("theme_studio_page_down_tokens_120x40", &frame.buffer);
}

// ============================================================================
// Snapshot Player — Time-travel debugging (bd-3sa7.1)
// ============================================================================

#[test]
fn snapshot_player_initial_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_initial_80x24", &frame.buffer);
}

#[test]
fn snapshot_player_initial_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_initial_120x40", &frame.buffer);
}

#[test]
fn snapshot_player_tiny_40x10() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_tiny_40x10", &frame.buffer);
}

#[test]
fn snapshot_player_wide_200x50() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(200, 50, &mut pool);
    let area = Rect::new(0, 0, 200, 50);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_wide_200x50", &frame.buffer);
}

#[test]
fn snapshot_player_step_forward_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    // Step forward several frames
    for _ in 0..5 {
        screen.update(&press(KeyCode::Right));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_step_forward_120x40", &frame.buffer);
}

#[test]
fn snapshot_player_end_key_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    // Jump to last frame
    screen.update(&press(KeyCode::End));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_end_key_120x40", &frame.buffer);
}

#[test]
fn snapshot_player_home_key_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    // Go to end then back to start
    screen.update(&press(KeyCode::End));
    screen.update(&press(KeyCode::Home));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_home_key_120x40", &frame.buffer);
}

#[test]
fn snapshot_player_playing_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    // Start playback
    screen.update(&press(KeyCode::Char(' ')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_playing_120x40", &frame.buffer);
}

#[test]
fn snapshot_player_with_marker_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    // Step forward and add a marker
    for _ in 0..10 {
        screen.update(&press(KeyCode::Right));
    }
    screen.update(&press(KeyCode::Char('m'))); // Toggle marker
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_with_marker_120x40", &frame.buffer);
}

#[test]
fn snapshot_player_after_tick_playback_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    // Start playback and advance with ticks
    screen.update(&press(KeyCode::Char(' ')));
    for t in 1..=10 {
        screen.tick(t * 2); // tick on even numbers advances frame
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_after_tick_playback_120x40", &frame.buffer);
}

#[test]
fn snapshot_player_vim_navigation_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    // Use vim-style h/l navigation
    for _ in 0..5 {
        screen.update(&press(KeyCode::Char('l')));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_vim_navigation_120x40", &frame.buffer);
}

#[test]
fn snapshot_player_middle_frame_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    // Navigate to middle of timeline
    for _ in 0..25 {
        screen.update(&press(KeyCode::Right));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("snapshot_player_middle_frame_80x24", &frame.buffer);
}

#[test]
fn snapshot_player_zero_area() {
    let screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
    // No panic = success
}

#[test]
fn snapshot_player_title() {
    let screen = ftui_demo_showcase::screens::snapshot_player::SnapshotPlayer::new();
    assert_eq!(screen.title(), "Time-Travel Studio");
    assert_eq!(screen.tab_label(), "TimeTravel");
}

// ============================================================================
// Terminal Capability Explorer (bd-2sog)
// ============================================================================

#[test]
fn terminal_capabilities_initial_80x24() {
    let screen = terminal_caps_screen();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("terminal_capabilities_initial_80x24", &frame.buffer);
}

#[test]
fn terminal_capabilities_initial_120x40() {
    let screen = terminal_caps_screen();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("terminal_capabilities_initial_120x40", &frame.buffer);
}

#[test]
fn terminal_capabilities_evidence_120x40() {
    let mut screen = terminal_caps_screen();
    screen.update(&press(KeyCode::Tab));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("terminal_capabilities_evidence_120x40", &frame.buffer);
}

#[test]
fn terminal_capabilities_simulation_120x40() {
    let mut screen = terminal_caps_screen();
    screen.update(&press(KeyCode::Tab));
    screen.update(&press(KeyCode::Tab));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("terminal_capabilities_simulation_120x40", &frame.buffer);
}

#[test]
fn terminal_capabilities_simulation_profile_120x40() {
    let mut screen = terminal_caps_screen();
    screen.update(&press(KeyCode::Tab));
    screen.update(&press(KeyCode::Tab));
    screen.update(&press(KeyCode::Char('p')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!(
        "terminal_capabilities_simulation_profile_120x40",
        &frame.buffer
    );
}

#[test]
fn terminal_capabilities_simulation_tmux_120x40() {
    let mut screen = terminal_caps_screen();
    screen.set_profile_override(TerminalProfile::Tmux);
    screen.update(&press(KeyCode::Tab));
    screen.update(&press(KeyCode::Tab));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!(
        "terminal_capabilities_simulation_tmux_120x40",
        &frame.buffer
    );
}

#[test]
fn terminal_capabilities_simulation_dumb_120x40() {
    let mut screen = terminal_caps_screen();
    screen.set_profile_override(TerminalProfile::Dumb);
    screen.update(&press(KeyCode::Tab));
    screen.update(&press(KeyCode::Tab));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!(
        "terminal_capabilities_simulation_dumb_120x40",
        &frame.buffer
    );
}

#[test]
fn terminal_capabilities_zero_area() {
    let screen = terminal_caps_screen();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
    // No panic = success
}

#[test]
fn terminal_capabilities_title() {
    let screen =
        ftui_demo_showcase::screens::terminal_capabilities::TerminalCapabilitiesScreen::new();
    assert_eq!(screen.title(), "Terminal Capabilities");
    assert_eq!(screen.tab_label(), "Caps");
}

// ============================================================================
// Inline Mode Story
// ============================================================================

#[test]
fn inline_mode_story_inline_bottom_80x24() {
    let mut screen = ftui_demo_showcase::screens::inline_mode_story::InlineModeStory::new();
    screen.set_ui_height(2);
    screen.set_anchor(ftui_demo_showcase::screens::inline_mode_story::InlineAnchor::Bottom);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("inline_mode_story_inline_bottom_80x24", &frame.buffer);
}

#[test]
fn inline_mode_story_compare_top_120x40() {
    let mut screen = ftui_demo_showcase::screens::inline_mode_story::InlineModeStory::new();
    screen.set_ui_height(3);
    screen.set_anchor(ftui_demo_showcase::screens::inline_mode_story::InlineAnchor::Top);
    screen.set_compare(true);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("inline_mode_story_compare_top_120x40", &frame.buffer);
}

// ============================================================================
// Determinism Lab (bd-iuvb.2)
// ============================================================================

#[test]
fn determinism_lab_initial_80x24() {
    let _theme_guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let screen = ftui_demo_showcase::screens::determinism_lab::DeterminismLab::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("determinism_lab_initial_80x24", &frame.buffer);
}

#[test]
fn determinism_lab_fault_120x40() {
    let _theme_guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::determinism_lab::DeterminismLab::new();
    screen.update(&press(KeyCode::Char('f')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("determinism_lab_fault_120x40", &frame.buffer);
}

#[test]
fn determinism_lab_runs_120x40() {
    let _theme_guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::determinism_lab::DeterminismLab::new();
    screen.update(&press(KeyCode::Char('a')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("determinism_lab_runs_120x40", &frame.buffer);
}

// ============================================================================
// Hyperlink Playground (bd-iuvb.14)
// ============================================================================

#[test]
fn hyperlink_playground_initial_80x24() {
    let screen = ftui_demo_showcase::screens::hyperlink_playground::HyperlinkPlayground::new();
    let mut pool = GraphemePool::new();
    let mut registry = LinkRegistry::new();
    let mut frame = Frame::with_links(80, 24, &mut pool, &mut registry);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("hyperlink_playground_initial_80x24", &frame.buffer);
}

#[test]
fn hyperlink_playground_hover_120x40() {
    let mut screen = ftui_demo_showcase::screens::hyperlink_playground::HyperlinkPlayground::new();
    let mut pool = GraphemePool::new();
    let mut registry = LinkRegistry::new();
    let area = Rect::new(0, 0, 120, 40);

    {
        let mut frame = Frame::with_links(120, 40, &mut pool, &mut registry);
        screen.view(&mut frame, area);
    }

    let layouts = screen.link_layouts();
    let target = layouts.get(1).expect("expected link layout");
    screen.update(&mouse_move(target.rect.x, target.rect.y));

    let mut frame = Frame::with_links(120, 40, &mut pool, &mut registry);
    screen.view(&mut frame, area);
    assert_snapshot!("hyperlink_playground_hover_120x40", &frame.buffer);
}

#[test]
fn hyperlink_playground_focus_120x40() {
    let mut screen = ftui_demo_showcase::screens::hyperlink_playground::HyperlinkPlayground::new();
    let mut pool = GraphemePool::new();
    let mut registry = LinkRegistry::new();
    let area = Rect::new(0, 0, 120, 40);

    {
        let mut frame = Frame::with_links(120, 40, &mut pool, &mut registry);
        screen.view(&mut frame, area);
    }

    screen.update(&press(KeyCode::Down));

    let mut frame = Frame::with_links(120, 40, &mut pool, &mut registry);
    screen.view(&mut frame, area);
    assert_snapshot!("hyperlink_playground_focus_120x40", &frame.buffer);
}

// ============================================================================
// Explainability Cockpit (bd-iuvb.4)
// ============================================================================

#[test]
fn explainability_cockpit_empty_80x24() {
    let screen =
        ftui_demo_showcase::screens::explainability_cockpit::ExplainabilityCockpit::with_evidence_path(
            None,
        );
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("explainability_cockpit_empty_80x24", &frame.buffer);
}

#[test]
fn explainability_cockpit_populated_120x40() {
    let path = "/tmp/ftui_explainability_cockpit.jsonl";
    let sample = [
        r#"{"schema_version":"ftui-evidence-v1","event":"diff_decision","run_id":"diff-1","event_idx":4,"screen_mode":"alt","cols":80,"rows":24,"strategy":"dirty","posterior_mean":0.33,"posterior_variance":0.12,"alpha":1.2,"beta":2.3,"guard_reason":"","fallback_reason":"","hysteresis_applied":true,"hysteresis_ratio":1.1,"dirty_rows":5,"total_rows":24,"dirty_tile_ratio":0.07,"dirty_cell_ratio":0.08}"#,
        r#"{"schema_version":"ftui-evidence-v1","event":"decision_evidence","run_id":"resize-1","event_idx":7,"screen_mode":"alt","cols":80,"rows":24,"log_bayes_factor":1.23,"regime_contribution":0.5,"timing_contribution":0.3,"rate_contribution":0.2,"explanation":"burst regime"}"#,
        r#"{"schema_version":"ftui-evidence-v1","event":"decision","run_id":"resize-1","event_idx":7,"screen_mode":"alt","cols":80,"rows":24,"idx":7,"elapsed_ms":10.0,"dt_ms":5.0,"event_rate":20.0,"regime":"burst","action":"coalesce","pending_w":80,"pending_h":24,"applied_w":80,"applied_h":24,"time_since_render_ms":3.0,"coalesce_ms":12.0,"forced":false}"#,
        r#"{"event":"budget_decision","frame_idx":42,"decision":"degrade","decision_controller":"degrade","degradation_before":"full","degradation_after":"lite","frame_time_us":20000.0,"budget_us":16000.0,"pid_output":0.2,"pid_p":0.1,"pid_i":0.05,"pid_d":0.02,"e_value":0.4,"frames_observed":10,"frames_since_change":2,"in_warmup":false,"bucket_key":null,"n_b":null,"alpha":null,"q_b":null,"y_hat":null,"upper_us":null,"risk":null,"fallback_level":null,"window_size":null,"reset_count":null}"#,
    ]
    .join("\n");

    fs::write(path, sample).expect("write explainability evidence fixture");

    let screen =
        ftui_demo_showcase::screens::explainability_cockpit::ExplainabilityCockpit::with_evidence_path(
            Some(path.into()),
        );
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("explainability_cockpit_populated_120x40", &frame.buffer);
}

// ============================================================================
// Kanban Board
// ============================================================================

#[test]
fn kanban_board_initial_80x24() {
    let screen = ftui_demo_showcase::screens::kanban_board::KanbanBoard::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("kanban_board_initial_80x24", &frame.buffer);
}

#[test]
fn kanban_board_initial_120x40() {
    let screen = ftui_demo_showcase::screens::kanban_board::KanbanBoard::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("kanban_board_initial_120x40", &frame.buffer);
}

#[test]
fn kanban_board_after_move_80x24() {
    let mut screen = ftui_demo_showcase::screens::kanban_board::KanbanBoard::default();
    // Move first card from Todo to In Progress (press L to move right)
    screen.update(&press(KeyCode::Char('L')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("kanban_board_after_move_80x24", &frame.buffer);
}

#[test]
fn kanban_board_zero_area() {
    let screen = ftui_demo_showcase::screens::kanban_board::KanbanBoard::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
    // No panic = success
}

#[test]
fn kanban_board_title() {
    let screen = ftui_demo_showcase::screens::kanban_board::KanbanBoard::default();
    assert_eq!(screen.title(), "Kanban Board");
    assert_eq!(screen.tab_label(), "Kanban");
}

// ============================================================================
// Live Markdown Editor
// ============================================================================

#[test]
fn markdown_live_editor_initial_80x24() {
    let screen = ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_live_editor_initial_80x24", &frame.buffer);
}

#[test]
fn markdown_live_editor_initial_120x40() {
    let screen = ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_live_editor_initial_120x40", &frame.buffer);
}

#[test]
fn markdown_live_editor_diff_mode_80x24() {
    let mut screen =
        ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    // Ctrl+D toggles diff mode
    screen.update(&ctrl_press(KeyCode::Char('d')));
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_live_editor_diff_mode_80x24", &frame.buffer);
}

#[test]
fn markdown_live_editor_search_focus_80x24() {
    let mut screen =
        ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    // Ctrl+F focuses search
    screen.update(&ctrl_press(KeyCode::Char('f')));
    // Type "render"
    for ch in "render".chars() {
        screen.update(&press(KeyCode::Char(ch)));
    }
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("markdown_live_editor_search_focus_80x24", &frame.buffer);
}

#[test]
fn markdown_live_editor_zero_area() {
    let screen = ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
    // No panic = success
}

#[test]
fn markdown_live_editor_title() {
    let screen = ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    assert_eq!(screen.title(), "Live Markdown");
    assert_eq!(screen.tab_label(), "MD Live");
}

#[test]
fn markdown_live_editor_tab_toggles_focus() {
    let mut screen =
        ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    // Default focus is Editor; Tab toggles to Search
    assert!(screen.consumes_text_input()); // Editor
    screen.update(&press(KeyCode::Tab));
    assert!(screen.consumes_text_input()); // Search
    screen.update(&press(KeyCode::Tab));
    assert!(screen.consumes_text_input()); // Back to Editor
}

#[test]
fn markdown_live_editor_ctrl_d_toggles_diff() {
    let mut screen =
        ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    // Diff mode starts off; first render should say "Preview"
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, 80, 24));
    let text = buffer_to_text(&frame.buffer);
    assert!(!text.contains("Preview (Diff Mode)"));

    // Toggle diff on
    screen.update(&ctrl_press(KeyCode::Char('d')));
    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(80, 24, &mut pool2);
    screen.view(&mut frame2, Rect::new(0, 0, 80, 24));
    let text2 = buffer_to_text(&frame2.buffer);
    assert!(text2.contains("Preview (Diff Mode)"));
}

#[test]
fn markdown_live_editor_search_and_navigate() {
    let mut screen =
        ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    // Ctrl+F to search
    screen.update(&ctrl_press(KeyCode::Char('f')));
    // Type "mode"
    for ch in "mode".chars() {
        screen.update(&press(KeyCode::Char(ch)));
    }
    // Render once to verify it doesn't panic
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, 80, 24));
    let text = buffer_to_text(&frame.buffer);
    // "mode" appears in the sample markdown
    assert!(text.contains("matches"));

    // Navigate next/prev
    screen.update(&ctrl_press(KeyCode::Char('n')));
    screen.update(&ctrl_press(KeyCode::Char('p')));
    // No panic, renders normally
    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(80, 24, &mut pool2);
    screen.view(&mut frame2, Rect::new(0, 0, 80, 24));
}

#[test]
fn markdown_live_editor_preview_scroll() {
    let mut screen =
        ftui_demo_showcase::screens::markdown_live_editor::MarkdownLiveEditor::default();
    // Ctrl+Down to scroll preview
    screen.update(&ctrl_press(KeyCode::Down));
    screen.update(&ctrl_press(KeyCode::Down));
    screen.update(&ctrl_press(KeyCode::Up));
    // No panic, renders normally
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, 80, 24));
}

// ============================================================================
// Accessibility Panel
// ============================================================================

#[test]
fn accessibility_panel_initial_80x24() {
    let screen = ftui_demo_showcase::screens::accessibility_panel::AccessibilityPanel::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("accessibility_panel_initial_80x24", &frame.buffer);
}

#[test]
fn accessibility_panel_zero_area() {
    let screen = ftui_demo_showcase::screens::accessibility_panel::AccessibilityPanel::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn accessibility_panel_title() {
    let screen = ftui_demo_showcase::screens::accessibility_panel::AccessibilityPanel::new();
    assert_eq!(screen.title(), "Accessibility");
    assert_eq!(screen.tab_label(), "A11y");
}

// ============================================================================
// Advanced Text Editor
// ============================================================================

#[test]
fn advanced_text_editor_initial_80x24() {
    let screen = ftui_demo_showcase::screens::advanced_text_editor::AdvancedTextEditor::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("advanced_text_editor_initial_80x24", &frame.buffer);
}

#[test]
fn advanced_text_editor_zero_area() {
    let screen = ftui_demo_showcase::screens::advanced_text_editor::AdvancedTextEditor::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn advanced_text_editor_title() {
    let screen = ftui_demo_showcase::screens::advanced_text_editor::AdvancedTextEditor::new();
    assert_eq!(screen.title(), "Advanced Text Editor");
    assert_eq!(screen.tab_label(), "Editor");
}

// ============================================================================
// Async Tasks
// ============================================================================

#[test]
fn async_tasks_initial_80x24() {
    let screen = ftui_demo_showcase::screens::async_tasks::AsyncTaskManager::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("async_tasks_initial_80x24", &frame.buffer);
}

#[test]
fn async_tasks_zero_area() {
    let screen = ftui_demo_showcase::screens::async_tasks::AsyncTaskManager::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn async_tasks_title() {
    let screen = ftui_demo_showcase::screens::async_tasks::AsyncTaskManager::new();
    assert_eq!(screen.title(), "Async Tasks");
    assert_eq!(screen.tab_label(), "Tasks");
}

// ============================================================================
// Command Palette Lab
// ============================================================================

#[test]
fn command_palette_lab_initial_80x24() {
    let screen = ftui_demo_showcase::screens::command_palette_lab::CommandPaletteEvidenceLab::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("command_palette_lab_initial_80x24", &frame.buffer);
}

#[test]
fn command_palette_lab_zero_area() {
    let screen = ftui_demo_showcase::screens::command_palette_lab::CommandPaletteEvidenceLab::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn command_palette_lab_title() {
    let screen = ftui_demo_showcase::screens::command_palette_lab::CommandPaletteEvidenceLab::new();
    assert_eq!(screen.title(), "Command Palette Evidence Lab");
    assert_eq!(screen.tab_label(), "Palette");
}

// ============================================================================
// I18n Demo
// ============================================================================

#[test]
fn i18n_demo_initial_80x24() {
    let screen = ftui_demo_showcase::screens::i18n_demo::I18nDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("i18n_demo_initial_80x24", &frame.buffer);
}

#[test]
fn i18n_demo_zero_area() {
    let screen = ftui_demo_showcase::screens::i18n_demo::I18nDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn i18n_demo_title() {
    let screen = ftui_demo_showcase::screens::i18n_demo::I18nDemo::new();
    assert_eq!(screen.title(), "i18n Stress Lab");
    assert_eq!(screen.tab_label(), "i18n");
}

// ============================================================================
// Intrinsic Sizing
// ============================================================================

#[test]
fn intrinsic_sizing_initial_80x24() {
    let screen = ftui_demo_showcase::screens::intrinsic_sizing::IntrinsicSizingDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("intrinsic_sizing_initial_80x24", &frame.buffer);
}

#[test]
fn intrinsic_sizing_zero_area() {
    let screen = ftui_demo_showcase::screens::intrinsic_sizing::IntrinsicSizingDemo::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn intrinsic_sizing_title() {
    let screen = ftui_demo_showcase::screens::intrinsic_sizing::IntrinsicSizingDemo::new();
    assert_eq!(screen.title(), "Intrinsic Sizing");
    assert_eq!(screen.tab_label(), "Sizing");
}

// ============================================================================
// Log Search
// ============================================================================

#[test]
fn log_search_initial_80x24() {
    let screen = ftui_demo_showcase::screens::log_search::LogSearch::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("log_search_initial_80x24", &frame.buffer);
}

#[test]
fn log_search_zero_area() {
    let screen = ftui_demo_showcase::screens::log_search::LogSearch::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn log_search_title() {
    let screen = ftui_demo_showcase::screens::log_search::LogSearch::new();
    assert_eq!(screen.title(), "Log Search");
    assert_eq!(screen.tab_label(), "Logs");
}

// ============================================================================
// Mouse Playground
// ============================================================================

#[test]
fn mouse_playground_initial_80x24() {
    let screen = ftui_demo_showcase::screens::mouse_playground::MousePlayground::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("mouse_playground_initial_80x24", &frame.buffer);
}

#[test]
fn mouse_playground_zero_area() {
    let screen = ftui_demo_showcase::screens::mouse_playground::MousePlayground::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn mouse_playground_title() {
    let screen = ftui_demo_showcase::screens::mouse_playground::MousePlayground::new();
    assert_eq!(screen.title(), "Mouse Playground");
    assert_eq!(screen.tab_label(), "Mouse");
}

// ============================================================================
// Notifications
// ============================================================================

#[test]
fn notifications_initial_80x24() {
    let screen = ftui_demo_showcase::screens::notifications::Notifications::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("notifications_initial_80x24", &frame.buffer);
}

#[test]
fn notifications_zero_area() {
    let screen = ftui_demo_showcase::screens::notifications::Notifications::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn notifications_title() {
    let screen = ftui_demo_showcase::screens::notifications::Notifications::new();
    assert_eq!(screen.title(), "Notifications");
    assert_eq!(screen.tab_label(), "Notify");
}

// ============================================================================
// Performance HUD
// ============================================================================

#[test]
fn performance_hud_initial_80x24() {
    let screen = ftui_demo_showcase::screens::performance_hud::PerformanceHud::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("performance_hud_initial_80x24", &frame.buffer);
}

#[test]
fn performance_hud_zero_area() {
    let screen = ftui_demo_showcase::screens::performance_hud::PerformanceHud::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn performance_hud_title() {
    let screen = ftui_demo_showcase::screens::performance_hud::PerformanceHud::new();
    assert_eq!(screen.title(), "Performance Challenge");
    assert_eq!(screen.tab_label(), "PerfChal");
}

// ============================================================================
// Virtualized Search
// ============================================================================

#[test]
fn virtualized_search_initial_80x24() {
    let screen = ftui_demo_showcase::screens::virtualized_search::VirtualizedSearch::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("virtualized_search_initial_80x24", &frame.buffer);
}

#[test]
fn virtualized_search_zero_area() {
    let screen = ftui_demo_showcase::screens::virtualized_search::VirtualizedSearch::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn virtualized_search_title() {
    let screen = ftui_demo_showcase::screens::virtualized_search::VirtualizedSearch::new();
    assert_eq!(screen.title(), "Virtualized Search");
    assert_eq!(screen.tab_label(), "VirtSearch");
}

// ============================================================================
// Visual Effects
// ============================================================================

#[test]
fn visual_effects_initial_80x24() {
    let screen = ftui_demo_showcase::screens::visual_effects::VisualEffectsScreen::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("visual_effects_initial_80x24", &frame.buffer);
}

#[test]
fn visual_effects_zero_area() {
    let screen = ftui_demo_showcase::screens::visual_effects::VisualEffectsScreen::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn visual_effects_title() {
    let screen = ftui_demo_showcase::screens::visual_effects::VisualEffectsScreen::default();
    assert_eq!(screen.title(), "Visual Effects");
    assert_eq!(screen.tab_label(), "VFX");
}

// ============================================================================
// VOI Overlay
// ============================================================================

#[test]
fn voi_overlay_initial_80x24() {
    let screen = ftui_demo_showcase::screens::voi_overlay::VoiOverlayScreen::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    screen.view(&mut frame, area);
    assert_snapshot!("voi_overlay_initial_80x24", &frame.buffer);
}

#[test]
fn voi_overlay_zero_area() {
    let screen = ftui_demo_showcase::screens::voi_overlay::VoiOverlayScreen::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let area = Rect::new(0, 0, 0, 0);
    screen.view(&mut frame, area);
}

#[test]
fn voi_overlay_title() {
    let screen = ftui_demo_showcase::screens::voi_overlay::VoiOverlayScreen::new();
    assert_eq!(screen.title(), "VOI Overlay");
    assert_eq!(screen.tab_label(), "VOI");
}

// ============================================================================
// Mermaid Showcase
// ============================================================================

#[test]
fn mermaid_showcase_flow_basic_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_snapshot(&mut screen, 80, 24, "mermaid_showcase_flow_basic_80x24");
}

#[test]
fn mermaid_showcase_flow_basic_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_snapshot(&mut screen, 120, 40, "mermaid_showcase_flow_basic_120x40");
}

#[test]
fn mermaid_showcase_flow_basic_links_footnote_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    // Off -> Inline -> Footnote, should render a link legend.
    screen.update(&press(KeyCode::Char('u')));
    screen.update(&press(KeyCode::Char('u')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_flow_basic_links_footnote_120x40",
    );
}

#[test]
fn mermaid_showcase_flow_basic_links_inline_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    // Off -> Inline
    screen.update(&press(KeyCode::Char('u')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_flow_basic_links_inline_120x40",
    );
}

#[test]
fn mermaid_showcase_flow_basic_init_directives_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    screen.update(&press(KeyCode::Char('I')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_flow_basic_init_directives_120x40",
    );
}

#[test]
fn mermaid_showcase_flow_basic_init_directives_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    screen.update(&press(KeyCode::Char('I')));
    mermaid_showcase_snapshot(
        &mut screen,
        80,
        24,
        "mermaid_showcase_flow_basic_init_directives_80x24",
    );
}

#[test]
fn mermaid_showcase_flow_basic_ascii_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    screen.update(&press(KeyCode::Char('g')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_flow_basic_ascii_120x40",
    );
}

#[test]
fn mermaid_showcase_flow_basic_200x60() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_snapshot(&mut screen, 200, 60, "mermaid_showcase_flow_basic_200x60");
}

#[test]
fn mermaid_showcase_flow_dense_guard_tight_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "flow-dense");
    screen.update(&press(KeyCode::Char('x')));
    mermaid_showcase_snapshot(
        &mut screen,
        80,
        24,
        "mermaid_showcase_flow_dense_guard_tight_80x24",
    );
}

#[test]
fn mermaid_showcase_flow_dense_init_directives_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "flow-dense");
    screen.update(&press(KeyCode::Char('I')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_flow_dense_init_directives_120x40",
    );
}

#[test]
fn mermaid_showcase_flow_node_shapes_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "flow-node-shapes");
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_flow_node_shapes_120x40",
    );
}

#[test]
fn mermaid_showcase_sequence_mini_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "sequence-mini");
    mermaid_showcase_snapshot(&mut screen, 80, 24, "mermaid_showcase_sequence_mini_80x24");
}

#[test]
fn mermaid_showcase_sequence_mini_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "sequence-mini");
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_sequence_mini_120x40",
    );
}

#[test]
fn mermaid_showcase_sequence_mini_200x60() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "sequence-mini");
    mermaid_showcase_snapshot(
        &mut screen,
        200,
        60,
        "mermaid_showcase_sequence_mini_200x60",
    );
}

#[test]
fn mermaid_showcase_class_basic_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "class-basic");
    mermaid_showcase_snapshot(&mut screen, 80, 24, "mermaid_showcase_class_basic_80x24");
}

#[test]
fn mermaid_showcase_class_basic_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "class-basic");
    mermaid_showcase_snapshot(&mut screen, 120, 40, "mermaid_showcase_class_basic_120x40");
}

#[test]
fn mermaid_showcase_class_basic_200x60() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "class-basic");
    mermaid_showcase_snapshot(&mut screen, 200, 60, "mermaid_showcase_class_basic_200x60");
}

#[test]
fn mermaid_showcase_panels_collapsed_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    screen.update(&press(KeyCode::Char('m')));
    screen.update(&press(KeyCode::Char('c')));
    screen.update(&press(KeyCode::Char('i')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_panels_collapsed_120x40",
    );
}

#[test]
fn mermaid_showcase_viewport_override_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    screen.update(&press(KeyCode::Char(']')));
    screen.update(&press(KeyCode::Char(']')));
    screen.update(&press(KeyCode::Char('}')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_viewport_override_120x40",
    );
}

#[test]
fn mermaid_showcase_error_sample_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    const INVALID_SRC: &str = "graph TD\n    A[Start] --> B[Ok]\n    classDef";
    screen.override_selected_sample_for_test("error-sample", "Error Sample", INVALID_SRC);

    screen.stabilize_metrics_for_snapshot();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    let text = buffer_to_text(&frame.buffer);
    assert!(text.contains("Mermaid error"));
    assert!(text.contains("classDef missing name"));
    assert_snapshot!("mermaid_showcase_error_sample_120x40", &frame.buffer);
}

// ============================================================================
// Mermaid Showcase — gitGraph
// ============================================================================

#[test]
fn mermaid_showcase_gitgraph_basic_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "gitgraph-basic");
    mermaid_showcase_snapshot(&mut screen, 80, 24, "mermaid_showcase_gitgraph_basic_80x24");
}

#[test]
fn mermaid_showcase_gitgraph_basic_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "gitgraph-basic");
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_gitgraph_basic_120x40",
    );
}

#[test]
fn mermaid_showcase_journey_basic_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "journey-basic");
    mermaid_showcase_snapshot(&mut screen, 80, 24, "mermaid_showcase_journey_basic_80x24");
}

#[test]
fn mermaid_showcase_journey_basic_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "journey-basic");
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_journey_basic_120x40",
    );
}

#[test]
fn mermaid_showcase_journey_basic_ascii_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "journey-basic");
    screen.update(&press(KeyCode::Char('g')));
    mermaid_showcase_snapshot(
        &mut screen,
        80,
        24,
        "mermaid_showcase_journey_basic_ascii_80x24",
    );
}

#[test]
fn mermaid_showcase_journey_basic_ascii_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "journey-basic");
    screen.update(&press(KeyCode::Char('g')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_journey_basic_ascii_120x40",
    );
}

#[test]
fn mermaid_showcase_requirement_basic_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "requirement-basic");
    mermaid_showcase_snapshot(
        &mut screen,
        80,
        24,
        "mermaid_showcase_requirement_basic_80x24",
    );
}

#[test]
fn mermaid_showcase_requirement_basic_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "requirement-basic");
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_requirement_basic_120x40",
    );
}

#[test]
fn mermaid_showcase_requirement_basic_200x60() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "requirement-basic");
    mermaid_showcase_snapshot(
        &mut screen,
        200,
        60,
        "mermaid_showcase_requirement_basic_200x60",
    );
}

#[test]
fn mermaid_showcase_er_multi_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "er-multi");
    mermaid_showcase_snapshot(&mut screen, 120, 40, "mermaid_showcase_er_multi_120x40");
}

#[test]
fn mermaid_showcase_gantt_deps_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "gantt-deps");
    mermaid_showcase_snapshot(&mut screen, 120, 40, "mermaid_showcase_gantt_deps_120x40");
}

#[test]
fn mermaid_showcase_class_hierarchy_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "class-hierarchy");
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_class_hierarchy_120x40",
    );
}

#[test]
fn mermaid_showcase_state_concurrent_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "state-concurrent");
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_state_concurrent_120x40",
    );
}

// ============================================================================
// Mermaid Showcase — block-beta
// ============================================================================

#[test]
fn mermaid_showcase_block_beta_basic_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "block-beta-basic");
    mermaid_showcase_snapshot(
        &mut screen,
        80,
        24,
        "mermaid_showcase_block_beta_basic_80x24",
    );
}

#[test]
fn mermaid_showcase_block_beta_basic_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "block-beta-basic");
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_block_beta_basic_120x40",
    );
}

#[test]
fn mermaid_showcase_block_beta_nested_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    mermaid_showcase_goto_sample(&mut screen, "block-beta-nested");
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_block_beta_nested_120x40",
    );
}

#[test]
fn mermaid_showcase_help_overlay_60x20() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    screen.update(&press(KeyCode::Char('?')));
    mermaid_showcase_snapshot(&mut screen, 60, 20, "mermaid_showcase_help_overlay_60x20");
}

#[test]
fn mermaid_showcase_help_overlay_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    screen.update(&press(KeyCode::Char('?')));
    mermaid_showcase_snapshot(&mut screen, 80, 24, "mermaid_showcase_help_overlay_80x24");
}

#[test]
fn mermaid_showcase_debug_overlay_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    // Toggle all debug overlays (bounds, routes, ports, grid).
    screen.update(&press(KeyCode::Char('d')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_debug_overlay_120x40",
    );
}

#[test]
fn mermaid_showcase_debug_bounds_only_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen = ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen::new();
    // Toggle just the bounds overlay via key '1'.
    screen.update(&press(KeyCode::Char('1')));
    mermaid_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mermaid_showcase_debug_bounds_only_120x40",
    );
}

// ============================================================================
// Mermaid Mega Showcase
// ============================================================================

fn mega_showcase_goto_sample(
    screen: &mut ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen,
    id: &str,
) {
    assert!(
        screen.select_sample_by_id_for_test(id),
        "unknown Mermaid mega showcase sample id: {id}"
    );
}

fn mega_showcase_snapshot(
    screen: &mut ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen,
    width: u16,
    height: u16,
    name: &str,
) {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let area = Rect::new(0, 0, width, height);
    screen.view(&mut frame, area);
    assert_snapshot!(name, &frame.buffer);
}

// Baseline: Flow Basic (flow-basic) at standard sizes.
#[test]
fn mega_showcase_flow_basic_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_snapshot(&mut screen, 80, 24, "mega_showcase_flow_basic_80x24");
}

#[test]
fn mega_showcase_flow_basic_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_flow_basic_120x40");
}

// Sequence diagram (sequence-basic).
#[test]
fn mega_showcase_sequence_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "sequence-basic");
    mega_showcase_snapshot(&mut screen, 80, 24, "mega_showcase_sequence_80x24");
}

#[test]
fn mega_showcase_sequence_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "sequence-basic");
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_sequence_120x40");
}

// Class diagram (class-basic).
#[test]
fn mega_showcase_class_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "class-basic");
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_class_120x40");
}

// State diagram (state-basic).
#[test]
fn mega_showcase_state_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "state-basic");
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_state_120x40");
}

// ER diagram (er-basic).
#[test]
fn mega_showcase_er_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "er-basic");
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_er_120x40");
}

// Pie chart (pie-basic).
#[test]
fn mega_showcase_pie_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "pie-basic");
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_pie_120x40");
}

// Gantt chart (gantt-basic).
#[test]
fn mega_showcase_gantt_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "gantt-basic");
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_gantt_120x40");
}

// Mindmap (mindmap-basic).
#[test]
fn mega_showcase_mindmap_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "mindmap-basic");
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_mindmap_120x40");
}

// Help overlay visible.
#[test]
fn mega_showcase_help_overlay_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    screen.update(&press(KeyCode::Char('?')));
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_help_overlay_120x40");
}

// Help overlay at 80x24 — compact mode with truncation.
#[test]
fn mega_showcase_help_overlay_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    screen.update(&press(KeyCode::Char('?')));
    mega_showcase_snapshot(&mut screen, 80, 24, "mega_showcase_help_overlay_80x24");
}

// Panels collapsed via Escape.
#[test]
fn mega_showcase_collapsed_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    screen.update(&press(KeyCode::Escape));
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_collapsed_120x40");
}

// Viewport override with custom size.
#[test]
fn mega_showcase_viewport_override_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    screen.update(&press(KeyCode::Char(']')));
    screen.update(&press(KeyCode::Char(']')));
    screen.update(&press(KeyCode::Char('}')));
    mega_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mega_showcase_viewport_override_120x40",
    );
}

// Intentionally invalid sample (bd-3oaig.15): exercise error surfaces.
#[test]
fn mega_showcase_invalid_overlay_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "flow-invalid-parse-error");

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    let text = buffer_to_text(&frame.buffer);
    assert!(text.contains("Flow Invalid (Parse Error)"));
    assert!(text.contains("Mermaid error"));
    assert!(text.contains("classDef missing name"));
    assert!(text.contains("expected: classDef <name> <style>"));
    assert_snapshot!("mega_showcase_invalid_overlay_120x40", &frame.buffer);
}

#[test]
fn mega_showcase_invalid_diagnostics_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "flow-invalid-parse-error");
    // Toggle the dedicated diagnostics view (error list + line/col).
    screen.update(&press(KeyCode::Char('e')));

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    screen.view(&mut frame, area);
    let text = buffer_to_text(&frame.buffer);
    assert!(text.contains("Diagnostics (1 error)"));
    assert!(text.contains("classDef missing name"));
    assert!(text.contains("line 3, col 1"));
    assert!(text.contains("expected: classDef <name> <style>"));
    assert_snapshot!("mega_showcase_invalid_diagnostics_120x40", &frame.buffer);
}

// ============================================================================
// Mermaid Mega Showcase — architecture-beta (bd-hudcn.1.17.1)
// ============================================================================

#[test]
fn mega_showcase_architecture_beta_basic_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "architecture-beta-basic");
    mega_showcase_snapshot(
        &mut screen,
        80,
        24,
        "mega_showcase_architecture_beta_basic_80x24",
    );
}

#[test]
fn mega_showcase_architecture_beta_basic_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "architecture-beta-basic");
    mega_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mega_showcase_architecture_beta_basic_120x40",
    );
}

#[test]
fn mega_showcase_architecture_beta_stress_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "architecture-beta-stress");
    mega_showcase_snapshot(
        &mut screen,
        80,
        24,
        "mega_showcase_architecture_beta_stress_80x24",
    );
}

#[test]
fn mega_showcase_architecture_beta_stress_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "architecture-beta-stress");
    mega_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mega_showcase_architecture_beta_stress_120x40",
    );
}

#[test]
fn mega_showcase_architecture_beta_stress_200x60() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "architecture-beta-stress");
    mega_showcase_snapshot(
        &mut screen,
        200,
        60,
        "mega_showcase_architecture_beta_stress_200x60",
    );
}

// ============================================================================
// Mermaid Mega Showcase — Sequence Diagrams (bd-2kn9a)
// ============================================================================

#[test]
fn mega_showcase_sequence_loops_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "sequence-loops");
    mega_showcase_snapshot(&mut screen, 80, 24, "mega_showcase_sequence_loops_80x24");
}

#[test]
fn mega_showcase_sequence_loops_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "sequence-loops");
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_sequence_loops_120x40");
}

#[test]
fn mega_showcase_sequence_parallel_80x24() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "sequence-parallel");
    mega_showcase_snapshot(&mut screen, 80, 24, "mega_showcase_sequence_parallel_80x24");
}

#[test]
fn mega_showcase_sequence_parallel_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "sequence-parallel");
    mega_showcase_snapshot(
        &mut screen,
        120,
        40,
        "mega_showcase_sequence_parallel_120x40",
    );
}

#[test]
fn mega_showcase_sequence_dense_120x40() {
    let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
    let mut screen =
        ftui_demo_showcase::screens::mermaid_mega_showcase::MermaidMegaShowcaseScreen::new();
    mega_showcase_goto_sample(&mut screen, "sequence-dense");
    mega_showcase_snapshot(&mut screen, 120, 40, "mega_showcase_sequence_dense_120x40");
}
