#![forbid(unsafe_code)]

//! Scrollbar widget.
//!
//! A widget to display a scrollbar.

use crate::mouse::MouseResult;
use crate::{StatefulWidget, Widget, draw_text_span};
use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::Style;
use ftui_text::display_width;

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

/// Hit data part for track (background).
pub const SCROLLBAR_PART_TRACK: u64 = 0;
/// Hit data part for thumb (draggable).
pub const SCROLLBAR_PART_THUMB: u64 = 1;
/// Hit data part for begin button (up/left).
pub const SCROLLBAR_PART_BEGIN: u64 = 2;
/// Hit data part for end button (down/right).
pub const SCROLLBAR_PART_END: u64 = 3;

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
    hit_id: Option<HitId>,
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
            hit_id: None,
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

    /// Set a hit ID for mouse interaction.
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.hit_id = Some(id);
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

    /// Handle a mouse event for this scrollbar.
    ///
    /// # Hit data convention
    ///
    /// The hit data (`u64`) is encoded as `(part << 56) | track_position` where
    /// `part` is one of `SCROLLBAR_PART_*` and `track_position` is the index
    /// within the rendered track.
    ///
    /// # Arguments
    ///
    /// * `event` — the mouse event from the terminal
    /// * `hit` — result of `frame.hit_test(event.x, event.y)`, if available
    /// * `expected_id` — the `HitId` this scrollbar was rendered with
    pub fn handle_mouse(
        &mut self,
        event: &MouseEvent,
        hit: Option<(HitId, HitRegion, u64)>,
        expected_id: HitId,
    ) -> MouseResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((id, HitRegion::Scrollbar, data)) = hit && id == expected_id {
                    let part = data >> 56;
                    match part {
                        SCROLLBAR_PART_BEGIN => {
                            self.scroll_up(1);
                            return MouseResult::Scrolled;
                        }
                        SCROLLBAR_PART_END => {
                            self.scroll_down(1);
                            return MouseResult::Scrolled;
                        }
                        SCROLLBAR_PART_TRACK | SCROLLBAR_PART_THUMB => {
                            // Proportional jump: position in track → position in content
                            let track_pos = (data & 0x00FF_FFFF_FFFF_FFFF) as usize;
                            // The track length is at most content_length worth of cells,
                            // but we don't know the exact rendered track length here.
                            // We use the track position as a direct scroll target,
                            // clamped to the valid range.
                            let max_pos = self.content_length.saturating_sub(self.viewport_length);
                            self.position = track_pos.min(max_pos);
                            return MouseResult::Scrolled;
                        }
                        _ => {}
                    }
                }
                MouseResult::Ignored
            }
            MouseEventKind::ScrollUp => {
                self.scroll_up(3);
                MouseResult::Scrolled
            }
            MouseEventKind::ScrollDown => {
                self.scroll_down(3);
                MouseResult::Scrolled
            }
            _ => MouseResult::Ignored,
        }
    }

    /// Scroll the content up by the given number of lines.
    pub fn scroll_up(&mut self, lines: usize) {
        self.position = self.position.saturating_sub(lines);
    }

    /// Scroll the content down by the given number of lines.
    ///
    /// Clamps so that the viewport stays within content bounds.
    pub fn scroll_down(&mut self, lines: usize) {
        let max_pos = self.content_length.saturating_sub(self.viewport_length);
        self.position = (self.position + lines).min(max_pos);
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
        let mut next_draw_index = 0;
        for i in 0..track_len {
            if i < next_draw_index {
                continue;
            }

            let is_thumb = i >= thumb_offset && i < thumb_offset + thumb_size;
            let (symbol, part) = if is_thumb {
                (thumb_char, SCROLLBAR_PART_THUMB)
            } else if i == 0 && self.begin_symbol.is_some() {
                (begin_char, SCROLLBAR_PART_BEGIN)
            } else if i == track_len - 1 && self.end_symbol.is_some() {
                (end_char, SCROLLBAR_PART_END)
            } else {
                (track_char, SCROLLBAR_PART_TRACK)
            };

            let symbol_width = display_width(symbol);
            if is_vertical {
                next_draw_index = i + 1;
            } else {
                next_draw_index = i + symbol_width;
            }

            let style = if !frame.buffer.degradation.apply_styling() {
                Style::default()
            } else if is_thumb {
                self.thumb_style
            } else {
                self.track_style
            };

            let (x, y) = if is_vertical {
                let x = match self.orientation {
                    // For VerticalRight, position so the symbol (including wide chars) fits in the area
                    ScrollbarOrientation::VerticalRight => {
                        area.right().saturating_sub(symbol_width.max(1) as u16)
                    }
                    ScrollbarOrientation::VerticalLeft => area.left(),
                    _ => unreachable!(),
                };
                (x, area.top().saturating_add(i as u16))
            } else {
                let y = match self.orientation {
                    ScrollbarOrientation::HorizontalBottom => area.bottom().saturating_sub(1),
                    ScrollbarOrientation::HorizontalTop => area.top(),
                    _ => unreachable!(),
                };
                (area.left().saturating_add(i as u16), y)
            };

            // Only draw if within bounds (redundant check but safe)
            if x < area.right() && y < area.bottom() {
                // Use draw_text_span to handle graphemes correctly.
                // Pass max_x that accommodates the symbol width for wide characters.
                draw_text_span(
                    frame,
                    x,
                    y,
                    symbol,
                    style,
                    x.saturating_add(symbol_width as u16),
                );

                if let Some(id) = self.hit_id {
                    let data = (part << 56) | (i as u64);
                    let hit_w = symbol_width.max(1) as u16;
                    frame.register_hit(Rect::new(x, y, hit_w, 1), id, HitRegion::Scrollbar, data);
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

    #[test]
    fn scrollbar_wide_symbols_horizontal() {
        let sb =
            Scrollbar::new(ScrollbarOrientation::HorizontalBottom).symbols("🔴", "👍", None, None);
        // Area width 4. Expect "🔴🔴" (2 chars * 2 width = 4 cells)
        let area = Rect::new(0, 0, 4, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 1, &mut pool);
        // Track only (thumb size 0 or pos 0?)
        // Let's make thumb small/invisible or check track part.
        // If content_length=10, viewport=10, thumb fills all.
        // Let's fill with thumb "👍"
        let mut state = ScrollbarState::new(10, 0, 10);

        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        // x=0: Head "👍" (wide emoji stored as grapheme, not direct char)
        let c0 = frame.buffer.get(0, 0).unwrap();
        assert!(!c0.is_empty() && !c0.is_continuation()); // Head
        // x=1: Continuation
        let c1 = frame.buffer.get(1, 0).unwrap();
        assert!(c1.is_continuation());

        // x=2: Head "👍"
        let c2 = frame.buffer.get(2, 0).unwrap();
        assert!(!c2.is_empty() && !c2.is_continuation()); // Head
        // x=3: Continuation
        let c3 = frame.buffer.get(3, 0).unwrap();
        assert!(c3.is_continuation());
    }

    #[test]
    fn scrollbar_wide_symbols_vertical() {
        let sb =
            Scrollbar::new(ScrollbarOrientation::VerticalRight).symbols("🔴", "👍", None, None);
        // Area height 2. Width 2 (to fit the wide char).
        let area = Rect::new(0, 0, 2, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 2, &mut pool);
        let mut state = ScrollbarState::new(10, 0, 10); // Fill with thumb

        StatefulWidget::render(&sb, area, &mut frame, &mut state);

        // Row 0: "👍" at x=0 (wide emoji stored as grapheme, not direct char)
        let r0_c0 = frame.buffer.get(0, 0).unwrap();
        assert!(!r0_c0.is_empty() && !r0_c0.is_continuation()); // Head
        let r0_c1 = frame.buffer.get(1, 0).unwrap();
        assert!(r0_c1.is_continuation()); // Tail

        // Row 1: "👍" at x=0 (should NOT be skipped)
        let r1_c0 = frame.buffer.get(0, 1).unwrap();
        assert!(!r1_c0.is_empty() && !r1_c0.is_continuation()); // Head
        let r1_c1 = frame.buffer.get(1, 1).unwrap();
        assert!(r1_c1.is_continuation()); // Tail
    }

    // --- Mouse handling tests ---

    use crate::mouse::MouseResult;
    use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};

    #[test]
    fn scrollbar_state_begin_button() {
        let mut state = ScrollbarState::new(100, 10, 20);
        let data = SCROLLBAR_PART_BEGIN << 56;
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let hit = Some((HitId::new(1), HitRegion::Scrollbar, data));
        let result = state.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.position, 9);
    }

    #[test]
    fn scrollbar_state_end_button() {
        let mut state = ScrollbarState::new(100, 10, 20);
        let data = SCROLLBAR_PART_END << 56;
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let hit = Some((HitId::new(1), HitRegion::Scrollbar, data));
        let result = state.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.position, 11);
    }

    #[test]
    fn scrollbar_state_track_click() {
        let mut state = ScrollbarState::new(100, 0, 20);
        let track_pos = 50u64;
        let data = (SCROLLBAR_PART_TRACK << 56) | track_pos;
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let hit = Some((HitId::new(1), HitRegion::Scrollbar, data));
        let result = state.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.position, 50);
    }

    #[test]
    fn scrollbar_state_track_click_clamps() {
        let mut state = ScrollbarState::new(100, 0, 20);
        let track_pos = 95u64;
        let data = (SCROLLBAR_PART_TRACK << 56) | track_pos;
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let hit = Some((HitId::new(1), HitRegion::Scrollbar, data));
        let result = state.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.position, 80); // content_length - viewport_length
    }

    #[test]
    fn scrollbar_state_scroll_wheel_up() {
        let mut state = ScrollbarState::new(100, 10, 20);
        let event = MouseEvent::new(MouseEventKind::ScrollUp, 0, 0);
        let result = state.handle_mouse(&event, None, HitId::new(1));
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.position, 7);
    }

    #[test]
    fn scrollbar_state_scroll_wheel_down() {
        let mut state = ScrollbarState::new(100, 10, 20);
        let event = MouseEvent::new(MouseEventKind::ScrollDown, 0, 0);
        let result = state.handle_mouse(&event, None, HitId::new(1));
        assert_eq!(result, MouseResult::Scrolled);
        assert_eq!(state.position, 13);
    }

    #[test]
    fn scrollbar_state_scroll_down_clamps() {
        let mut state = ScrollbarState::new(100, 78, 20);
        state.scroll_down(5);
        assert_eq!(state.position, 80); // max = 100 - 20
    }

    #[test]
    fn scrollbar_state_scroll_up_clamps() {
        let mut state = ScrollbarState::new(100, 2, 20);
        state.scroll_up(5);
        assert_eq!(state.position, 0);
    }

    #[test]
    fn scrollbar_state_wrong_id_ignored() {
        let mut state = ScrollbarState::new(100, 10, 20);
        let data = SCROLLBAR_PART_BEGIN << 56;
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let hit = Some((HitId::new(99), HitRegion::Scrollbar, data));
        let result = state.handle_mouse(&event, hit, HitId::new(1));
        assert_eq!(result, MouseResult::Ignored);
        assert_eq!(state.position, 10);
    }

    #[test]
    fn scrollbar_state_right_click_ignored() {
        let mut state = ScrollbarState::new(100, 10, 20);
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Right), 0, 0);
        let result = state.handle_mouse(&event, None, HitId::new(1));
        assert_eq!(result, MouseResult::Ignored);
    }
}
