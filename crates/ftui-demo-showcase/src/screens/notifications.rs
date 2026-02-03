#![forbid(unsafe_code)]

//! Notifications screen — demonstrates the toast notification system.
//!
//! Shows interactive toasts with various styles, positions, priorities,
//! and action buttons. Demonstrates the full notification queue lifecycle:
//! push, display, auto-dismiss, manual dismiss, and action invocation.

use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::notification_queue::{
    NotificationPriority, NotificationQueue, NotificationStack, QueueConfig,
};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::toast::{Toast, ToastAction, ToastIcon, ToastPosition, ToastStyle};

use super::{HelpEntry, Screen};
use crate::theme;

/// Notification demo screen state.
pub struct Notifications {
    /// The notification queue managing visible and pending toasts.
    queue: NotificationQueue,
    /// Global tick counter from the app.
    tick_count: u64,
    /// Counter for generating unique toast content.
    toast_counter: u64,
    /// Last action ID invoked (displayed in the info panel).
    last_action: Option<String>,
}

impl Default for Notifications {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifications {
    /// Create a new notifications screen with a default queue.
    pub fn new() -> Self {
        Self {
            queue: NotificationQueue::new(
                QueueConfig::new()
                    .max_visible(4)
                    .max_queued(20)
                    .position(ToastPosition::TopRight),
            ),
            tick_count: 0,
            toast_counter: 0,
            last_action: None,
        }
    }

    /// Push a success toast.
    fn push_success(&mut self) {
        self.toast_counter += 1;
        let toast = Toast::new(format!("Operation #{} completed", self.toast_counter))
            .icon(ToastIcon::Success)
            .style_variant(ToastStyle::Success)
            .duration(Duration::from_secs(5));
        self.queue.push(toast, NotificationPriority::Normal);
    }

    /// Push an error toast with an action button.
    fn push_error(&mut self) {
        self.toast_counter += 1;
        let toast = Toast::new(format!("Error #{}: Connection failed", self.toast_counter))
            .icon(ToastIcon::Error)
            .title("Error")
            .style_variant(ToastStyle::Error)
            .duration(Duration::from_secs(8))
            .action(ToastAction::new("Retry", "retry"));
        self.queue.push(toast, NotificationPriority::High);
    }

    /// Push a warning toast.
    fn push_warning(&mut self) {
        self.toast_counter += 1;
        let toast = Toast::new(format!("Warning #{}: Low disk space", self.toast_counter))
            .icon(ToastIcon::Warning)
            .style_variant(ToastStyle::Warning)
            .duration(Duration::from_secs(6));
        self.queue.push(toast, NotificationPriority::Normal);
    }

    /// Push an info toast.
    fn push_info(&mut self) {
        self.toast_counter += 1;
        let toast = Toast::new(format!("Info #{}: Update available", self.toast_counter))
            .icon(ToastIcon::Info)
            .style_variant(ToastStyle::Info)
            .duration(Duration::from_secs(4));
        self.queue.push(toast, NotificationPriority::Low);
    }

    /// Push an urgent toast with multiple actions.
    fn push_urgent(&mut self) {
        self.toast_counter += 1;
        let toast = Toast::new(format!("Critical #{}: Action required", self.toast_counter))
            .icon(ToastIcon::Error)
            .title("Critical Alert")
            .style_variant(ToastStyle::Error)
            .persistent()
            .action(ToastAction::new("Acknowledge", "ack"))
            .action(ToastAction::new("Snooze", "snooze"));
        self.queue.push(toast, NotificationPriority::Urgent);
    }

    /// Render the instructions panel.
    fn render_instructions(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Notification Demo")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::DEEP));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let lines = [
            "Press keys to trigger notifications:",
            "",
            "  s  Success notification",
            "  e  Error with Retry action",
            "  w  Warning notification",
            "  i  Info notification",
            "  u  Urgent with Ack/Snooze actions",
            "  d  Dismiss all notifications",
            "",
            &format!(
                "Queue: {} visible, {} pending",
                self.queue.visible().len(),
                self.queue.pending_count(),
            ),
            &format!("Total shown: {}", self.queue.stats().total_pushed,),
            &format!(
                "Last action: {}",
                self.last_action.as_deref().unwrap_or("(none)"),
            ),
        ];

        for (i, line) in lines.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }
            let row_area = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
            let style = if line.starts_with("  ") && line.len() > 3 {
                Style::new().fg(theme::accent::INFO)
            } else {
                Style::new().fg(theme::fg::MUTED)
            };
            Paragraph::new(*line).style(style).render(row_area, frame);
        }
    }
}

impl Screen for Notifications {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                KeyCode::Char('s') => self.push_success(),
                KeyCode::Char('e') => self.push_error(),
                KeyCode::Char('w') => self.push_warning(),
                KeyCode::Char('i') => self.push_info(),
                KeyCode::Char('u') => self.push_urgent(),
                KeyCode::Char('d') => self.queue.dismiss_all(),
                _ => {}
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Split: left panel for instructions, full area for notification overlay
        let chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(40.0), Constraint::Min(1)])
            .split(area);

        self.render_instructions(frame, chunks[0]);

        // Render the notification stack overlay on the right portion
        NotificationStack::new(&self.queue)
            .margin(theme::spacing::INLINE)
            .render(chunks[1], frame);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "s",
                action: "Success toast",
            },
            HelpEntry {
                key: "e",
                action: "Error toast (with Retry)",
            },
            HelpEntry {
                key: "w",
                action: "Warning toast",
            },
            HelpEntry {
                key: "i",
                action: "Info toast",
            },
            HelpEntry {
                key: "u",
                action: "Urgent toast (with actions)",
            },
            HelpEntry {
                key: "d",
                action: "Dismiss all",
            },
        ]
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        // Process queue expiry and promotion at ~10Hz (100ms tick rate)
        let actions = self.queue.tick(Duration::from_millis(100));
        // Check for any queue-level actions (currently just Show/Hide)
        let _ = actions;
    }

    fn title(&self) -> &'static str {
        "Notifications"
    }

    fn tab_label(&self) -> &'static str {
        "Notify"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn notifications_screen_default() {
        let screen = Notifications::new();
        assert_eq!(screen.tick_count, 0);
        assert_eq!(screen.toast_counter, 0);
        assert!(screen.last_action.is_none());
        assert!(screen.queue.visible().is_empty());
    }

    #[test]
    fn push_success_increments_counter() {
        let mut screen = Notifications::new();
        screen.push_success();
        assert_eq!(screen.toast_counter, 1);
        assert_eq!(screen.queue.stats().total_pushed, 1);
    }

    #[test]
    fn push_all_types() {
        let mut screen = Notifications::new();
        screen.push_success();
        screen.push_error();
        screen.push_warning();
        screen.push_info();
        screen.push_urgent();
        assert_eq!(screen.toast_counter, 5);
        assert_eq!(screen.queue.stats().total_pushed, 5);
        // Items start in the pending queue and are promoted to visible during tick
        screen.tick(1);
        assert_eq!(screen.queue.visible().len(), 4);
        assert_eq!(screen.queue.pending_count(), 1);
    }

    #[test]
    fn dismiss_all_after_tick() {
        let mut screen = Notifications::new();
        screen.push_success();
        screen.push_info();
        screen.tick(1); // promote to visible
        assert!(!screen.queue.visible().is_empty());

        screen.queue.dismiss_all();
        // After dismiss, visible toasts start exit animation
    }

    #[test]
    fn key_s_triggers_success() {
        use super::Screen;
        let mut screen = Notifications::new();
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('s'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&event);
        assert_eq!(screen.toast_counter, 1);
    }

    #[test]
    fn key_e_triggers_error() {
        use super::Screen;
        let mut screen = Notifications::new();
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('e'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&event);
        assert_eq!(screen.toast_counter, 1);
    }

    #[test]
    fn key_u_triggers_urgent() {
        use super::Screen;
        let mut screen = Notifications::new();
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('u'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&event);
        assert_eq!(screen.toast_counter, 1);
    }

    #[test]
    fn render_empty_does_not_panic() {
        use super::Screen;
        let screen = Notifications::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        screen.view(&mut frame, area);
    }

    #[test]
    fn render_with_toasts_does_not_panic() {
        use super::Screen;
        let mut screen = Notifications::new();
        screen.push_success();
        screen.push_error();
        screen.push_warning();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        screen.view(&mut frame, area);
    }

    #[test]
    fn render_tiny_area_does_not_panic() {
        use super::Screen;
        let mut screen = Notifications::new();
        screen.push_success();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        screen.view(&mut frame, area);
    }

    #[test]
    fn render_zero_area_does_not_panic() {
        use super::Screen;
        let screen = Notifications::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let area = Rect::new(0, 0, 0, 0);
        screen.view(&mut frame, area);
    }

    #[test]
    fn tick_processes_queue() {
        use super::Screen;
        let mut screen = Notifications::new();
        screen.push_success();
        screen.tick(1);
        // Should not panic; queue processes normally
        assert_eq!(screen.tick_count, 1);
    }

    #[test]
    fn keybindings_returns_entries() {
        use super::Screen;
        let screen = Notifications::new();
        let bindings = screen.keybindings();
        assert_eq!(bindings.len(), 6);
        assert_eq!(bindings[0].key, "s");
    }

    #[test]
    fn title_and_label() {
        use super::Screen;
        let screen = Notifications::new();
        assert_eq!(screen.title(), "Notifications");
        assert_eq!(screen.tab_label(), "Notify");
    }
}
