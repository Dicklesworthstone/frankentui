#![forbid(unsafe_code)]

//! Scrollbar widget.
//!
//! A widget to display a scrollbar.

use crate::{StatefulWidget, Widget};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;

/// Scrollbar orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollbarOrientation {
    /// Vertical scrollbar on the right side.
    #[default]
    VerticalRight,
    /// Vertical scrollbar on the left side.
    VerticalLeft,
    /// Horizontal scrollbar on the bottom.
    HorizontalBottom,
    /// Horizontal scrollbar on the top.
    HorizontalTop,
}

/// A widget to display a scrollbar.
#[derive(Debug, Clone, Default)]
pub struct Scrollbar<'a> {
    orientation: ScrollbarOrientation,
    thumb_style: Style,
    track_style: Style,
    begin_symbol: Option<&'a str>,
    end_symbol: Option<&'a str>,
    track_symbol: Option<&'a str>,
    thumb_symbol: Option<&'a str>,
}

impl<'a> Scrollbar<'a> {
    /// Create a new scrollbar with the given orientation.
    pub fn new(orientation: ScrollbarOrientation) -> Self {
        Self {
            orientation,
            thumb_style: Style::default(),
            track_style: Style::default(),
            begin_symbol: None,
            end_symbol: None,
            track_symbol: None,
            thumb_symbol: None,
        }
    }

    /// Set the style for the thumb (draggable indicator).
    pub fn thumb_style(mut self, style: Style) -> Self {
        self.thumb_style = style;
        self
    }

    /// Set the style for the track background.
    pub fn track_style(mut self, style: Style) -> Self {
        self.track_style = style;
        self
    }

    /// Set custom symbols for track, thumb, begin, and end markers.
    pub fn symbols(
        mut self,
        track: &'a str,
        thumb: &'a str,
        begin: Option<&'a str>,
        end: Option<&'a str>,
    ) -> Self {
        self.track_symbol = Some(track);
        self.thumb_symbol = Some(thumb);
        self.begin_symbol = begin;
        self.end_symbol = end;
        self
    }
}

/// Mutable state for a [`Scrollbar`] widget.
#[derive(Debug, Clone, Default)]
pub struct ScrollbarState {
    /// Total number of scrollable content units.
    pub content_length: usize,
    /// Current scroll position within the content.
    pub position: usize,
    /// Number of content units visible in the viewport.
    pub viewport_length: usize,
}

impl ScrollbarState {
    /// Create a new scrollbar state with given content, position, and viewport sizes.
    pub fn new(content_length: usize, position: usize, viewport_length: usize) -> Self {
        Self {
            content_length,
            position,
            viewport_length,
        }
    }
}

impl<'a> StatefulWidget for Scrollbar<'a> {
    type State = ScrollbarState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Scrollbar",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        // Scrollbar is decorative — skip at EssentialOnly+
        if !frame.buffer.degradation.render_decorative() {
            return;
        }

        if area.is_empty() || state.content_length == 0 {
            return;
        }

        let is_vertical = match self.orientation {
            ScrollbarOrientation::VerticalRight | ScrollbarOrientation::VerticalLeft => true,
            ScrollbarOrientation::HorizontalBottom | ScrollbarOrientation::HorizontalTop => false,
        };

        let length = if is_vertical { area.height } else { area.width } as usize;
        if length == 0 {
            return;
        }

        // Calculate scrollbar layout
        // Simplified logic: track is the full length
        let track_len = length;

        // Calculate thumb size and position
        let viewport_ratio = state.viewport_length as f64 / state.content_length as f64;
        let thumb_size = (track_len as f64 * viewport_ratio).max(1.0).round() as usize;
        let thumb_size = thumb_size.min(track_len);

        let max_pos = state.content_length.saturating_sub(state.viewport_length);
        let pos_ratio = if max_pos == 0 {
            0.0
        } else {
            state.position.min(max_pos) as f64 / max_pos as f64
        };

        let available_track = track_len.saturating_sub(thumb_size);
        let thumb_offset = (available_track as f64 * pos_ratio).round() as usize;

        // Symbols
        let track_char = self
            .track_symbol
            .unwrap_or(if is_vertical { "│" } else { "─" });
        let thumb_char = self.thumb_symbol.unwrap_or("█");
        let begin_char = self
            .begin_symbol
            .unwrap_or(if is_vertical { "▲" } else { "◄" });
        let end_char = self
            .end_symbol
            .unwrap_or(if is_vertical { "▼" } else { "►" });

        // Draw
        for i in 0..track_len {
            let is_thumb = i >= thumb_offset && i < thumb_offset + thumb_size;
            let symbol = if is_thumb {
                thumb_char
            } else if i == 0 && self.begin_symbol.is_some() {
                begin_char
            } else if i == track_len - 1 && self.end_symbol.is_some() {
                end_char
            } else {
                track_char
            };

            let style = if !frame.buffer.degradation.apply_styling() {
                Style::default()
            } else if is_thumb {
                self.thumb_style
            } else {
                self.track_style
            };

            let (x, y) = if is_vertical {
                let x = match self.orientation {
                    ScrollbarOrientation::VerticalRight => area.right().saturating_sub(1),
                    ScrollbarOrientation::VerticalLeft => area.left(),
                    _ => unreachable!(),
                };
                (x, area.top() + i as u16)
            } else {
                let y = match self.orientation {
                    ScrollbarOrientation::HorizontalBottom => area.bottom().saturating_sub(1),
                    ScrollbarOrientation::HorizontalTop => area.top(),
                    _ => unreachable!(),
                };
                (area.left() + i as u16, y)
            };

            // Only draw if within bounds (redundant check but safe)
            if x < area.right() && y < area.bottom() {
                // We use draw_text_span-like logic but for single cell
                use ftui_render::cell::Cell;
                if let Some(c) = symbol.chars().next() {
                    let mut cell = Cell::from_char(c);
                    crate::apply_style(&mut cell, style);
                    frame.buffer.set(x, y, cell);
                }
            }
        }
    }
}

impl<'a> Widget for Scrollbar<'a> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = ScrollbarState::default();
        StatefulWidget::render(self, area, frame, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn scrollbar_empty_area() {
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let mut state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);
    }

    #[test]
    fn scrollbar_zero_content() {
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        let mut state = ScrollbarState::new(0, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);
        // Should not render anything when content_length is 0
    }

    #[test]
    fn scrollbar_vertical_right_renders() {
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        let mut state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        // Thumb should be at the top (position=0), track should have chars
        let top_cell = frame.buffer.get(0, 0).unwrap();
        assert!(top_cell.content.as_char().is_some());
    }

    #[test]
    fn scrollbar_vertical_left_renders() {
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalLeft);
        let area = Rect::new(0, 0, 1, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        let mut state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        let top_cell = frame.buffer.get(0, 0).unwrap();
        assert!(top_cell.content.as_char().is_some());
    }

    #[test]
    fn scrollbar_horizontal_renders() {
        let sb = Scrollbar::new(ScrollbarOrientation::HorizontalBottom);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let mut state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        let left_cell = frame.buffer.get(0, 0).unwrap();
        assert!(left_cell.content.as_char().is_some());
    }

    #[test]
    fn scrollbar_thumb_moves_with_position() {
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 10);

        // Position at start
        let mut pool1 = GraphemePool::new();
        let mut frame1 = Frame::new(1, 10, &mut pool1);
        let mut state1 = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame1, &mut state1);

        // Position at end
        let mut pool2 = GraphemePool::new();
        let mut frame2 = Frame::new(1, 10, &mut pool2);
        let mut state2 = ScrollbarState::new(100, 90, 10);
        StatefulWidget::render(&sb, area, &mut frame2, &mut state2);

        // The thumb char (█) should be at different positions
        let thumb_char = '█';
        let thumb_pos_1 = (0..10u16)
            .find(|&y| frame1.buffer.get(0, y).unwrap().content.as_char() == Some(thumb_char));
        let thumb_pos_2 = (0..10u16)
            .find(|&y| frame2.buffer.get(0, y).unwrap().content.as_char() == Some(thumb_char));

        // At start, thumb should be near top; at end, near bottom
        assert!(thumb_pos_1.unwrap_or(0) < thumb_pos_2.unwrap_or(0));
    }

    #[test]
    fn scrollbar_state_constructor() {
        let state = ScrollbarState::new(200, 50, 20);
        assert_eq!(state.content_length, 200);
        assert_eq!(state.position, 50);
        assert_eq!(state.viewport_length, 20);
    }

    #[test]
    fn scrollbar_content_fits_viewport() {
        // When viewport >= content, thumb should fill the whole track
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        let mut state = ScrollbarState::new(5, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        // All cells should be thumb (█)
        let thumb_char = '█';
        for y in 0..10u16 {
            assert_eq!(
                frame.buffer.get(0, y).unwrap().content.as_char(),
                Some(thumb_char)
            );
        }
    }

    #[test]
    fn scrollbar_horizontal_top_renders() {
        let sb = Scrollbar::new(ScrollbarOrientation::HorizontalTop);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let mut state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        let left_cell = frame.buffer.get(0, 0).unwrap();
        assert!(left_cell.content.as_char().is_some());
    }

    #[test]
    fn scrollbar_custom_symbols() {
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight).symbols(
            ".",
            "#",
            Some("^"),
            Some("v"),
        );
        let area = Rect::new(0, 0, 1, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 5, &mut pool);
        let mut state = ScrollbarState::new(50, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        // Should use our custom symbols
        let mut chars: Vec<Option<char>> = Vec::new();
        for y in 0..5u16 {
            chars.push(frame.buffer.get(0, y).unwrap().content.as_char());
        }
        // At least some cells should have our custom chars
        assert!(chars.contains(&Some('#')) || chars.contains(&Some('.')));
    }

    #[test]
    fn scrollbar_position_clamped_beyond_max() {
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        // Position way beyond content_length
        let mut state = ScrollbarState::new(100, 500, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        // Should still render without panic, thumb at bottom
        let thumb_char = '█';
        let thumb_pos = (0..10u16)
            .find(|&y| frame.buffer.get(0, y).unwrap().content.as_char() == Some(thumb_char));
        assert!(thumb_pos.is_some());
    }

    #[test]
    fn scrollbar_state_default() {
        let state = ScrollbarState::default();
        assert_eq!(state.content_length, 0);
        assert_eq!(state.position, 0);
        assert_eq!(state.viewport_length, 0);
    }

    #[test]
    fn scrollbar_widget_trait_renders() {
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 5, &mut pool);
        // Widget trait uses default state (content_length=0, so no rendering)
        Widget::render(&sb, area, &mut frame);
        // Should not panic with default state
    }

    #[test]
    fn scrollbar_orientation_default_is_vertical_right() {
        assert_eq!(
            ScrollbarOrientation::default(),
            ScrollbarOrientation::VerticalRight
        );
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_essential_only_skips_entirely() {
        use ftui_render::budget::DegradationLevel;

        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        frame.buffer.degradation = DegradationLevel::EssentialOnly;
        let mut state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        // Scrollbar is decorative, should be skipped at EssentialOnly
        for y in 0..10u16 {
            assert!(
                frame.buffer.get(0, y).unwrap().is_empty(),
                "cell at y={y} should be empty at EssentialOnly"
            );
        }
    }

    #[test]
    fn degradation_skeleton_skips_entirely() {
        use ftui_render::budget::DegradationLevel;

        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        let mut state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        for y in 0..10u16 {
            assert!(
                frame.buffer.get(0, y).unwrap().is_empty(),
                "cell at y={y} should be empty at Skeleton"
            );
        }
    }

    #[test]
    fn degradation_full_renders_scrollbar() {
        use ftui_render::budget::DegradationLevel;

        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        frame.buffer.degradation = DegradationLevel::Full;
        let mut state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        // Should render something (thumb or track)
        let top_cell = frame.buffer.get(0, 0).unwrap();
        assert!(top_cell.content.as_char().is_some());
    }

    #[test]
    fn degradation_simple_borders_still_renders() {
        use ftui_render::budget::DegradationLevel;

        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let area = Rect::new(0, 0, 1, 10);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        frame.buffer.degradation = DegradationLevel::SimpleBorders;
        let mut state = ScrollbarState::new(100, 0, 10);
        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        // SimpleBorders still renders decorative content
        let top_cell = frame.buffer.get(0, 0).unwrap();
        assert!(top_cell.content.as_char().is_some());
    }
}
