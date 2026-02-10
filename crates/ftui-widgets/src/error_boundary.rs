#![forbid(unsafe_code)]

//! Widget error boundaries with panic recovery.
//!
//! Wraps any widget in a safety boundary that catches panics during rendering
//! and displays a fallback error indicator instead of crashing the application.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::{display_width, grapheme_width};
use unicode_segmentation::UnicodeSegmentation;

use crate::{StatefulWidget, Widget, apply_style, draw_text_span, set_style_area};

/// Captured error from a widget panic.
#[derive(Debug, Clone)]
pub struct CapturedError {
    /// Error message extracted from the panic payload.
    pub message: String,
    /// Name of the widget that panicked.
    pub widget_name: &'static str,
    /// Area the widget was rendering into.
    pub area: Rect,
    /// When the error was captured.
    pub timestamp: Instant,
}

impl CapturedError {
    fn from_panic(
        payload: Box<dyn std::any::Any + Send>,
        widget_name: &'static str,
        area: Rect,
    ) -> Self {
        let mut message = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };
        if let Some(stripped) = message.strip_prefix("internal error: entered unreachable code: ") {
            message = stripped.to_string();
        }

        Self {
            message,
            widget_name,
            area,
            timestamp: Instant::now(),
        }
    }
}

/// State for an error boundary.
#[derive(Debug, Clone, Default)]
pub enum ErrorBoundaryState {
    /// Widget is rendering normally.
    #[default]
    Healthy,
    /// Widget panicked and is showing fallback.
    Failed(CapturedError),
    /// Attempting recovery after failure.
    Recovering {
        /// Number of recovery attempts so far.
        attempts: u32,
        /// The error that triggered recovery.
        last_error: CapturedError,
    },
}

impl ErrorBoundaryState {
    /// Returns the current error, if any.
    #[must_use = "use the returned error for diagnostics"]
    pub fn error(&self) -> Option<&CapturedError> {
        match self {
            Self::Healthy => None,
            Self::Failed(e) => Some(e),
            Self::Recovering { last_error, .. } => Some(last_error),
        }
    }

    /// Returns true if in a failed or recovering state.
    pub fn is_failed(&self) -> bool {
        !matches!(self, Self::Healthy)
    }

    /// Reset to healthy state.
    pub fn reset(&mut self) {
        *self = Self::Healthy;
    }

    /// Attempt recovery. Returns true if recovery attempt was initiated.
    pub fn try_recover(&mut self, max_attempts: u32) -> bool {
        match self {
            Self::Failed(error) => {
                if max_attempts > 0 {
                    *self = Self::Recovering {
                        attempts: 1,
                        last_error: error.clone(),
                    };
                    true
                } else {
                    false
                }
            }
            Self::Recovering {
                attempts,
                last_error,
            } => {
                if *attempts < max_attempts {
                    *attempts += 1;
                    true
                } else {
                    *self = Self::Failed(last_error.clone());
                    false
                }
            }
            Self::Healthy => true,
        }
    }
}

/// A widget wrapper that catches panics from an inner widget.
///
/// When the inner widget panics during rendering, the error boundary
/// captures the panic and renders a fallback error indicator instead.
///
/// Uses `StatefulWidget` so the error state persists across renders.
///
/// # Example
///
/// ```ignore
/// let boundary = ErrorBoundary::new(my_widget, "my_widget");
/// let mut state = ErrorBoundaryState::default();
/// boundary.render(area, &mut buf, &mut state);
/// ```
#[derive(Debug, Clone)]
pub struct ErrorBoundary<W> {
    inner: W,
    widget_name: &'static str,
    max_recovery_attempts: u32,
}

impl<W: Widget> ErrorBoundary<W> {
    /// Create a new error boundary wrapping the given widget.
    pub fn new(inner: W, widget_name: &'static str) -> Self {
        Self {
            inner,
            widget_name,
            max_recovery_attempts: 3,
        }
    }

    /// Set maximum recovery attempts before permanent fallback.
    #[must_use]
    pub fn max_recovery_attempts(mut self, max: u32) -> Self {
        self.max_recovery_attempts = max;
        self
    }

    /// Get the widget name.
    pub fn widget_name(&self) -> &'static str {
        self.widget_name
    }
}

impl<W: Widget> StatefulWidget for ErrorBoundary<W> {
    type State = ErrorBoundaryState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut ErrorBoundaryState) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "ErrorBoundary",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            return;
        }

        match state {
            ErrorBoundaryState::Healthy | ErrorBoundaryState::Recovering { .. } => {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    self.inner.render(area, frame);
                }));

                match result {
                    Ok(()) => {
                        if matches!(state, ErrorBoundaryState::Recovering { .. }) {
                            *state = ErrorBoundaryState::Healthy;
                        }
                    }
                    Err(payload) => {
                        let error = CapturedError::from_panic(payload, self.widget_name, area);
                        clear_area(frame, area);
                        render_error_fallback(frame, area, &error);
                        *state = ErrorBoundaryState::Failed(error);
                    }
                }
            }
            ErrorBoundaryState::Failed(error) => {
                render_error_fallback(frame, area, error);
            }
        }
    }
}

/// Clear an area of the buffer to spaces.
fn clear_area(frame: &mut Frame, area: Rect) {
    let blank = Cell::from_char(' ');
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            frame.buffer.set_fast(x, y, blank);
        }
    }
}

/// Render a fallback error indicator in the given area.
fn render_error_fallback(frame: &mut Frame, area: Rect, error: &CapturedError) {
    let error_fg = PackedRgba::rgb(255, 60, 60);
    let error_bg = PackedRgba::rgb(40, 0, 0);
    let error_style = Style::new().fg(error_fg).bg(error_bg);
    let border_style = Style::new().fg(error_fg);

    set_style_area(&mut frame.buffer, area, Style::new().bg(error_bg));

    if area.width < 3 || area.height < 1 {
        // Too small for border, just show "!"
        if area.width >= 1 && area.height >= 1 {
            let mut cell = Cell::from_char('!');
            apply_style(&mut cell, error_style);
            frame.buffer.set_fast(area.x, area.y, cell);
        }
        return;
    }

    let top = area.y;
    let bottom = area.y.saturating_add(area.height).saturating_sub(1);
    let left = area.x;
    let right = area.x.saturating_add(area.width).saturating_sub(1);

    // Top border
    for x in left..=right {
        let c = if x == left && area.height > 1 {
            '┌'
        } else if x == right && area.height > 1 {
            '┐'
        } else {
            '─'
        };
        let mut cell = Cell::from_char(c);
        apply_style(&mut cell, border_style);
        frame.buffer.set_fast(x, top, cell);
    }

    // Bottom border
    if area.height > 1 {
        for x in left..=right {
            let c = if x == left {
                '└'
            } else if x == right {
                '┘'
            } else {
                '─'
            };
            let mut cell = Cell::from_char(c);
            apply_style(&mut cell, border_style);
            frame.buffer.set_fast(x, bottom, cell);
        }
    }

    // Side borders
    if area.height > 2 {
        for y in (top + 1)..bottom {
            let mut cell_l = Cell::from_char('│');
            apply_style(&mut cell_l, border_style);
            frame.buffer.set_fast(left, y, cell_l);

            let mut cell_r = Cell::from_char('│');
            apply_style(&mut cell_r, border_style);
            frame.buffer.set_fast(right, y, cell_r);
        }
    }

    // Title "[Error]" on top border
    if area.width >= 9 {
        let title_x = left.saturating_add(1);
        draw_text_span(frame, title_x, top, "[Error]", border_style, right);
    }

    // Error message inside
    if area.height >= 3 && area.width >= 5 {
        let inner_left = left.saturating_add(2);
        let inner_right = right;
        let inner_y = top.saturating_add(1);
        let max_chars = (inner_right.saturating_sub(inner_left)) as usize;

        let msg: String = if display_width(error.message.as_str()) > max_chars.saturating_sub(2) {
            let mut truncated = String::new();
            let mut w = 0;
            let limit = max_chars.saturating_sub(3);
            for grapheme in error.message.graphemes(true) {
                let gw = grapheme_width(grapheme);
                if w + gw > limit {
                    break;
                }
                truncated.push_str(grapheme);
                w += gw;
            }
            format!("! {truncated}\u{2026}")
        } else {
            format!("! {}", error.message)
        };

        draw_text_span(frame, inner_left, inner_y, &msg, error_style, inner_right);

        // Widget name on next line if space
        if area.height >= 4 {
            let name_msg = format!("  in: {}", error.widget_name);
            let name_style = Style::new().fg(PackedRgba::rgb(180, 180, 180)).bg(error_bg);
            draw_text_span(
                frame,
                inner_left,
                inner_y.saturating_add(1),
                &name_msg,
                name_style,
                inner_right,
            );
        }

        // Retry hint on next available line
        if area.height >= 5 {
            let hint_style = Style::new().fg(PackedRgba::rgb(120, 120, 120)).bg(error_bg);
            draw_text_span(
                frame,
                inner_left,
                inner_y.saturating_add(2),
                "  Press R to retry",
                hint_style,
                inner_right,
            );
        }
    }
}

/// A standalone fallback widget for rendering error indicators.
///
/// Can be used independently of `ErrorBoundary` when you need to
/// display an error state in a widget area.
#[derive(Debug, Clone)]
pub struct FallbackWidget {
    error: CapturedError,
    show_retry_hint: bool,
}

impl FallbackWidget {
    /// Create a new fallback widget for the given error.
    pub fn new(error: CapturedError) -> Self {
        Self {
            error,
            show_retry_hint: true,
        }
    }

    /// Create a fallback with a simple message and widget name.
    pub fn from_message(message: impl Into<String>, widget_name: &'static str) -> Self {
        Self::new(CapturedError {
            message: message.into(),
            widget_name,
            area: Rect::default(),
            timestamp: Instant::now(),
        })
    }

    /// Disable the retry hint.
    #[must_use]
    pub fn without_retry_hint(mut self) -> Self {
        self.show_retry_hint = false;
        self
    }
}

impl Widget for FallbackWidget {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "FallbackWidget",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            return;
        }
        render_error_fallback(frame, area, &self.error);

        // If retry hint is disabled, overwrite it with background
        if !self.show_retry_hint && area.height >= 5 {
            let error_bg = PackedRgba::rgb(40, 0, 0);
            let bg_style = Style::new().bg(error_bg);
            let inner_y = area.y.saturating_add(3);
            let inner_left = area.x.saturating_add(2);
            let inner_right = area.x.saturating_add(area.width).saturating_sub(1);
            // Clear the retry hint line
            for x in inner_left..inner_right {
                if let Some(cell) = frame.buffer.get_mut(x, inner_y) {
                    cell.content = ftui_render::cell::CellContent::from_char(' ');
                    apply_style(cell, bg_style);
                }
            }
        }
    }
}

/// Type alias for custom fallback factory functions.
pub type FallbackFactory = Box<dyn Fn(&CapturedError) -> FallbackWidget + Send + Sync>;

/// A widget wrapper with custom fallback support.
///
/// Like `ErrorBoundary`, but accepts a custom factory for producing
/// fallback widgets when the inner widget panics.
pub struct CustomErrorBoundary<W> {
    inner: W,
    widget_name: &'static str,
    max_recovery_attempts: u32,
    fallback_factory: Option<FallbackFactory>,
}

impl<W: Widget> CustomErrorBoundary<W> {
    /// Create with a custom fallback factory.
    pub fn new(inner: W, widget_name: &'static str) -> Self {
        Self {
            inner,
            widget_name,
            max_recovery_attempts: 3,
            fallback_factory: None,
        }
    }

    /// Set the fallback factory.
    #[must_use]
    pub fn fallback_factory(
        mut self,
        factory: impl Fn(&CapturedError) -> FallbackWidget + Send + Sync + 'static,
    ) -> Self {
        self.fallback_factory = Some(Box::new(factory));
        self
    }

    /// Set maximum recovery attempts.
    #[must_use]
    pub fn max_recovery_attempts(mut self, max: u32) -> Self {
        self.max_recovery_attempts = max;
        self
    }
}

impl<W: Widget> StatefulWidget for CustomErrorBoundary<W> {
    type State = ErrorBoundaryState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut ErrorBoundaryState) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "CustomErrorBoundary",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            return;
        }

        match state {
            ErrorBoundaryState::Healthy | ErrorBoundaryState::Recovering { .. } => {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    self.inner.render(area, frame);
                }));

                match result {
                    Ok(()) => {
                        if matches!(state, ErrorBoundaryState::Recovering { .. }) {
                            *state = ErrorBoundaryState::Healthy;
                        }
                    }
                    Err(payload) => {
                        let error = CapturedError::from_panic(payload, self.widget_name, area);
                        clear_area(frame, area);
                        if let Some(factory) = &self.fallback_factory {
                            let fallback = factory(&error);
                            fallback.render(area, frame);
                        } else {
                            render_error_fallback(frame, area, &error);
                        }
                        *state = ErrorBoundaryState::Failed(error);
                    }
                }
            }
            ErrorBoundaryState::Failed(error) => {
                if let Some(factory) = &self.fallback_factory {
                    let fallback = factory(error);
                    fallback.render(area, frame);
                } else {
                    render_error_fallback(frame, area, error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    struct PanickingWidget;

    impl Widget for PanickingWidget {
        fn render(&self, _area: Rect, _frame: &mut Frame) {
            unreachable!("widget exploded");
        }
    }

    struct GoodWidget;

    impl Widget for GoodWidget {
        fn render(&self, area: Rect, frame: &mut Frame) {
            if area.width > 0 && area.height > 0 {
                frame.buffer.set(area.x, area.y, Cell::from_char('G'));
            }
        }
    }

    #[test]
    fn healthy_widget_renders_normally() {
        let boundary = ErrorBoundary::new(GoodWidget, "good");
        let mut state = ErrorBoundaryState::default();
        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);

        boundary.render(area, &mut frame, &mut state);

        assert!(!state.is_failed());
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('G'));
    }

    #[test]
    fn catches_panic_without_propagating() {
        let boundary = ErrorBoundary::new(PanickingWidget, "panicker");
        let mut state = ErrorBoundaryState::default();
        let area = Rect::new(0, 0, 30, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);

        boundary.render(area, &mut frame, &mut state);

        assert!(state.is_failed());
        let err = state.error().unwrap();
        assert_eq!(err.message, "widget exploded");
        assert_eq!(err.widget_name, "panicker");
    }

    #[test]
    fn failed_state_shows_fallback_on_rerender() {
        let boundary = ErrorBoundary::new(PanickingWidget, "panicker");
        let mut state = ErrorBoundaryState::default();
        let area = Rect::new(0, 0, 30, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);

        boundary.render(area, &mut frame, &mut state);

        // Second render shows fallback without re-trying
        let mut pool2 = GraphemePool::new();
        let mut frame2 = Frame::new(30, 5, &mut pool2);
        boundary.render(area, &mut frame2, &mut state);

        assert!(state.is_failed());
        assert_eq!(
            frame2.buffer.get(0, 0).unwrap().content.as_char(),
            Some('┌')
        );
    }

    #[test]
    fn recovery_resets_on_success() {
        let good = ErrorBoundary::new(GoodWidget, "good");
        let mut state = ErrorBoundaryState::Failed(CapturedError {
            message: "old error".to_string(),
            widget_name: "old",
            area: Rect::new(0, 0, 10, 5),
            timestamp: Instant::now(),
        });

        assert!(state.try_recover(3));
        assert!(matches!(state, ErrorBoundaryState::Recovering { .. }));

        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        good.render(area, &mut frame, &mut state);

        assert!(!state.is_failed());
        assert!(matches!(state, ErrorBoundaryState::Healthy));
    }

    #[test]
    fn recovery_respects_max_attempts() {
        let mut state = ErrorBoundaryState::Failed(CapturedError {
            message: "error".to_string(),
            widget_name: "w",
            area: Rect::new(0, 0, 1, 1),
            timestamp: Instant::now(),
        });

        assert!(state.try_recover(2));
        assert!(matches!(
            state,
            ErrorBoundaryState::Recovering { attempts: 1, .. }
        ));

        assert!(state.try_recover(2));
        assert!(matches!(
            state,
            ErrorBoundaryState::Recovering { attempts: 2, .. }
        ));

        assert!(!state.try_recover(2));
        assert!(matches!(state, ErrorBoundaryState::Failed(_)));
    }

    #[test]
    fn zero_max_recovery_denies_immediately() {
        let mut state = ErrorBoundaryState::Failed(CapturedError {
            message: "error".to_string(),
            widget_name: "w",
            area: Rect::new(0, 0, 1, 1),
            timestamp: Instant::now(),
        });

        assert!(!state.try_recover(0));
        assert!(matches!(state, ErrorBoundaryState::Failed(_)));
    }

    #[test]
    fn reset_clears_error() {
        let mut state = ErrorBoundaryState::Failed(CapturedError {
            message: "error".to_string(),
            widget_name: "w",
            area: Rect::new(0, 0, 1, 1),
            timestamp: Instant::now(),
        });

        state.reset();
        assert!(!state.is_failed());
        assert!(matches!(state, ErrorBoundaryState::Healthy));
    }

    #[test]
    fn empty_area_is_noop() {
        let boundary = ErrorBoundary::new(PanickingWidget, "panicker");
        let mut state = ErrorBoundaryState::default();
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);

        boundary.render(area, &mut frame, &mut state);

        assert!(!state.is_failed());
    }

    #[test]
    fn small_area_shows_minimal_fallback() {
        let boundary = ErrorBoundary::new(PanickingWidget, "panicker");
        let mut state = ErrorBoundaryState::default();
        let area = Rect::new(0, 0, 2, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);

        boundary.render(area, &mut frame, &mut state);

        assert!(state.is_failed());
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('!'));
    }

    #[test]
    fn captured_error_extracts_string_panic() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("test error".to_string());
        let error = CapturedError::from_panic(payload, "test", Rect::new(0, 0, 1, 1));
        assert_eq!(error.message, "test error");
    }

    #[test]
    fn captured_error_extracts_str_panic() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("static error");
        let error = CapturedError::from_panic(payload, "test", Rect::new(0, 0, 1, 1));
        assert_eq!(error.message, "static error");
    }

    #[test]
    fn captured_error_handles_unknown_panic() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(42u32);
        let error = CapturedError::from_panic(payload, "test", Rect::new(0, 0, 1, 1));
        assert_eq!(error.message, "unknown panic");
    }

    #[test]
    fn failed_state_renders_fallback_directly() {
        let boundary = ErrorBoundary::new(GoodWidget, "good");
        let mut state = ErrorBoundaryState::Failed(CapturedError {
            message: "previous error".to_string(),
            widget_name: "other",
            area: Rect::new(0, 0, 30, 5),
            timestamp: Instant::now(),
        });

        let area = Rect::new(0, 0, 30, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        boundary.render(area, &mut frame, &mut state);

        assert!(state.is_failed());
        // Should see border, not 'G'
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('┌'));
    }

    #[test]
    fn fallback_widget_renders_standalone() {
        let fallback = FallbackWidget::from_message("render failed", "my_widget");
        let area = Rect::new(0, 0, 30, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        fallback.render(area, &mut frame);

        // Should show error border
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('┌'));
    }

    #[test]
    fn fallback_widget_without_retry_hint() {
        let fallback = FallbackWidget::from_message("error", "w").without_retry_hint();
        let area = Rect::new(0, 0, 30, 6);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 6, &mut pool);
        fallback.render(area, &mut frame);

        // Retry hint line (y=3) should be blank spaces, not text
        // The hint would be at inner_y + 2 = area.y + 3 = 3
        let hint_cell = frame.buffer.get(4, 3).unwrap();
        assert_eq!(hint_cell.content.as_char(), Some(' '));
    }

    #[test]
    fn fallback_widget_empty_area() {
        let fallback = FallbackWidget::from_message("error", "w");
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        fallback.render(area, &mut frame);
        // Should not panic
    }

    #[test]
    fn custom_error_boundary_uses_factory() {
        let boundary =
            CustomErrorBoundary::new(PanickingWidget, "panicker").fallback_factory(|error| {
                FallbackWidget::from_message(
                    format!("CUSTOM: {}", error.message),
                    error.widget_name,
                )
                .without_retry_hint()
            });
        let mut state = ErrorBoundaryState::default();
        let area = Rect::new(0, 0, 40, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 5, &mut pool);
        boundary.render(area, &mut frame, &mut state);

        assert!(state.is_failed());
        // Should show the custom error (border should still appear)
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('┌'));
    }

    #[test]
    fn custom_error_boundary_default_fallback() {
        let boundary = CustomErrorBoundary::new(PanickingWidget, "panicker");
        let mut state = ErrorBoundaryState::default();
        let area = Rect::new(0, 0, 30, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        boundary.render(area, &mut frame, &mut state);

        assert!(state.is_failed());
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('┌'));
    }

    #[test]
    fn retry_hint_shows_in_tall_area() {
        let boundary = ErrorBoundary::new(PanickingWidget, "panicker");
        let mut state = ErrorBoundaryState::default();
        let area = Rect::new(0, 0, 30, 6);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 6, &mut pool);
        boundary.render(area, &mut frame, &mut state);

        assert!(state.is_failed());
        // Retry hint at inner_y + 2 = 3
        // The text "Press R to retry" starts at inner_left (x=2) + 2 spaces
        // "  Press R to retry" -> 'P' at x=4
        let p_cell = frame.buffer.get(4, 3).unwrap();
        assert_eq!(p_cell.content.as_char(), Some('P'));
    }

    #[test]
    fn error_in_sibling_does_not_affect_other() {
        let bad = ErrorBoundary::new(PanickingWidget, "bad");
        let good = ErrorBoundary::new(GoodWidget, "good");
        let mut bad_state = ErrorBoundaryState::default();
        let mut good_state = ErrorBoundaryState::default();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);
        let area_a = Rect::new(0, 0, 10, 5);
        let area_b = Rect::new(10, 0, 10, 5);

        bad.render(area_a, &mut frame, &mut bad_state);
        good.render(area_b, &mut frame, &mut good_state);

        assert!(bad_state.is_failed());
        assert!(!good_state.is_failed());
        assert_eq!(
            frame.buffer.get(10, 0).unwrap().content.as_char(),
            Some('G')
        );
    }

    #[test]
    fn max_recovery_attempts_builder() {
        let boundary = ErrorBoundary::new(GoodWidget, "good").max_recovery_attempts(5);
        assert_eq!(boundary.max_recovery_attempts, 5);
    }

    #[test]
    fn widget_name_accessor() {
        let boundary = ErrorBoundary::new(GoodWidget, "my_widget");
        assert_eq!(boundary.widget_name(), "my_widget");
    }

    #[test]
    fn error_state_error_accessor_recovering() {
        let err = CapturedError {
            message: "fail".to_string(),
            widget_name: "w",
            area: Rect::new(0, 0, 1, 1),
            timestamp: Instant::now(),
        };
        let state = ErrorBoundaryState::Recovering {
            attempts: 2,
            last_error: err,
        };
        assert!(state.is_failed());
        assert_eq!(state.error().unwrap().message, "fail");
    }

    #[test]
    fn try_recover_on_healthy_returns_true() {
        let mut state = ErrorBoundaryState::Healthy;
        assert!(state.try_recover(3));
        assert!(matches!(state, ErrorBoundaryState::Healthy));
    }

    #[test]
    fn captured_error_strips_unreachable_prefix() {
        let msg = "internal error: entered unreachable code: widget exploded";
        let payload: Box<dyn std::any::Any + Send> = Box::new(msg.to_string());
        let error = CapturedError::from_panic(payload, "test", Rect::new(0, 0, 1, 1));
        assert_eq!(error.message, "widget exploded");
    }

    #[test]
    fn default_state_is_healthy() {
        let state = ErrorBoundaryState::default();
        assert!(!state.is_failed());
        assert!(state.error().is_none());
    }

    #[test]
    fn custom_boundary_max_recovery_builder() {
        let boundary = CustomErrorBoundary::new(GoodWidget, "good").max_recovery_attempts(7);
        assert_eq!(boundary.max_recovery_attempts, 7);
    }

    #[test]
    fn fallback_widget_new_directly() {
        let err = CapturedError {
            message: "direct error".to_string(),
            widget_name: "direct",
            area: Rect::new(0, 0, 10, 5),
            timestamp: Instant::now(),
        };
        let fallback = FallbackWidget::new(err);
        assert!(fallback.show_retry_hint);
        assert_eq!(fallback.error.message, "direct error");
    }

    #[test]
    fn recovering_state_panics_revert_to_failed() {
        let boundary = ErrorBoundary::new(PanickingWidget, "bad");
        let err = CapturedError {
            message: "initial".to_string(),
            widget_name: "bad",
            area: Rect::new(0, 0, 30, 5),
            timestamp: Instant::now(),
        };
        let mut state = ErrorBoundaryState::Recovering {
            attempts: 1,
            last_error: err,
        };

        let area = Rect::new(0, 0, 30, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        boundary.render(area, &mut frame, &mut state);

        // Panic during recovery should set state to Failed.
        assert!(matches!(state, ErrorBoundaryState::Failed(_)));
    }
}
