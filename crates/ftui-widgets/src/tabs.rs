#![forbid(unsafe_code)]

//! Tabs widget.
//!
//! Provides a horizontal tab bar with keyboard navigation, overflow handling,
//! closable tabs, and tab reordering helpers.

use crate::mouse::MouseResult;
use crate::{StatefulWidget, Widget, clear_text_row, draw_text_span};
use ftui_core::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_style::Style;
use ftui_text::display_width;
#[cfg(feature = "tracing")]
use web_time::Instant;

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

        let left_marker_w = display_width(self.overflow_left_marker);
        let right_marker_w = display_width(self.overflow_right_marker);

        let mut available_width = area_width;
        let mut start = state.offset;
        let mut end = self.visible_end(state, available_width);

        // If active is out of view (e.g. initial render with small width), jump to it
        if state.active >= end {
            start = state.active;
            state.offset = start;
            end = self.visible_end(state, available_width);
        }

        // Iteratively refine width based on overflow markers
        for _ in 0..3 {
            let overflow_left = start > 0;
            let overflow_right = end < self.tabs.len();

            let mut next_width = area_width;
            if overflow_left {
                next_width = next_width.saturating_sub(left_marker_w);
            }
            if overflow_right {
                next_width = next_width.saturating_sub(right_marker_w);
            }

            if next_width == available_width {
                break;
            }
            available_width = next_width;

            // Re-calculate with new width
            end = self.visible_end(state, available_width);

            // Ensure active is still visible
            if state.active >= end {
                start = state.active;
                state.offset = start;
                end = self.visible_end(state, available_width);
            }
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
        if frame.a11y_enabled() {
            frame.push_a11y_nodes(ftui_a11y::Accessible::accessibility_nodes(self, area));
        }

        let deg = frame.buffer.degradation;
        let base_style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };

        clear_text_row(frame, area, base_style);

        if !deg.render_content() || self.tabs.is_empty() {
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

        let mut left = area.x;
        let mut right = area.right();
        if overflow_left {
            draw_text_span(
                frame,
                area.x,
                area.y,
                self.overflow_left_marker,
                base_style,
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
                base_style,
                area.right(),
            );
        }

        let mut x = left;
        for idx in start..end {
            if x >= right {
                break;
            }
            if idx > start && !self.separator.is_empty() {
                x = draw_text_span(frame, x, area.y, self.separator, base_style, right);
                if x >= right {
                    break;
                }
            }
            let tab = &self.tabs[idx];
            let label = self.tab_label(tab, idx == state.active);
            let mut tab_style = base_style;
            if deg.apply_styling() {
                tab_style = self.style.merge(&tab.style);
                if idx == state.active {
                    tab_style = self.active_style.merge(&tab_style);
                }
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

impl ftui_a11y::Accessible for Tabs<'_> {
    fn accessibility_nodes(&self, area: Rect) -> Vec<ftui_a11y::node::A11yNodeInfo> {
        use ftui_a11y::node::{A11yNodeInfo, A11yRole};

        let base_id = crate::a11y_node_id(area);
        let tab_count = self.tabs.len();
        let child_ids: Vec<u64> = (0..tab_count).map(|i| base_id + 1 + i as u64).collect();

        let group_node = A11yNodeInfo::new(base_id, A11yRole::Group, area)
            .with_name(format!("{tab_count} tabs"))
            .with_children(child_ids);

        let mut nodes = vec![group_node];
        for (i, tab) in self.tabs.iter().enumerate() {
            let tab_id = base_id + 1 + i as u64;
            nodes.push(
                A11yNodeInfo::new(tab_id, A11yRole::Tab, area)
                    .with_name(&tab.title)
                    .with_parent(base_id),
            );
        }
        nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyCode, KeyEvent};
    use ftui_render::budget::DegradationLevel;
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
        let _trace_test_guard = crate::tracing_test_support::acquire();
        let subscriber = tracing_subscriber::registry().with(TabsTraceCapture {
            state: Arc::clone(&state),
        });
        let _guard = tracing::subscriber::set_default(subscriber);

        let tabs = Tabs::new(vec![Tab::new("A"), Tab::new("B"), Tab::new("C")]);
        let mut tabs_state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        tracing::callsite::rebuild_interest_cache();
        StatefulWidget::render(&tabs, Rect::new(0, 0, 20, 1), &mut frame, &mut tabs_state);
        tracing::callsite::rebuild_interest_cache();
        assert!(tabs_state.handle_key(&KeyEvent::new(KeyCode::Right), 3));
        tracing::callsite::rebuild_interest_cache();

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

    // --- bd-1lg.28: Selection & switching tests ---

    #[test]
    fn tabs_select_zero_count_resets() {
        let mut state = TabsState {
            active: 3,
            offset: 2,
        };
        assert!(!state.select(0, 0));
        assert_eq!(state.active, 0);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn tabs_select_same_tab_returns_false() {
        let mut state = TabsState::default();
        state.select(2, 5);
        assert!(!state.select(2, 5));
    }

    #[test]
    fn tabs_select_out_of_range_clamps() {
        let mut state = TabsState::default();
        assert!(state.select(100, 5));
        assert_eq!(state.active, 4); // clamped to last
    }

    #[test]
    fn tabs_select_updates_offset_when_active_before_offset() {
        let mut state = TabsState {
            active: 3,
            offset: 3,
        };
        assert!(state.select(1, 5));
        assert_eq!(state.active, 1);
        assert_eq!(state.offset, 1); // offset scrolled back to active
    }

    #[test]
    fn tabs_next_at_last_tab_returns_false() {
        let mut state = TabsState::default();
        state.select(4, 5);
        assert!(!state.next(5));
        assert_eq!(state.active, 4);
    }

    #[test]
    fn tabs_next_empty_returns_false() {
        let mut state = TabsState::default();
        assert!(!state.next(0));
    }

    #[test]
    fn tabs_previous_at_first_tab_returns_false() {
        let mut state = TabsState::default();
        assert!(!state.previous(5));
        assert_eq!(state.active, 0);
    }

    #[test]
    fn tabs_previous_empty_returns_false() {
        let mut state = TabsState::default();
        assert!(!state.previous(0));
    }

    #[test]
    fn tabs_handle_key_unhandled_returns_false() {
        let mut state = TabsState::default();
        assert!(!state.handle_key(&KeyEvent::new(KeyCode::Enter), 3));
        assert!(!state.handle_key(&KeyEvent::new(KeyCode::Escape), 3));
        assert!(!state.handle_key(&KeyEvent::new(KeyCode::Up), 3));
    }

    #[test]
    fn tabs_handle_key_number_at_exact_tab_count_returns_false() {
        let mut state = TabsState::default();
        // 3 tabs: valid are '1','2','3'; '4' is out of range
        assert!(!state.handle_key(&KeyEvent::new(KeyCode::Char('4')), 3));
    }

    #[test]
    fn tabs_handle_key_number_one_selects_first() {
        let mut state = TabsState::default();
        state.select(2, 5);
        assert!(state.handle_key(&KeyEvent::new(KeyCode::Char('1')), 5));
        assert_eq!(state.active, 0);
    }

    // --- Mouse handling tests ---

    use crate::mouse::MouseResult;
    use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};

    #[test]
    fn tabs_mouse_click_selects() {
        let mut state = TabsState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 0);
        let hit = Some((HitId::new(1), HitRegion::Content, 2u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 5);
        assert_eq!(result, MouseResult::Selected(2));
        assert_eq!(state.active, 2);
    }

    #[test]
    fn tabs_mouse_click_same_tab_activates() {
        let mut state = TabsState::default();
        state.select(2, 5);
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 0);
        let hit = Some((HitId::new(1), HitRegion::Content, 2u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 5);
        assert_eq!(result, MouseResult::Activated(2));
    }

    #[test]
    fn tabs_mouse_click_wrong_id_ignored() {
        let mut state = TabsState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 0);
        let hit = Some((HitId::new(99), HitRegion::Content, 2u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 5);
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn tabs_mouse_right_click_ignored() {
        let mut state = TabsState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Right), 5, 0);
        let hit = Some((HitId::new(1), HitRegion::Content, 2u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 5);
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn tabs_mouse_click_out_of_range() {
        let mut state = TabsState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 0);
        let hit = Some((HitId::new(1), HitRegion::Content, 20u64));
        let result = state.handle_mouse(&event, hit, HitId::new(1), 5);
        assert_eq!(result, MouseResult::Ignored);
    }

    #[test]
    fn tabs_mouse_no_hit_ignored() {
        let mut state = TabsState::default();
        let event = MouseEvent::new(MouseEventKind::Down(MouseButton::Left), 5, 0);
        let result = state.handle_mouse(&event, None, HitId::new(1), 5);
        assert_eq!(result, MouseResult::Ignored);
    }

    // --- Close active tests ---

    #[test]
    fn tabs_close_active_empty_returns_none() {
        let mut tabs = Tabs::new(Vec::<Tab>::new());
        let mut state = TabsState::default();
        assert!(tabs.close_active(&mut state).is_none());
    }

    #[test]
    fn tabs_close_active_last_remaining_resets_state() {
        let mut tabs = Tabs::new(vec![Tab::new("Only").closable(true)]);
        let mut state = TabsState::default();
        let removed = tabs.close_active(&mut state);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().title(), "Only");
        assert!(tabs.tabs().is_empty());
        assert_eq!(state.active, 0);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn tabs_close_active_middle_shifts_active() {
        let mut tabs = Tabs::new(vec![
            Tab::new("A"),
            Tab::new("B").closable(true),
            Tab::new("C"),
        ]);
        let mut state = TabsState::default();
        state.select(1, 3); // select B
        let removed = tabs.close_active(&mut state);
        assert_eq!(removed.unwrap().title(), "B");
        assert_eq!(tabs.tabs().len(), 2);
        // Active should stay at 1 (now "C"), or adjust if at end
        assert!(state.active < tabs.tabs().len());
    }

    #[test]
    fn tabs_close_active_at_end_moves_active_back() {
        let mut tabs = Tabs::new(vec![
            Tab::new("A"),
            Tab::new("B"),
            Tab::new("C").closable(true),
        ]);
        let mut state = TabsState::default();
        state.select(2, 3); // select C (last)
        tabs.close_active(&mut state);
        assert_eq!(tabs.tabs().len(), 2);
        assert_eq!(state.active, 1); // moved back to B
    }

    // --- Reorder tests ---

    #[test]
    fn tabs_move_active_left_at_boundary_returns_false() {
        let mut tabs = Tabs::new(vec![Tab::new("A"), Tab::new("B")]);
        let mut state = TabsState::default(); // active = 0
        assert!(!tabs.move_active_left(&mut state));
    }

    #[test]
    fn tabs_move_active_right_at_boundary_returns_false() {
        let mut tabs = Tabs::new(vec![Tab::new("A"), Tab::new("B")]);
        let mut state = TabsState::default();
        state.select(1, 2); // active = last
        assert!(!tabs.move_active_right(&mut state));
    }

    #[test]
    fn tabs_move_active_single_tab_returns_false() {
        let mut tabs = Tabs::new(vec![Tab::new("Only")]);
        let mut state = TabsState::default();
        assert!(!tabs.move_active_left(&mut state));
        assert!(!tabs.move_active_right(&mut state));
    }

    // --- Render tests ---

    #[test]
    fn tabs_render_empty() {
        let tabs = Tabs::new(Vec::<Tab>::new());
        let mut state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 20, 1), &mut frame, &mut state);
        // Should not panic; row should be blank
        let row = row_text(&frame, 0);
        assert_eq!(row.trim(), "");
    }

    #[test]
    fn tabs_render_single_tab() {
        let tabs = Tabs::new(vec![Tab::new("Solo")]);
        let mut state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 20, 1), &mut frame, &mut state);
        let row = row_text(&frame, 0);
        assert!(row.contains("[Solo]"));
    }

    #[test]
    fn tabs_render_empty_clears_stale_row() {
        let populated = Tabs::new(vec![Tab::new("LongTab"), Tab::new("Other")]);
        let empty = Tabs::new(Vec::<Tab>::new());
        let mut state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);

        StatefulWidget::render(&populated, Rect::new(0, 0, 20, 1), &mut frame, &mut state);
        assert_ne!(row_text(&frame, 0), " ".repeat(20));

        StatefulWidget::render(&empty, Rect::new(0, 0, 20, 1), &mut frame, &mut state);
        assert_eq!(row_text(&frame, 0), " ".repeat(20));
    }

    #[test]
    fn tabs_render_shorter_titles_clear_stale_suffix() {
        let long = Tabs::new(vec![Tab::new("LongTitle"), Tab::new("Second")]);
        let short = Tabs::new(vec![Tab::new("A"), Tab::new("B")]);
        let mut state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);

        StatefulWidget::render(&long, Rect::new(0, 0, 20, 1), &mut frame, &mut state);
        StatefulWidget::render(&short, Rect::new(0, 0, 20, 1), &mut frame, &mut state);

        assert_eq!(row_text(&frame, 0), "[A]  B              ");
    }

    #[test]
    fn tabs_no_styling_drops_configured_styles() {
        let tabs = Tabs::new(vec![Tab::new("One").style(Style::new().italic())])
            .style(Style::new().bold())
            .active_style(Style::new().underline());
        let plain_tabs = Tabs::new(vec![Tab::new("One")]);
        let mut state = TabsState::default();
        let mut plain_state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut plain_pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let mut plain_frame = Frame::new(10, 1, &mut plain_pool);
        frame.buffer.degradation = DegradationLevel::NoStyling;
        plain_frame.buffer.degradation = DegradationLevel::NoStyling;

        StatefulWidget::render(&tabs, Rect::new(0, 0, 10, 1), &mut frame, &mut state);
        StatefulWidget::render(
            &plain_tabs,
            Rect::new(0, 0, 10, 1),
            &mut plain_frame,
            &mut plain_state,
        );

        for x in 0..10 {
            let cell = frame.buffer.get(x, 0).expect("styled tab cell");
            let plain = plain_frame.buffer.get(x, 0).expect("plain tab cell");
            assert_eq!(cell, plain);
        }
    }

    #[test]
    fn tabs_render_zero_area() {
        let tabs = Tabs::new(vec![Tab::new("A"), Tab::new("B")]);
        let mut state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        // Zero width area
        StatefulWidget::render(&tabs, Rect::new(0, 0, 0, 1), &mut frame, &mut state);
        // Should not panic
    }

    #[test]
    fn tabs_render_closable_shows_marker() {
        let tabs = Tabs::new(vec![Tab::new("File").closable(true)]);
        let mut state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 20, 1), &mut frame, &mut state);
        let row = row_text(&frame, 0);
        assert!(row.contains("x"), "closable tab should show close marker");
    }

    #[test]
    fn tabs_render_active_tab_bracketed() {
        let tabs = Tabs::new(vec![Tab::new("A"), Tab::new("B"), Tab::new("C")]);
        let mut state = TabsState::default();
        state.select(1, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 30, 1), &mut frame, &mut state);
        let row = row_text(&frame, 0);
        // Active tab B should be bracketed, A and C should not
        assert!(row.contains("[B]"), "active tab should be bracketed");
        assert!(row.contains(" A "), "inactive tab A should be space-padded");
        assert!(row.contains(" C "), "inactive tab C should be space-padded");
    }

    #[test]
    fn tabs_no_overflow_when_all_fit() {
        let tabs = Tabs::new(vec![Tab::new("A"), Tab::new("B")]);
        let mut state = TabsState::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 30, 1), &mut frame, &mut state);
        let row = row_text(&frame, 0);
        // Should not contain overflow markers
        assert!(!row.starts_with('<'), "no left overflow marker expected");
        assert!(!row.ends_with('>'), "no right overflow marker expected");
    }

    // --- Tab struct tests ---

    #[test]
    fn tab_new_defaults() {
        let tab = Tab::new("test");
        assert_eq!(tab.title(), "test");
        assert!(!tab.is_closable());
    }

    #[test]
    fn tab_closable_builder() {
        let tab = Tab::new("temp").closable(true);
        assert!(tab.is_closable());
    }

    // --- Widget trait stateless render ---

    #[test]
    fn tabs_widget_stateless_render() {
        let tabs = Tabs::new(vec![Tab::new("X"), Tab::new("Y")]);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        Widget::render(&tabs, Rect::new(0, 0, 20, 1), &mut frame);
        let row = row_text(&frame, 0);
        // Default state: active=0, so X should be bracketed
        assert!(row.contains("[X]"));
    }

    // --- Overflow visible range tests ---

    #[test]
    fn tabs_overflow_both_sides() {
        let tabs = Tabs::new((0..10).map(|i| Tab::new(format!("Tab{i}"))));
        let mut state = TabsState::default();
        state.select(5, 10); // middle tab
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(15, 1, &mut pool);
        StatefulWidget::render(&tabs, Rect::new(0, 0, 15, 1), &mut frame, &mut state);
        let row = row_text(&frame, 0);
        // Should show both overflow markers
        assert!(row.starts_with('<'), "expected left overflow marker");
        assert!(
            row.trim_end().ends_with('>'),
            "expected right overflow marker"
        );
    }
}
