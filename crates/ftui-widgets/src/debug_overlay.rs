#![forbid(unsafe_code)]

//! Debug overlay utilities for widget introspection.
//!
//! Provides:
//! - A shared registry for collecting per-widget render metadata.
//! - Wrapper widgets that record boundaries and render times.
//! - A DebugOverlay widget that draws boundaries and labels.

use crate::{StatefulWidget, Widget};
use ftui_core::event::{Event, KeyCode, KeyEventKind, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::{BorderChars, Draw};
use ftui_render::frame::Frame;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Render metadata for a single widget.
#[derive(Debug, Clone)]
pub struct WidgetDebugInfo {
    pub name: String,
    pub area: Rect,
    pub render_time: Option<Duration>,
    pub hit_areas: Vec<Rect>,
}

impl WidgetDebugInfo {
    #[must_use]
    pub fn new(name: impl Into<String>, area: Rect) -> Self {
        Self {
            name: name.into(),
            area,
            render_time: None,
            hit_areas: Vec::new(),
        }
    }
}

/// Shared state for debug overlay data collection.
#[derive(Debug)]
pub struct DebugOverlayState {
    enabled: AtomicBool,
    entries: Mutex<Vec<WidgetDebugInfo>>,
    hover: Mutex<Option<(u16, u16)>>,
}

impl DebugOverlayState {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(env_enabled()),
            entries: Mutex::new(Vec::new()),
            hover: Mutex::new(None),
        })
    }

    #[inline]
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        let prev = self.enabled.swap(enabled, Ordering::Relaxed);
        if prev != enabled {
            #[cfg(feature = "tracing")]
            tracing::info!(enabled = enabled, "Debug overlay toggled");
        }
    }

    pub fn toggle(&self) -> bool {
        let next = !self.enabled.fetch_xor(true, Ordering::Relaxed);
        #[cfg(feature = "tracing")]
        tracing::info!(enabled = next, "Debug overlay toggled");
        next
    }

    /// Toggle overlay when F12 is pressed.
    pub fn toggle_on_f12(&self, event: &Event) -> bool {
        if let Event::Key(key) = event
            && matches!(key.code, KeyCode::F(12))
            && key.kind == KeyEventKind::Press
        {
            self.toggle();
            return true;
        }
        false
    }

    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }

    pub fn record(&self, info: WidgetDebugInfo) {
        if !self.enabled() {
            return;
        }
        if let Ok(mut entries) = self.entries.lock() {
            entries.push(info);
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<WidgetDebugInfo> {
        if let Ok(entries) = self.entries.lock() {
            entries.clone()
        } else {
            Vec::new()
        }
    }

    /// Set the current hover position (mouse coordinates).
    pub fn set_hover(&self, position: Option<(u16, u16)>) {
        if let Ok(mut hover) = self.hover.lock() {
            *hover = position;
        }
    }

    /// Update hover position from a mouse event.
    #[must_use = "use the returned hover position (if any)"]
    pub fn update_hover_from_event(&self, event: &Event) -> Option<(u16, u16)> {
        let Event::Mouse(mouse) = event else {
            return self.hover_position();
        };

        if matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
            self.set_hover(Some((mouse.x, mouse.y)));
        }
        self.hover_position()
    }

    #[must_use = "use the returned hover position (if any)"]
    pub fn hover_position(&self) -> Option<(u16, u16)> {
        self.hover.lock().ok().and_then(|hover| *hover)
    }
}

/// Display options for the debug overlay.
#[derive(Debug, Clone)]
pub struct DebugOverlayOptions {
    pub show_boundaries: bool,
    pub show_names: bool,
    pub show_render_times: bool,
    pub show_hit_areas: bool,
    pub clear_on_render: bool,
    pub palette: DebugOverlayPalette,
}

impl Default for DebugOverlayOptions {
    fn default() -> Self {
        Self {
            show_boundaries: true,
            show_names: true,
            show_render_times: true,
            show_hit_areas: true,
            clear_on_render: true,
            palette: DebugOverlayPalette::default(),
        }
    }
}

/// Color palette for the debug overlay.
#[derive(Debug, Clone)]
pub struct DebugOverlayPalette {
    pub border_colors: [PackedRgba; 6],
    pub label_fg: PackedRgba,
    pub label_bg: PackedRgba,
    pub hit_color: PackedRgba,
    pub hit_hot_color: PackedRgba,
}

impl Default for DebugOverlayPalette {
    fn default() -> Self {
        Self {
            border_colors: [
                PackedRgba::rgb(240, 80, 80),
                PackedRgba::rgb(80, 200, 120),
                PackedRgba::rgb(80, 150, 240),
                PackedRgba::rgb(240, 200, 80),
                PackedRgba::rgb(200, 120, 240),
                PackedRgba::rgb(80, 220, 220),
            ],
            label_fg: PackedRgba::rgb(255, 255, 255),
            label_bg: PackedRgba::rgb(0, 0, 0),
            hit_color: PackedRgba::rgb(255, 140, 0),
            hit_hot_color: PackedRgba::rgb(255, 230, 0),
        }
    }
}

/// Debug overlay widget.
#[derive(Debug, Clone)]
pub struct DebugOverlay {
    state: Arc<DebugOverlayState>,
    options: DebugOverlayOptions,
}

impl DebugOverlay {
    #[must_use]
    pub fn new(state: Arc<DebugOverlayState>) -> Self {
        Self {
            state,
            options: DebugOverlayOptions::default(),
        }
    }

    #[must_use]
    pub fn options(mut self, options: DebugOverlayOptions) -> Self {
        self.options = options;
        self
    }

    fn format_label(&self, info: &WidgetDebugInfo) -> String {
        if !self.options.show_render_times {
            return info.name.clone();
        }
        match info.render_time {
            Some(time) => format!("{} {}us", info.name, time.as_micros()),
            None => info.name.clone(),
        }
    }
}

impl Widget for DebugOverlay {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "DebugOverlay",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if !self.state.enabled() {
            return;
        }

        let entries = self.state.snapshot();
        if self.options.clear_on_render {
            self.state.clear();
        }

        for (idx, info) in entries.iter().enumerate() {
            let Some(rect) = info.area.intersection_opt(&area) else {
                continue;
            };
            if rect.is_empty() {
                continue;
            }

            let color =
                self.options.palette.border_colors[idx % self.options.palette.border_colors.len()];
            if self.options.show_boundaries {
                let border_cell = Cell::from_char('+').with_fg(color);
                frame
                    .buffer
                    .draw_border(rect, BorderChars::ASCII, border_cell);
            }

            if self.options.show_names {
                let label = self.format_label(info);
                if !label.is_empty() {
                    let label_cell = Cell::from_char(' ')
                        .with_fg(self.options.palette.label_fg)
                        .with_bg(self.options.palette.label_bg);
                    let label_x = rect.x.saturating_add(1);
                    let max_x = rect.right();
                    let _ = frame
                        .buffer
                        .print_text_clipped(label_x, rect.y, &label, label_cell, max_x);
                }
            }

            if self.options.show_hit_areas {
                let hover = self.state.hover_position();
                for hit in &info.hit_areas {
                    if let Some(hit_rect) = hit.intersection_opt(&area)
                        && !hit_rect.is_empty()
                    {
                        let is_hot = hover.map(|(x, y)| hit_rect.contains(x, y)).unwrap_or(false);
                        let color = if is_hot {
                            self.options.palette.hit_hot_color
                        } else {
                            self.options.palette.hit_color
                        };
                        let hit_cell = Cell::from_char('.').with_fg(color);
                        frame.buffer.draw_rect_outline(hit_rect, hit_cell);
                    }
                }
            }
        }
    }
}

/// Wrapper widget that records debug metadata for a widget.
#[derive(Debug, Clone)]
pub struct DebugOverlayStateful<W> {
    inner: W,
    name: String,
    state: Arc<DebugOverlayState>,
    track_render_time: bool,
    hit_areas: Vec<Rect>,
}

impl<W> DebugOverlayStateful<W> {
    pub fn new(inner: W, name: impl Into<String>, state: Arc<DebugOverlayState>) -> Self {
        Self {
            inner,
            name: name.into(),
            state,
            track_render_time: true,
            hit_areas: Vec::new(),
        }
    }

    #[must_use]
    pub fn track_render_time(mut self, enabled: bool) -> Self {
        self.track_render_time = enabled;
        self
    }

    /// Provide static hit areas for overlay visualization.
    #[must_use]
    pub fn hit_areas(mut self, areas: Vec<Rect>) -> Self {
        self.hit_areas = areas;
        self
    }

    pub fn inner(&self) -> &W {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

/// Wrapper state for DebugOverlayStateful.
#[derive(Debug, Clone, Default)]
pub struct DebugOverlayStatefulState<S> {
    pub inner: S,
}

impl<S> DebugOverlayStatefulState<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<W: StatefulWidget> StatefulWidget for DebugOverlayStateful<W> {
    type State = DebugOverlayStatefulState<W::State>;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "DebugOverlayStateful",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if !self.state.enabled() {
            self.inner.render(area, frame, &mut state.inner);
            return;
        }

        let start = if self.track_render_time {
            Some(Instant::now())
        } else {
            None
        };

        self.inner.render(area, frame, &mut state.inner);

        let render_time = start.map(|t| t.elapsed());
        #[cfg(feature = "tracing")]
        trace_widget_render(&self.name, area, render_time);
        let mut info = WidgetDebugInfo::new(self.name.clone(), area);
        info.render_time = render_time;
        info.hit_areas = self.hit_areas.clone();
        self.state.record(info);
    }
}

impl<W: Widget> Widget for DebugOverlayStateful<W> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if !self.state.enabled() {
            self.inner.render(area, frame);
            return;
        }

        let start = if self.track_render_time {
            Some(Instant::now())
        } else {
            None
        };

        self.inner.render(area, frame);

        let render_time = start.map(|t| t.elapsed());
        #[cfg(feature = "tracing")]
        trace_widget_render(&self.name, area, render_time);
        let mut info = WidgetDebugInfo::new(self.name.clone(), area);
        info.render_time = render_time;
        info.hit_areas = self.hit_areas.clone();
        self.state.record(info);
    }
}

#[cfg(feature = "tracing")]
fn trace_widget_render(name: &str, area: Rect, render_time: Option<Duration>) {
    if let Some(time) = render_time {
        tracing::trace!(
            widget = %name,
            render_time_us = %time.as_micros(),
            area = ?area,
            "Widget render complete"
        );
    }
}

fn env_enabled() -> bool {
    std::env::var("FTUI_DEBUG_OVERLAY")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    struct StubWidget;

    impl StatefulWidget for StubWidget {
        type State = ();

        fn render(&self, _area: Rect, _frame: &mut Frame, _state: &mut Self::State) {}
    }

    #[test]
    fn state_records_only_when_enabled() {
        let state = DebugOverlayState::new();
        let info = WidgetDebugInfo::new("stub", Rect::new(0, 0, 2, 2));
        state.record(info);
        assert!(state.snapshot().is_empty());

        state.set_enabled(true);
        state.record(WidgetDebugInfo::new("stub", Rect::new(0, 0, 2, 2)));
        assert_eq!(state.snapshot().len(), 1);
    }

    #[test]
    fn wrapper_records_entry() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);

        let widget = DebugOverlayStateful::new(StubWidget, "Stub", state.clone())
            .hit_areas(vec![Rect::new(1, 1, 2, 1)]);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 2, &mut pool);
        let mut widget_state = DebugOverlayStatefulState::new(());
        widget.render(Rect::new(0, 0, 4, 2), &mut frame, &mut widget_state);

        let entries = state.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Stub");
        assert_eq!(entries[0].hit_areas.len(), 1);
    }

    #[test]
    fn overlay_draws_ascii_border() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        state.record(WidgetDebugInfo::new("Stub", Rect::new(0, 0, 4, 3)));

        let options = DebugOverlayOptions {
            show_names: false,
            show_render_times: false,
            ..Default::default()
        };
        let overlay = DebugOverlay::new(state).options(options);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 4, &mut pool);
        overlay.render(Rect::new(0, 0, 6, 4), &mut frame);

        let cell = frame.buffer.get(0, 0).expect("cell exists");
        assert_eq!(cell.content.as_char(), Some('+'));

        let cell = frame.buffer.get(1, 0).expect("cell exists");
        assert_eq!(cell.content.as_char(), Some('-'));

        let cell = frame.buffer.get(0, 1).expect("cell exists");
        assert_eq!(cell.content.as_char(), Some('|'));
    }

    #[test]
    fn overlay_draws_label_text() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        state.record(WidgetDebugInfo::new("Hi", Rect::new(0, 0, 4, 3)));

        let options = DebugOverlayOptions {
            show_render_times: false,
            ..Default::default()
        };
        let overlay = DebugOverlay::new(state).options(options);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 4, &mut pool);
        overlay.render(Rect::new(0, 0, 6, 4), &mut frame);

        let cell = frame.buffer.get(1, 0).expect("label cell exists");
        assert_eq!(cell.content.as_char(), Some('H'));
    }

    #[test]
    fn overlay_draws_hot_hit_area() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        state.set_hover(Some((2, 2)));

        let mut info = WidgetDebugInfo::new("Hit", Rect::new(0, 0, 4, 3));
        info.hit_areas = vec![Rect::new(2, 2, 1, 1)];
        state.record(info);

        let options = DebugOverlayOptions {
            show_boundaries: false,
            show_names: false,
            show_render_times: false,
            show_hit_areas: true,
            ..Default::default()
        };
        let expected = options.palette.hit_hot_color;

        let overlay = DebugOverlay::new(state).options(options);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 4, &mut pool);
        overlay.render(Rect::new(0, 0, 6, 4), &mut frame);

        let cell = frame.buffer.get(2, 2).expect("hit cell exists");
        assert_eq!(cell.content.as_char(), Some('.'));
        assert_eq!(cell.fg, expected);
    }

    #[test]
    fn overlay_clears_entries_on_render() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        state.record(WidgetDebugInfo::new("Stub", Rect::new(0, 0, 2, 2)));

        let overlay = DebugOverlay::new(state.clone());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 4, &mut pool);
        overlay.render(Rect::new(0, 0, 4, 4), &mut frame);

        assert!(state.snapshot().is_empty());
    }

    #[test]
    fn overlay_preserves_outside_area() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        state.record(WidgetDebugInfo::new("Stub", Rect::new(0, 0, 3, 3)));

        let overlay = DebugOverlay::new(state);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 6, &mut pool);
        frame.buffer.set(5, 5, Cell::from_char('#'));

        overlay.render(Rect::new(0, 0, 4, 4), &mut frame);

        let cell = frame.buffer.get(5, 5).expect("sentinel cell exists");
        assert_eq!(cell.content.as_char(), Some('#'));
    }

    #[test]
    fn toggle_on_f12_enables_overlay() {
        let state = DebugOverlayState::new();
        assert!(!state.enabled());

        let event = Event::Key(ftui_core::event::KeyEvent {
            code: KeyCode::F(12),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });

        assert!(state.toggle_on_f12(&event));
        assert!(state.enabled());
    }

    #[test]
    fn update_hover_from_mouse_event_sets_position() {
        let state = DebugOverlayState::new();
        let event = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::Moved,
            7,
            9,
        ));
        assert_eq!(state.update_hover_from_event(&event), Some((7, 9)));
    }

    #[test]
    fn toggle_cycles_enabled_state() {
        let state = DebugOverlayState::new();
        assert!(!state.enabled());
        let next = state.toggle();
        assert!(next);
        assert!(state.enabled());
        let next2 = state.toggle();
        assert!(!next2);
        assert!(!state.enabled());
    }

    #[test]
    fn clear_removes_all_entries() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        state.record(WidgetDebugInfo::new("a", Rect::new(0, 0, 2, 2)));
        state.record(WidgetDebugInfo::new("b", Rect::new(0, 0, 2, 2)));
        assert_eq!(state.snapshot().len(), 2);
        state.clear();
        assert!(state.snapshot().is_empty());
    }

    #[test]
    fn widget_debug_info_defaults() {
        let info = WidgetDebugInfo::new("test", Rect::new(3, 4, 5, 6));
        assert_eq!(info.name, "test");
        assert_eq!(info.area, Rect::new(3, 4, 5, 6));
        assert!(info.render_time.is_none());
        assert!(info.hit_areas.is_empty());
    }

    #[test]
    fn format_label_with_render_time() {
        let state = DebugOverlayState::new();
        let overlay = DebugOverlay::new(state);
        let mut info = WidgetDebugInfo::new("Button", Rect::new(0, 0, 5, 1));
        info.render_time = Some(Duration::from_micros(42));
        let label = overlay.format_label(&info);
        assert_eq!(label, "Button 42us");
    }

    #[test]
    fn format_label_without_render_time_option() {
        let state = DebugOverlayState::new();
        let options = DebugOverlayOptions {
            show_render_times: false,
            ..Default::default()
        };
        let overlay = DebugOverlay::new(state).options(options);
        let mut info = WidgetDebugInfo::new("Button", Rect::new(0, 0, 5, 1));
        info.render_time = Some(Duration::from_micros(42));
        let label = overlay.format_label(&info);
        assert_eq!(label, "Button");
    }

    #[test]
    fn format_label_no_render_time_recorded() {
        let state = DebugOverlayState::new();
        let overlay = DebugOverlay::new(state);
        let info = WidgetDebugInfo::new("Panel", Rect::new(0, 0, 5, 1));
        let label = overlay.format_label(&info);
        assert_eq!(label, "Panel");
    }

    #[test]
    fn wrapper_inner_accessors() {
        let state = DebugOverlayState::new();
        let mut wrapper = DebugOverlayStateful::new(StubWidget, "S", state);
        let _inner_ref: &StubWidget = wrapper.inner();
        let _inner_mut: &mut StubWidget = wrapper.inner_mut();
        let _inner_owned: StubWidget = wrapper.into_inner();
    }

    #[test]
    fn stateful_state_wraps_inner() {
        let state = DebugOverlayStatefulState::new(42u32);
        assert_eq!(state.inner, 42);
    }

    #[test]
    fn toggle_on_non_f12_returns_false() {
        let state = DebugOverlayState::new();
        let event = Event::Key(ftui_core::event::KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        assert!(!state.toggle_on_f12(&event));
        assert!(!state.enabled());
    }

    #[test]
    fn update_hover_non_mouse_returns_current() {
        let state = DebugOverlayState::new();
        state.set_hover(Some((3, 4)));
        let event = Event::Key(ftui_core::event::KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        assert_eq!(state.update_hover_from_event(&event), Some((3, 4)));
    }

    #[test]
    fn hover_position_default_is_none() {
        let state = DebugOverlayState::new();
        assert!(state.hover_position().is_none());
    }

    #[test]
    fn options_default_values() {
        let opts = DebugOverlayOptions::default();
        assert!(opts.show_boundaries);
        assert!(opts.show_names);
        assert!(opts.show_render_times);
        assert!(opts.show_hit_areas);
        assert!(opts.clear_on_render);
    }

    #[test]
    fn palette_default_has_six_border_colors() {
        let palette = DebugOverlayPalette::default();
        assert_eq!(palette.border_colors.len(), 6);
    }

    #[test]
    fn wrapper_widget_impl_disabled_skips_recording() {
        let state = DebugOverlayState::new();
        // Disabled by default.
        let wrapper = DebugOverlayStateful::new(StubWidget, "Skip", state.clone());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 2, &mut pool);
        <DebugOverlayStateful<StubWidget> as StatefulWidget>::render(
            &wrapper,
            Rect::new(0, 0, 4, 2),
            &mut frame,
            &mut DebugOverlayStatefulState::new(()),
        );
        assert!(
            state.snapshot().is_empty(),
            "disabled wrapper should not record"
        );
    }

    #[test]
    fn wrapper_track_render_time_false() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        let wrapper =
            DebugOverlayStateful::new(StubWidget, "NoTime", state.clone()).track_render_time(false);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 2, &mut pool);
        let mut ws = DebugOverlayStatefulState::new(());
        wrapper.render(Rect::new(0, 0, 4, 2), &mut frame, &mut ws);
        let entries = state.snapshot();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].render_time.is_none());
    }

    #[test]
    fn overlay_does_not_clear_entries_when_clear_on_render_false() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        state.record(WidgetDebugInfo::new("Stub", Rect::new(0, 0, 2, 2)));

        let options = DebugOverlayOptions {
            clear_on_render: false,
            show_names: false,
            show_render_times: false,
            ..Default::default()
        };
        let overlay = DebugOverlay::new(state.clone()).options(options);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 4, &mut pool);
        overlay.render(Rect::new(0, 0, 4, 4), &mut frame);

        assert_eq!(state.snapshot().len(), 1);
    }

    #[test]
    fn overlay_disabled_does_not_clear_entries() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        state.record(WidgetDebugInfo::new("Stub", Rect::new(0, 0, 2, 2)));
        state.set_enabled(false);

        let overlay = DebugOverlay::new(state.clone());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 4, &mut pool);
        overlay.render(Rect::new(0, 0, 4, 4), &mut frame);

        assert_eq!(state.snapshot().len(), 1);
    }

    #[test]
    fn update_hover_from_scroll_event_does_not_change_position() {
        let state = DebugOverlayState::new();
        state.set_hover(Some((3, 4)));

        let event = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::ScrollDown,
            7,
            9,
        ));

        assert_eq!(state.update_hover_from_event(&event), Some((3, 4)));
    }

    #[test]
    fn update_hover_from_drag_event_sets_position() {
        let state = DebugOverlayState::new();
        let event = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::Drag(ftui_core::event::MouseButton::Left),
            7,
            9,
        ));

        assert_eq!(state.update_hover_from_event(&event), Some((7, 9)));
    }

    #[test]
    fn overlay_draws_non_hot_hit_area() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);

        let mut info = WidgetDebugInfo::new("Hit", Rect::new(0, 0, 4, 3));
        info.hit_areas = vec![Rect::new(2, 2, 1, 1)];
        state.record(info);

        let options = DebugOverlayOptions {
            show_boundaries: false,
            show_names: false,
            show_render_times: false,
            show_hit_areas: true,
            ..Default::default()
        };
        let expected = options.palette.hit_color;

        let overlay = DebugOverlay::new(state).options(options);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 4, &mut pool);
        overlay.render(Rect::new(0, 0, 6, 4), &mut frame);

        let cell = frame.buffer.get(2, 2).expect("hit cell exists");
        assert_eq!(cell.content.as_char(), Some('.'));
        assert_eq!(cell.fg, expected);
    }

    #[test]
    fn overlay_label_clips_at_rect_right_edge() {
        let state = DebugOverlayState::new();
        state.set_enabled(true);
        state.record(WidgetDebugInfo::new("WXYZ", Rect::new(0, 0, 3, 2)));

        let options = DebugOverlayOptions {
            show_boundaries: false,
            show_render_times: false,
            ..Default::default()
        };
        let overlay = DebugOverlay::new(state).options(options);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 2, &mut pool);
        frame.buffer.set(3, 0, Cell::from_char('#'));

        overlay.render(Rect::new(0, 0, 4, 2), &mut frame);

        let cell = frame.buffer.get(1, 0).expect("label cell exists");
        assert_eq!(cell.content.as_char(), Some('W'));
        let cell = frame.buffer.get(2, 0).expect("label cell exists");
        assert_eq!(cell.content.as_char(), Some('X'));

        let cell = frame.buffer.get(3, 0).expect("sentinel cell exists");
        assert_eq!(cell.content.as_char(), Some('#'));
    }

    #[test]
    fn wrapper_widget_impl_records_entry() {
        #[derive(Debug, Clone, Copy)]
        struct StatelessStub;

        impl Widget for StatelessStub {
            fn render(&self, _area: Rect, _frame: &mut Frame) {}
        }

        let state = DebugOverlayState::new();
        state.set_enabled(true);

        let wrapper = DebugOverlayStateful::new(StatelessStub, "Stateless", state.clone());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 2, &mut pool);
        wrapper.render(Rect::new(0, 0, 4, 2), &mut frame);

        let entries = state.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Stateless");
    }
}
