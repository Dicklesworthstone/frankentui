#![forbid(unsafe_code)]

//! Tabs widget.
//!
//! Provides a horizontal tab bar with keyboard navigation, overflow handling,
//! closable tabs, and tab reordering helpers.

use crate::mouse::MouseResult;
use crate::{StatefulWidget, Widget, draw_text_span, set_style_area};
use ftui_core::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::Style;
use ftui_text::display_width;
#[cfg(feature = "tracing")]
use std::time::Instant;

/// A single tab entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab<'a> {
    title: String,
    style: Style,
    closable: bool,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> Tab<'a> {
    /// Create a new tab with a title.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            style: Style::default(),
            closable: false,
            _marker: std::marker::PhantomData,
        }
    }

    /// Set style for this tab.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set whether this tab can be closed.
    #[must_use]
    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    /// Get tab title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Whether this tab can be closed.
    #[must_use]
    pub const fn is_closable(&self) -> bool {
        self.closable
    }
}

/// State for a [`Tabs`] widget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabsState {
    /// Active tab index.
    pub active: usize,
    /// Left-most tab index when overflow scrolling is active.
    pub offset: usize,
}

impl TabsState {
    /// Select a specific tab index.
    pub fn select(&mut self, index: usize, tab_count: usize) -> bool {
        if tab_count == 0 {
            self.active = 0;
            self.offset = 0;
            return false;
        }
        let next = index.min(tab_count.saturating_sub(1));
        if self.active == next {
            return false;
        }
        #[cfg(feature = "tracing")]
        let old = self.active;
        self.active = next;
        if self.active < self.offset {
            self.offset = self.active;
        }
        #[cfg(feature = "tracing")]
        Self::log_switch("select", old, self.active);
        true
    }

    /// Move active tab right by one.
    pub fn next(&mut self, tab_count: usize) -> bool {
        if tab_count == 0 {
            return false;
        }
        self.select(
            self.active
                .saturating_add(1)
                .min(tab_count.saturating_sub(1)),
            tab_count,
        )
    }

    /// Move active tab left by one.
    pub fn previous(&mut self, tab_count: usize) -> bool {
        if tab_count == 0 {
            return false;
        }
        self.select(self.active.saturating_sub(1), tab_count)
    }

    /// Handle keyboard tab switching.
    ///
    /// Supported:
    /// - `Left` / `Right`
    /// - number keys `1..9`
    pub fn handle_key(&mut self, key: &KeyEvent, tab_count: usize) -> bool {
        match key.code {
            KeyCode::Left => self.previous(tab_count),
            KeyCode::Right => self.next(tab_count),
            KeyCode::Char(ch) if ('1'..='9').contains(&ch) => {
                let idx = ch as usize - '1' as usize;
                if idx >= tab_count {
                    false
                } else {
                    self.select(idx, tab_count)
                }
            }
            _ => false,
        }
    }

    /// Handle mouse selection for tabs.
    ///
    /// Hit data convention: each tab row registers `data = tab_index as u64`.
    pub fn handle_mouse(
        &mut self,
        event: &MouseEvent,
        hit: Option<(HitId, HitRegion, u64)>,
        expected_id: HitId,
        tab_count: usize,
    ) -> MouseResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((id, HitRegion::Content, data)) = hit
                    && id == expected_id
                {
                    let idx = data as usize;
                    if idx < tab_count {
                        if self.active == idx {
                            return MouseResult::Activated(idx);
                        }
                        self.select(idx, tab_count);
                        return MouseResult::Selected(idx);
                    }
                }
                MouseResult::Ignored
            }
            _ => MouseResult::Ignored,
        }
    }

    #[cfg(feature = "tracing")]
    fn log_switch(reason: &str, from: usize, to: usize) {
        tracing::debug!(message = "tabs.switch", reason, from, to);
    }
}

/// Tabs widget.
#[derive(Debug, Clone, Default)]
pub struct Tabs<'a> {
    tabs: Vec<Tab<'a>>,
    style: Style,
    active_style: Style,
    separator: &'a str,
    close_marker: &'a str,
    overflow_left_marker: &'a str,
    overflow_right_marker: &'a str,
    hit_id: Option<HitId>,
}

impl<'a> Tabs<'a> {
    /// Create tabs from an iterator.
    #[must_use]
    pub fn new(tabs: impl IntoIterator<Item = Tab<'a>>) -> Self {
        Self {
            tabs: tabs.into_iter().collect(),
            style: Style::default(),
            active_style: Style::default(),
            separator: " ",
            close_marker: " x",
            overflow_left_marker: "<",
            overflow_right_marker: ">",
            hit_id: None,
        }
    }

    /// Set base style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set active tab style.
    #[must_use]
    pub fn active_style(mut self, style: Style) -> Self {
        self.active_style = style;
        self
    }

    /// Set separator between tabs.
    #[must_use]
    pub fn separator(mut self, separator: &'a str) -> Self {
        self.separator = separator;
        self
    }

    /// Set hit id for mouse interactions.
    #[must_use]
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.hit_id = Some(id);
        self
    }

    /// Immutable tab slice.
    #[must_use]
    pub fn tabs(&self) -> &[Tab<'a>] {
        &self.tabs
    }

    fn tab_label(&self, tab: &Tab<'_>, active: bool) -> String {
        let mut out = String::new();
        if active {
            out.push('[');
        } else {
            out.push(' ');
        }
        out.push_str(tab.title());
        if tab.is_closable() {
            out.push_str(self.close_marker);
        }
        if active {
            out.push(']');
        } else {
            out.push(' ');
        }
        out
    }

    fn visible_end(&self, state: &TabsState, width: usize) -> usize {
        if self.tabs.is_empty() || width == 0 {
            return state.offset;
        }
        let sep_width = display_width(self.separator);
        let mut used = 0usize;
        let mut end = state.offset;

        for idx in state.offset..self.tabs.len() {
            let w = display_width(
                self.tab_label(&self.tabs[idx], idx == state.active)
                    .as_str(),
            );
            let extra = if idx == state.offset { 0 } else { sep_width };
            if end == state.offset {
                // Always allow at least one tab; draw helper clips if too long.
                used = w;
                end = idx + 1;
                if used > width {
                    break;
                }
                continue;
            }
            if used.saturating_add(extra).saturating_add(w) > width {
                break;
            }
            used = used.saturating_add(extra).saturating_add(w);
            end = idx + 1;
        }

        end.max((state.offset + 1).min(self.tabs.len()))
    }

    fn compute_visible_range(
        &self,
        state: &mut TabsState,
        area_width: usize,
    ) -> (usize, usize, bool, bool) {
        if self.tabs.is_empty() || area_width == 0 {
            state.active = 0;
            state.offset = 0;
            return (0, 0, false, false);
        }
        state.active = state.active.min(self.tabs.len().saturating_sub(1));
        state.offset = state.offset.min(self.tabs.len().saturating_sub(1));
        if state.active < state.offset {
            state.offset = state.active;
        }

        let mut start = state.offset;
        let mut end = self.visible_end(state, area_width);

        if state.active >= end {
            start = state.active;
            state.offset = start;
            end = self.visible_end(state, area_width);
        }

        let overflow_left = start > 0;
        let overflow_right = end < self.tabs.len();
        (start, end, overflow_left, overflow_right)
    }

    /// Close the active tab if it is closable.
    pub fn close_active(&mut self, state: &mut TabsState) -> Option<Tab<'a>> {
        if self.tabs.is_empty() {
            state.active = 0;
            state.offset = 0;
            return None;
        }
        state.active = state.active.min(self.tabs.len().saturating_sub(1));
        if !self.tabs[state.active].is_closable() {
            return None;
        }
        let removed = self.tabs.remove(state.active);
        if self.tabs.is_empty() {
            state.active = 0;
            state.offset = 0;
        } else if state.active >= self.tabs.len() {
            state.active = self.tabs.len().saturating_sub(1);
            state.offset = state.offset.min(state.active);
        }
        Some(removed)
    }

    /// Move active tab one position to the left.
    pub fn move_active_left(&mut self, state: &mut TabsState) -> bool {
        if self.tabs.len() < 2 || state.active == 0 || state.active >= self.tabs.len() {
            return false;
        }
        self.tabs.swap(state.active, state.active - 1);
        state.active -= 1;
        state.offset = state.offset.min(state.active);
        true
    }

    /// Move active tab one position to the right.
    pub fn move_active_right(&mut self, state: &mut TabsState) -> bool {
        if self.tabs.len() < 2 || state.active + 1 >= self.tabs.len() {
            return false;
        }
        self.tabs.swap(state.active, state.active + 1);
        state.active += 1;
        true
    }
}

impl StatefulWidget for Tabs<'_> {
    type State = TabsState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        #[cfg(feature = "tracing")]
        let render_start = Instant::now();

        if area.is_empty() || area.height == 0 {
            return;
        }
        if self.tabs.is_empty() {
            return;
        }

        let (start, end, overflow_left, overflow_right) =
            self.compute_visible_range(state, area.width as usize);

        #[cfg(feature = "tracing")]
        let tab_count = self.tabs.len();
        #[cfg(feature = "tracing")]
        let active_tab = state.active.min(self.tabs.len().saturating_sub(1));
        #[cfg(feature = "tracing")]
        let render_span = tracing::debug_span!(
            "tabs.render",
            tab_count,
            active_tab,
            overflow = overflow_left || overflow_right,
            render_duration_us = tracing::field::Empty
        );
        #[cfg(feature = "tracing")]
        let _render_guard = render_span.enter();

        set_style_area(
            &mut frame.buffer,
            Rect::new(area.x, area.y, area.width, 1),
            self.style,
        );

        let mut left = area.x;
        let mut right = area.right();
        if overflow_left {
            draw_text_span(
                frame,
                area.x,
                area.y,
                self.overflow_left_marker,
                self.style,
                area.right(),
            );
            left = left.saturating_add(display_width(self.overflow_left_marker) as u16);
        }
        if overflow_right {
            right = right.saturating_sub(display_width(self.overflow_right_marker) as u16);
            draw_text_span(
                frame,
                right,
                area.y,
                self.overflow_right_marker,
                self.style,
                area.right(),
            );
        }

        let mut x = left;
        for idx in start..end {
            if x >= right {
                break;
            }
            if idx > start && !self.separator.is_empty() {
                x = draw_text_span(frame, x, area.y, self.separator, self.style, right);
                if x >= right {
                    break;
                }
            }
            let tab = &self.tabs[idx];
            let label = self.tab_label(tab, idx == state.active);
            let mut tab_style = self.style.merge(&tab.style);
            if idx == state.active {
                tab_style = self.active_style.merge(&tab_style);
            }
            let before = x;
            x = draw_text_span(frame, x, area.y, &label, tab_style, right);
            if let Some(id) = self.hit_id {
                let width = x.saturating_sub(before).max(1);
                frame.register_hit(
                    Rect::new(before, area.y, width, 1),
                    id,
                    HitRegion::Content,
                    idx as u64,
                );
            }
        }

        #[cfg(feature = "tracing")]
        {
            let elapsed_us = render_start.elapsed().as_micros() as u64;
            render_span.record("render_duration_us", elapsed_us);
        }
    }
}

impl Widget for Tabs<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let mut state = TabsState::default();
        StatefulWidget::render(self, area, frame, &mut state);
    }

    fn is_essential(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyCode, KeyEvent};
    use ftui_render::grapheme_pool::GraphemePool;
    #[cfg(feature = "tracing")]
    use std::sync::{Arc, Mutex};
    #[cfg(feature = "tracing")]
    use tracing::Subscriber;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::Layer;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::layer::{Context, SubscriberExt};

    fn row_text(frame: &Frame, y: u16) -> String {
        let mut out = String::new();
        for x in 0..frame.buffer.width() {
            let ch = frame
                .buffer
                .get(x, y)
                .and_then(|cell| cell.content.as_char())
                .unwrap_or(' ');
            out.push(ch);
        }
        out
    }

    #[test]
    fn tabs_render_basic() {
        let tabs = Tabs::new(vec![Tab::new("One"), Tab::new("Two"), Tab::new("Three")]);
        let mut state = TabsState::default();
        state.select(1, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 30, 1), &mut frame, &mut state);
        let row = row_text(&frame, 0);
        assert!(row.contains("[Two]"));
    }

    #[test]
    fn tabs_keyboard_switching_arrows_and_numbers() {
        let mut state = TabsState::default();
        assert!(state.handle_key(&KeyEvent::new(KeyCode::Right), 4));
        assert_eq!(state.active, 1);
        assert!(state.handle_key(&KeyEvent::new(KeyCode::Left), 4));
        assert_eq!(state.active, 0);
        assert!(state.handle_key(&KeyEvent::new(KeyCode::Char('3')), 4));
        assert_eq!(state.active, 2);
        assert!(!state.handle_key(&KeyEvent::new(KeyCode::Char('9')), 4));
        assert_eq!(state.active, 2);
    }

    #[test]
    fn tabs_overflow_markers_render_when_needed() {
        let tabs = Tabs::new((0..8).map(|i| Tab::new(format!("Tab{i}"))));
        let mut state = TabsState::default();
        state.select(0, 8);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 12, 1), &mut frame, &mut state);
        assert_eq!(
            frame.buffer.get(11, 0).and_then(|c| c.content.as_char()),
            Some('>')
        );

        state.select(7, 8);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 12, 1), &mut frame, &mut state);
        assert_eq!(
            frame.buffer.get(0, 0).and_then(|c| c.content.as_char()),
            Some('<')
        );
    }

    #[test]
    fn tabs_close_active_respects_closable() {
        let mut tabs = Tabs::new(vec![
            Tab::new("Pinned").closable(false),
            Tab::new("Temp").closable(true),
        ]);
        let mut state = TabsState::default();
        state.select(0, 2);
        assert!(tabs.close_active(&mut state).is_none());
        state.select(1, 2);
        assert!(tabs.close_active(&mut state).is_some());
        assert_eq!(tabs.tabs().len(), 1);
        assert_eq!(tabs.tabs()[0].title(), "Pinned");
    }

    #[test]
    fn tabs_reorder_active_left_and_right() {
        let mut tabs = Tabs::new(vec![Tab::new("A"), Tab::new("B"), Tab::new("C")]);
        let mut state = TabsState::default();
        state.select(1, 3);
        assert!(tabs.move_active_left(&mut state));
        assert_eq!(state.active, 0);
        assert_eq!(tabs.tabs()[0].title(), "B");
        assert!(tabs.move_active_right(&mut state));
        assert_eq!(state.active, 1);
        assert_eq!(tabs.tabs()[1].title(), "B");
    }

    #[test]
    fn tabs_hit_regions_encode_tab_index() {
        let tabs = Tabs::new(vec![Tab::new("A"), Tab::new("B")]).hit_id(HitId::new(5));
        let mut state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(20, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 20, 1), &mut frame, &mut state);
        let hit_a = frame.hit_test(1, 0);
        let hit_b = frame.hit_test(6, 0);
        assert_eq!(hit_a.map(|(_, _, data)| data), Some(0));
        assert_eq!(hit_b.map(|(_, _, data)| data), Some(1));
    }

    #[cfg(feature = "tracing")]
    #[derive(Default)]
    struct TabsTraceState {
        saw_render_span: bool,
        saw_switch_event: bool,
        saw_duration_record: bool,
    }

    #[cfg(feature = "tracing")]
    struct TabsTraceCapture {
        state: Arc<Mutex<TabsTraceState>>,
    }

    #[cfg(feature = "tracing")]
    impl<S> Layer<S> for TabsTraceCapture
    where
        S: Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::Id,
            _ctx: Context<'_, S>,
        ) {
            if attrs.metadata().name() == "tabs.render" {
                self.state.lock().expect("tabs trace lock").saw_render_span = true;
            }
        }

        fn on_record(
            &self,
            id: &tracing::Id,
            values: &tracing::span::Record<'_>,
            ctx: Context<'_, S>,
        ) {
            let Some(span) = ctx.span(id) else {
                return;
            };
            if span.metadata().name() != "tabs.render" {
                return;
            }
            struct V {
                saw: bool,
            }
            impl tracing::field::Visit for V {
                fn record_u64(&mut self, field: &tracing::field::Field, _value: u64) {
                    if field.name() == "render_duration_us" {
                        self.saw = true;
                    }
                }

                fn record_debug(
                    &mut self,
                    _field: &tracing::field::Field,
                    _value: &dyn std::fmt::Debug,
                ) {
                }
            }
            let mut v = V { saw: false };
            values.record(&mut v);
            if v.saw {
                self.state
                    .lock()
                    .expect("tabs trace lock")
                    .saw_duration_record = true;
            }
        }

        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            struct Msg {
                message: Option<String>,
            }
            impl tracing::field::Visit for Msg {
                fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                    if field.name() == "message" {
                        self.message = Some(value.to_string());
                    }
                }

                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.message = Some(format!("{value:?}").trim_matches('"').to_string());
                    }
                }
            }
            let mut msg = Msg { message: None };
            event.record(&mut msg);
            if msg.message.as_deref() == Some("tabs.switch") {
                self.state.lock().expect("tabs trace lock").saw_switch_event = true;
            }
        }
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn tabs_tracing_span_and_switch_event_emitted() {
        let state = Arc::new(Mutex::new(TabsTraceState::default()));
        let subscriber = tracing_subscriber::registry().with(TabsTraceCapture {
            state: Arc::clone(&state),
        });
        let _guard = tracing::subscriber::set_default(subscriber);

        let tabs = Tabs::new(vec![Tab::new("A"), Tab::new("B"), Tab::new("C")]);
        let mut tabs_state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 20, 1), &mut frame, &mut tabs_state);
        assert!(tabs_state.handle_key(&KeyEvent::new(KeyCode::Right), 3));

        let snapshot = state.lock().expect("tabs trace lock");
        assert!(snapshot.saw_render_span, "expected tabs.render span");
        assert!(
            snapshot.saw_duration_record,
            "expected render_duration_us record"
        );
        assert!(
            snapshot.saw_switch_event,
            "expected tabs.switch debug event"
        );
    }
}
