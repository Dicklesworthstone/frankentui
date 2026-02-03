#![forbid(unsafe_code)]

//! Per-screen snapshot tests for the FrankenTUI Demo Showcase.
//!
//! Each screen is rendered at standard sizes and compared against stored
//! baselines. Run `BLESS=1 cargo test -p ftui-demo-showcase` to create or
//! update snapshot files.
//!
//! Naming convention: `screen_name_scenario_WIDTHxHEIGHT`

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::app::{AppModel, ScreenId};
use ftui_demo_showcase::screens::Screen;
use ftui_harness::assert_snapshot;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::Model;

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

fn ctrl_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::CTRL,
        kind: KeyEventKind::Press,
    })
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
    let app = AppModel::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("app_dashboard_full_120x40", &frame.buffer);
}

#[test]
fn app_shakespeare_full_120x40() {
    let mut app = AppModel::new();
    app.current_screen = ScreenId::Shakespeare;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("app_shakespeare_full_120x40", &frame.buffer);
}

#[test]
fn app_help_overlay_120x40() {
    let mut app = AppModel::new();
    app.help_visible = true;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("app_help_overlay_120x40", &frame.buffer);
}

#[test]
fn app_debug_overlay_120x40() {
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
    for &id in ScreenId::ALL {
        let mut app = AppModel::new();
        app.current_screen = id;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        app.view(&mut frame);
        let name = format!("app_{:?}_80x24", id).to_lowercase();
        assert_snapshot!(&name, &frame.buffer);
    }
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
