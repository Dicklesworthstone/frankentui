#![forbid(unsafe_code)]

//! Notification queue manager for handling multiple concurrent toast notifications.
//!
//! The queue system provides:
//! - FIFO ordering with priority support (Urgent notifications jump ahead)
//! - Maximum visible limit with automatic stacking
//! - Content-based deduplication within a configurable time window
//! - Automatic expiry processing via tick-based updates
//!
//! # Example
//!
//! ```ignore
//! let mut queue = NotificationQueue::new(QueueConfig::default());
//!
//! // Push notifications
//! queue.push(Toast::new("File saved").icon(ToastIcon::Success), NotificationPriority::Normal);
//! queue.push(Toast::new("Error!").icon(ToastIcon::Error), NotificationPriority::Urgent);
//!
//! // Process in your event loop
//! let actions = queue.tick(Duration::from_millis(16));
//! for action in actions {
//!     match action {
//!         QueueAction::Show(toast) => { /* render toast */ }
//!         QueueAction::Hide(id) => { /* remove toast */ }
//!     }
//! }
//! ```

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;

use crate::Widget;
use crate::toast::{Toast, ToastId, ToastPosition};

/// Priority level for notifications.
///
/// Higher priority notifications are displayed sooner.
/// `Urgent` notifications jump to the front of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum NotificationPriority {
    /// Low priority, displayed last.
    Low = 0,
    /// Normal priority (default).
    #[default]
    Normal = 1,
    /// High priority, displayed before Normal/Low.
    High = 2,
    /// Urgent priority, jumps to front immediately.
    Urgent = 3,
}

/// Configuration for the notification queue.
#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// Maximum number of toasts visible at once.
    pub max_visible: usize,
    /// Maximum number of notifications waiting in queue.
    pub max_queued: usize,
    /// Default auto-dismiss duration.
    pub default_duration: Duration,
    /// Anchor position for the toast stack.
    pub position: ToastPosition,
    /// Vertical spacing between stacked toasts.
    pub stagger_offset: u16,
    /// Time window for deduplication (in ms).
    pub dedup_window_ms: u64,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            max_visible: 3,
            max_queued: 10,
            default_duration: Duration::from_secs(5),
            position: ToastPosition::TopRight,
            stagger_offset: 1,
            dedup_window_ms: 1000,
        }
    }
}

impl QueueConfig {
    /// Create a new configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum visible toasts.
    #[must_use]
    pub fn max_visible(mut self, max: usize) -> Self {
        self.max_visible = max;
        self
    }

    /// Set maximum queued notifications.
    #[must_use]
    pub fn max_queued(mut self, max: usize) -> Self {
        self.max_queued = max;
        self
    }

    /// Set default duration for auto-dismiss.
    #[must_use]
    pub fn default_duration(mut self, duration: Duration) -> Self {
        self.default_duration = duration;
        self
    }

    /// Set anchor position for the toast stack.
    #[must_use]
    pub fn position(mut self, position: ToastPosition) -> Self {
        self.position = position;
        self
    }

    /// Set vertical spacing between stacked toasts.
    #[must_use]
    pub fn stagger_offset(mut self, offset: u16) -> Self {
        self.stagger_offset = offset;
        self
    }

    /// Set deduplication time window in milliseconds.
    #[must_use]
    pub fn dedup_window_ms(mut self, ms: u64) -> Self {
        self.dedup_window_ms = ms;
        self
    }
}

/// Internal representation of a queued notification.
#[derive(Debug)]
struct QueuedNotification {
    toast: Toast,
    priority: NotificationPriority,
    /// When the notification was queued (for potential time-based priority decay).
    #[allow(dead_code)]
    created_at: Instant,
    content_hash: u64,
}

impl QueuedNotification {
    fn new(toast: Toast, priority: NotificationPriority) -> Self {
        let content_hash = Self::compute_hash(&toast);
        Self {
            toast,
            priority,
            created_at: Instant::now(),
            content_hash,
        }
    }

    fn compute_hash(toast: &Toast) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        toast.content.message.hash(&mut hasher);
        if let Some(ref title) = toast.content.title {
            title.hash(&mut hasher);
        }
        hasher.finish()
    }
}

/// Actions returned by `tick()` to be processed by the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueAction {
    /// Show a new toast at the given position.
    Show(ToastId),
    /// Hide an existing toast.
    Hide(ToastId),
    /// Reposition a toast (for stacking adjustments).
    Reposition(ToastId),
}

/// Queue statistics for monitoring and debugging.
#[derive(Debug, Clone, Default)]
pub struct QueueStats {
    /// Total notifications pushed.
    pub total_pushed: u64,
    /// Notifications rejected due to queue overflow.
    pub overflow_count: u64,
    /// Notifications rejected due to deduplication.
    pub dedup_count: u64,
    /// Notifications dismissed by user.
    pub user_dismissed: u64,
    /// Notifications expired automatically.
    pub auto_expired: u64,
}

/// Notification queue manager.
///
/// Manages multiple toast notifications with priority ordering, deduplication,
/// and automatic expiry. Use `push` to add notifications and `tick` to process
/// expiry in your event loop.
#[derive(Debug)]
pub struct NotificationQueue {
    /// Pending notifications waiting to be displayed.
    queue: VecDeque<QueuedNotification>,
    /// Currently visible toasts.
    visible: Vec<Toast>,
    /// Configuration.
    config: QueueConfig,
    /// Deduplication window.
    dedup_window: Duration,
    /// Recent content hashes for deduplication.
    recent_hashes: HashMap<u64, Instant>,
    /// Statistics.
    stats: QueueStats,
}

/// Widget that renders the visible toasts in a queue.
///
/// This is a thin renderer over `NotificationQueue`, keeping stacking logic
/// centralized in the queue while ensuring the draw path stays deterministic.
pub struct NotificationStack<'a> {
    queue: &'a NotificationQueue,
    margin: u16,
}

impl<'a> NotificationStack<'a> {
    /// Create a new notification stack renderer.
    pub fn new(queue: &'a NotificationQueue) -> Self {
        Self { queue, margin: 1 }
    }

    /// Set the margin from the screen edge.
    #[must_use]
    pub fn margin(mut self, margin: u16) -> Self {
        self.margin = margin;
        self
    }
}

impl Widget for NotificationStack<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() || self.queue.visible().is_empty() {
            return;
        }

        let positions = self
            .queue
            .calculate_positions(area.width, area.height, self.margin);

        for (toast, (_, rel_x, rel_y)) in self.queue.visible().iter().zip(positions.iter()) {
            let (toast_width, toast_height) = toast.calculate_dimensions();
            let x = area.x.saturating_add(*rel_x);
            let y = area.y.saturating_add(*rel_y);
            let toast_area = Rect::new(x, y, toast_width, toast_height);
            let render_area = toast_area.intersection(&area);
            if !render_area.is_empty() {
                toast.render(render_area, frame);
            }
        }
    }
}

impl NotificationQueue {
    /// Create a new notification queue with the given configuration.
    pub fn new(config: QueueConfig) -> Self {
        let dedup_window = Duration::from_millis(config.dedup_window_ms);
        Self {
            queue: VecDeque::new(),
            visible: Vec::new(),
            config,
            dedup_window,
            recent_hashes: HashMap::new(),
            stats: QueueStats::default(),
        }
    }

    /// Create a new queue with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(QueueConfig::default())
    }

    /// Push a notification to the queue.
    ///
    /// Returns `true` if the notification was accepted, `false` if it was
    /// rejected due to deduplication or queue overflow.
    pub fn push(&mut self, toast: Toast, priority: NotificationPriority) -> bool {
        self.stats.total_pushed += 1;

        let queued = QueuedNotification::new(toast, priority);

        // Check deduplication
        if !self.dedup_check(queued.content_hash) {
            self.stats.dedup_count += 1;
            return false;
        }

        // Check queue overflow
        if self.queue.len() >= self.config.max_queued {
            self.stats.overflow_count += 1;
            // Drop oldest low-priority item if possible
            if let Some(idx) = self.find_lowest_priority_index() {
                if self.queue[idx].priority < priority {
                    self.queue.remove(idx);
                } else {
                    return false; // New item is lower or equal priority
                }
            } else {
                return false;
            }
        }

        // Insert based on priority
        if priority == NotificationPriority::Urgent {
            // Urgent jumps to front
            self.queue.push_front(queued);
        } else {
            // Insert in priority order
            let insert_idx = self
                .queue
                .iter()
                .position(|q| q.priority < priority)
                .unwrap_or(self.queue.len());
            self.queue.insert(insert_idx, queued);
        }

        true
    }

    /// Push a notification with normal priority.
    pub fn notify(&mut self, toast: Toast) -> bool {
        self.push(toast, NotificationPriority::Normal)
    }

    /// Push an urgent notification.
    pub fn urgent(&mut self, toast: Toast) -> bool {
        self.push(toast, NotificationPriority::Urgent)
    }

    /// Dismiss a specific notification by ID.
    pub fn dismiss(&mut self, id: ToastId) {
        // Check visible toasts
        if let Some(idx) = self.visible.iter().position(|t| t.id == id) {
            self.visible[idx].dismiss();
            self.stats.user_dismissed += 1;
        }

        // Check queue
        if let Some(idx) = self.queue.iter().position(|q| q.toast.id == id) {
            self.queue.remove(idx);
            self.stats.user_dismissed += 1;
        }
    }

    /// Dismiss all notifications.
    pub fn dismiss_all(&mut self) {
        for toast in &mut self.visible {
            toast.dismiss();
        }
        self.stats.user_dismissed += self.queue.len() as u64;
        self.queue.clear();
    }

    /// Process a time tick, handling expiry and promotion.
    ///
    /// Call this regularly in your event loop (e.g., every frame or every 16ms).
    /// Returns a list of actions to perform.
    pub fn tick(&mut self, _delta: Duration) -> Vec<QueueAction> {
        let mut actions = Vec::new();

        // Clean expired dedup hashes
        let now = Instant::now();
        self.recent_hashes
            .retain(|_, t| now.duration_since(*t) < self.dedup_window);

        // Process visible toasts for expiry
        let mut i = 0;
        while i < self.visible.len() {
            if !self.visible[i].is_visible() {
                let id = self.visible[i].id;
                self.visible.remove(i);
                self.stats.auto_expired += 1;
                actions.push(QueueAction::Hide(id));
            } else {
                i += 1;
            }
        }

        // Promote from queue to visible
        while self.visible.len() < self.config.max_visible {
            if let Some(queued) = self.queue.pop_front() {
                let id = queued.toast.id;
                self.visible.push(queued.toast);
                actions.push(QueueAction::Show(id));
            } else {
                break;
            }
        }

        actions
    }

    /// Get currently visible toasts.
    pub fn visible(&self) -> &[Toast] {
        &self.visible
    }

    /// Get mutable access to visible toasts.
    pub fn visible_mut(&mut self) -> &mut [Toast] {
        &mut self.visible
    }

    /// Get the number of notifications waiting in the queue.
    pub fn pending_count(&self) -> usize {
        self.queue.len()
    }

    /// Get the number of visible toasts.
    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    /// Get the total count (visible + pending).
    pub fn total_count(&self) -> usize {
        self.visible.len() + self.queue.len()
    }

    /// Check if the queue is empty (no visible or pending notifications).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty() && self.queue.is_empty()
    }

    /// Get queue statistics.
    pub fn stats(&self) -> &QueueStats {
        &self.stats
    }

    /// Get the configuration.
    pub fn config(&self) -> &QueueConfig {
        &self.config
    }

    /// Calculate stacking positions for all visible toasts.
    ///
    /// Returns a list of (ToastId, x, y) positions.
    pub fn calculate_positions(
        &self,
        terminal_width: u16,
        terminal_height: u16,
        margin: u16,
    ) -> Vec<(ToastId, u16, u16)> {
        let mut positions = Vec::with_capacity(self.visible.len());
        let is_top = matches!(
            self.config.position,
            ToastPosition::TopLeft | ToastPosition::TopCenter | ToastPosition::TopRight
        );

        let mut y_offset: u16 = 0;

        for toast in &self.visible {
            let (toast_width, toast_height) = toast.calculate_dimensions();
            let (base_x, base_y) = self.config.position.calculate_position(
                terminal_width,
                terminal_height,
                toast_width,
                toast_height,
                margin,
            );

            let y = if is_top {
                base_y.saturating_add(y_offset)
            } else {
                base_y.saturating_sub(y_offset)
            };

            positions.push((toast.id, base_x, y));
            y_offset = y_offset
                .saturating_add(toast_height)
                .saturating_add(self.config.stagger_offset);
        }

        positions
    }

    // --- Internal methods ---

    /// Check if a content hash is a duplicate within the dedup window.
    fn dedup_check(&mut self, hash: u64) -> bool {
        let now = Instant::now();

        // Clean old hashes
        self.recent_hashes
            .retain(|_, t| now.duration_since(*t) < self.dedup_window);

        // Check if duplicate
        if self.recent_hashes.contains_key(&hash) {
            return false;
        }

        self.recent_hashes.insert(hash, now);
        true
    }

    /// Find the index of the lowest priority item in the queue.
    fn find_lowest_priority_index(&self) -> Option<usize> {
        self.queue
            .iter()
            .enumerate()
            .min_by_key(|(_, q)| q.priority)
            .map(|(i, _)| i)
    }
}

impl Default for NotificationQueue {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    fn make_toast(msg: &str) -> Toast {
        Toast::with_id(ToastId::new(0), msg).persistent() // Use persistent for testing
    }

    #[test]
    fn test_queue_new() {
        let queue = NotificationQueue::with_defaults();
        assert!(queue.is_empty());
        assert_eq!(queue.visible_count(), 0);
        assert_eq!(queue.pending_count(), 0);
    }

    #[test]
    fn test_queue_push_and_tick() {
        let mut queue = NotificationQueue::with_defaults();

        queue.push(make_toast("Hello"), NotificationPriority::Normal);
        assert_eq!(queue.pending_count(), 1);
        assert_eq!(queue.visible_count(), 0);

        // Tick promotes from queue to visible
        let actions = queue.tick(Duration::from_millis(16));
        assert_eq!(queue.pending_count(), 0);
        assert_eq!(queue.visible_count(), 1);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], QueueAction::Show(_)));
    }

    #[test]
    fn test_queue_fifo() {
        let config = QueueConfig::default().max_visible(1);
        let mut queue = NotificationQueue::new(config);

        queue.push(make_toast("First"), NotificationPriority::Normal);
        queue.push(make_toast("Second"), NotificationPriority::Normal);
        queue.push(make_toast("Third"), NotificationPriority::Normal);

        queue.tick(Duration::from_millis(16));
        assert_eq!(queue.visible()[0].content.message, "First");

        // Dismiss first, tick to get second
        queue.visible_mut()[0].dismiss();
        queue.tick(Duration::from_millis(16));
        assert_eq!(queue.visible()[0].content.message, "Second");
    }

    #[test]
    fn test_queue_max_visible() {
        let config = QueueConfig::default().max_visible(2);
        let mut queue = NotificationQueue::new(config);

        queue.push(make_toast("A"), NotificationPriority::Normal);
        queue.push(make_toast("B"), NotificationPriority::Normal);
        queue.push(make_toast("C"), NotificationPriority::Normal);

        queue.tick(Duration::from_millis(16));

        assert_eq!(queue.visible_count(), 2);
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn test_queue_priority_urgent() {
        let config = QueueConfig::default().max_visible(1);
        let mut queue = NotificationQueue::new(config);

        queue.push(make_toast("Normal1"), NotificationPriority::Normal);
        queue.push(make_toast("Normal2"), NotificationPriority::Normal);
        queue.push(make_toast("Urgent"), NotificationPriority::Urgent);

        queue.tick(Duration::from_millis(16));
        // Urgent should jump to front
        assert_eq!(queue.visible()[0].content.message, "Urgent");
    }

    #[test]
    fn test_queue_priority_ordering() {
        let config = QueueConfig::default().max_visible(0); // No auto-promote
        let mut queue = NotificationQueue::new(config);

        queue.push(make_toast("Low"), NotificationPriority::Low);
        queue.push(make_toast("Normal"), NotificationPriority::Normal);
        queue.push(make_toast("High"), NotificationPriority::High);

        // Queue should be ordered High, Normal, Low
        let messages: Vec<_> = queue
            .queue
            .iter()
            .map(|q| q.toast.content.message.as_str())
            .collect();
        assert_eq!(messages, vec!["High", "Normal", "Low"]);
    }

    #[test]
    fn test_queue_dedup() {
        let config = QueueConfig::default().dedup_window_ms(1000);
        let mut queue = NotificationQueue::new(config);

        assert!(queue.push(make_toast("Same message"), NotificationPriority::Normal));
        assert!(!queue.push(make_toast("Same message"), NotificationPriority::Normal));

        assert_eq!(queue.stats().dedup_count, 1);
    }

    #[test]
    fn test_queue_overflow() {
        let config = QueueConfig::default().max_queued(2);
        let mut queue = NotificationQueue::new(config);

        assert!(queue.push(make_toast("A"), NotificationPriority::Normal));
        assert!(queue.push(make_toast("B"), NotificationPriority::Normal));
        // Third should fail (queue full)
        assert!(!queue.push(make_toast("C"), NotificationPriority::Normal));

        assert_eq!(queue.stats().overflow_count, 1);
    }

    #[test]
    fn test_queue_overflow_drops_lower_priority() {
        let config = QueueConfig::default().max_queued(2);
        let mut queue = NotificationQueue::new(config);

        assert!(queue.push(make_toast("Low1"), NotificationPriority::Low));
        assert!(queue.push(make_toast("Low2"), NotificationPriority::Low));
        // High priority should drop a low priority item
        assert!(queue.push(make_toast("High"), NotificationPriority::High));

        assert_eq!(queue.pending_count(), 2);
        let messages: Vec<_> = queue
            .queue
            .iter()
            .map(|q| q.toast.content.message.as_str())
            .collect();
        assert!(messages.contains(&"High"));
    }

    #[test]
    fn test_queue_dismiss() {
        let mut queue = NotificationQueue::with_defaults();

        queue.push(make_toast("Test"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));

        let id = queue.visible()[0].id;
        queue.dismiss(id);
        queue.tick(Duration::from_millis(16));

        assert_eq!(queue.visible_count(), 0);
        assert_eq!(queue.stats().user_dismissed, 1);
    }

    #[test]
    fn test_queue_dismiss_all() {
        let mut queue = NotificationQueue::with_defaults();

        queue.push(make_toast("A"), NotificationPriority::Normal);
        queue.push(make_toast("B"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));

        queue.dismiss_all();
        queue.tick(Duration::from_millis(16));

        assert!(queue.is_empty());
    }

    #[test]
    fn test_queue_calculate_positions_top() {
        let config = QueueConfig::default().position(ToastPosition::TopRight);
        let mut queue = NotificationQueue::new(config);

        queue.push(make_toast("A"), NotificationPriority::Normal);
        queue.push(make_toast("B"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));

        let positions = queue.calculate_positions(80, 24, 1);
        assert_eq!(positions.len(), 2);

        // First toast should be at top, second below
        assert!(positions[0].2 < positions[1].2);
    }

    #[test]
    fn test_queue_calculate_positions_bottom() {
        let config = QueueConfig::default().position(ToastPosition::BottomRight);
        let mut queue = NotificationQueue::new(config);

        queue.push(make_toast("A"), NotificationPriority::Normal);
        queue.push(make_toast("B"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));

        let positions = queue.calculate_positions(80, 24, 1);
        assert_eq!(positions.len(), 2);

        // First toast should be at bottom, second above
        assert!(positions[0].2 > positions[1].2);
    }

    #[test]
    fn test_queue_notify_helper() {
        let mut queue = NotificationQueue::with_defaults();
        assert!(queue.notify(make_toast("Normal")));
        queue.tick(Duration::from_millis(16));
        assert_eq!(queue.visible_count(), 1);
    }

    #[test]
    fn test_queue_urgent_helper() {
        let config = QueueConfig::default().max_visible(1);
        let mut queue = NotificationQueue::new(config);

        queue.notify(make_toast("Normal"));
        queue.urgent(make_toast("Urgent"));
        queue.tick(Duration::from_millis(16));

        assert_eq!(queue.visible()[0].content.message, "Urgent");
    }

    #[test]
    fn test_queue_stats() {
        let mut queue = NotificationQueue::with_defaults();

        queue.push(make_toast("A"), NotificationPriority::Normal);
        queue.push(make_toast("A"), NotificationPriority::Normal); // Dedup
        queue.tick(Duration::from_millis(16));

        assert_eq!(queue.stats().total_pushed, 2);
        assert_eq!(queue.stats().dedup_count, 1);
    }

    #[test]
    fn test_queue_config_builder() {
        let config = QueueConfig::new()
            .max_visible(5)
            .max_queued(20)
            .default_duration(Duration::from_secs(10))
            .position(ToastPosition::BottomLeft)
            .stagger_offset(2)
            .dedup_window_ms(500);

        assert_eq!(config.max_visible, 5);
        assert_eq!(config.max_queued, 20);
        assert_eq!(config.default_duration, Duration::from_secs(10));
        assert_eq!(config.position, ToastPosition::BottomLeft);
        assert_eq!(config.stagger_offset, 2);
        assert_eq!(config.dedup_window_ms, 500);
    }

    #[test]
    fn test_queue_total_count() {
        let config = QueueConfig::default().max_visible(1);
        let mut queue = NotificationQueue::new(config);

        queue.push(make_toast("A"), NotificationPriority::Normal);
        queue.push(make_toast("B"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));

        assert_eq!(queue.total_count(), 2);
        assert_eq!(queue.visible_count(), 1);
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn queue_config_default_values() {
        let config = QueueConfig::default();
        assert_eq!(config.max_visible, 3);
        assert_eq!(config.max_queued, 10);
        assert_eq!(config.default_duration, Duration::from_secs(5));
        assert_eq!(config.position, ToastPosition::TopRight);
        assert_eq!(config.stagger_offset, 1);
        assert_eq!(config.dedup_window_ms, 1000);
    }

    #[test]
    fn notification_priority_default_is_normal() {
        assert_eq!(
            NotificationPriority::default(),
            NotificationPriority::Normal
        );
    }

    #[test]
    fn notification_priority_ordering() {
        assert!(NotificationPriority::Low < NotificationPriority::Normal);
        assert!(NotificationPriority::Normal < NotificationPriority::High);
        assert!(NotificationPriority::High < NotificationPriority::Urgent);
    }

    #[test]
    fn queue_default_trait_delegates_to_with_defaults() {
        let queue = NotificationQueue::default();
        assert!(queue.is_empty());
        assert_eq!(queue.config().max_visible, 3);
    }

    #[test]
    fn is_empty_false_when_pending() {
        let mut queue = NotificationQueue::with_defaults();
        queue.push(make_toast("X"), NotificationPriority::Normal);
        assert!(!queue.is_empty());
    }

    #[test]
    fn is_empty_false_when_visible() {
        let mut queue = NotificationQueue::with_defaults();
        queue.push(make_toast("X"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));
        assert!(!queue.is_empty());
    }

    #[test]
    fn visible_mut_allows_modification() {
        let mut queue = NotificationQueue::with_defaults();
        queue.push(make_toast("Original"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));

        // Dismiss via visible_mut
        queue.visible_mut()[0].dismiss();
        queue.tick(Duration::from_millis(16));
        assert_eq!(queue.visible_count(), 0);
    }

    #[test]
    fn config_accessor_returns_config() {
        let config = QueueConfig::default().max_visible(7).stagger_offset(3);
        let queue = NotificationQueue::new(config);
        assert_eq!(queue.config().max_visible, 7);
        assert_eq!(queue.config().stagger_offset, 3);
    }

    #[test]
    fn dismiss_all_clears_queue_and_visible() {
        let config = QueueConfig::default().max_visible(1);
        let mut queue = NotificationQueue::new(config);

        queue.push(make_toast("A"), NotificationPriority::Normal);
        queue.push(make_toast("B"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));

        // After tick: A is visible, B is pending.
        assert_eq!(queue.visible_count(), 1);
        assert_eq!(queue.pending_count(), 1);

        queue.dismiss_all();
        // dismiss_all: marks visible toasts dismissed, clears queue,
        // increments user_dismissed by queue.len() (1 for B)
        assert_eq!(queue.stats().user_dismissed, 1);
        assert_eq!(queue.pending_count(), 0);

        // Next tick removes the dismissed visible toast
        queue.tick(Duration::from_millis(16));
        assert!(queue.is_empty());
    }

    #[test]
    fn queue_action_equality() {
        let id = ToastId::new(42);
        assert_eq!(QueueAction::Show(id), QueueAction::Show(id));
        assert_eq!(QueueAction::Hide(id), QueueAction::Hide(id));
        assert_eq!(QueueAction::Reposition(id), QueueAction::Reposition(id));
        assert_ne!(QueueAction::Show(id), QueueAction::Hide(id));
    }

    #[test]
    fn queue_stats_default_all_zero() {
        let stats = QueueStats::default();
        assert_eq!(stats.total_pushed, 0);
        assert_eq!(stats.overflow_count, 0);
        assert_eq!(stats.dedup_count, 0);
        assert_eq!(stats.user_dismissed, 0);
        assert_eq!(stats.auto_expired, 0);
    }

    #[test]
    fn calculate_positions_empty_returns_empty() {
        let queue = NotificationQueue::with_defaults();
        let positions = queue.calculate_positions(80, 24, 1);
        assert!(positions.is_empty());
    }

    #[test]
    fn notification_stack_empty_area_renders_nothing() {
        let mut queue = NotificationQueue::with_defaults();
        queue.push(make_toast("Hello"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let empty_area = Rect::new(0, 0, 0, 0);

        // Should not panic
        NotificationStack::new(&queue).render(empty_area, &mut frame);
    }

    #[test]
    fn notification_stack_margin_builder() {
        let queue = NotificationQueue::with_defaults();
        let stack = NotificationStack::new(&queue).margin(5);
        assert_eq!(stack.margin, 5);
    }

    #[test]
    fn notification_stack_renders_visible_toast() {
        let mut queue = NotificationQueue::with_defaults();
        queue.push(make_toast("Hello"), NotificationPriority::Normal);
        queue.tick(Duration::from_millis(16));

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);

        NotificationStack::new(&queue)
            .margin(0)
            .render(area, &mut frame);

        let (_, x, y) = queue.calculate_positions(40, 10, 0)[0];
        let cell = frame.buffer.get(x, y).expect("cell should exist");
        assert!(!cell.is_empty(), "stack should render toast content");
    }
}
