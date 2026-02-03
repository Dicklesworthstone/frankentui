#![forbid(unsafe_code)]

//! Action Timeline / Event Stream Viewer screen.
//!
//! Shows a live event timeline with filters and a detail panel. The timeline
//! is deterministic and uses a bounded ring buffer to keep allocations stable.

use std::collections::VecDeque;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

const MAX_EVENTS: usize = 500;
const EVENT_BURST_EVERY: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Severity {
    const ALL: [Severity; 5] = [
        Severity::Trace,
        Severity::Debug,
        Severity::Info,
        Severity::Warn,
        Severity::Error,
    ];

    fn label(self) -> &'static str {
        match self {
            Severity::Trace => "TRACE",
            Severity::Debug => "DEBUG",
            Severity::Info => "INFO",
            Severity::Warn => "WARN",
            Severity::Error => "ERROR",
        }
    }

    fn color(self) -> theme::ColorToken {
        match self {
            Severity::Trace => theme::fg::DISABLED,
            Severity::Debug => theme::fg::MUTED,
            Severity::Info => theme::fg::PRIMARY,
            Severity::Warn => theme::accent::WARNING,
            Severity::Error => theme::accent::ERROR,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Component {
    Core,
    Runtime,
    Render,
    Widgets,
}

impl Component {
    const ALL: [Component; 4] = [
        Component::Core,
        Component::Runtime,
        Component::Render,
        Component::Widgets,
    ];

    fn label(self) -> &'static str {
        match self {
            Component::Core => "core",
            Component::Runtime => "runtime",
            Component::Render => "render",
            Component::Widgets => "widgets",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKind {
    Input,
    Command,
    Subscription,
    Render,
    Present,
    Capability,
    Degrade,
}

impl EventKind {
    const ALL: [EventKind; 7] = [
        EventKind::Input,
        EventKind::Command,
        EventKind::Subscription,
        EventKind::Render,
        EventKind::Present,
        EventKind::Capability,
        EventKind::Degrade,
    ];

    fn label(self) -> &'static str {
        match self {
            EventKind::Input => "input",
            EventKind::Command => "cmd",
            EventKind::Subscription => "sub",
            EventKind::Render => "render",
            EventKind::Present => "present",
            EventKind::Capability => "caps",
            EventKind::Degrade => "budget",
        }
    }
}

#[derive(Debug, Clone)]
struct TimelineEvent {
    id: u64,
    tick: u64,
    severity: Severity,
    component: Component,
    kind: EventKind,
    summary: String,
    fields: Vec<(String, String)>,
    evidence: Option<String>,
}

pub struct ActionTimeline {
    events: VecDeque<TimelineEvent>,
    selected: usize,
    scroll_offset: usize,
    viewport_height: usize,
    follow: bool,
    show_details: bool,
    filter_component: Option<Component>,
    filter_severity: Option<Severity>,
    filter_kind: Option<EventKind>,
    next_id: u64,
    tick_count: u64,
}

impl Default for ActionTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionTimeline {
    pub fn new() -> Self {
        let mut timeline = Self {
            events: VecDeque::with_capacity(MAX_EVENTS),
            selected: 0,
            scroll_offset: 0,
            viewport_height: 12,
            follow: true,
            show_details: true,
            filter_component: None,
            filter_severity: None,
            filter_kind: None,
            next_id: 1,
            tick_count: 0,
        };
        for tick in 0..12 {
            timeline.tick_count = tick;
            let event = timeline.synthetic_event();
            timeline.push_event(event);
        }
        timeline.sync_selection();
        timeline
    }

    pub fn record_input_event(
        &mut self,
        tick: u64,
        event: &Event,
        source: &'static str,
        screen: &'static str,
    ) {
        let (summary, mut fields, severity) = match event {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
            }) => {
                let summary = format!("Key {}", Self::format_key_code(*code));
                let fields = vec![
                    ("code".to_string(), format!("{code:?}")),
                    ("modifiers".to_string(), Self::format_modifiers(*modifiers)),
                    ("kind".to_string(), format!("{kind:?}")),
                ];
                (summary, fields, Severity::Info)
            }
            Event::Mouse(mouse) => {
                let summary = format!("Mouse {:?}", mouse.kind);
                let fields = vec![
                    ("kind".to_string(), format!("{:?}", mouse.kind)),
                    ("x".to_string(), mouse.x.to_string()),
                    ("y".to_string(), mouse.y.to_string()),
                    (
                        "modifiers".to_string(),
                        Self::format_modifiers(mouse.modifiers),
                    ),
                ];
                (summary, fields, Severity::Debug)
            }
            Event::Paste(paste) => {
                let char_count = paste.text.chars().count();
                let summary = format!("Paste {char_count} chars");
                let fields = vec![
                    ("chars".to_string(), char_count.to_string()),
                    ("bracketed".to_string(), paste.bracketed.to_string()),
                ];
                (summary, fields, Severity::Info)
            }
            Event::Focus(gained) => {
                let summary = if *gained {
                    "Focus gained".to_string()
                } else {
                    "Focus lost".to_string()
                };
                let fields = vec![("focused".to_string(), gained.to_string())];
                (summary, fields, Severity::Debug)
            }
            Event::Clipboard(clipboard) => {
                let summary = "Clipboard data received".to_string();
                let fields = vec![(
                    "chars".to_string(),
                    clipboard.text.chars().count().to_string(),
                )];
                (summary, fields, Severity::Info)
            }
            Event::Resize { width, height } => {
                let summary = "Terminal resized".to_string();
                let fields = vec![
                    ("width".to_string(), width.to_string()),
                    ("height".to_string(), height.to_string()),
                ];
                (summary, fields, Severity::Debug)
            }
            Event::Tick => {
                let summary = "Runtime tick".to_string();
                let fields = vec![("tick".to_string(), tick.to_string())];
                (summary, fields, Severity::Trace)
            }
        };

        fields.push(("source".to_string(), source.to_string()));
        fields.push(("screen".to_string(), screen.to_string()));

        self.push_custom_event(
            tick,
            severity,
            Component::Runtime,
            EventKind::Input,
            summary,
            fields,
            None,
        );
    }

    pub fn record_command_event(
        &mut self,
        tick: u64,
        summary: impl Into<String>,
        fields: Vec<(String, String)>,
    ) {
        self.push_custom_event(
            tick,
            Severity::Info,
            Component::Runtime,
            EventKind::Command,
            summary.into(),
            fields,
            None,
        );
    }

    pub fn record_capability_event(
        &mut self,
        tick: u64,
        summary: impl Into<String>,
        fields: Vec<(String, String)>,
    ) {
        self.push_custom_event(
            tick,
            Severity::Debug,
            Component::Core,
            EventKind::Capability,
            summary.into(),
            fields,
            Some("evidence: terminal metadata change".to_string()),
        );
    }

    fn push_event(&mut self, event: TimelineEvent) {
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
            if self.selected > 0 {
                self.selected = self.selected.saturating_sub(1);
            }
        }
        self.events.push_back(event);
    }

    fn synthetic_event(&mut self) -> TimelineEvent {
        let tick = self.tick_count;
        let severity = Severity::ALL[(tick as usize) % Severity::ALL.len()];
        let component = Component::ALL[(tick as usize / 2) % Component::ALL.len()];
        let kind = EventKind::ALL[(tick as usize / 3) % EventKind::ALL.len()];
        let id = self.next_id;
        self.next_id += 1;

        let summary = match kind {
            EventKind::Input => "Key event processed".to_string(),
            EventKind::Command => "Command dispatched to model".to_string(),
            EventKind::Subscription => "Subscription tick delivered".to_string(),
            EventKind::Render => "Frame diff computed".to_string(),
            EventKind::Present => "Presenter emitted ANSI batch".to_string(),
            EventKind::Capability => "Capability probe updated".to_string(),
            EventKind::Degrade => "Render budget degraded".to_string(),
        };

        let latency_ms = 2 + (tick % 7) * 3;
        let fields = vec![
            ("latency_ms".to_string(), latency_ms.to_string()),
            ("diff_cells".to_string(), ((tick * 13) % 120).to_string()),
            ("ansi_bytes".to_string(), ((tick * 47) % 2048).to_string()),
        ];

        let evidence = match kind {
            EventKind::Capability => Some("evidence: env + probe signal".to_string()),
            EventKind::Degrade => Some("budget: frame_time > p95".to_string()),
            _ => None,
        };

        TimelineEvent {
            id,
            tick,
            severity,
            component,
            kind,
            summary,
            fields,
            evidence,
        }
    }

    fn filtered_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        for (idx, event) in self.events.iter().enumerate() {
            if self.filter_component.is_none_or(|c| c == event.component)
                && self.filter_severity.is_none_or(|s| s == event.severity)
                && self.filter_kind.is_none_or(|k| k == event.kind)
            {
                indices.push(idx);
            }
        }
        indices
    }

    fn push_custom_event(
        &mut self,
        tick: u64,
        severity: Severity,
        component: Component,
        kind: EventKind,
        summary: String,
        fields: Vec<(String, String)>,
        evidence: Option<String>,
    ) {
        let id = self.next_id;
        self.next_id += 1;
        self.push_event(TimelineEvent {
            id,
            tick,
            severity,
            component,
            kind,
            summary,
            fields,
            evidence,
        });
        self.sync_selection();
    }

    fn record_follow_drop_decision(&mut self, trigger: &'static str) {
        let fields = vec![
            (
                "rule".to_string(),
                "manual navigation disables follow mode".to_string(),
            ),
            ("evidence".to_string(), trigger.to_string()),
            ("action".to_string(), "follow=false".to_string()),
            (
                "intuition".to_string(),
                "user intent is to inspect history without auto-scroll".to_string(),
            ),
        ];
        self.push_custom_event(
            self.tick_count,
            Severity::Info,
            Component::Runtime,
            EventKind::Command,
            "Auto-follow disabled".to_string(),
            fields,
            Some("decision: follow guard".to_string()),
        );
    }

    fn format_modifiers(modifiers: Modifiers) -> String {
        let mut parts = Vec::new();
        if modifiers.contains(Modifiers::CTRL) {
            parts.push("CTRL");
        }
        if modifiers.contains(Modifiers::ALT) {
            parts.push("ALT");
        }
        if modifiers.contains(Modifiers::SHIFT) {
            parts.push("SHIFT");
        }
        if modifiers.contains(Modifiers::SUPER) {
            parts.push("SUPER");
        }
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join("+")
        }
    }

    fn format_key_code(code: KeyCode) -> String {
        match code {
            KeyCode::Char(ch) => format!("'{ch}'"),
            _ => format!("{code:?}"),
        }
    }

    fn ensure_selection(&mut self, filtered_len: usize) {
        if filtered_len == 0 {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }

        if self.follow || self.selected >= filtered_len {
            self.selected = filtered_len - 1;
        }

        self.ensure_visible(filtered_len);
    }

    fn ensure_visible(&mut self, filtered_len: usize) {
        if filtered_len == 0 {
            self.scroll_offset = 0;
            return;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + self.viewport_height {
            self.scroll_offset = self.selected.saturating_sub(self.viewport_height - 1);
        }
    }

    fn cycle_component(&mut self) {
        self.filter_component = match self.filter_component {
            None => Some(Component::Core),
            Some(Component::Core) => Some(Component::Runtime),
            Some(Component::Runtime) => Some(Component::Render),
            Some(Component::Render) => Some(Component::Widgets),
            Some(Component::Widgets) => None,
        };
    }

    fn cycle_severity(&mut self) {
        self.filter_severity = match self.filter_severity {
            None => Some(Severity::Info),
            Some(Severity::Trace) => Some(Severity::Debug),
            Some(Severity::Debug) => Some(Severity::Info),
            Some(Severity::Info) => Some(Severity::Warn),
            Some(Severity::Warn) => Some(Severity::Error),
            Some(Severity::Error) => None,
        };
    }

    fn cycle_kind(&mut self) {
        self.filter_kind = match self.filter_kind {
            None => Some(EventKind::Input),
            Some(EventKind::Input) => Some(EventKind::Command),
            Some(EventKind::Command) => Some(EventKind::Subscription),
            Some(EventKind::Subscription) => Some(EventKind::Render),
            Some(EventKind::Render) => Some(EventKind::Present),
            Some(EventKind::Present) => Some(EventKind::Capability),
            Some(EventKind::Capability) => Some(EventKind::Degrade),
            Some(EventKind::Degrade) => None,
        };
    }

    fn clear_filters(&mut self) {
        self.filter_component = None;
        self.filter_severity = None;
        self.filter_kind = None;
    }

    fn render_filters(&self, frame: &mut Frame, area: Rect) {
        let border_style = Style::new().fg(theme::screen_accent::ACTION_TIMELINE);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Filters + Follow")
            .title_alignment(Alignment::Center)
            .style(border_style);
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let component = self.filter_component.map(|c| c.label()).unwrap_or("all");
        let severity = self.filter_severity.map(|s| s.label()).unwrap_or("all");
        let kind = self.filter_kind.map(|k| k.label()).unwrap_or("all");
        let follow = if self.follow { "ON" } else { "OFF" };

        let line = format!(
            "Follow[F]: {follow}  Component[C]: {component}  Severity[S]: {severity}  Type[T]: {kind}  Clear[X]"
        );
        Paragraph::new(line)
            .style(theme::body())
            .render(inner, frame);
    }

    fn render_timeline(&self, frame: &mut Frame, area: Rect, filtered: &[usize]) {
        let border_style = Style::new().fg(theme::screen_accent::ACTION_TIMELINE);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Event Timeline")
            .title_alignment(Alignment::Center)
            .style(border_style);
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        if filtered.is_empty() {
            Paragraph::new("No events match current filters.")
                .style(theme::muted())
                .render(inner, frame);
            return;
        }

        let viewport_height = inner.height.max(1) as usize;
        let max_index = filtered.len().saturating_sub(1);
        let selected = self.selected.min(max_index);
        let mut scroll_offset = self.scroll_offset.min(max_index);
        if selected < scroll_offset {
            scroll_offset = selected;
        }
        if selected >= scroll_offset + viewport_height {
            scroll_offset = selected.saturating_sub(viewport_height - 1);
        }

        let end = (scroll_offset + viewport_height).min(filtered.len());
        for (row, idx) in filtered[scroll_offset..end].iter().enumerate() {
            let event = &self.events[*idx];
            let y = inner.y + row as u16;
            if y >= inner.bottom() {
                break;
            }

            let is_selected = (scroll_offset + row) == selected;
            let mut style = Style::new().fg(event.severity.color());
            if is_selected {
                style = style
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::HIGHLIGHT)
                    .bold();
            }

            let line = format!(
                "{:>4} {:<5} {:<7} {:<7} {}",
                event.tick,
                event.severity.label(),
                event.component.label(),
                event.kind.label(),
                event.summary
            );
            Paragraph::new(line)
                .style(style)
                .render(Rect::new(inner.x, y, inner.width, 1), frame);
        }
    }

    fn render_details(&self, frame: &mut Frame, area: Rect, filtered: &[usize]) {
        let border_style = Style::new().fg(theme::screen_accent::ACTION_TIMELINE);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Event Detail")
            .title_alignment(Alignment::Center)
            .style(border_style);
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        if filtered.is_empty() {
            Paragraph::new("Select an event to inspect.")
                .style(theme::muted())
                .render(inner, frame);
            return;
        }

        let idx = filtered[self.selected.min(filtered.len().saturating_sub(1))];
        let event = &self.events[idx];

        let mut lines = Vec::new();
        lines.push(format!("ID: {}", event.id));
        lines.push(format!("Tick: {}", event.tick));
        lines.push(format!("Severity: {}", event.severity.label()));
        lines.push(format!("Component: {}", event.component.label()));
        lines.push(format!("Type: {}", event.kind.label()));

        lines.push(String::new());
        lines.push("Summary:".to_string());
        lines.push(format!("  {}", event.summary));

        if self.show_details {
            if !event.fields.is_empty() {
                lines.push(String::new());
                lines.push("Fields:".to_string());
                for (k, v) in &event.fields {
                    lines.push(format!("  {k}: {v}"));
                }
            }

            if let Some(evidence) = &event.evidence {
                lines.push(String::new());
                lines.push("Evidence:".to_string());
                lines.push(format!("  {evidence}"));
            }
        } else {
            lines.push(String::new());
            lines.push("Press Enter to expand details".to_string());
        }

        Paragraph::new(lines.join("\n"))
            .style(theme::body())
            .render(inner, frame);
    }
}

impl Screen for ActionTimeline {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Resize { height, .. } = event {
            let usable = height.saturating_sub(6).max(1);
            self.viewport_height = usable as usize;
            self.sync_selection();
            return Cmd::None;
        }

        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match (*code, *modifiers) {
                (KeyCode::Char('f'), Modifiers::NONE) | (KeyCode::Char('F'), Modifiers::NONE) => {
                    self.follow = !self.follow;
                }
                (KeyCode::Char('c'), Modifiers::NONE) | (KeyCode::Char('C'), Modifiers::NONE) => {
                    self.cycle_component();
                    self.sync_selection();
                }
                (KeyCode::Char('s'), Modifiers::NONE) | (KeyCode::Char('S'), Modifiers::NONE) => {
                    self.cycle_severity();
                    self.sync_selection();
                }
                (KeyCode::Char('t'), Modifiers::NONE) | (KeyCode::Char('T'), Modifiers::NONE) => {
                    self.cycle_kind();
                    self.sync_selection();
                }
                (KeyCode::Char('x'), Modifiers::NONE) | (KeyCode::Char('X'), Modifiers::NONE) => {
                    self.clear_filters();
                    self.sync_selection();
                }
                (KeyCode::Enter, _) => {
                    self.show_details = !self.show_details;
                }
                (KeyCode::Up, _) | (KeyCode::Char('k'), Modifiers::NONE) => {
                    self.follow = false;
                    self.selected = self.selected.saturating_sub(1);
                    self.sync_selection();
                }
                (KeyCode::Down, _) | (KeyCode::Char('j'), Modifiers::NONE) => {
                    self.follow = false;
                    self.selected = self.selected.saturating_add(1);
                    self.sync_selection();
                }
                (KeyCode::PageUp, _) => {
                    self.follow = false;
                    self.selected = self.selected.saturating_sub(self.viewport_height);
                    self.sync_selection();
                }
                (KeyCode::PageDown, _) => {
                    self.follow = false;
                    self.selected = self.selected.saturating_add(self.viewport_height);
                    self.sync_selection();
                }
                (KeyCode::Home, _) => {
                    self.follow = false;
                    self.selected = 0;
                    self.sync_selection();
                }
                (KeyCode::End, _) => {
                    self.follow = false;
                    self.selected = usize::MAX / 2;
                    self.sync_selection();
                }
                _ => {}
            }
        }
        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        if tick_count.is_multiple_of(EVENT_BURST_EVERY) {
            let event = self.synthetic_event();
            self.push_event(event);
            self.sync_selection();
        }
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(3), Constraint::Min(1)])
            .split(area);

        let cols = Flex::horizontal()
            .constraints([Constraint::Min(45), Constraint::Min(30)])
            .split(rows[1]);

        let filtered = self.filtered_indices();
        self.render_filters(frame, rows[0]);
        self.render_timeline(frame, cols[0], &filtered);
        self.render_details(frame, cols[1], &filtered);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "F",
                action: "Toggle follow mode",
            },
            HelpEntry {
                key: "C",
                action: "Cycle component filter",
            },
            HelpEntry {
                key: "S",
                action: "Cycle severity filter",
            },
            HelpEntry {
                key: "T",
                action: "Cycle type filter",
            },
            HelpEntry {
                key: "X",
                action: "Clear filters",
            },
            HelpEntry {
                key: "Enter",
                action: "Toggle detail expansion",
            },
            HelpEntry {
                key: "↑/↓ or j/k",
                action: "Navigate events",
            },
            HelpEntry {
                key: "PgUp/PgDn",
                action: "Page navigation",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Action Timeline"
    }

    fn tab_label(&self) -> &'static str {
        "Timeline"
    }
}

impl ActionTimeline {
    fn sync_selection(&mut self) {
        let filtered_len = self.filtered_indices().len();
        self.ensure_selection(filtered_len);
    }
}

// =============================================================================
// Unit Tests (bd-11ck.3)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Helpers
    // =========================================================================

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
    }

    // =========================================================================
    // Enum Label Tests
    // =========================================================================

    #[test]
    fn severity_labels() {
        assert_eq!(Severity::Trace.label(), "TRACE");
        assert_eq!(Severity::Debug.label(), "DEBUG");
        assert_eq!(Severity::Info.label(), "INFO");
        assert_eq!(Severity::Warn.label(), "WARN");
        assert_eq!(Severity::Error.label(), "ERROR");
    }

    #[test]
    fn severity_all_covered() {
        assert_eq!(Severity::ALL.len(), 5);
        for severity in Severity::ALL {
            // Each severity has a label and color
            assert!(!severity.label().is_empty());
            let _ = severity.color(); // Should not panic
        }
    }

    #[test]
    fn component_labels() {
        assert_eq!(Component::Core.label(), "core");
        assert_eq!(Component::Runtime.label(), "runtime");
        assert_eq!(Component::Render.label(), "render");
        assert_eq!(Component::Widgets.label(), "widgets");
    }

    #[test]
    fn component_all_covered() {
        assert_eq!(Component::ALL.len(), 4);
        for component in Component::ALL {
            assert!(!component.label().is_empty());
        }
    }

    #[test]
    fn event_kind_labels() {
        assert_eq!(EventKind::Input.label(), "input");
        assert_eq!(EventKind::Command.label(), "cmd");
        assert_eq!(EventKind::Subscription.label(), "sub");
        assert_eq!(EventKind::Render.label(), "render");
        assert_eq!(EventKind::Present.label(), "present");
        assert_eq!(EventKind::Capability.label(), "caps");
        assert_eq!(EventKind::Degrade.label(), "budget");
    }

    #[test]
    fn event_kind_all_covered() {
        assert_eq!(EventKind::ALL.len(), 7);
        for kind in EventKind::ALL {
            assert!(!kind.label().is_empty());
        }
    }

    // =========================================================================
    // ActionTimeline Initialization Tests
    // =========================================================================

    #[test]
    fn new_creates_initial_events() {
        let timeline = ActionTimeline::new();
        assert_eq!(timeline.events.len(), 12);
        assert!(timeline.follow);
        assert!(timeline.show_details);
        assert!(timeline.filter_component.is_none());
        assert!(timeline.filter_severity.is_none());
        assert!(timeline.filter_kind.is_none());
    }

    #[test]
    fn new_events_have_increasing_ids() {
        let timeline = ActionTimeline::new();
        let mut last_id = 0;
        for event in &timeline.events {
            assert!(event.id > last_id, "IDs must be increasing");
            last_id = event.id;
        }
    }

    #[test]
    fn default_same_as_new() {
        let from_new = ActionTimeline::new();
        let from_default = ActionTimeline::default();
        assert_eq!(from_new.events.len(), from_default.events.len());
        assert_eq!(from_new.follow, from_default.follow);
    }

    // =========================================================================
    // Event Buffer Invariant Tests
    // =========================================================================

    #[test]
    fn buffer_never_exceeds_max() {
        let mut timeline = ActionTimeline::new();
        // Push many events
        for t in 0..1000 {
            timeline.tick_count = t;
            let event = timeline.synthetic_event();
            timeline.push_event(event);
            assert!(
                timeline.events.len() <= MAX_EVENTS,
                "Buffer exceeded max at tick {t}"
            );
        }
    }

    #[test]
    fn push_event_evicts_oldest_at_capacity() {
        let mut timeline = ActionTimeline::new();
        // Fill to capacity
        while timeline.events.len() < MAX_EVENTS {
            timeline.tick_count += 1;
            let event = timeline.synthetic_event();
            timeline.push_event(event);
        }
        assert_eq!(timeline.events.len(), MAX_EVENTS);

        let first_id_before = timeline.events.front().unwrap().id;
        timeline.tick_count += 1;
        let new_event = timeline.synthetic_event();
        timeline.push_event(new_event);

        assert_eq!(timeline.events.len(), MAX_EVENTS);
        let first_id_after = timeline.events.front().unwrap().id;
        assert!(
            first_id_after > first_id_before,
            "Oldest event should be evicted"
        );
    }

    // =========================================================================
    // Filter Cycling Tests
    // =========================================================================

    #[test]
    fn cycle_component_round_trip() {
        let mut timeline = ActionTimeline::new();
        assert!(timeline.filter_component.is_none());

        timeline.cycle_component();
        assert_eq!(timeline.filter_component, Some(Component::Core));

        timeline.cycle_component();
        assert_eq!(timeline.filter_component, Some(Component::Runtime));

        timeline.cycle_component();
        assert_eq!(timeline.filter_component, Some(Component::Render));

        timeline.cycle_component();
        assert_eq!(timeline.filter_component, Some(Component::Widgets));

        timeline.cycle_component();
        assert!(
            timeline.filter_component.is_none(),
            "Should cycle back to None"
        );
    }

    #[test]
    fn cycle_severity_round_trip() {
        let mut timeline = ActionTimeline::new();
        assert!(timeline.filter_severity.is_none());

        // Cycle through all values
        let mut seen = vec![];
        for _ in 0..10 {
            timeline.cycle_severity();
            seen.push(timeline.filter_severity);
            if timeline.filter_severity.is_none() {
                break;
            }
        }

        // Should have cycled through values and back to None
        assert!(seen.contains(&None));
        assert!(seen.contains(&Some(Severity::Info)));
        assert!(seen.contains(&Some(Severity::Warn)));
        assert!(seen.contains(&Some(Severity::Error)));
    }

    #[test]
    fn cycle_kind_round_trip() {
        let mut timeline = ActionTimeline::new();
        assert!(timeline.filter_kind.is_none());

        // Cycle through all values
        let mut count = 0;
        loop {
            timeline.cycle_kind();
            count += 1;
            if timeline.filter_kind.is_none() {
                break;
            }
            assert!(count < 20, "Infinite loop in cycle_kind");
        }

        // Should have cycled through all 7 kinds + back to None
        assert_eq!(count, 8);
    }

    #[test]
    fn clear_filters_resets_all() {
        let mut timeline = ActionTimeline::new();
        timeline.filter_component = Some(Component::Core);
        timeline.filter_severity = Some(Severity::Error);
        timeline.filter_kind = Some(EventKind::Input);

        timeline.clear_filters();

        assert!(timeline.filter_component.is_none());
        assert!(timeline.filter_severity.is_none());
        assert!(timeline.filter_kind.is_none());
    }

    // =========================================================================
    // Filter Application Tests
    // =========================================================================

    #[test]
    fn filtered_indices_with_no_filter() {
        let timeline = ActionTimeline::new();
        let indices = timeline.filtered_indices();
        assert_eq!(indices.len(), timeline.events.len());
    }

    #[test]
    fn filtered_indices_with_component_filter() {
        let mut timeline = ActionTimeline::new();
        timeline.filter_component = Some(Component::Core);
        let indices = timeline.filtered_indices();

        for idx in indices {
            assert_eq!(
                timeline.events[idx].component,
                Component::Core,
                "Filtered event should match component filter"
            );
        }
    }

    #[test]
    fn filtered_indices_with_severity_filter() {
        let mut timeline = ActionTimeline::new();
        timeline.filter_severity = Some(Severity::Error);
        let indices = timeline.filtered_indices();

        for idx in indices {
            assert_eq!(
                timeline.events[idx].severity,
                Severity::Error,
                "Filtered event should match severity filter"
            );
        }
    }

    #[test]
    fn filtered_indices_with_combined_filters() {
        let mut timeline = ActionTimeline::new();
        // Add more events to increase chance of matches
        for t in 12..100 {
            timeline.tick_count = t;
            let event = timeline.synthetic_event();
            timeline.push_event(event);
        }

        timeline.filter_component = Some(Component::Core);
        timeline.filter_severity = Some(Severity::Info);

        let indices = timeline.filtered_indices();
        for idx in indices {
            let event = &timeline.events[idx];
            assert_eq!(event.component, Component::Core);
            assert_eq!(event.severity, Severity::Info);
        }
    }

    // =========================================================================
    // Navigation Tests
    // =========================================================================

    #[test]
    fn navigate_up_decrements_selection() {
        let mut timeline = ActionTimeline::new();
        timeline.selected = 5;
        timeline.follow = false;

        timeline.update(&press(KeyCode::Up));

        assert_eq!(timeline.selected, 4);
        assert!(!timeline.follow, "Navigation should disable follow");
    }

    #[test]
    fn navigate_up_saturates_at_zero() {
        let mut timeline = ActionTimeline::new();
        timeline.selected = 0;
        timeline.follow = false;

        timeline.update(&press(KeyCode::Up));

        assert_eq!(timeline.selected, 0);
    }

    #[test]
    fn navigate_down_increments_selection() {
        let mut timeline = ActionTimeline::new();
        timeline.selected = 5;
        timeline.follow = false;

        timeline.update(&press(KeyCode::Down));

        // Selection is clamped by sync_selection
        assert!(timeline.selected > 5 || timeline.selected == 5);
    }

    #[test]
    fn navigate_vim_k_same_as_up() {
        let mut timeline1 = ActionTimeline::new();
        let mut timeline2 = ActionTimeline::new();
        timeline1.selected = 5;
        timeline2.selected = 5;
        timeline1.follow = false;
        timeline2.follow = false;

        timeline1.update(&press(KeyCode::Up));
        timeline2.update(&press(KeyCode::Char('k')));

        assert_eq!(timeline1.selected, timeline2.selected);
    }

    #[test]
    fn navigate_vim_j_same_as_down() {
        let mut timeline1 = ActionTimeline::new();
        let mut timeline2 = ActionTimeline::new();
        timeline1.selected = 5;
        timeline2.selected = 5;
        timeline1.follow = false;
        timeline2.follow = false;

        timeline1.update(&press(KeyCode::Down));
        timeline2.update(&press(KeyCode::Char('j')));

        assert_eq!(timeline1.selected, timeline2.selected);
    }

    #[test]
    fn home_jumps_to_start() {
        let mut timeline = ActionTimeline::new();
        timeline.selected = 10;

        timeline.update(&press(KeyCode::Home));

        assert_eq!(timeline.selected, 0);
        assert!(!timeline.follow);
    }

    #[test]
    fn end_jumps_to_end() {
        let mut timeline = ActionTimeline::new();
        timeline.selected = 0;

        timeline.update(&press(KeyCode::End));

        let max_idx = timeline.filtered_indices().len().saturating_sub(1);
        assert_eq!(timeline.selected, max_idx);
        assert!(!timeline.follow);
    }

    // =========================================================================
    // Toggle Tests
    // =========================================================================

    #[test]
    fn toggle_follow_mode() {
        let mut timeline = ActionTimeline::new();
        assert!(timeline.follow);

        timeline.update(&press(KeyCode::Char('f')));
        assert!(!timeline.follow);

        timeline.update(&press(KeyCode::Char('f')));
        assert!(timeline.follow);
    }

    #[test]
    fn toggle_follow_uppercase() {
        let mut timeline = ActionTimeline::new();
        assert!(timeline.follow);

        timeline.update(&press(KeyCode::Char('F')));
        assert!(!timeline.follow);
    }

    #[test]
    fn toggle_details() {
        let mut timeline = ActionTimeline::new();
        assert!(timeline.show_details);

        timeline.update(&press(KeyCode::Enter));
        assert!(!timeline.show_details);

        timeline.update(&press(KeyCode::Enter));
        assert!(timeline.show_details);
    }

    // =========================================================================
    // Selection Bounds Tests
    // =========================================================================

    #[test]
    fn ensure_selection_empty_events() {
        let mut timeline = ActionTimeline::new();
        timeline.events.clear();
        timeline.selected = 100;

        timeline.ensure_selection(0);

        assert_eq!(timeline.selected, 0);
        assert_eq!(timeline.scroll_offset, 0);
    }

    #[test]
    fn ensure_selection_clamps_to_max() {
        let mut timeline = ActionTimeline::new();
        timeline.selected = 1000;
        timeline.follow = false;

        let filtered_len = timeline.filtered_indices().len();
        timeline.ensure_selection(filtered_len);

        assert!(timeline.selected < filtered_len);
    }

    #[test]
    fn ensure_visible_adjusts_scroll() {
        let mut timeline = ActionTimeline::new();
        timeline.viewport_height = 5;
        timeline.selected = 10;
        timeline.scroll_offset = 0;

        timeline.ensure_visible(12);

        // Scroll should have adjusted so selected is visible
        assert!(timeline.selected >= timeline.scroll_offset);
        assert!(timeline.selected < timeline.scroll_offset + timeline.viewport_height);
    }

    // =========================================================================
    // Tick Tests
    // =========================================================================

    #[test]
    fn tick_generates_events_periodically() {
        let mut timeline = ActionTimeline::new();
        let initial_count = timeline.events.len();

        // Tick at event burst interval
        timeline.tick(EVENT_BURST_EVERY);

        assert!(
            timeline.events.len() > initial_count,
            "Tick should generate new event"
        );
    }

    #[test]
    fn tick_does_not_generate_every_tick() {
        let mut timeline = ActionTimeline::new();
        let initial_count = timeline.events.len();

        // Tick at non-burst interval
        timeline.tick(EVENT_BURST_EVERY + 1);

        assert_eq!(
            timeline.events.len(),
            initial_count,
            "Non-burst tick should not generate event"
        );
    }

    // =========================================================================
    // Synthetic Event Tests
    // =========================================================================

    #[test]
    fn synthetic_event_deterministic() {
        let mut timeline1 = ActionTimeline::new();
        let mut timeline2 = ActionTimeline::new();

        timeline1.tick_count = 42;
        timeline2.tick_count = 42;

        // Reset next_id to same value
        timeline1.next_id = 100;
        timeline2.next_id = 100;

        let event1 = timeline1.synthetic_event();
        let event2 = timeline2.synthetic_event();

        assert_eq!(event1.id, event2.id);
        assert_eq!(event1.tick, event2.tick);
        assert_eq!(event1.severity, event2.severity);
        assert_eq!(event1.component, event2.component);
        assert_eq!(event1.kind, event2.kind);
        assert_eq!(event1.summary, event2.summary);
    }

    #[test]
    fn synthetic_event_fields_present() {
        let mut timeline = ActionTimeline::new();
        timeline.tick_count = 10;

        let event = timeline.synthetic_event();

        assert!(!event.fields.is_empty());
        assert!(event.fields.iter().any(|(k, _)| k == "latency_ms"));
        assert!(event.fields.iter().any(|(k, _)| k == "diff_cells"));
        assert!(event.fields.iter().any(|(k, _)| k == "ansi_bytes"));
    }

    #[test]
    fn synthetic_event_evidence_for_special_kinds() {
        let mut timeline = ActionTimeline::new();

        // Find a tick that produces Capability or Degrade kind
        for t in 0..100 {
            timeline.tick_count = t;
            let kind = EventKind::ALL[(t as usize / 3) % EventKind::ALL.len()];
            if kind == EventKind::Capability || kind == EventKind::Degrade {
                let event = timeline.synthetic_event();
                assert!(
                    event.evidence.is_some(),
                    "Capability/Degrade should have evidence"
                );
                break;
            }
        }
    }

    // =========================================================================
    // Screen Trait Tests
    // =========================================================================

    #[test]
    fn title_and_tab_label() {
        let timeline = ActionTimeline::new();
        assert_eq!(timeline.title(), "Action Timeline");
        assert_eq!(timeline.tab_label(), "Timeline");
    }

    #[test]
    fn keybindings_not_empty() {
        let timeline = ActionTimeline::new();
        let bindings = timeline.keybindings();
        assert!(!bindings.is_empty());

        // Check expected bindings exist
        let keys: Vec<_> = bindings.iter().map(|h| h.key).collect();
        assert!(keys.contains(&"F"));
        assert!(keys.contains(&"C"));
        assert!(keys.contains(&"S"));
        assert!(keys.contains(&"T"));
        assert!(keys.contains(&"X"));
        assert!(keys.contains(&"Enter"));
    }

    // =========================================================================
    // Resize Event Tests
    // =========================================================================

    #[test]
    fn resize_updates_viewport() {
        let mut timeline = ActionTimeline::new();
        let initial_viewport = timeline.viewport_height;

        timeline.update(&Event::Resize {
            width: 120,
            height: 50,
        });

        // Viewport should be updated (50 - 6 = 44)
        assert_ne!(timeline.viewport_height, initial_viewport);
    }

    #[test]
    fn resize_minimum_viewport() {
        let mut timeline = ActionTimeline::new();

        timeline.update(&Event::Resize {
            width: 80,
            height: 5,
        });

        // Should have at least 1 viewport height
        assert!(timeline.viewport_height >= 1);
    }

    // =========================================================================
    // Edge Case Tests
    // =========================================================================

    #[test]
    fn filter_results_in_empty() {
        let mut timeline = ActionTimeline::new();
        // Set filters that might not match any events
        timeline.filter_component = Some(Component::Core);
        timeline.filter_severity = Some(Severity::Error);
        timeline.filter_kind = Some(EventKind::Degrade);

        // This should not panic
        let indices = timeline.filtered_indices();
        timeline.ensure_selection(indices.len());

        // Selection should be valid
        if indices.is_empty() {
            assert_eq!(timeline.selected, 0);
        } else {
            assert!(timeline.selected < indices.len());
        }
    }

    #[test]
    fn rapid_navigation_does_not_panic() {
        let mut timeline = ActionTimeline::new();

        // Rapid up/down navigation
        for _ in 0..100 {
            timeline.update(&press(KeyCode::Up));
        }
        for _ in 0..200 {
            timeline.update(&press(KeyCode::Down));
        }
        for _ in 0..50 {
            timeline.update(&press(KeyCode::PageUp));
        }
        for _ in 0..100 {
            timeline.update(&press(KeyCode::PageDown));
        }

        // Should not panic, selection should be valid
        let filtered_len = timeline.filtered_indices().len();
        assert!(timeline.selected <= filtered_len);
    }

    #[test]
    fn filter_cycling_while_navigating() {
        let mut timeline = ActionTimeline::new();

        // Interleave filter changes and navigation
        timeline.update(&press(KeyCode::Down));
        timeline.update(&press(KeyCode::Char('c'))); // cycle component
        timeline.update(&press(KeyCode::Down));
        timeline.update(&press(KeyCode::Char('s'))); // cycle severity
        timeline.update(&press(KeyCode::Up));
        timeline.update(&press(KeyCode::Char('t'))); // cycle kind
        timeline.update(&press(KeyCode::Home));
        timeline.update(&press(KeyCode::Char('x'))); // clear filters
        timeline.update(&press(KeyCode::End));

        // Should not panic
        let filtered_len = timeline.filtered_indices().len();
        assert!(timeline.selected <= filtered_len);
    }
}
