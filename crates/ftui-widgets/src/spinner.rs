#![forbid(unsafe_code)]

//! Spinner widget.

use crate::block::Block;
use crate::{StatefulWidget, Widget, set_style_area};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::display_width;

/// Braille dot spinner animation frames.
pub const DOTS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// ASCII line spinner animation frames.
pub const LINE: &[&str] = &["|", "/", "-", "\\"];

/// A widget to display a spinner.
#[derive(Debug, Clone, Default)]
pub struct Spinner<'a> {
    block: Option<Block<'a>>,
    style: Style,
    frames: &'a [&'a str],
    label: Option<&'a str>,
}

impl<'a> Spinner<'a> {
    /// Create a new spinner with default dot frames.
    pub fn new() -> Self {
        Self {
            block: None,
            style: Style::default(),
            frames: DOTS,
            label: None,
        }
    }

    /// Wrap the spinner in a [`Block`] container.
    #[must_use]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Set the base style for the spinner.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the animation frame characters.
    #[must_use]
    pub fn frames(mut self, frames: &'a [&'a str]) -> Self {
        self.frames = frames;
        self
    }

    /// Set a text label displayed next to the spinner.
    #[must_use]
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
}

/// Mutable state for a [`Spinner`] widget.
#[derive(Debug, Clone, Default)]
pub struct SpinnerState {
    /// Index of the currently displayed animation frame.
    pub current_frame: usize,
}

impl SpinnerState {
    /// Advance to the next animation frame.
    pub fn tick(&mut self) {
        self.current_frame = self.current_frame.wrapping_add(1);
    }
}

impl<'a> StatefulWidget for Spinner<'a> {
    type State = SpinnerState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Spinner",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        let deg = frame.buffer.degradation;

        // Skeleton+: skip entirely (spinner is decorative)
        if !deg.render_content() {
            return;
        }

        // EssentialOnly: spinner is decorative, only show label text
        if !deg.render_decorative() {
            if let Some(label) = self.label {
                crate::draw_text_span(frame, area.x, area.y, label, Style::default(), area.right());
            }
            return;
        }

        let spinner_area = match &self.block {
            Some(b) => {
                b.render(area, frame);
                b.inner(area)
            }
            None => area,
        };

        if spinner_area.is_empty() {
            return;
        }

        let style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };

        if deg.apply_styling() {
            set_style_area(&mut frame.buffer, spinner_area, self.style);
        }

        // At NoStyling, use static ASCII frame instead of animated Unicode
        if self.frames.is_empty() {
            return;
        }
        let frame_char = if deg.use_unicode_borders() {
            let frame_idx = state.current_frame % self.frames.len();
            self.frames[frame_idx]
        } else {
            // Use first ASCII-safe frame, or fallback to "*"
            let frame_idx = state.current_frame % self.frames.len();
            let candidate = self.frames[frame_idx];
            if candidate.is_ascii() { candidate } else { "*" }
        };

        let mut x = spinner_area.left();
        let y = spinner_area.top();

        crate::draw_text_span(frame, x, y, frame_char, style, spinner_area.right());

        let w = display_width(frame_char);
        x += w as u16;

        // Render label
        if let Some(label) = self.label {
            x += 1;
            if x < spinner_area.right() {
                crate::draw_text_span(frame, x, y, label, style, spinner_area.right());
            }
        }
    }
}

impl<'a> Widget for Spinner<'a> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = SpinnerState::default();
        StatefulWidget::render(self, area, frame, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::buffer::Buffer;
    use ftui_render::grapheme_pool::GraphemePool;

    fn cell_char(buf: &Buffer, x: u16, y: u16) -> Option<char> {
        buf.get(x, y).and_then(|c| c.content.as_char())
    }

    // --- SpinnerState tests ---

    #[test]
    fn state_default() {
        let state = SpinnerState::default();
        assert_eq!(state.current_frame, 0);
    }

    #[test]
    fn state_tick_increments() {
        let mut state = SpinnerState::default();
        state.tick();
        assert_eq!(state.current_frame, 1);
        state.tick();
        assert_eq!(state.current_frame, 2);
    }

    #[test]
    fn state_tick_wraps_on_overflow() {
        let mut state = SpinnerState {
            current_frame: usize::MAX,
        };
        state.tick();
        assert_eq!(state.current_frame, 0);
    }

    // --- Builder tests ---

    #[test]
    fn default_uses_dots_frames() {
        let spinner = Spinner::new();
        assert_eq!(spinner.frames.len(), DOTS.len());
        assert_eq!(spinner.frames, DOTS);
    }

    #[test]
    fn custom_frames() {
        let frames: &[&str] = &["A", "B", "C"];
        let spinner = Spinner::new().frames(frames);
        assert_eq!(spinner.frames.len(), 3);
    }

    #[test]
    fn builder_label() {
        let spinner = Spinner::new().label("Loading...");
        assert_eq!(spinner.label, Some("Loading..."));
    }

    // --- Rendering tests ---

    #[test]
    fn render_zero_area() {
        let spinner = Spinner::new();
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Widget::render(&spinner, area, &mut frame);
        // Should not panic
    }

    #[test]
    fn stateless_render_uses_frame_zero() {
        let frames: &[&str] = &["A", "B", "C"];
        let spinner = Spinner::new().frames(frames);
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        Widget::render(&spinner, area, &mut frame);

        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
    }

    #[test]
    fn stateful_render_cycles_frames() {
        let frames: &[&str] = &["X", "Y", "Z"];
        let spinner = Spinner::new().frames(frames);
        let area = Rect::new(0, 0, 5, 1);

        // Frame 0 -> "X"
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        let mut state = SpinnerState { current_frame: 0 };
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('X'));

        // Frame 1 -> "Y"
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        state.current_frame = 1;
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('Y'));

        // Frame 2 -> "Z"
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        state.current_frame = 2;
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('Z'));

        // Frame 3 wraps -> "X"
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        state.current_frame = 3;
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('X'));
    }

    #[test]
    fn render_with_label() {
        let frames: &[&str] = &["*"];
        let spinner = Spinner::new().frames(frames).label("Go");
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let mut state = SpinnerState::default();
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);

        // "*" at x=0, then space, then "Go" at x=2
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('*'));
        assert_eq!(cell_char(&frame.buffer, 2, 0), Some('G'));
        assert_eq!(cell_char(&frame.buffer, 3, 0), Some('o'));
    }

    #[test]
    fn render_with_block() {
        let frames: &[&str] = &["!"];
        let spinner = Spinner::new().frames(frames).block(Block::bordered());
        let area = Rect::new(0, 0, 10, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 3, &mut pool);
        let mut state = SpinnerState::default();
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);

        // Inside the border at (1, 1)
        assert_eq!(cell_char(&frame.buffer, 1, 1), Some('!'));
    }

    #[test]
    fn render_line_frames() {
        let spinner = Spinner::new().frames(LINE);
        let area = Rect::new(0, 0, 5, 1);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        let mut state = SpinnerState { current_frame: 0 };
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('|'));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        state.current_frame = 1;
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('/'));
    }

    #[test]
    fn large_frame_index_wraps_correctly() {
        let frames: &[&str] = &["A", "B"];
        let spinner = Spinner::new().frames(frames);
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        let mut state = SpinnerState {
            current_frame: 1000,
        };
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);
        // 1000 % 2 = 0 -> "A"
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('A'));
    }

    #[test]
    fn dots_frame_set_has_expected_length() {
        assert_eq!(DOTS.len(), 10);
    }

    #[test]
    fn line_frame_set_has_expected_length() {
        assert_eq!(LINE.len(), 4);
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_skeleton_skips_entirely() {
        use ftui_render::budget::DegradationLevel;

        let frames: &[&str] = &["*"];
        let spinner = Spinner::new().frames(frames).label("Loading");
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        let mut state = SpinnerState::default();
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);

        // Nothing rendered at Skeleton
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn degradation_essential_only_shows_label_only() {
        use ftui_render::budget::DegradationLevel;

        let frames: &[&str] = &["*"];
        let spinner = Spinner::new().frames(frames).label("Go");
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::EssentialOnly;
        let mut state = SpinnerState::default();
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);

        // Label "Go" rendered, no spinner frame
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('G'));
        assert_eq!(cell_char(&frame.buffer, 1, 0), Some('o'));
    }

    #[test]
    fn degradation_simple_borders_uses_ascii_fallback() {
        use ftui_render::budget::DegradationLevel;

        // Use Unicode frames that should fall back to ASCII
        let spinner = Spinner::new(); // default DOTS frames are Unicode
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::SimpleBorders;
        let mut state = SpinnerState::default();
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);

        // Should use "*" fallback since DOTS are non-ASCII
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('*'));
    }

    #[test]
    fn degradation_full_uses_unicode_frames() {
        use ftui_render::budget::DegradationLevel;

        let spinner = Spinner::new(); // DOTS frames
        let area = Rect::new(0, 0, 5, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        frame.buffer.degradation = DegradationLevel::Full;
        let mut state = SpinnerState::default();
        StatefulWidget::render(&spinner, area, &mut frame, &mut state);

        // Should use the first DOTS frame '⠋'
        assert_eq!(cell_char(&frame.buffer, 0, 0), Some('⠋'));
    }
}
