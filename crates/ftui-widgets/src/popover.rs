// SPDX-License-Identifier: Apache-2.0
//! Popover widget for anchored floating content.
//!
//! [`Popover`] renders a lightweight floating panel positioned relative to an
//! anchor rectangle. It automatically flips placement when there isn't enough
//! space, making it suitable for tooltips, dropdowns, and context menus.
//!
//! # Migration rationale
//!
//! Web frameworks use portals, popovers, and floating-ui for content that
//! renders outside the normal document flow. This widget provides an explicit,
//! terminal-native equivalent that the migration code emitter can target.
//!
//! # Differences from Modal
//!
//! - **No backdrop**: Popover renders only the content, not a full-screen overlay
//! - **Anchor-relative positioning**: Content floats near a reference element
//! - **Auto-flip**: Placement adjusts to stay within viewport bounds
//! - **Lightweight**: No focus trapping or animation built in (compose with FocusTrap if needed)
//!
//! # Example
//!
//! ```ignore
//! use ftui_widgets::popover::{Popover, Placement};
//! use ftui_layout::Rect;
//!
//! let anchor = Rect::new(10, 5, 20, 1); // The button/element to anchor to
//! let popover = Popover::new(anchor, Placement::Below)
//!     .width(30)
//!     .max_height(10)
//!     .with_border(true);
//!
//! // Render content inside the popover
//! popover.render_with(area, frame, |content_area, frame| {
//!     // Draw your dropdown/tooltip content here
//! });
//! ```

#![forbid(unsafe_code)]

use ftui_layout::Rect;
use ftui_render::frame::Frame;

/// Where to place the popover relative to the anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Above the anchor, horizontally aligned to its left edge.
    Above,
    /// Below the anchor, horizontally aligned to its left edge.
    Below,
    /// To the left of the anchor, vertically aligned to its top edge.
    Left,
    /// To the right of the anchor, vertically aligned to its top edge.
    Right,
    /// Above the anchor, horizontally centered.
    AboveCentered,
    /// Below the anchor, horizontally centered.
    BelowCentered,
}

impl Placement {
    /// Return the opposite placement for flip logic.
    fn flip(self) -> Self {
        match self {
            Self::Above | Self::AboveCentered => Self::Below,
            Self::Below | Self::BelowCentered => Self::Above,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    /// Whether this is a vertical (above/below) placement.
    fn is_vertical(self) -> bool {
        matches!(
            self,
            Self::Above | Self::Below | Self::AboveCentered | Self::BelowCentered
        )
    }
}

/// Configuration for a popover widget.
#[derive(Debug, Clone)]
pub struct Popover {
    /// The anchor rectangle to position relative to.
    pub anchor: Rect,
    /// Preferred placement direction.
    pub placement: Placement,
    /// Desired width of the popover content area. If `None`, uses anchor width.
    pub width: Option<u16>,
    /// Maximum height of the popover. If `None`, fills available space.
    pub max_height: Option<u16>,
    /// Whether to draw a border around the popover.
    pub bordered: bool,
    /// Gap between anchor and popover (in cells).
    pub gap: u16,
    /// Whether to auto-flip when there isn't enough space.
    pub auto_flip: bool,
}

impl Popover {
    /// Create a popover anchored to the given rectangle.
    pub fn new(anchor: Rect, placement: Placement) -> Self {
        Self {
            anchor,
            placement,
            width: None,
            max_height: None,
            bordered: false,
            gap: 0,
            auto_flip: true,
        }
    }

    /// Set the desired width.
    #[must_use]
    pub fn width(mut self, w: u16) -> Self {
        self.width = Some(w);
        self
    }

    /// Set the maximum height.
    #[must_use]
    pub fn max_height(mut self, h: u16) -> Self {
        self.max_height = Some(h);
        self
    }

    /// Enable or disable the border.
    #[must_use]
    pub fn with_border(mut self, bordered: bool) -> Self {
        self.bordered = bordered;
        self
    }

    /// Set the gap between anchor and popover.
    #[must_use]
    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    /// Enable or disable auto-flip.
    #[must_use]
    pub fn auto_flip(mut self, flip: bool) -> Self {
        self.auto_flip = flip;
        self
    }

    /// Compute the content area for this popover within the given viewport.
    ///
    /// Returns the [`Rect`] where content should be rendered, accounting for
    /// placement, flip logic, and viewport bounds. Returns `None` if the
    /// popover cannot fit at all.
    pub fn compute_area(&self, viewport: Rect) -> Option<Rect> {
        let content_width = self.width.unwrap_or(self.anchor.width);
        if content_width == 0 {
            return None;
        }

        let placement = if self.auto_flip {
            self.resolve_placement(viewport, content_width)
        } else {
            self.placement
        };

        let (x, y, w, h) = self.layout(placement, viewport, content_width);
        if w == 0 || h == 0 {
            return None;
        }
        Some(Rect::new(x, y, w, h))
    }

    /// Render the popover border (if enabled) and invoke the callback for content.
    ///
    /// The callback receives the inner content area (inside the border if any).
    pub fn render_with<F>(&self, viewport: Rect, frame: &mut Frame, render_content: F)
    where
        F: FnOnce(Rect, &mut Frame),
    {
        let Some(area) = self.compute_area(viewport) else {
            return;
        };

        let content_area = if self.bordered {
            // Draw a simple box border
            let buf = &mut frame.buffer;
            draw_border(buf, area);

            // Content area is inset by 1 on each side
            let inner = if area.width >= 2 && area.height >= 2 {
                Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2)
            } else {
                area
            };
            if !inner.is_empty() {
                buf.fill(inner, ftui_render::cell::Cell::from_char(' '));
            }
            inner
        } else {
            area
        };

        if frame.a11y_enabled() {
            let bounds = area.intersection(&frame.buffer.current_scissor());
            if !bounds.is_empty() {
                let container = ftui_a11y::node::A11yNodeInfo::new(
                    ftui_a11y::node::A11yNodeInfo::stable_id_for(
                        ftui_a11y::node::A11yRole::Dialog,
                        area,
                    ),
                    ftui_a11y::node::A11yRole::Dialog,
                    bounds,
                )
                .with_name("Popover");
                frame.with_a11y_scope(container, |frame| render_content(content_area, frame));
                return;
            }
        }

        render_content(content_area, frame);
    }

    /// Resolve placement with flip logic.
    fn resolve_placement(&self, viewport: Rect, content_width: u16) -> Placement {
        let primary = self.placement;
        let available = self.available_space(primary, viewport);
        let needed = self.needed_space(primary, content_width);

        if available >= needed {
            return primary;
        }

        // Try the opposite direction
        let flipped = primary.flip();
        let flipped_available = self.available_space(flipped, viewport);
        if flipped_available >= needed {
            return flipped;
        }

        // Fall back to whichever has more space
        if flipped_available > available {
            flipped
        } else {
            primary
        }
    }

    /// How much space is available in the given direction.
    fn available_space(&self, placement: Placement, viewport: Rect) -> u16 {
        match placement {
            Placement::Above | Placement::AboveCentered => self.anchor.y.saturating_sub(viewport.y),
            Placement::Below | Placement::BelowCentered => {
                let bottom = viewport.y.saturating_add(viewport.height);
                let anchor_bottom = self.anchor.y.saturating_add(self.anchor.height);
                bottom.saturating_sub(anchor_bottom)
            }
            Placement::Left => self.anchor.x.saturating_sub(viewport.x),
            Placement::Right => {
                let right = viewport.x.saturating_add(viewport.width);
                let anchor_right = self.anchor.x.saturating_add(self.anchor.width);
                right.saturating_sub(anchor_right)
            }
        }
    }

    /// Minimum space needed for the popover in the given direction.
    fn needed_space(&self, placement: Placement, content_width: u16) -> u16 {
        let border_overhead = if self.bordered { 2 } else { 0 };
        if placement.is_vertical() {
            // Need max_height (or at least 1 line) + border + gap
            let height = self.max_height.unwrap_or(1);
            height
                .saturating_add(border_overhead)
                .saturating_add(self.gap)
        } else {
            // Need at least content_width + border + gap
            content_width
                .saturating_add(border_overhead)
                .saturating_add(self.gap)
        }
    }

    /// Compute the actual layout rect for the given placement.
    fn layout(
        &self,
        placement: Placement,
        viewport: Rect,
        content_width: u16,
    ) -> (u16, u16, u16, u16) {
        let border_overhead = if self.bordered { 2 } else { 0 };
        let total_width = content_width.saturating_add(border_overhead);

        // Compute x position
        let x = match placement {
            Placement::Above | Placement::Below => clamp_x(self.anchor.x, total_width, viewport),
            Placement::AboveCentered | Placement::BelowCentered => {
                let center = self.anchor.x.saturating_add(self.anchor.width / 2);
                let start = center.saturating_sub(total_width / 2);
                clamp_x(start, total_width, viewport)
            }
            Placement::Left => {
                let end = self.anchor.x.saturating_sub(self.gap);
                end.saturating_sub(total_width)
            }
            Placement::Right => self
                .anchor
                .x
                .saturating_add(self.anchor.width)
                .saturating_add(self.gap),
        };

        // Compute y position and available height
        let (y, available_height) = match placement {
            Placement::Above | Placement::AboveCentered => {
                let space_above = self
                    .anchor
                    .y
                    .saturating_sub(viewport.y)
                    .saturating_sub(self.gap);
                let max_h = self.max_height.unwrap_or(space_above).min(space_above);
                let total_h = max_h.saturating_add(border_overhead);
                let y_pos = self
                    .anchor
                    .y
                    .saturating_sub(self.gap)
                    .saturating_sub(total_h);
                (y_pos.max(viewport.y), total_h)
            }
            Placement::Below | Placement::BelowCentered => {
                let y_start = self
                    .anchor
                    .y
                    .saturating_add(self.anchor.height)
                    .saturating_add(self.gap);
                let bottom = viewport.y.saturating_add(viewport.height);
                let space_below = bottom.saturating_sub(y_start);
                let max_h = self.max_height.unwrap_or(space_below).min(space_below);
                let total_h = max_h.saturating_add(border_overhead).min(space_below);
                (y_start, total_h)
            }
            Placement::Left | Placement::Right => {
                let y_start = self.anchor.y;
                let bottom = viewport.y.saturating_add(viewport.height);
                let space_below = bottom.saturating_sub(y_start);
                let max_h = self
                    .max_height
                    .map(|h| h.saturating_add(border_overhead))
                    .unwrap_or(space_below)
                    .min(space_below);
                (y_start, max_h)
            }
        };

        // Clamp width to viewport
        let vp_right = viewport.x.saturating_add(viewport.width);
        let clamped_width = total_width.min(vp_right.saturating_sub(x));

        (x, y, clamped_width, available_height)
    }
}

/// Clamp x position so the popover doesn't overflow the viewport.
fn clamp_x(x: u16, width: u16, viewport: Rect) -> u16 {
    let vp_right = viewport.x.saturating_add(viewport.width);
    if x.saturating_add(width) > vp_right {
        vp_right.saturating_sub(width)
    } else {
        x.max(viewport.x)
    }
}

/// Draw a simple single-line border around a rect.
fn draw_border(buf: &mut ftui_render::buffer::Buffer, area: Rect) {
    use ftui_render::cell::Cell;

    if area.width < 2 || area.height < 2 {
        return;
    }
    let x = area.x;
    let y = area.y;
    let w = area.width;
    let h = area.height;

    // Corners
    buf.set_fast(x, y, Cell::from_char('┌'));
    buf.set_fast(x + w - 1, y, Cell::from_char('┐'));
    buf.set_fast(x, y + h - 1, Cell::from_char('└'));
    buf.set_fast(x + w - 1, y + h - 1, Cell::from_char('┘'));

    // Top and bottom edges
    for col in (x + 1)..(x + w - 1) {
        buf.set_fast(col, y, Cell::from_char('─'));
        buf.set_fast(col, y + h - 1, Cell::from_char('─'));
    }

    // Left and right edges
    for row in (y + 1)..(y + h - 1) {
        buf.set_fast(x, row, Cell::from_char('│'));
        buf.set_fast(x + w - 1, row, Cell::from_char('│'));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    fn viewport() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    #[test]
    fn below_basic_placement() {
        let anchor = Rect::new(10, 5, 20, 1);
        let popover = Popover::new(anchor, Placement::Below)
            .width(20)
            .max_height(5);
        let area = popover.compute_area(viewport()).unwrap();
        assert_eq!(area.x, 10);
        assert_eq!(area.y, 6); // anchor.y + anchor.height
        assert_eq!(area.width, 20);
        assert_eq!(area.height, 5);
    }

    #[test]
    fn above_basic_placement() {
        let anchor = Rect::new(10, 10, 20, 1);
        let popover = Popover::new(anchor, Placement::Above)
            .width(20)
            .max_height(5);
        let area = popover.compute_area(viewport()).unwrap();
        assert_eq!(area.x, 10);
        assert_eq!(area.width, 20);
        assert!(area.y + area.height <= anchor.y);
    }

    #[test]
    fn right_basic_placement() {
        let anchor = Rect::new(10, 5, 10, 1);
        let popover = Popover::new(anchor, Placement::Right)
            .width(15)
            .max_height(3);
        let area = popover.compute_area(viewport()).unwrap();
        assert_eq!(area.x, 20); // anchor.x + anchor.width
        assert_eq!(area.y, 5);
        assert_eq!(area.width, 15);
    }

    #[test]
    fn left_basic_placement() {
        let anchor = Rect::new(30, 5, 10, 1);
        let popover = Popover::new(anchor, Placement::Left)
            .width(15)
            .max_height(3);
        let area = popover.compute_area(viewport()).unwrap();
        assert!(area.x + area.width <= 30);
    }

    #[test]
    fn auto_flip_below_to_above() {
        // Anchor near the bottom, should flip to above
        let anchor = Rect::new(10, 22, 20, 1);
        let popover = Popover::new(anchor, Placement::Below)
            .width(20)
            .max_height(5);
        let area = popover.compute_area(viewport()).unwrap();
        // Should be above the anchor since there's no room below
        assert!(area.y + area.height <= 22);
    }

    #[test]
    fn auto_flip_above_to_below() {
        // Anchor near the top, should flip to below
        let anchor = Rect::new(10, 1, 20, 1);
        let popover = Popover::new(anchor, Placement::Above)
            .width(20)
            .max_height(5);
        let area = popover.compute_area(viewport()).unwrap();
        // Should be below the anchor since there's no room above
        assert!(area.y >= anchor.y + anchor.height);
    }

    #[test]
    fn auto_flip_disabled() {
        let anchor = Rect::new(10, 22, 20, 1);
        let popover = Popover::new(anchor, Placement::Below)
            .width(20)
            .max_height(5)
            .auto_flip(false);
        let area = popover.compute_area(viewport()).unwrap();
        // Should stay below even with limited space
        assert!(area.y >= anchor.y + anchor.height);
    }

    #[test]
    fn width_clamped_to_viewport() {
        let anchor = Rect::new(70, 5, 5, 1);
        let popover = Popover::new(anchor, Placement::Below).width(20);
        let area = popover.compute_area(viewport()).unwrap();
        assert!(area.x + area.width <= 80);
    }

    #[test]
    fn border_adds_overhead() {
        let anchor = Rect::new(10, 5, 20, 1);
        let popover = Popover::new(anchor, Placement::Below)
            .width(20)
            .max_height(5)
            .with_border(true);
        let area = popover.compute_area(viewport()).unwrap();
        // Total area includes border overhead
        assert_eq!(area.width, 22); // 20 content + 2 border
        assert_eq!(area.height, 7); // 5 content + 2 border
    }

    #[test]
    fn gap_creates_space() {
        let anchor = Rect::new(10, 5, 20, 1);
        let popover = Popover::new(anchor, Placement::Below)
            .width(20)
            .max_height(5)
            .gap(1);
        let area = popover.compute_area(viewport()).unwrap();
        assert_eq!(area.y, 7); // anchor.y + anchor.height + gap
    }

    #[test]
    fn centered_placement() {
        let anchor = Rect::new(30, 5, 20, 1);
        let popover = Popover::new(anchor, Placement::BelowCentered)
            .width(10)
            .max_height(3);
        let area = popover.compute_area(viewport()).unwrap();
        // Center of anchor is at 40, popover width 10, so x should be ~35
        let anchor_center = anchor.x + anchor.width / 2;
        let popover_center = area.x + area.width / 2;
        assert!((anchor_center as i32 - popover_center as i32).unsigned_abs() <= 1);
    }

    #[test]
    fn zero_width_returns_none() {
        let anchor = Rect::new(10, 5, 0, 1);
        let popover = Popover::new(anchor, Placement::Below);
        assert!(popover.compute_area(viewport()).is_none());
    }

    #[test]
    fn placement_flip_roundtrip() {
        assert_eq!(Placement::Above.flip(), Placement::Below);
        assert_eq!(Placement::Below.flip(), Placement::Above);
        assert_eq!(Placement::Left.flip(), Placement::Right);
        assert_eq!(Placement::Right.flip(), Placement::Left);
    }

    #[test]
    fn placement_is_vertical() {
        assert!(Placement::Above.is_vertical());
        assert!(Placement::Below.is_vertical());
        assert!(Placement::AboveCentered.is_vertical());
        assert!(Placement::BelowCentered.is_vertical());
        assert!(!Placement::Left.is_vertical());
        assert!(!Placement::Right.is_vertical());
    }

    #[test]
    fn right_placement_with_gap() {
        let anchor = Rect::new(10, 5, 10, 1);
        let popover = Popover::new(anchor, Placement::Right)
            .width(15)
            .max_height(3)
            .gap(2);
        let area = popover.compute_area(viewport()).unwrap();
        assert_eq!(area.x, 22); // anchor.x + anchor.width + gap
    }

    #[test]
    fn max_height_limits_popover() {
        let anchor = Rect::new(10, 5, 20, 1);
        let popover = Popover::new(anchor, Placement::Below)
            .width(20)
            .max_height(3);
        let area = popover.compute_area(viewport()).unwrap();
        assert!(area.height <= 3);
    }

    #[test]
    fn height_limited_by_viewport() {
        // Anchor near bottom, even max_height 100 should be clamped
        let anchor = Rect::new(10, 20, 20, 1);
        let popover = Popover::new(anchor, Placement::Below)
            .width(20)
            .max_height(100);
        let area = popover.compute_area(viewport()).unwrap();
        assert!(area.y + area.height <= 24); // viewport height
    }

    #[test]
    fn popover_debug_impl() {
        let popover = Popover::new(Rect::new(0, 0, 10, 1), Placement::Below);
        let _ = format!("{popover:?}");
    }

    #[test]
    fn bordered_render_clears_stale_inner_content() {
        let popover = Popover::new(Rect::new(2, 1, 5, 1), Placement::Below)
            .width(5)
            .max_height(1)
            .with_border(true);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 10, &mut pool);
        let viewport = Rect::new(0, 0, 20, 10);

        popover.render_with(viewport, &mut frame, |inner, frame| {
            for (i, ch) in "ABCDE".chars().enumerate() {
                frame.buffer.set(
                    inner.x + i as u16,
                    inner.y,
                    ftui_render::cell::Cell::from_char(ch),
                );
            }
        });
        popover.render_with(viewport, &mut frame, |inner, frame| {
            for (i, ch) in "XY".chars().enumerate() {
                frame.buffer.set(
                    inner.x + i as u16,
                    inner.y,
                    ftui_render::cell::Cell::from_char(ch),
                );
            }
        });

        let area = popover.compute_area(viewport).unwrap();
        let inner_y = area.y + 1;
        assert_eq!(
            frame
                .buffer
                .get(area.x + 1, inner_y)
                .unwrap()
                .content
                .as_char(),
            Some('X')
        );
        assert_eq!(
            frame
                .buffer
                .get(area.x + 2, inner_y)
                .unwrap()
                .content
                .as_char(),
            Some('Y')
        );
        assert_eq!(
            frame
                .buffer
                .get(area.x + 3, inner_y)
                .unwrap()
                .content
                .as_char(),
            Some(' ')
        );
        assert_eq!(
            frame
                .buffer
                .get(area.x + 4, inner_y)
                .unwrap()
                .content
                .as_char(),
            Some(' ')
        );
        assert_eq!(
            frame
                .buffer
                .get(area.x + 5, inner_y)
                .unwrap()
                .content
                .as_char(),
            Some(' ')
        );
    }
}
