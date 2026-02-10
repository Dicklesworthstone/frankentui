#![forbid(unsafe_code)]

//! Modal container widget with backdrop, positioning, and size constraints.
//!
//! This widget renders:
//! 1) a full-screen backdrop (tinted overlay), then
//! 2) the content widget in a positioned rectangle.
//!
//! Optionally registers hit regions for backdrop vs content so callers can
//! implement close-on-backdrop click behavior using the hit grid.

use crate::Widget;
use crate::set_style_area;
use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::{Rect, Size};
use ftui_render::cell::PackedRgba;
use ftui_render::frame::{Frame, HitData, HitId, HitRegion};
use ftui_style::Style;

/// Hit region tag for the modal backdrop.
pub const MODAL_HIT_BACKDROP: HitRegion = HitRegion::Custom(1);
/// Hit region tag for the modal content.
pub const MODAL_HIT_CONTENT: HitRegion = HitRegion::Custom(2);

/// Modal action emitted by `ModalState::handle_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalAction {
    /// The modal should close.
    Close,
    /// Backdrop was clicked.
    BackdropClicked,
    /// Escape was pressed.
    EscapePressed,
}

/// Backdrop configuration (color + opacity).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackdropConfig {
    /// Backdrop color (alpha will be scaled by `opacity`).
    pub color: PackedRgba,
    /// Opacity in `[0.0, 1.0]`.
    pub opacity: f32,
}

impl BackdropConfig {
    /// Create a new backdrop config.
    pub fn new(color: PackedRgba, opacity: f32) -> Self {
        Self { color, opacity }
    }

    /// Set backdrop color.
    #[must_use]
    pub fn color(mut self, color: PackedRgba) -> Self {
        self.color = color;
        self
    }

    /// Set backdrop opacity.
    #[must_use]
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }
}

impl Default for BackdropConfig {
    fn default() -> Self {
        Self {
            color: PackedRgba::rgb(0, 0, 0),
            opacity: 0.6,
        }
    }
}

/// Modal size constraints (min/max width/height).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModalSizeConstraints {
    pub min_width: Option<u16>,
    pub max_width: Option<u16>,
    pub min_height: Option<u16>,
    pub max_height: Option<u16>,
}

impl ModalSizeConstraints {
    /// Create an unconstrained size spec.
    pub const fn new() -> Self {
        Self {
            min_width: None,
            max_width: None,
            min_height: None,
            max_height: None,
        }
    }

    /// Set minimum width.
    #[must_use]
    pub fn min_width(mut self, value: u16) -> Self {
        self.min_width = Some(value);
        self
    }

    /// Set maximum width.
    #[must_use]
    pub fn max_width(mut self, value: u16) -> Self {
        self.max_width = Some(value);
        self
    }

    /// Set minimum height.
    #[must_use]
    pub fn min_height(mut self, value: u16) -> Self {
        self.min_height = Some(value);
        self
    }

    /// Set maximum height.
    #[must_use]
    pub fn max_height(mut self, value: u16) -> Self {
        self.max_height = Some(value);
        self
    }

    /// Clamp the given size to these constraints (but never exceed available).
    pub fn clamp(self, available: Size) -> Size {
        let mut width = available.width;
        let mut height = available.height;

        if let Some(max_width) = self.max_width {
            width = width.min(max_width);
        }
        if let Some(max_height) = self.max_height {
            height = height.min(max_height);
        }
        if let Some(min_width) = self.min_width {
            width = width.max(min_width).min(available.width);
        }
        if let Some(min_height) = self.min_height {
            height = height.max(min_height).min(available.height);
        }

        Size::new(width, height)
    }
}

/// Modal positioning options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModalPosition {
    #[default]
    Center,
    CenterOffset {
        x: i16,
        y: i16,
    },
    TopCenter {
        margin: u16,
    },
    Custom {
        x: u16,
        y: u16,
    },
}

impl ModalPosition {
    fn resolve(self, area: Rect, size: Size) -> Rect {
        let base_x = area.x as i32;
        let base_y = area.y as i32;
        let max_x = base_x + (area.width as i32 - size.width as i32);
        let max_y = base_y + (area.height as i32 - size.height as i32);

        let (mut x, mut y) = match self {
            Self::Center => (
                base_x + (area.width as i32 - size.width as i32) / 2,
                base_y + (area.height as i32 - size.height as i32) / 2,
            ),
            Self::CenterOffset { x, y } => (
                base_x + (area.width as i32 - size.width as i32) / 2 + x as i32,
                base_y + (area.height as i32 - size.height as i32) / 2 + y as i32,
            ),
            Self::TopCenter { margin } => (
                base_x + (area.width as i32 - size.width as i32) / 2,
                base_y + margin as i32,
            ),
            Self::Custom { x, y } => (x as i32, y as i32),
        };

        x = x.clamp(base_x, max_x);
        y = y.clamp(base_y, max_y);

        Rect::new(x as u16, y as u16, size.width, size.height)
    }
}

/// Modal configuration.
#[derive(Debug, Clone)]
pub struct ModalConfig {
    pub position: ModalPosition,
    pub backdrop: BackdropConfig,
    pub size: ModalSizeConstraints,
    pub close_on_backdrop: bool,
    pub close_on_escape: bool,
    pub hit_id: Option<HitId>,
}

impl Default for ModalConfig {
    fn default() -> Self {
        Self {
            position: ModalPosition::Center,
            backdrop: BackdropConfig::default(),
            size: ModalSizeConstraints::default(),
            close_on_backdrop: true,
            close_on_escape: true,
            hit_id: None,
        }
    }
}

impl ModalConfig {
    #[must_use]
    pub fn position(mut self, position: ModalPosition) -> Self {
        self.position = position;
        self
    }

    #[must_use]
    pub fn backdrop(mut self, backdrop: BackdropConfig) -> Self {
        self.backdrop = backdrop;
        self
    }

    #[must_use]
    pub fn size(mut self, size: ModalSizeConstraints) -> Self {
        self.size = size;
        self
    }

    #[must_use]
    pub fn close_on_backdrop(mut self, close: bool) -> Self {
        self.close_on_backdrop = close;
        self
    }

    #[must_use]
    pub fn close_on_escape(mut self, close: bool) -> Self {
        self.close_on_escape = close;
        self
    }

    #[must_use]
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.hit_id = Some(id);
        self
    }
}

/// Stateful helper for modal close behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModalState {
    open: bool,
}

impl Default for ModalState {
    fn default() -> Self {
        Self { open: true }
    }
}

impl ModalState {
    #[inline]
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Handle events and return a modal action if triggered.
    ///
    /// The caller should pass the hit-test result for the mouse event
    /// (usually from the last rendered frame).
    pub fn handle_event(
        &mut self,
        event: &Event,
        hit: Option<(HitId, HitRegion, HitData)>,
        config: &ModalConfig,
    ) -> Option<ModalAction> {
        if !self.open {
            return None;
        }

        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Escape,
                kind: KeyEventKind::Press,
                ..
            }) if config.close_on_escape => {
                self.open = false;
                return Some(ModalAction::EscapePressed);
            }
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                ..
            }) if config.close_on_backdrop => {
                if let (Some((id, region, _)), Some(expected)) = (hit, config.hit_id)
                    && id == expected
                    && region == MODAL_HIT_BACKDROP
                {
                    self.open = false;
                    return Some(ModalAction::BackdropClicked);
                }
            }
            _ => {}
        }

        None
    }
}

/// Modal container widget.
///
/// Invariants:
/// - `content_rect()` is always clamped within the given `area`.
/// - Size constraints are applied before positioning and never exceed `area`.
///
/// Failure modes:
/// - If the available `area` is empty or constraints clamp to zero size,
///   the content is not rendered.
/// - `close_on_backdrop` requires `hit_id` to be set; otherwise backdrop clicks
///   cannot be distinguished from content clicks.
#[derive(Debug, Clone)]
pub struct Modal<C> {
    content: C,
    config: ModalConfig,
}

impl<C> Modal<C> {
    /// Create a new modal with content.
    pub fn new(content: C) -> Self {
        Self {
            content,
            config: ModalConfig::default(),
        }
    }

    /// Set modal configuration.
    #[must_use]
    pub fn config(mut self, config: ModalConfig) -> Self {
        self.config = config;
        self
    }

    /// Set modal position.
    #[must_use]
    pub fn position(mut self, position: ModalPosition) -> Self {
        self.config.position = position;
        self
    }

    /// Set backdrop configuration.
    #[must_use]
    pub fn backdrop(mut self, backdrop: BackdropConfig) -> Self {
        self.config.backdrop = backdrop;
        self
    }

    /// Set size constraints.
    #[must_use]
    pub fn size(mut self, size: ModalSizeConstraints) -> Self {
        self.config.size = size;
        self
    }

    /// Set close-on-backdrop behavior.
    #[must_use]
    pub fn close_on_backdrop(mut self, close: bool) -> Self {
        self.config.close_on_backdrop = close;
        self
    }

    /// Set close-on-escape behavior.
    #[must_use]
    pub fn close_on_escape(mut self, close: bool) -> Self {
        self.config.close_on_escape = close;
        self
    }

    /// Set the hit id used for backdrop/content hit regions.
    #[must_use]
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.config.hit_id = Some(id);
        self
    }

    /// Compute the content rectangle for the given area.
    pub fn content_rect(&self, area: Rect) -> Rect {
        let available = Size::new(area.width, area.height);
        let size = self.config.size.clamp(available);
        if size.width == 0 || size.height == 0 {
            return Rect::new(area.x, area.y, 0, 0);
        }
        self.config.position.resolve(area, size)
    }
}

impl<C: Widget> Widget for Modal<C> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        // Backdrop (full area), preserving existing glyphs.
        let opacity = self.config.backdrop.opacity.clamp(0.0, 1.0);
        if opacity > 0.0 {
            let bg = self.config.backdrop.color.with_opacity(opacity);
            set_style_area(&mut frame.buffer, area, Style::new().bg(bg));
        }

        // Register hit regions BEFORE content renders so the inner widget
        // (e.g. Dialog) can overlay more specific hits (buttons) on top.
        let content_area = self.content_rect(area);
        if let Some(hit_id) = self.config.hit_id {
            frame.register_hit(area, hit_id, MODAL_HIT_BACKDROP, 0);
            if !content_area.is_empty() {
                frame.register_hit(content_area, hit_id, MODAL_HIT_CONTENT, 0);
            }
        }

        if !content_area.is_empty() {
            self.content.render(content_area, frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    #[derive(Debug, Clone)]
    struct Stub;

    impl Widget for Stub {
        fn render(&self, _area: Rect, _frame: &mut Frame) {}
    }

    #[test]
    fn center_positioning() {
        let modal = Modal::new(Stub).size(
            ModalSizeConstraints::new()
                .min_width(10)
                .max_width(10)
                .min_height(4)
                .max_height(4),
        );
        let area = Rect::new(0, 0, 40, 20);
        let rect = modal.content_rect(area);
        assert_eq!(rect, Rect::new(15, 8, 10, 4));
    }

    #[test]
    fn offset_positioning() {
        let modal = Modal::new(Stub)
            .size(
                ModalSizeConstraints::new()
                    .min_width(10)
                    .max_width(10)
                    .min_height(4)
                    .max_height(4),
            )
            .position(ModalPosition::CenterOffset { x: -2, y: 3 });
        let area = Rect::new(0, 0, 40, 20);
        let rect = modal.content_rect(area);
        assert_eq!(rect, Rect::new(13, 11, 10, 4));
    }

    #[test]
    fn size_constraints_respect_available() {
        let modal = Modal::new(Stub).size(
            ModalSizeConstraints::new()
                .min_width(10)
                .max_width(30)
                .min_height(6)
                .max_height(20),
        );
        let area = Rect::new(0, 0, 8, 4);
        let rect = modal.content_rect(area);
        assert_eq!(rect.width, 8);
        assert_eq!(rect.height, 4);
    }

    #[test]
    fn hit_regions_registered() {
        let modal = Modal::new(Stub)
            .size(
                ModalSizeConstraints::new()
                    .min_width(6)
                    .max_width(6)
                    .min_height(3)
                    .max_height(3),
            )
            .hit_id(HitId::new(7));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(20, 10, &mut pool);
        let area = Rect::new(0, 0, 20, 10);
        modal.render(area, &mut frame);

        let backdrop_hit = frame.hit_test(0, 0);
        assert_eq!(backdrop_hit, Some((HitId::new(7), MODAL_HIT_BACKDROP, 0)));

        let content = modal.content_rect(area);
        let cx = content.x + 1;
        let cy = content.y + 1;
        let content_hit = frame.hit_test(cx, cy);
        assert_eq!(content_hit, Some((HitId::new(7), MODAL_HIT_CONTENT, 0)));
    }

    #[test]
    fn backdrop_click_triggers_close() {
        let mut state = ModalState::default();
        let config = ModalConfig::default().hit_id(HitId::new(9));
        let hit = Some((HitId::new(9), MODAL_HIT_BACKDROP, 0));
        let event = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            0,
            0,
        ));

        let action = state.handle_event(&event, hit, &config);
        assert_eq!(action, Some(ModalAction::BackdropClicked));
        assert!(!state.is_open());
    }

    #[test]
    fn content_rect_within_bounds_for_positions() {
        let base_constraints = ModalSizeConstraints::new()
            .min_width(2)
            .min_height(2)
            .max_width(30)
            .max_height(10);
        let positions = [
            ModalPosition::Center,
            ModalPosition::CenterOffset { x: 3, y: -2 },
            ModalPosition::TopCenter { margin: 1 },
            ModalPosition::Custom { x: 100, y: 100 },
        ];
        let areas = [
            Rect::new(0, 0, 10, 6),
            Rect::new(2, 3, 40, 20),
            Rect::new(5, 1, 8, 4),
        ];

        for area in areas {
            for &position in &positions {
                let modal = Modal::new(Stub).size(base_constraints).position(position);
                let rect = modal.content_rect(area);
                if rect.is_empty() {
                    continue;
                }
                assert!(rect.x >= area.x);
                assert!(rect.y >= area.y);
                assert!(rect.right() <= area.right());
                assert!(rect.bottom() <= area.bottom());
            }
        }
    }

    // --- BackdropConfig tests ---

    #[test]
    fn backdrop_config_default() {
        let bd = BackdropConfig::default();
        assert_eq!(bd.color, PackedRgba::rgb(0, 0, 0));
        assert!((bd.opacity - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn backdrop_config_new_and_builders() {
        let bd = BackdropConfig::new(PackedRgba::rgb(255, 0, 0), 0.8)
            .color(PackedRgba::rgb(0, 255, 0))
            .opacity(0.3);
        assert_eq!(bd.color, PackedRgba::rgb(0, 255, 0));
        assert!((bd.opacity - 0.3).abs() < f32::EPSILON);
    }

    // --- ModalSizeConstraints tests ---

    #[test]
    fn size_constraints_unconstrained() {
        let c = ModalSizeConstraints::new();
        let result = c.clamp(Size::new(40, 20));
        assert_eq!(result, Size::new(40, 20));
    }

    #[test]
    fn size_constraints_max_only() {
        let c = ModalSizeConstraints::new().max_width(10).max_height(5);
        let result = c.clamp(Size::new(40, 20));
        assert_eq!(result, Size::new(10, 5));
    }

    #[test]
    fn size_constraints_min_only() {
        let c = ModalSizeConstraints::new().min_width(10).min_height(5);
        // Available is larger than min: result = available
        assert_eq!(c.clamp(Size::new(40, 20)), Size::new(40, 20));
    }

    #[test]
    fn size_constraints_min_exceeds_available() {
        let c = ModalSizeConstraints::new().min_width(50).min_height(30);
        // min > available: clamped back to available
        let result = c.clamp(Size::new(10, 6));
        assert_eq!(result, Size::new(10, 6));
    }

    #[test]
    fn size_constraints_zero_available() {
        let c = ModalSizeConstraints::new()
            .min_width(10)
            .max_width(20)
            .min_height(5)
            .max_height(10);
        let result = c.clamp(Size::new(0, 0));
        assert_eq!(result, Size::new(0, 0));
    }

    #[test]
    fn size_constraints_min_and_max_equal() {
        let c = ModalSizeConstraints::new()
            .min_width(10)
            .max_width(10)
            .min_height(5)
            .max_height(5);
        let result = c.clamp(Size::new(40, 20));
        assert_eq!(result, Size::new(10, 5));
    }

    #[test]
    fn size_constraints_default_is_unconstrained() {
        let c = ModalSizeConstraints::default();
        assert_eq!(c.min_width, None);
        assert_eq!(c.max_width, None);
        assert_eq!(c.min_height, None);
        assert_eq!(c.max_height, None);
    }

    // --- ModalPosition tests ---

    #[test]
    fn position_top_center() {
        let pos = ModalPosition::TopCenter { margin: 2 };
        let area = Rect::new(0, 0, 40, 20);
        let size = Size::new(10, 4);
        let rect = pos.resolve(area, size);
        assert_eq!(rect.x, 15); // centered: (40-10)/2 = 15
        assert_eq!(rect.y, 2); // margin from top
        assert_eq!(rect.width, 10);
        assert_eq!(rect.height, 4);
    }

    #[test]
    fn position_custom_within_bounds() {
        let pos = ModalPosition::Custom { x: 5, y: 3 };
        let area = Rect::new(0, 0, 40, 20);
        let size = Size::new(10, 4);
        let rect = pos.resolve(area, size);
        assert_eq!(rect, Rect::new(5, 3, 10, 4));
    }

    #[test]
    fn position_custom_clamped_to_area() {
        // Custom position beyond area bounds gets clamped
        let pos = ModalPosition::Custom { x: 100, y: 100 };
        let area = Rect::new(0, 0, 40, 20);
        let size = Size::new(10, 4);
        let rect = pos.resolve(area, size);
        assert_eq!(rect.x, 30); // max_x = 40-10
        assert_eq!(rect.y, 16); // max_y = 20-4
    }

    #[test]
    fn position_center_offset_clamped() {
        // Large negative offset gets clamped to area origin
        let pos = ModalPosition::CenterOffset { x: -100, y: -100 };
        let area = Rect::new(0, 0, 40, 20);
        let size = Size::new(10, 4);
        let rect = pos.resolve(area, size);
        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, 0);
    }

    #[test]
    fn position_default_is_center() {
        assert_eq!(ModalPosition::default(), ModalPosition::Center);
    }

    #[test]
    fn position_resolve_with_nonzero_area_origin() {
        let pos = ModalPosition::Center;
        let area = Rect::new(10, 5, 40, 20);
        let size = Size::new(10, 4);
        let rect = pos.resolve(area, size);
        // center_x = 10 + (40-10)/2 = 25
        // center_y = 5 + (20-4)/2 = 13
        assert_eq!(rect, Rect::new(25, 13, 10, 4));
    }

    #[test]
    fn position_top_center_with_area_offset() {
        let pos = ModalPosition::TopCenter { margin: 1 };
        let area = Rect::new(5, 3, 20, 10);
        let size = Size::new(8, 4);
        let rect = pos.resolve(area, size);
        // center_x = 5 + (20-8)/2 = 11
        // y = 3 + 1 = 4
        assert_eq!(rect, Rect::new(11, 4, 8, 4));
    }

    // --- ModalConfig tests ---

    #[test]
    fn modal_config_default_values() {
        let config = ModalConfig::default();
        assert_eq!(config.position, ModalPosition::Center);
        assert!(config.close_on_backdrop);
        assert!(config.close_on_escape);
        assert!(config.hit_id.is_none());
    }

    #[test]
    fn modal_config_builder_chain() {
        let config = ModalConfig::default()
            .position(ModalPosition::TopCenter { margin: 5 })
            .backdrop(BackdropConfig::new(PackedRgba::rgb(255, 0, 0), 0.5))
            .size(ModalSizeConstraints::new().max_width(20))
            .close_on_backdrop(false)
            .close_on_escape(false)
            .hit_id(HitId::new(42));
        assert_eq!(config.position, ModalPosition::TopCenter { margin: 5 });
        assert!(!config.close_on_backdrop);
        assert!(!config.close_on_escape);
        assert_eq!(config.hit_id, Some(HitId::new(42)));
    }

    // --- ModalState tests ---

    #[test]
    fn modal_state_default_is_open() {
        let state = ModalState::default();
        assert!(state.is_open());
    }

    #[test]
    fn modal_state_open_close_lifecycle() {
        let mut state = ModalState::default();
        assert!(state.is_open());
        state.close();
        assert!(!state.is_open());
        state.open();
        assert!(state.is_open());
    }

    #[test]
    fn modal_state_escape_closes() {
        let mut state = ModalState::default();
        let config = ModalConfig::default();
        let event = Event::Key(KeyEvent::new(KeyCode::Escape));
        let action = state.handle_event(&event, None, &config);
        assert_eq!(action, Some(ModalAction::EscapePressed));
        assert!(!state.is_open());
    }

    #[test]
    fn modal_state_escape_disabled() {
        let mut state = ModalState::default();
        let config = ModalConfig::default().close_on_escape(false);
        let event = Event::Key(KeyEvent::new(KeyCode::Escape));
        let action = state.handle_event(&event, None, &config);
        assert_eq!(action, None);
        assert!(state.is_open());
    }

    #[test]
    fn modal_state_closed_ignores_events() {
        let mut state = ModalState::default();
        state.close();
        let config = ModalConfig::default();
        let event = Event::Key(KeyEvent::new(KeyCode::Escape));
        let action = state.handle_event(&event, None, &config);
        assert_eq!(action, None);
        assert!(!state.is_open());
    }

    #[test]
    fn modal_state_content_click_does_not_close() {
        let mut state = ModalState::default();
        let config = ModalConfig::default().hit_id(HitId::new(1));
        let hit = Some((HitId::new(1), MODAL_HIT_CONTENT, 0));
        let event = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            5,
            5,
        ));
        let action = state.handle_event(&event, hit, &config);
        assert_eq!(action, None);
        assert!(state.is_open());
    }

    #[test]
    fn modal_state_backdrop_click_wrong_hit_id() {
        let mut state = ModalState::default();
        let config = ModalConfig::default().hit_id(HitId::new(1));
        // Hit id doesn't match config
        let hit = Some((HitId::new(999), MODAL_HIT_BACKDROP, 0));
        let event = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            0,
            0,
        ));
        let action = state.handle_event(&event, hit, &config);
        assert_eq!(action, None);
        assert!(state.is_open());
    }

    #[test]
    fn modal_state_backdrop_click_no_hit_id_in_config() {
        let mut state = ModalState::default();
        // close_on_backdrop is true, but config has no hit_id
        let config = ModalConfig::default();
        let hit = Some((HitId::new(1), MODAL_HIT_BACKDROP, 0));
        let event = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            0,
            0,
        ));
        let action = state.handle_event(&event, hit, &config);
        assert_eq!(action, None);
        assert!(state.is_open());
    }

    #[test]
    fn modal_state_backdrop_click_disabled() {
        let mut state = ModalState::default();
        let config = ModalConfig::default()
            .hit_id(HitId::new(1))
            .close_on_backdrop(false);
        let hit = Some((HitId::new(1), MODAL_HIT_BACKDROP, 0));
        let event = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            0,
            0,
        ));
        let action = state.handle_event(&event, hit, &config);
        assert_eq!(action, None);
        assert!(state.is_open());
    }

    #[test]
    fn modal_state_right_click_does_not_close() {
        let mut state = ModalState::default();
        let config = ModalConfig::default().hit_id(HitId::new(1));
        let hit = Some((HitId::new(1), MODAL_HIT_BACKDROP, 0));
        let event = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Right),
            0,
            0,
        ));
        let action = state.handle_event(&event, hit, &config);
        assert_eq!(action, None);
        assert!(state.is_open());
    }

    #[test]
    fn modal_state_no_hit_data_backdrop_click() {
        let mut state = ModalState::default();
        let config = ModalConfig::default().hit_id(HitId::new(1));
        // No hit (mouse missed all regions)
        let event = Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            0,
            0,
        ));
        let action = state.handle_event(&event, None, &config);
        assert_eq!(action, None);
        assert!(state.is_open());
    }

    // --- Modal widget tests ---

    #[test]
    fn modal_content_rect_zero_area() {
        let modal = Modal::new(Stub);
        let area = Rect::new(0, 0, 0, 0);
        let rect = modal.content_rect(area);
        assert!(rect.is_empty());
    }

    #[test]
    fn modal_render_empty_area_does_nothing() {
        let modal = Modal::new(Stub);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(10, 10, &mut pool);
        // Empty area should be a no-op
        modal.render(Rect::new(0, 0, 0, 0), &mut frame);
        // No hits registered
        assert_eq!(frame.hit_test(0, 0), None);
    }

    #[test]
    fn modal_no_hit_regions_without_hit_id() {
        let modal = Modal::new(Stub).size(
            ModalSizeConstraints::new()
                .min_width(4)
                .max_width(4)
                .min_height(2)
                .max_height(2),
        );
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(20, 10, &mut pool);
        modal.render(Rect::new(0, 0, 20, 10), &mut frame);
        // No hit_id -> no hit regions
        assert_eq!(frame.hit_test(0, 0), None);
        assert_eq!(frame.hit_test(10, 5), None);
    }

    #[test]
    fn modal_builder_methods() {
        let modal = Modal::new(Stub)
            .position(ModalPosition::TopCenter { margin: 3 })
            .backdrop(BackdropConfig::new(PackedRgba::rgb(0, 0, 0), 0.5))
            .size(ModalSizeConstraints::new().max_width(10).max_height(5))
            .close_on_backdrop(false)
            .close_on_escape(false)
            .hit_id(HitId::new(99));

        assert_eq!(
            modal.config.position,
            ModalPosition::TopCenter { margin: 3 }
        );
        assert!(!modal.config.close_on_backdrop);
        assert!(!modal.config.close_on_escape);
        assert_eq!(modal.config.hit_id, Some(HitId::new(99)));
    }

    #[test]
    fn modal_config_method_replaces_full_config() {
        let config = ModalConfig::default()
            .close_on_escape(false)
            .hit_id(HitId::new(5));
        let modal = Modal::new(Stub).config(config);
        assert!(!modal.config.close_on_escape);
        assert_eq!(modal.config.hit_id, Some(HitId::new(5)));
    }

    #[test]
    fn modal_content_rect_size_bigger_than_area() {
        // When content size exceeds area, it gets clamped
        let modal = Modal::new(Stub).size(
            ModalSizeConstraints::new()
                .min_width(100)
                .max_width(100)
                .min_height(100)
                .max_height(100),
        );
        let area = Rect::new(0, 0, 20, 10);
        let rect = modal.content_rect(area);
        // min > available: clamped to available
        assert_eq!(rect.width, 20);
        assert_eq!(rect.height, 10);
    }

    // --- ModalAction tests ---

    #[test]
    fn modal_action_variants_are_distinct() {
        assert_ne!(ModalAction::Close, ModalAction::BackdropClicked);
        assert_ne!(ModalAction::Close, ModalAction::EscapePressed);
        assert_ne!(ModalAction::BackdropClicked, ModalAction::EscapePressed);
    }

    // --- Hit region constants ---

    #[test]
    fn hit_region_constants_are_distinct() {
        assert_ne!(MODAL_HIT_BACKDROP, MODAL_HIT_CONTENT);
    }
}
