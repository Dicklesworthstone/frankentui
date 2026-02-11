//! Integration tests for chrome hit region registration (bd-iuvb.17.4).

use ftui_core::geometry::Rect;
use ftui_demo_showcase::app::ScreenId;
use ftui_demo_showcase::chrome::{
    A11yPanelState, HitLayer, OVERLAY_A11Y, OVERLAY_DEBUG, OVERLAY_HELP_CLOSE,
    OVERLAY_HELP_CONTENT, OVERLAY_PERF_HUD, OVERLAY_TOUR, STATUS_A11Y_TOGGLE, STATUS_DEBUG_TOGGLE,
    STATUS_HELP_TOGGLE, STATUS_MOUSE_TOGGLE, STATUS_PALETTE_TOGGLE, STATUS_PERF_TOGGLE,
    StatusBarState, classify_hit, render_a11y_panel, render_help_overlay, render_status_bar,
};
use ftui_render::frame::{Frame, HitId};
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::MouseCapturePolicy;

#[test]
fn status_bar_registers_mouse_toggle_hit_region() {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(120, 1, &mut pool);
    let area = Rect::new(0, 0, 120, 1);

    let state = StatusBarState {
        current_screen: ScreenId::Dashboard,
        screen_title: "Dashboard",
        screen_index: 0,
        screen_count: 10,
        tick_count: 50,
        frame_count: 100,
        terminal_width: 120,
        terminal_height: 40,
        theme_name: "Neon",
        inline_mode: false,
        mouse_capture_enabled: true,
        mouse_capture_policy: MouseCapturePolicy::Auto,
        help_visible: false,
        palette_visible: false,
        perf_hud_visible: false,
        debug_visible: false,
        a11y_high_contrast: false,
        a11y_reduced_motion: false,
        a11y_large_text: false,
        can_undo: false,
        can_redo: false,
        undo_description: None,
    };
    render_status_bar(&state, &mut frame, area);

    let target = HitId::new(STATUS_MOUSE_TOGGLE);
    let mut found = false;
    for x in 0..120u16 {
        if let Some((id, _, _)) = frame.hit_test(x, 0)
            && id == target
        {
            found = true;
            break;
        }
    }
    assert!(found, "Status bar should register mouse toggle hit region");
}

#[test]
fn help_overlay_registers_close_and_content_hit_regions() {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);

    let bindings = vec![];
    render_help_overlay(ScreenId::Dashboard, &bindings, &mut frame, area);

    let close_target = HitId::new(OVERLAY_HELP_CLOSE);
    let content_target = HitId::new(OVERLAY_HELP_CONTENT);
    let mut found_close = false;
    let mut found_content = false;
    for y in 0..40u16 {
        for x in 0..120u16 {
            if let Some((id, _, _)) = frame.hit_test(x, y) {
                if id == close_target {
                    found_close = true;
                }
                if id == content_target {
                    found_content = true;
                }
            }
            if found_close && found_content {
                break;
            }
        }
    }
    assert!(found_close, "Help overlay should register close hit region");
    assert!(
        found_content,
        "Help overlay should register content hit region"
    );
}

#[test]
fn a11y_panel_registers_dismiss_hit_region() {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);

    let state = A11yPanelState {
        high_contrast: false,
        reduced_motion: false,
        large_text: false,
        base_theme: "Neon",
    };
    render_a11y_panel(&state, &mut frame, area);

    let target = HitId::new(OVERLAY_A11Y);
    let mut found = false;
    for y in 0..40u16 {
        for x in 0..120u16 {
            if let Some((id, _, _)) = frame.hit_test(x, y)
                && id == target
            {
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }
    assert!(found, "A11y panel should register overlay hit region");
}

#[test]
fn classify_hit_routes_all_status_toggles() {
    for &raw in &[
        STATUS_HELP_TOGGLE,
        STATUS_PALETTE_TOGGLE,
        STATUS_A11Y_TOGGLE,
        STATUS_PERF_TOGGLE,
        STATUS_DEBUG_TOGGLE,
        STATUS_MOUSE_TOGGLE,
    ] {
        assert!(
            matches!(classify_hit(HitId::new(raw)), HitLayer::StatusToggle(_)),
            "classify_hit({raw}) should map to StatusToggle"
        );
    }
}

#[test]
fn classify_hit_routes_all_overlays() {
    for &raw in &[
        OVERLAY_HELP_CLOSE,
        OVERLAY_HELP_CONTENT,
        OVERLAY_A11Y,
        OVERLAY_PERF_HUD,
        OVERLAY_DEBUG,
        OVERLAY_TOUR,
    ] {
        assert!(
            matches!(classify_hit(HitId::new(raw)), HitLayer::Overlay(_)),
            "classify_hit({raw}) should map to Overlay"
        );
    }
}
