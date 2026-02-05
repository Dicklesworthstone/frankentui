#![forbid(unsafe_code)]

//! Accessibility mode snapshot tests for the FrankenTUI Demo Showcase.
//!
//! This module tests visual output under different accessibility configurations:
//! - High contrast mode
//! - Large text mode
//! - Reduced motion mode
//! - Combined accessibility modes
//!
//! Run `BLESS=1 cargo test -p ftui-demo-showcase` to update snapshot baselines.
//! Set `E2E_JSONL=1` or run in CI for verbose JSONL logging.
//!
//! Naming convention: `a11y_{mode}_{screen}_{WIDTHxHEIGHT}`

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::app::{AppModel, AppMsg};
use ftui_demo_showcase::screens::Screen;
use ftui_demo_showcase::theme::{self, ScopedRenderLock};
use ftui_harness::assert_snapshot;
use ftui_harness::determinism::DeterminismFixture;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::program::Model;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::Instant;

// ---------------------------------------------------------------------------
// JSONL Logging Infrastructure
// ---------------------------------------------------------------------------

fn generate_run_id() -> String {
    fixture().run_id().to_string()
}

fn fixture() -> &'static DeterminismFixture {
    static FIXTURE: OnceLock<DeterminismFixture> = OnceLock::new();
    FIXTURE.get_or_init(|| DeterminismFixture::new("a11y_snapshots", 42))
}

fn hash_frame(frame: &ftui_render::buffer::Buffer) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Hash dimensions
    frame.width().hash(&mut hasher);
    frame.height().hash(&mut hasher);
    // Hash raw buffer content via cell count and sampling
    let total_cells = (frame.width() as usize) * (frame.height() as usize);
    total_cells.hash(&mut hasher);
    // Sample cells at regular intervals for a fast hash
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            if let Some(cell) = frame.get(x, y) {
                cell.content.raw().hash(&mut hasher);
                // Only hash every 4th cell's colors for speed
                if (x + y) % 4 == 0 {
                    cell.fg.hash(&mut hasher);
                    cell.bg.hash(&mut hasher);
                }
            }
        }
    }
    hasher.finish()
}

fn hash_a11y_settings(settings: &theme::A11ySettings) -> u64 {
    let mut hasher = DefaultHasher::new();
    settings.high_contrast.hash(&mut hasher);
    settings.reduced_motion.hash(&mut hasher);
    settings.large_text.hash(&mut hasher);
    hasher.finish()
}

#[allow(clippy::too_many_arguments)]
fn log_e2e(
    run_id: &str,
    case: &str,
    a11y: &theme::A11ySettings,
    width: u16,
    height: u16,
    frame_hash: u64,
    settings_hash: u64,
    setup_ms: f64,
    render_ms: f64,
    snapshot_ms: f64,
    total_ms: f64,
) {
    if std::env::var("E2E_JSONL").is_ok() || std::env::var("CI").is_ok() {
        eprintln!(
            r#"{{"run_id":"{}","case":"{}","env":{{"os":"{}","test_module":"a11y_snapshots"}},"seed":null,"timings":{{"setup_ms":{:.3},"render_ms":{:.3},"snapshot_ms":{:.3},"total_ms":{:.3}}},"checksums":{{"frame_hash":{},"a11y_settings_hash":{}}},"capabilities":{{"high_contrast":{},"reduced_motion":{},"large_text":{},"terminal_width":{},"terminal_height":{}}},"outcome":"pass"}}"#,
            run_id,
            case,
            std::env::consts::OS,
            setup_ms,
            render_ms,
            snapshot_ms,
            total_ms,
            frame_hash,
            settings_hash,
            a11y.high_contrast,
            a11y.reduced_motion,
            a11y.large_text,
            width,
            height
        );
    }
}

fn a11y_seed() -> u64 {
    std::env::var("A11Y_TEST_SEED")
        .ok()
        .and_then(|val| val.parse::<u64>().ok())
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn log_transition_e2e(
    run_id: &str,
    case: &str,
    step: &str,
    a11y: &theme::A11ySettings,
    width: u16,
    height: u16,
    frame_hash: u64,
    settings_hash: u64,
    render_ms: f64,
    total_ms: f64,
    seed: u64,
) {
    if std::env::var("E2E_JSONL").is_ok() || std::env::var("CI").is_ok() {
        eprintln!(
            r#"{{"run_id":"{}","case":"{}","step":"{}","env":{{"os":"{}","test_module":"a11y_transitions"}},"seed":{},"timings":{{"render_ms":{:.3},"total_ms":{:.3}}},"checksums":{{"frame_hash":{},"a11y_settings_hash":{}}},"capabilities":{{"high_contrast":{},"reduced_motion":{},"large_text":{},"terminal_width":{},"terminal_height":{}}},"outcome":"pass"}}"#,
            run_id,
            case,
            step,
            std::env::consts::OS,
            seed,
            render_ms,
            total_ms,
            frame_hash,
            settings_hash,
            a11y.high_contrast,
            a11y.reduced_motion,
            a11y.large_text,
            width,
            height
        );
    }
}

// ---------------------------------------------------------------------------
// Test Helpers
// ---------------------------------------------------------------------------

// NOTE: This module uses ScopedRenderLock from theme.rs to acquire both theme and
// a11y locks in a consistent order, preventing deadlocks in parallel test runs.

struct A11yTestContext {
    app: AppModel,
    run_id: String,
    setup_start: Instant,
}

impl A11yTestContext {
    fn new() -> Self {
        let setup_start = Instant::now();
        Self {
            app: AppModel::new(),
            run_id: generate_run_id(),
            setup_start,
        }
    }

    fn with_high_contrast(mut self) -> Self {
        self.app.a11y.high_contrast = true;
        self
    }

    fn with_reduced_motion(mut self) -> Self {
        self.app.a11y.reduced_motion = true;
        self
    }

    fn with_large_text(mut self) -> Self {
        self.app.a11y.large_text = true;
        self
    }

    fn with_all_a11y(mut self) -> Self {
        self.app.a11y = theme::A11ySettings::all();
        self
    }

    fn render_and_snapshot(mut self, name: &str, width: u16, height: u16) {
        let setup_elapsed = self.setup_start.elapsed();

        // Acquire the global theme+a11y lock with the desired settings.
        let motion_scale = if self.app.a11y.reduced_motion {
            0.0
        } else {
            1.0
        };
        // Always set theme state explicitly for test isolation.
        // Use HighContrast theme to match app behavior in apply_a11y_settings().
        let theme_id = if self.app.a11y.high_contrast {
            theme::ThemeId::HighContrast
        } else {
            theme::ThemeId::CyberpunkAurora
        };
        let _render_lock = ScopedRenderLock::new(theme_id, self.app.a11y.large_text, motion_scale);

        // Update terminal dimensions for proper rendering
        self.app.terminal_width = width;
        self.app.terminal_height = height;

        let render_start = Instant::now();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(width, height, &mut pool);
        self.app.view(&mut frame);
        let render_elapsed = render_start.elapsed();

        let frame_hash = hash_frame(&frame.buffer);
        let settings_hash = hash_a11y_settings(&self.app.a11y);

        let snapshot_start = Instant::now();
        assert_snapshot!(name, &frame.buffer);
        let snapshot_elapsed = snapshot_start.elapsed();

        let total_elapsed = self.setup_start.elapsed();

        log_e2e(
            &self.run_id,
            name,
            &self.app.a11y,
            width,
            height,
            frame_hash,
            settings_hash,
            setup_elapsed.as_secs_f64() * 1000.0,
            render_elapsed.as_secs_f64() * 1000.0,
            snapshot_elapsed.as_secs_f64() * 1000.0,
            total_elapsed.as_secs_f64() * 1000.0,
        );
    }
}

/// Helper for per-screen a11y testing.
/// Always sets theme state explicitly to ensure test isolation.
fn render_screen_with_a11y<S: Screen>(
    screen: &S,
    a11y: &theme::A11ySettings,
    width: u16,
    height: u16,
) -> Frame<'static> {
    // Acquire the global theme+a11y lock with the desired settings.
    let motion_scale = if a11y.reduced_motion { 0.0 } else { 1.0 };
    // Always set theme state explicitly for test isolation.
    let theme_id = if a11y.high_contrast {
        theme::ThemeId::HighContrast
    } else {
        theme::ThemeId::CyberpunkAurora
    };
    let _render_lock = ScopedRenderLock::new(theme_id, a11y.large_text, motion_scale);

    let pool = Box::leak(Box::new(GraphemePool::new()));
    let mut frame = Frame::new(width, height, pool);
    let area = Rect::new(0, 0, width, height);
    screen.view(&mut frame, area);
    frame
}

#[allow(clippy::too_many_arguments)]
fn render_transition_step(
    run_id: &str,
    case: &str,
    step: &str,
    app: &mut AppModel,
    width: u16,
    height: u16,
    seed: u64,
    start: &Instant,
) -> u64 {
    let render_start = Instant::now();

    let theme_id = if app.a11y.high_contrast {
        theme::ThemeId::HighContrast
    } else {
        theme::ThemeId::CyberpunkAurora
    };
    let motion_scale = if app.a11y.reduced_motion { 0.0 } else { 1.0 };
    let _render_lock = ScopedRenderLock::new(theme_id, app.a11y.large_text, motion_scale);

    app.terminal_width = width;
    app.terminal_height = height;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    app.view(&mut frame);

    let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;
    let frame_hash = hash_frame(&frame.buffer);
    let settings_hash = hash_a11y_settings(&app.a11y);

    log_transition_e2e(
        run_id,
        case,
        step,
        &app.a11y,
        width,
        height,
        frame_hash,
        settings_hash,
        render_ms,
        total_ms,
        seed,
    );

    frame_hash
}

// ============================================================================
// High Contrast Mode Tests
// ============================================================================

#[test]
fn a11y_high_contrast_dashboard_80x24() {
    A11yTestContext::new()
        .with_high_contrast()
        .render_and_snapshot("a11y_high_contrast_dashboard_80x24", 80, 24);
}

#[test]
fn a11y_high_contrast_dashboard_120x40() {
    A11yTestContext::new()
        .with_high_contrast()
        .render_and_snapshot("a11y_high_contrast_dashboard_120x40", 120, 40);
}

#[test]
fn a11y_high_contrast_shakespeare_80x24() {
    let mut ctx = A11yTestContext::new().with_high_contrast();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::Shakespeare;
    ctx.render_and_snapshot("a11y_high_contrast_shakespeare_80x24", 80, 24);
}

#[test]
fn a11y_high_contrast_shakespeare_120x40() {
    let mut ctx = A11yTestContext::new().with_high_contrast();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::Shakespeare;
    ctx.render_and_snapshot("a11y_high_contrast_shakespeare_120x40", 120, 40);
}

#[test]
fn a11y_high_contrast_widget_gallery_80x24() {
    let mut ctx = A11yTestContext::new().with_high_contrast();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::WidgetGallery;
    ctx.render_and_snapshot("a11y_high_contrast_widget_gallery_80x24", 80, 24);
}

#[test]
fn a11y_high_contrast_widget_gallery_120x40() {
    let mut ctx = A11yTestContext::new().with_high_contrast();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::WidgetGallery;
    ctx.render_and_snapshot("a11y_high_contrast_widget_gallery_120x40", 120, 40);
}

#[test]
fn a11y_high_contrast_forms_input_80x24() {
    let mut ctx = A11yTestContext::new().with_high_contrast();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::FormsInput;
    ctx.render_and_snapshot("a11y_high_contrast_forms_input_80x24", 80, 24);
}

#[test]
fn a11y_high_contrast_forms_input_120x40() {
    let mut ctx = A11yTestContext::new().with_high_contrast();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::FormsInput;
    ctx.render_and_snapshot("a11y_high_contrast_forms_input_120x40", 120, 40);
}

#[test]
fn a11y_high_contrast_data_viz_80x24() {
    let mut ctx = A11yTestContext::new().with_high_contrast();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::DataViz;
    ctx.render_and_snapshot("a11y_high_contrast_data_viz_80x24", 80, 24);
}

#[test]
fn a11y_high_contrast_data_viz_120x40() {
    let mut ctx = A11yTestContext::new().with_high_contrast();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::DataViz;
    ctx.render_and_snapshot("a11y_high_contrast_data_viz_120x40", 120, 40);
}

#[test]
fn a11y_high_contrast_tiny_40x10() {
    A11yTestContext::new()
        .with_high_contrast()
        .render_and_snapshot("a11y_high_contrast_tiny_40x10", 40, 10);
}

// ============================================================================
// Large Text Mode Tests
// ============================================================================

#[test]
fn a11y_large_text_dashboard_80x24() {
    A11yTestContext::new()
        .with_large_text()
        .render_and_snapshot("a11y_large_text_dashboard_80x24", 80, 24);
}

#[test]
fn a11y_large_text_dashboard_120x40() {
    A11yTestContext::new()
        .with_large_text()
        .render_and_snapshot("a11y_large_text_dashboard_120x40", 120, 40);
}

#[test]
fn a11y_large_text_shakespeare_80x24() {
    let mut ctx = A11yTestContext::new().with_large_text();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::Shakespeare;
    ctx.render_and_snapshot("a11y_large_text_shakespeare_80x24", 80, 24);
}

#[test]
fn a11y_large_text_shakespeare_120x40() {
    let mut ctx = A11yTestContext::new().with_large_text();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::Shakespeare;
    ctx.render_and_snapshot("a11y_large_text_shakespeare_120x40", 120, 40);
}

#[test]
fn a11y_large_text_widget_gallery_80x24() {
    let mut ctx = A11yTestContext::new().with_large_text();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::WidgetGallery;
    ctx.render_and_snapshot("a11y_large_text_widget_gallery_80x24", 80, 24);
}

#[test]
fn a11y_large_text_widget_gallery_120x40() {
    let mut ctx = A11yTestContext::new().with_large_text();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::WidgetGallery;
    ctx.render_and_snapshot("a11y_large_text_widget_gallery_120x40", 120, 40);
}

#[test]
fn a11y_large_text_forms_input_80x24() {
    let mut ctx = A11yTestContext::new().with_large_text();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::FormsInput;
    ctx.render_and_snapshot("a11y_large_text_forms_input_80x24", 80, 24);
}

#[test]
fn a11y_large_text_forms_input_120x40() {
    let mut ctx = A11yTestContext::new().with_large_text();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::FormsInput;
    ctx.render_and_snapshot("a11y_large_text_forms_input_120x40", 120, 40);
}

#[test]
fn a11y_large_text_tiny_40x10() {
    A11yTestContext::new()
        .with_large_text()
        .render_and_snapshot("a11y_large_text_tiny_40x10", 40, 10);
}

// ============================================================================
// Reduced Motion Mode Tests
// ============================================================================

#[test]
fn a11y_reduced_motion_dashboard_80x24() {
    A11yTestContext::new()
        .with_reduced_motion()
        .render_and_snapshot("a11y_reduced_motion_dashboard_80x24", 80, 24);
}

#[test]
fn a11y_reduced_motion_dashboard_120x40() {
    A11yTestContext::new()
        .with_reduced_motion()
        .render_and_snapshot("a11y_reduced_motion_dashboard_120x40", 120, 40);
}

#[test]
fn a11y_reduced_motion_data_viz_80x24() {
    let mut ctx = A11yTestContext::new().with_reduced_motion();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::DataViz;
    ctx.render_and_snapshot("a11y_reduced_motion_data_viz_80x24", 80, 24);
}

#[test]
fn a11y_reduced_motion_data_viz_120x40() {
    let mut ctx = A11yTestContext::new().with_reduced_motion();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::DataViz;
    ctx.render_and_snapshot("a11y_reduced_motion_data_viz_120x40", 120, 40);
}

#[test]
fn a11y_reduced_motion_widget_gallery_80x24() {
    let mut ctx = A11yTestContext::new().with_reduced_motion();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::WidgetGallery;
    ctx.render_and_snapshot("a11y_reduced_motion_widget_gallery_80x24", 80, 24);
}

#[test]
fn a11y_reduced_motion_widget_gallery_120x40() {
    let mut ctx = A11yTestContext::new().with_reduced_motion();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::WidgetGallery;
    ctx.render_and_snapshot("a11y_reduced_motion_widget_gallery_120x40", 120, 40);
}

#[test]
fn a11y_reduced_motion_after_ticks_120x40() {
    let mut ctx = A11yTestContext::new().with_reduced_motion();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::DataViz;
    // With reduced motion enabled, animations should be static.
    // We test the visual output to ensure motion_scale=0 is applied correctly.
    ctx.render_and_snapshot("a11y_reduced_motion_after_ticks_120x40", 120, 40);
}

#[test]
fn a11y_reduced_motion_tiny_40x10() {
    A11yTestContext::new()
        .with_reduced_motion()
        .render_and_snapshot("a11y_reduced_motion_tiny_40x10", 40, 10);
}

// ============================================================================
// Combined Accessibility Modes Tests
// ============================================================================

#[test]
fn a11y_all_modes_dashboard_80x24() {
    A11yTestContext::new().with_all_a11y().render_and_snapshot(
        "a11y_all_modes_dashboard_80x24",
        80,
        24,
    );
}

#[test]
fn a11y_all_modes_dashboard_120x40() {
    A11yTestContext::new().with_all_a11y().render_and_snapshot(
        "a11y_all_modes_dashboard_120x40",
        120,
        40,
    );
}

#[test]
fn a11y_all_modes_shakespeare_80x24() {
    let mut ctx = A11yTestContext::new().with_all_a11y();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::Shakespeare;
    ctx.render_and_snapshot("a11y_all_modes_shakespeare_80x24", 80, 24);
}

#[test]
fn a11y_all_modes_widget_gallery_120x40() {
    let mut ctx = A11yTestContext::new().with_all_a11y();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::WidgetGallery;
    ctx.render_and_snapshot("a11y_all_modes_widget_gallery_120x40", 120, 40);
}

#[test]
fn a11y_all_modes_forms_input_120x40() {
    let mut ctx = A11yTestContext::new().with_all_a11y();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::FormsInput;
    ctx.render_and_snapshot("a11y_all_modes_forms_input_120x40", 120, 40);
}

#[test]
fn a11y_all_modes_data_viz_120x40() {
    let mut ctx = A11yTestContext::new().with_all_a11y();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::DataViz;
    ctx.render_and_snapshot("a11y_all_modes_data_viz_120x40", 120, 40);
}

#[test]
fn a11y_all_modes_tiny_40x10() {
    A11yTestContext::new()
        .with_all_a11y()
        .render_and_snapshot("a11y_all_modes_tiny_40x10", 40, 10);
}

#[test]
fn a11y_all_modes_wide_200x50() {
    A11yTestContext::new().with_all_a11y().render_and_snapshot(
        "a11y_all_modes_wide_200x50",
        200,
        50,
    );
}

// ============================================================================
// High Contrast + Large Text (common combination)
// ============================================================================

#[test]
fn a11y_high_contrast_large_text_dashboard_80x24() {
    A11yTestContext::new()
        .with_high_contrast()
        .with_large_text()
        .render_and_snapshot("a11y_high_contrast_large_text_dashboard_80x24", 80, 24);
}

#[test]
fn a11y_high_contrast_large_text_dashboard_120x40() {
    A11yTestContext::new()
        .with_high_contrast()
        .with_large_text()
        .render_and_snapshot("a11y_high_contrast_large_text_dashboard_120x40", 120, 40);
}

#[test]
fn a11y_high_contrast_large_text_forms_input_120x40() {
    let mut ctx = A11yTestContext::new()
        .with_high_contrast()
        .with_large_text();
    ctx.app.current_screen = ftui_demo_showcase::app::ScreenId::FormsInput;
    ctx.render_and_snapshot("a11y_high_contrast_large_text_forms_input_120x40", 120, 40);
}

// ============================================================================
// Per-Screen Individual A11y Tests (Dashboard screen variants)
// ============================================================================

#[test]
fn a11y_dashboard_screen_high_contrast_80x24() {
    let screen = ftui_demo_showcase::screens::dashboard::Dashboard::new();
    let a11y = theme::A11ySettings {
        high_contrast: true,
        reduced_motion: false,
        large_text: false,
    };
    let frame = render_screen_with_a11y(&screen, &a11y, 80, 24);
    assert_snapshot!("a11y_dashboard_screen_high_contrast_80x24", &frame.buffer);
}

#[test]
fn a11y_dashboard_screen_large_text_80x24() {
    let screen = ftui_demo_showcase::screens::dashboard::Dashboard::new();
    let a11y = theme::A11ySettings {
        high_contrast: false,
        reduced_motion: false,
        large_text: true,
    };
    let frame = render_screen_with_a11y(&screen, &a11y, 80, 24);
    assert_snapshot!("a11y_dashboard_screen_large_text_80x24", &frame.buffer);
}

#[test]
fn a11y_dashboard_screen_reduced_motion_80x24() {
    let screen = ftui_demo_showcase::screens::dashboard::Dashboard::new();
    let a11y = theme::A11ySettings {
        high_contrast: false,
        reduced_motion: true,
        large_text: false,
    };
    let frame = render_screen_with_a11y(&screen, &a11y, 80, 24);
    assert_snapshot!("a11y_dashboard_screen_reduced_motion_80x24", &frame.buffer);
}

// ============================================================================
// Accessibility Panel Screen A11y Tests
// ============================================================================

#[test]
fn a11y_accessibility_panel_high_contrast_80x24() {
    let mut screen = ftui_demo_showcase::screens::accessibility_panel::AccessibilityPanel::new();
    let a11y = theme::A11ySettings {
        high_contrast: true,
        reduced_motion: false,
        large_text: false,
    };
    screen.sync_a11y(a11y, theme::ThemeId::CyberpunkAurora);
    let frame = render_screen_with_a11y(&screen, &a11y, 80, 24);
    assert_snapshot!(
        "a11y_accessibility_panel_high_contrast_80x24",
        &frame.buffer
    );
}

#[test]
fn a11y_accessibility_panel_large_text_80x24() {
    let mut screen = ftui_demo_showcase::screens::accessibility_panel::AccessibilityPanel::new();
    let a11y = theme::A11ySettings {
        high_contrast: false,
        reduced_motion: false,
        large_text: true,
    };
    screen.sync_a11y(a11y, theme::ThemeId::CyberpunkAurora);
    let frame = render_screen_with_a11y(&screen, &a11y, 80, 24);
    assert_snapshot!("a11y_accessibility_panel_large_text_80x24", &frame.buffer);
}

#[test]
fn a11y_accessibility_panel_reduced_motion_80x24() {
    let mut screen = ftui_demo_showcase::screens::accessibility_panel::AccessibilityPanel::new();
    let a11y = theme::A11ySettings {
        high_contrast: false,
        reduced_motion: true,
        large_text: false,
    };
    screen.sync_a11y(a11y, theme::ThemeId::CyberpunkAurora);
    let frame = render_screen_with_a11y(&screen, &a11y, 80, 24);
    assert_snapshot!(
        "a11y_accessibility_panel_reduced_motion_80x24",
        &frame.buffer
    );
}

#[test]
fn a11y_accessibility_panel_all_modes_120x40() {
    let mut screen = ftui_demo_showcase::screens::accessibility_panel::AccessibilityPanel::new();
    let a11y = theme::A11ySettings {
        high_contrast: true,
        reduced_motion: true,
        large_text: true,
    };
    screen.sync_a11y(a11y, theme::ThemeId::CyberpunkAurora);
    let frame = render_screen_with_a11y(&screen, &a11y, 120, 40);
    assert_snapshot!("a11y_accessibility_panel_all_modes_120x40", &frame.buffer);
}

// ============================================================================
// Edge Cases & Regression Tests
// ============================================================================

#[test]
fn a11y_zero_area_high_contrast() {
    let _render_lock = ScopedRenderLock::new(theme::ThemeId::HighContrast, false, 1.0);
    let mut app = AppModel::new();
    app.a11y.high_contrast = true;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    app.view(&mut frame);
    // No panic = success
}

#[test]
fn a11y_zero_area_large_text() {
    let _render_lock = ScopedRenderLock::new(theme::ThemeId::CyberpunkAurora, true, 1.0);
    let mut app = AppModel::new();
    app.a11y.large_text = true;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    app.view(&mut frame);
    // No panic = success
}

#[test]
fn a11y_zero_area_all_modes() {
    let _render_lock = ScopedRenderLock::new(theme::ThemeId::HighContrast, true, 0.0);
    let mut app = AppModel::new();
    app.a11y = theme::A11ySettings::all();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    app.view(&mut frame);
    // No panic = success
}

#[test]
fn a11y_settings_toggle_idempotent() {
    // Verify toggling settings twice returns to original state
    let mut app = AppModel::new();
    let original_high_contrast = app.a11y.high_contrast;
    let original_reduced_motion = app.a11y.reduced_motion;
    let original_large_text = app.a11y.large_text;

    // Toggle on
    app.a11y.high_contrast = !app.a11y.high_contrast;
    app.a11y.reduced_motion = !app.a11y.reduced_motion;
    app.a11y.large_text = !app.a11y.large_text;

    // Toggle off (back to original)
    app.a11y.high_contrast = !app.a11y.high_contrast;
    app.a11y.reduced_motion = !app.a11y.reduced_motion;
    app.a11y.large_text = !app.a11y.large_text;

    assert_eq!(app.a11y.high_contrast, original_high_contrast);
    assert_eq!(app.a11y.reduced_motion, original_reduced_motion);
    assert_eq!(app.a11y.large_text, original_large_text);
}

#[test]
fn a11y_settings_all_equals_individual() {
    // Verify A11ySettings::all() matches individual true settings
    let all = theme::A11ySettings::all();
    let individual = theme::A11ySettings {
        high_contrast: true,
        reduced_motion: true,
        large_text: true,
    };
    assert_eq!(all, individual);
}

#[test]
fn a11y_settings_none_equals_default() {
    // Verify A11ySettings::none() matches default
    let none = theme::A11ySettings::none();
    let default = theme::A11ySettings::default();
    assert_eq!(none, default);
}

// ============================================================================
// Transition Regression Tests (mode toggles should be stable and round-trip)
// ============================================================================

#[test]
fn a11y_transition_high_contrast_roundtrip() {
    // Invariant: toggling high-contrast on/off should round-trip to baseline.
    let mut app = AppModel::new();
    let run_id = generate_run_id();
    let start = Instant::now();
    let seed = a11y_seed();
    let width = 80;
    let height = 24;

    let baseline = render_transition_step(
        &run_id,
        "a11y_transition_high_contrast_roundtrip",
        "baseline",
        &mut app,
        width,
        height,
        seed,
        &start,
    );

    app.a11y.high_contrast = true;
    let on_hash = render_transition_step(
        &run_id,
        "a11y_transition_high_contrast_roundtrip",
        "high_contrast_on",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    let on_repeat = render_transition_step(
        &run_id,
        "a11y_transition_high_contrast_roundtrip",
        "high_contrast_on_repeat",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    assert_eq!(
        on_hash, on_repeat,
        "High contrast render should be stable across repeated frames"
    );

    app.a11y.high_contrast = false;
    let off_hash = render_transition_step(
        &run_id,
        "a11y_transition_high_contrast_roundtrip",
        "high_contrast_off",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    assert_eq!(
        baseline, off_hash,
        "High contrast round-trip should restore baseline frame"
    );
}

#[test]
fn a11y_transition_reduced_motion_roundtrip() {
    // Invariant: reduced-motion toggles should be stable and round-trip cleanly.
    let mut app = AppModel::new();
    app.current_screen = ftui_demo_showcase::app::ScreenId::DataViz;
    let run_id = generate_run_id();
    let start = Instant::now();
    let seed = a11y_seed();
    let width = 80;
    let height = 24;

    let baseline = render_transition_step(
        &run_id,
        "a11y_transition_reduced_motion_roundtrip",
        "baseline",
        &mut app,
        width,
        height,
        seed,
        &start,
    );

    app.a11y.reduced_motion = true;
    let on_hash = render_transition_step(
        &run_id,
        "a11y_transition_reduced_motion_roundtrip",
        "reduced_motion_on",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    let on_repeat = render_transition_step(
        &run_id,
        "a11y_transition_reduced_motion_roundtrip",
        "reduced_motion_on_repeat",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    assert_eq!(
        on_hash, on_repeat,
        "Reduced-motion render should be stable across repeated frames"
    );

    app.a11y.reduced_motion = false;
    let off_hash = render_transition_step(
        &run_id,
        "a11y_transition_reduced_motion_roundtrip",
        "reduced_motion_off",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    assert_eq!(
        baseline, off_hash,
        "Reduced-motion round-trip should restore baseline frame"
    );
}

#[test]
fn a11y_transition_large_text_roundtrip() {
    // Invariant: large-text toggles should be stable and round-trip cleanly.
    let mut app = AppModel::new();
    let run_id = generate_run_id();
    let start = Instant::now();
    let seed = a11y_seed();
    let width = 80;
    let height = 24;

    let baseline = render_transition_step(
        &run_id,
        "a11y_transition_large_text_roundtrip",
        "baseline",
        &mut app,
        width,
        height,
        seed,
        &start,
    );

    app.a11y.large_text = true;
    let on_hash = render_transition_step(
        &run_id,
        "a11y_transition_large_text_roundtrip",
        "large_text_on",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    let on_repeat = render_transition_step(
        &run_id,
        "a11y_transition_large_text_roundtrip",
        "large_text_on_repeat",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    assert_eq!(
        on_hash, on_repeat,
        "Large-text render should be stable across repeated frames"
    );

    app.a11y.large_text = false;
    let off_hash = render_transition_step(
        &run_id,
        "a11y_transition_large_text_roundtrip",
        "large_text_off",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    assert_eq!(
        baseline, off_hash,
        "Large-text round-trip should restore baseline frame"
    );
}

#[test]
fn a11y_transition_all_modes_roundtrip() {
    // Failure modes: theme globals leak or motion scale persists after toggling off.
    let mut app = AppModel::new();
    let run_id = generate_run_id();
    let start = Instant::now();
    let seed = a11y_seed();
    let width = 80;
    let height = 24;

    let baseline = render_transition_step(
        &run_id,
        "a11y_transition_all_modes_roundtrip",
        "baseline",
        &mut app,
        width,
        height,
        seed,
        &start,
    );

    app.a11y = theme::A11ySettings::all();
    let all_on = render_transition_step(
        &run_id,
        "a11y_transition_all_modes_roundtrip",
        "all_modes_on",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    let all_on_repeat = render_transition_step(
        &run_id,
        "a11y_transition_all_modes_roundtrip",
        "all_modes_on_repeat",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    assert_eq!(
        all_on, all_on_repeat,
        "All-modes render should be stable across repeated frames"
    );

    app.a11y = theme::A11ySettings::none();
    let all_off = render_transition_step(
        &run_id,
        "a11y_transition_all_modes_roundtrip",
        "all_modes_off",
        &mut app,
        width,
        height,
        seed,
        &start,
    );
    assert_eq!(
        baseline, all_off,
        "All-modes round-trip should restore baseline frame"
    );
}

// ============================================================================
// Full App Integration with A11y Panel Visible
// ============================================================================

#[test]
fn a11y_panel_visible_80x24() {
    let _render_lock = ScopedRenderLock::new(theme::ThemeId::CyberpunkAurora, false, 1.0);
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("a11y_panel_visible_80x24", &frame.buffer);
}

#[test]
fn a11y_panel_visible_120x40() {
    let _render_lock = ScopedRenderLock::new(theme::ThemeId::CyberpunkAurora, false, 1.0);
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("a11y_panel_visible_120x40", &frame.buffer);
}

#[test]
fn a11y_panel_with_high_contrast_120x40() {
    let _render_lock = ScopedRenderLock::new(theme::ThemeId::HighContrast, false, 1.0);
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;
    app.a11y.high_contrast = true;
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("a11y_panel_with_high_contrast_120x40", &frame.buffer);
}

#[test]
fn a11y_panel_with_all_modes_120x40() {
    let _render_lock = ScopedRenderLock::new(theme::ThemeId::HighContrast, true, 0.0);
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;
    app.a11y = theme::A11ySettings::all();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);
    assert_snapshot!("a11y_panel_with_all_modes_120x40", &frame.buffer);
}

// ============================================================================
// Determinism Tests (verify same input produces same output)
// ============================================================================

#[test]
fn a11y_determinism_high_contrast() {
    let _render_lock = ScopedRenderLock::new(theme::ThemeId::HighContrast, false, 1.0);
    // Render twice and verify identical output
    let mut app1 = AppModel::new();
    app1.a11y.high_contrast = true;
    let mut pool1 = GraphemePool::new();
    let mut frame1 = Frame::new(80, 24, &mut pool1);
    app1.view(&mut frame1);
    let hash1 = hash_frame(&frame1.buffer);

    let mut app2 = AppModel::new();
    app2.a11y.high_contrast = true;
    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(80, 24, &mut pool2);
    app2.view(&mut frame2);
    let hash2 = hash_frame(&frame2.buffer);

    assert_eq!(
        hash1, hash2,
        "High contrast mode should produce deterministic output"
    );
}

#[test]
fn a11y_determinism_all_modes() {
    let _render_lock = ScopedRenderLock::new(theme::ThemeId::HighContrast, true, 0.0);
    // Render twice with all a11y modes and verify identical output
    let mut app1 = AppModel::new();
    app1.a11y = theme::A11ySettings::all();
    let mut pool1 = GraphemePool::new();
    let mut frame1 = Frame::new(120, 40, &mut pool1);
    app1.view(&mut frame1);
    let hash1 = hash_frame(&frame1.buffer);

    let mut app2 = AppModel::new();
    app2.a11y = theme::A11ySettings::all();
    let mut pool2 = GraphemePool::new();
    let mut frame2 = Frame::new(120, 40, &mut pool2);
    app2.view(&mut frame2);
    let hash2 = hash_frame(&frame2.buffer);

    assert_eq!(
        hash1, hash2,
        "All a11y modes should produce deterministic output"
    );
}

// ============================================================================
// UX/A11y Review Tests (bd-2o55.6)
// ============================================================================

fn log_ux_jsonl(test: &str, check: &str, passed: bool, notes: &str) {
    if std::env::var("E2E_JSONL").is_ok() || std::env::var("CI").is_ok() {
        eprintln!(r#"{{"test":"{test}","check":"{check}","passed":{passed},"notes":"{notes}"}}"#);
    }
}

fn shift_key(ch: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(ch),
        modifiers: Modifiers::SHIFT,
        kind: KeyEventKind::Press,
    })
}

fn key_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::empty(),
        kind: KeyEventKind::Press,
    })
}

fn frame_contains(app: &AppModel, width: u16, height: u16, needle: &str) -> bool {
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
    text.contains(needle)
}

#[test]
fn a11y_keybinding_shift_a_toggles_panel() {
    let mut app = AppModel::new();
    assert!(!app.a11y_panel_visible);

    let _ = app.update(AppMsg::ScreenEvent(shift_key('A')));
    log_ux_jsonl(
        "keybinding_shift_a",
        "toggle_on",
        app.a11y_panel_visible,
        "Shift+A opens the A11y panel",
    );
    assert!(app.a11y_panel_visible);

    let _ = app.update(AppMsg::ScreenEvent(shift_key('A')));
    log_ux_jsonl(
        "keybinding_shift_a",
        "toggle_off",
        !app.a11y_panel_visible,
        "Shift+A closes the A11y panel",
    );
    assert!(!app.a11y_panel_visible);
}

#[test]
fn a11y_panel_escape_closes() {
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;

    let _ = app.update(AppMsg::ScreenEvent(key_press(KeyCode::Escape)));
    log_ux_jsonl(
        "keybinding_escape",
        "close_panel",
        !app.a11y_panel_visible,
        "Escape closes the A11y panel",
    );
    assert!(!app.a11y_panel_visible);
}

#[test]
fn a11y_panel_toggle_keys_apply_states() {
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;

    let _ = app.update(AppMsg::ScreenEvent(shift_key('H')));
    let _ = app.update(AppMsg::ScreenEvent(shift_key('M')));
    let _ = app.update(AppMsg::ScreenEvent(shift_key('L')));

    log_ux_jsonl(
        "a11y_toggles",
        "states_enabled",
        app.a11y.high_contrast && app.a11y.reduced_motion && app.a11y.large_text,
        "H/M/L toggles enable high contrast, reduced motion, and large text",
    );

    assert!(app.a11y.high_contrast);
    assert!(app.a11y.reduced_motion);
    assert!(app.a11y.large_text);
}

#[test]
fn a11y_panel_non_modal_allows_help_overlay() {
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;

    let _ = app.update(AppMsg::ScreenEvent(key_press(KeyCode::Char('?'))));
    log_ux_jsonl(
        "focus_order",
        "help_overlay",
        app.help_visible,
        "Help overlay still toggles while A11y panel is visible",
    );
    assert!(app.help_visible);
}

#[test]
fn a11y_help_overlay_documents_keybindings() {
    let mut app = AppModel::new();
    app.help_visible = true;

    let has_panel_entry = frame_contains(&app, 120, 40, "Toggle A11y panel");
    let has_modes_entry = frame_contains(&app, 120, 40, "A11y: high contrast");

    log_ux_jsonl(
        "help_overlay",
        "a11y_entries",
        has_panel_entry && has_modes_entry,
        "Help overlay documents A11y keybindings",
    );

    assert!(has_panel_entry);
    assert!(has_modes_entry);
}

#[test]
fn a11y_panel_text_indicators_visible() {
    let mut app = AppModel::new();
    app.a11y_panel_visible = true;
    app.a11y.high_contrast = true;
    app.a11y.reduced_motion = true;
    app.a11y.large_text = true;

    let has_high_contrast = frame_contains(&app, 120, 40, "High Contrast: ON");
    let has_reduced_motion = frame_contains(&app, 120, 40, "Reduced Motion: ON");
    let has_large_text = frame_contains(&app, 120, 40, "Large Text: ON");

    log_ux_jsonl(
        "legibility",
        "text_indicators",
        has_high_contrast && has_reduced_motion && has_large_text,
        "Panel includes ON/OFF text indicators (not color-only)",
    );

    assert!(has_high_contrast);
    assert!(has_reduced_motion);
    assert!(has_large_text);
}

#[test]
fn a11y_status_bar_indicator_shows_flags() {
    let mut app = AppModel::new();
    app.a11y.high_contrast = true;
    app.a11y.reduced_motion = true;
    app.a11y.large_text = false;

    let has_indicator = frame_contains(&app, 120, 40, "A11y:HC RM");
    log_ux_jsonl(
        "status_bar",
        "a11y_flags",
        has_indicator,
        "Status bar shows text flags for active accessibility modes",
    );
    assert!(has_indicator);
}
