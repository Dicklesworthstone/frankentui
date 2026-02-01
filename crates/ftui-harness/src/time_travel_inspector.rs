#![forbid(unsafe_code)]

//! Simple debug UI for inspecting TimeTravel recordings.
//!
//! The inspector renders a one-line header with frame metadata and
//! a preview of the selected frame underneath.

use crate::time_travel::TimeTravel;
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::drawing::Draw;

/// Time-travel inspector for stepping through recorded frames.
#[derive(Debug, Clone, Default)]
pub struct TimeTravelInspector {
    index: usize,
}

impl TimeTravelInspector {
    /// Create a new inspector at the oldest frame (index 0).
    pub fn new() -> Self {
        Self { index: 0 }
    }

    /// Current frame index (0 = oldest retained).
    pub fn index(&self) -> usize {
        self.index
    }

    /// Seek to a specific frame index (clamped to the available range).
    pub fn seek(&mut self, index: usize, time_travel: &TimeTravel) {
        if time_travel.is_empty() {
            self.index = 0;
            return;
        }
        self.index = index.min(time_travel.len().saturating_sub(1));
    }

    /// Step backward by one frame (toward older frames).
    pub fn step_back(&mut self) {
        if self.index > 0 {
            self.index -= 1;
        }
    }

    /// Step forward by one frame (toward newer frames).
    pub fn step_forward(&mut self, time_travel: &TimeTravel) {
        if self.index + 1 < time_travel.len() {
            self.index += 1;
        }
    }

    /// Render the inspector view: header + frame preview.
    pub fn render(&self, time_travel: &TimeTravel) -> Option<Buffer> {
        let frame = time_travel.get(self.index)?;
        let width = frame.width();
        let height = frame.height();
        let out_height = height.saturating_add(1);
        if out_height == 0 || width == 0 {
            return None;
        }

        let mut out = Buffer::new(width, out_height);
        let header = self.header_text(time_travel);
        let header_cell = Cell::from_char(' ');
        out.print_text_clipped(0, 0, &header, header_cell, width);

        let src_rect = Rect::from_size(width, height);
        out.copy_from(&frame, src_rect, 0, 1);
        Some(out)
    }

    fn header_text(&self, time_travel: &TimeTravel) -> String {
        let count = time_travel.len();
        let index_display = if count == 0 { 0 } else { self.index + 1 };
        let meta = time_travel.metadata(self.index);
        let render_us = meta.map(|m| m.render_time.as_micros()).unwrap_or(0);
        let events = meta.map(|m| m.event_count).unwrap_or(0);
        let hash = meta.and_then(|m| m.model_hash);
        if let Some(hash) = hash {
            format!(
                "Frame {}/{} | {}us | events={} | hash={}",
                index_display, count, render_us, events, hash
            )
        } else {
            format!(
                "Frame {}/{} | {}us | events={}",
                index_display, count, render_us, events
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time_travel::FrameMetadata;
    use std::time::Duration;

    #[test]
    fn inspector_renders_header_and_frame() {
        let mut tt = TimeTravel::new(4);
        let mut buf = Buffer::new(2, 1);
        buf.set(0, 0, Cell::from_char('A'));
        tt.record(
            &buf,
            FrameMetadata::new(0, Duration::from_millis(1))
                .with_events(2)
                .with_model_hash(9),
        );

        let inspector = TimeTravelInspector::new();
        let out = inspector.render(&tt).expect("rendered buffer");
        assert_eq!(out.height(), 2);
        assert_eq!(out.get(0, 1).unwrap().content.as_char(), Some('A'));
        assert_eq!(out.get(0, 0).unwrap().content.as_char(), Some('F'));
    }

    #[test]
    fn inspector_seek_and_step_clamp() {
        let mut tt = TimeTravel::new(2);
        let mut buf = Buffer::new(1, 1);
        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, FrameMetadata::new(0, Duration::from_millis(1)));
        buf.set(0, 0, Cell::from_char('B'));
        tt.record(&buf, FrameMetadata::new(1, Duration::from_millis(1)));

        let mut inspector = TimeTravelInspector::new();
        inspector.seek(99, &tt);
        assert_eq!(inspector.index(), 1);

        inspector.step_forward(&tt);
        assert_eq!(inspector.index(), 1);

        inspector.step_back();
        assert_eq!(inspector.index(), 0);
        inspector.step_back();
        assert_eq!(inspector.index(), 0);
    }
}
