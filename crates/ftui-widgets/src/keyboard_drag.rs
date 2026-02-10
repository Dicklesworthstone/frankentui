#![forbid(unsafe_code)]

//! Keyboard-driven drag-and-drop support (bd-1csc.4).
//!
//! This module enables drag operations via keyboard for accessibility, complementing
//! the mouse-based drag protocol in [`crate::drag`].
//!
//! # Usage
//!
//! ```ignore
//! use ftui_widgets::keyboard_drag::{KeyboardDragManager, KeyboardDragConfig};
//! use ftui_widgets::drag::DragPayload;
//!
//! let config = KeyboardDragConfig::default();
//! let mut manager = KeyboardDragManager::new(config);
//!
//! // User picks up an item (Space/Enter on a draggable)
//! manager.start_drag(source_id, payload);
//!
//! // User navigates targets (Arrow keys)
//! manager.navigate_targets(Direction::Right, &available_targets);
//!
//! // User drops (Space/Enter) or cancels (Escape)
//! if let Some(result) = manager.complete_drag(drop_target) {
//!     // Handle drop result
//! }
//! ```
//!
//! # Invariants
//!
//! 1. A keyboard drag is either `Inactive`, `Holding`, or `Navigating`:
//!    - `Inactive`: No drag in progress
//!    - `Holding`: Item picked up, awaiting target navigation
//!    - `Navigating`: Actively navigating between drop targets
//! 2. `start_drag` can only be called in `Inactive` mode.
//! 3. `navigate_targets` can only be called in `Holding` or `Navigating` mode.
//! 4. `complete_drag` transitions to `Inactive` regardless of success/failure.
//! 5. `cancel_drag` always transitions to `Inactive`.
//!
//! # Failure Modes
//!
//! | Failure | Cause | Fallback |
//! |---------|-------|----------|
//! | No valid targets | All targets reject payload type | Stay in Holding mode |
//! | Focus loss | Window deactivated | Auto-cancel drag |
//! | Invalid source | Source widget destroyed | Cancel drag gracefully |
//!
//! # Accessibility
//!
//! The module supports screen reader announcements via [`Announcement`] queue:
//! - "Picked up: {item description}"
//! - "Drop target: {target name} ({position})"
//! - "Dropped on: {target name}" or "Drop cancelled"

use crate::drag::DragPayload;
use crate::measure_cache::WidgetId;
use ftui_core::geometry::Rect;
use ftui_render::cell::CellContent;
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_style::Style;

// ---------------------------------------------------------------------------
// KeyboardDragMode
// ---------------------------------------------------------------------------

/// Current mode of a keyboard drag operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyboardDragMode {
    /// No keyboard drag in progress.
    #[default]
    Inactive,
    /// Item picked up, awaiting target selection.
    Holding,
    /// Actively navigating between drop targets.
    Navigating,
}

impl KeyboardDragMode {
    /// Returns true if a drag is in progress.
    #[must_use]
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Inactive)
    }

    /// Returns the stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Holding => "holding",
            Self::Navigating => "navigating",
        }
    }
}

// ---------------------------------------------------------------------------
// Direction
// ---------------------------------------------------------------------------

/// Navigation direction for keyboard drag target selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    /// Returns the opposite direction.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Down => Self::Up,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    /// Returns true for vertical directions.
    #[must_use]
    pub const fn is_vertical(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }
}

// ---------------------------------------------------------------------------
// DropTargetInfo
// ---------------------------------------------------------------------------

/// Information about a potential drop target for keyboard navigation.
#[derive(Debug, Clone)]
pub struct DropTargetInfo {
    /// Unique identifier for the target widget.
    pub id: WidgetId,
    /// Human-readable name for accessibility.
    pub name: String,
    /// Bounding rectangle in screen coordinates.
    pub bounds: Rect,
    /// Accepted drag types (MIME-like patterns).
    pub accepted_types: Vec<String>,
    /// Whether this target is currently enabled.
    pub enabled: bool,
}

impl DropTargetInfo {
    /// Create a new drop target info.
    #[must_use]
    pub fn new(id: WidgetId, name: impl Into<String>, bounds: Rect) -> Self {
        Self {
            id,
            name: name.into(),
            bounds,
            accepted_types: Vec::new(),
            enabled: true,
        }
    }

    /// Add accepted drag types.
    #[must_use]
    pub fn with_accepted_types(mut self, types: Vec<String>) -> Self {
        self.accepted_types = types;
        self
    }

    /// Set enabled state.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Check if this target can accept the given payload type.
    #[must_use]
    pub fn can_accept(&self, drag_type: &str) -> bool {
        if !self.enabled {
            return false;
        }
        if self.accepted_types.is_empty() {
            return true; // Accept any if no filter specified
        }
        self.accepted_types.iter().any(|pattern| {
            if pattern == "*" || pattern == "*/*" {
                true
            } else if let Some(prefix) = pattern.strip_suffix("/*") {
                drag_type.starts_with(prefix)
                    && drag_type.as_bytes().get(prefix.len()) == Some(&b'/')
            } else {
                pattern == drag_type
            }
        })
    }

    /// Returns the center point of this target's bounds.
    #[must_use]
    pub fn center(&self) -> (u16, u16) {
        (
            self.bounds.x + self.bounds.width / 2,
            self.bounds.y + self.bounds.height / 2,
        )
    }
}

// ---------------------------------------------------------------------------
// Announcement
// ---------------------------------------------------------------------------

/// Screen reader announcement for accessibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    /// The text to announce.
    pub text: String,
    /// Priority level (higher = more important).
    pub priority: AnnouncementPriority,
}

/// Priority level for announcements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum AnnouncementPriority {
    /// Low priority, may be skipped if queue is full.
    Low,
    /// Normal priority.
    #[default]
    Normal,
    /// High priority, interrupts current announcement.
    High,
}

impl Announcement {
    /// Create a normal priority announcement.
    #[must_use]
    pub fn normal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            priority: AnnouncementPriority::Normal,
        }
    }

    /// Create a high priority announcement.
    #[must_use]
    pub fn high(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            priority: AnnouncementPriority::High,
        }
    }
}

// ---------------------------------------------------------------------------
// KeyboardDragConfig
// ---------------------------------------------------------------------------

/// Configuration for keyboard drag behavior.
#[derive(Debug, Clone)]
pub struct KeyboardDragConfig {
    /// Keys that activate drag (pick up or drop).
    /// Default: Space, Enter
    pub activate_keys: Vec<ActivateKey>,

    /// Whether Escape cancels the drag.
    pub cancel_on_escape: bool,

    /// Style for highlighting the selected drop target.
    pub target_highlight_style: TargetHighlightStyle,

    /// Style for highlighting invalid drop targets.
    pub invalid_target_style: TargetHighlightStyle,

    /// Whether to wrap around when navigating past the last/first target.
    pub wrap_navigation: bool,

    /// Maximum announcements to queue.
    pub max_announcement_queue: usize,
}

/// Keys that can activate drag operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivateKey {
    Space,
    Enter,
}

impl Default for KeyboardDragConfig {
    fn default() -> Self {
        Self {
            activate_keys: vec![ActivateKey::Space, ActivateKey::Enter],
            cancel_on_escape: true,
            target_highlight_style: TargetHighlightStyle::default(),
            invalid_target_style: TargetHighlightStyle::invalid_default(),
            wrap_navigation: true,
            max_announcement_queue: 5,
        }
    }
}

// ---------------------------------------------------------------------------
// TargetHighlightStyle
// ---------------------------------------------------------------------------

/// Visual style for highlighting drop targets during keyboard drag.
#[derive(Debug, Clone)]
pub struct TargetHighlightStyle {
    /// Border style (character to use for highlighting).
    pub border_char: char,
    /// Foreground color for the highlight border.
    pub border_fg: PackedRgba,
    /// Background color for the target area.
    pub background: Option<PackedRgba>,
    /// Whether to render a pulsing animation.
    pub animate_pulse: bool,
}

impl Default for TargetHighlightStyle {
    fn default() -> Self {
        Self {
            border_char: '█',
            border_fg: PackedRgba::rgb(100, 180, 255), // Blue highlight
            background: Some(PackedRgba::rgba(100, 180, 255, 40)), // Subtle blue tint
            animate_pulse: true,
        }
    }
}

impl TargetHighlightStyle {
    /// Style for invalid drop targets.
    #[must_use]
    pub fn invalid_default() -> Self {
        Self {
            border_char: '▪',
            border_fg: PackedRgba::rgb(180, 100, 100), // Red highlight
            background: Some(PackedRgba::rgba(180, 100, 100, 20)), // Subtle red tint
            animate_pulse: false,
        }
    }

    /// Create a custom style.
    #[must_use]
    pub fn new(border_char: char, fg: PackedRgba) -> Self {
        Self {
            border_char,
            border_fg: fg,
            background: None,
            animate_pulse: false,
        }
    }

    /// Set background color.
    #[must_use]
    pub fn with_background(mut self, bg: PackedRgba) -> Self {
        self.background = Some(bg);
        self
    }

    /// Enable pulse animation.
    #[must_use]
    pub fn with_pulse(mut self) -> Self {
        self.animate_pulse = true;
        self
    }
}

// ---------------------------------------------------------------------------
// KeyboardDragState
// ---------------------------------------------------------------------------

/// State of an active keyboard drag operation.
#[derive(Debug, Clone)]
pub struct KeyboardDragState {
    /// Widget that initiated the drag.
    pub source_id: WidgetId,
    /// Data being dragged.
    pub payload: DragPayload,
    /// Currently selected drop target index (into available targets list).
    pub selected_target_index: Option<usize>,
    /// Current mode.
    pub mode: KeyboardDragMode,
    /// Animation tick for pulse effect.
    pub animation_tick: u8,
}

impl KeyboardDragState {
    /// Create a new keyboard drag state.
    fn new(source_id: WidgetId, payload: DragPayload) -> Self {
        Self {
            source_id,
            payload,
            selected_target_index: None,
            mode: KeyboardDragMode::Holding,
            animation_tick: 0,
        }
    }

    /// Advance the animation tick.
    pub fn tick_animation(&mut self) {
        self.animation_tick = self.animation_tick.wrapping_add(1);
    }

    /// Get the pulse intensity (0.0 to 1.0) for animation.
    #[must_use]
    pub fn pulse_intensity(&self) -> f32 {
        // Simple sine-based pulse: 0.5 + 0.5 * sin(tick * 0.15)
        let angle = self.animation_tick as f32 * 0.15;
        0.5 + 0.5 * angle.sin()
    }
}

// ---------------------------------------------------------------------------
// KeyboardDragManager
// ---------------------------------------------------------------------------

/// Manager for keyboard-driven drag operations.
#[derive(Debug)]
pub struct KeyboardDragManager {
    /// Configuration.
    config: KeyboardDragConfig,
    /// Current drag state (if any).
    state: Option<KeyboardDragState>,
    /// Announcement queue.
    announcements: Vec<Announcement>,
}

impl KeyboardDragManager {
    /// Create a new keyboard drag manager.
    #[must_use]
    pub fn new(config: KeyboardDragConfig) -> Self {
        Self {
            config,
            state: None,
            announcements: Vec::new(),
        }
    }

    /// Create with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(KeyboardDragConfig::default())
    }

    /// Get the current drag mode.
    #[must_use]
    pub fn mode(&self) -> KeyboardDragMode {
        self.state
            .as_ref()
            .map(|s| s.mode)
            .unwrap_or(KeyboardDragMode::Inactive)
    }

    /// Check if a drag is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state.is_some()
    }

    /// Get the current drag state.
    #[must_use = "use the returned state (if any)"]
    pub fn state(&self) -> Option<&KeyboardDragState> {
        self.state.as_ref()
    }

    /// Get mutable access to the drag state.
    #[must_use = "use the returned state (if any)"]
    pub fn state_mut(&mut self) -> Option<&mut KeyboardDragState> {
        self.state.as_mut()
    }

    /// Start a keyboard drag operation.
    ///
    /// Returns `true` if the drag was started successfully.
    /// Returns `false` if a drag is already in progress.
    pub fn start_drag(&mut self, source_id: WidgetId, payload: DragPayload) -> bool {
        if self.state.is_some() {
            return false;
        }

        let description = payload
            .display_text
            .as_deref()
            .or_else(|| payload.as_text())
            .unwrap_or("item");

        self.queue_announcement(Announcement::high(format!("Picked up: {description}")));

        self.state = Some(KeyboardDragState::new(source_id, payload));
        true
    }

    /// Navigate to the next drop target in the given direction.
    ///
    /// Returns the newly selected target info if navigation succeeded.
    #[must_use = "use the returned target (if any)"]
    pub fn navigate_targets<'a>(
        &mut self,
        direction: Direction,
        targets: &'a [DropTargetInfo],
    ) -> Option<&'a DropTargetInfo> {
        let state = self.state.as_mut()?;

        if targets.is_empty() {
            return None;
        }

        // Filter to valid targets that can accept the payload
        let valid_indices: Vec<usize> = targets
            .iter()
            .enumerate()
            .filter(|(_, t)| t.can_accept(&state.payload.drag_type))
            .map(|(i, _)| i)
            .collect();

        if valid_indices.is_empty() {
            self.queue_announcement(Announcement::normal("No valid drop targets available"));
            return None;
        }

        // Update mode to navigating
        state.mode = KeyboardDragMode::Navigating;

        // Find current position among valid targets
        let current_valid_idx = state
            .selected_target_index
            .and_then(|idx| valid_indices.iter().position(|&i| i == idx));

        // Calculate next index based on direction and current selection
        let next_valid_idx = match (current_valid_idx, direction) {
            (None, _) => 0, // No selection, start at first
            (Some(idx), Direction::Down | Direction::Right) => {
                if idx + 1 < valid_indices.len() {
                    idx + 1
                } else if self.config.wrap_navigation {
                    0
                } else {
                    idx
                }
            }
            (Some(idx), Direction::Up | Direction::Left) => {
                if idx > 0 {
                    idx - 1
                } else if self.config.wrap_navigation {
                    valid_indices.len() - 1
                } else {
                    idx
                }
            }
        };

        let target_idx = valid_indices[next_valid_idx];
        state.selected_target_index = Some(target_idx);

        let target = &targets[target_idx];
        let position = format!("{} of {}", next_valid_idx + 1, valid_indices.len());
        self.queue_announcement(Announcement::normal(format!(
            "Drop target: {} ({})",
            target.name, position
        )));

        Some(target)
    }

    /// Navigate to a specific target by index.
    pub fn select_target(&mut self, target_index: usize, targets: &[DropTargetInfo]) -> bool {
        let Some(state) = self.state.as_mut() else {
            return false;
        };

        if target_index >= targets.len() {
            return false;
        }

        let target = &targets[target_index];
        if !target.can_accept(&state.payload.drag_type) {
            return false;
        }

        state.mode = KeyboardDragMode::Navigating;
        state.selected_target_index = Some(target_index);

        self.queue_announcement(Announcement::normal(format!(
            "Drop target: {}",
            target.name
        )));
        true
    }

    /// Complete the drag operation (drop on selected target).
    ///
    /// Returns `None` if no target is selected or no drag is active.
    /// Returns `Some((payload, target_index))` with the payload and target index.
    #[must_use = "use the returned (payload, target_index) to complete the drop"]
    pub fn complete_drag(&mut self) -> Option<(DragPayload, usize)> {
        let state = self.state.take()?;
        let target_idx = state.selected_target_index?;

        Some((state.payload, target_idx))
    }

    /// Complete the drag with a specific target and get the drop result info.
    #[must_use = "use the drop result (if any) to apply the drop"]
    pub fn drop_on_target(&mut self, targets: &[DropTargetInfo]) -> Option<KeyboardDropResult> {
        let state = self.state.take()?;
        let target_idx = state.selected_target_index?;
        let target = targets.get(target_idx)?;

        self.queue_announcement(Announcement::high(format!("Dropped on: {}", target.name)));

        Some(KeyboardDropResult {
            payload: state.payload,
            source_id: state.source_id,
            target_id: target.id,
            target_index: target_idx,
        })
    }

    /// Cancel the current drag operation.
    ///
    /// Returns the payload if a drag was active.
    #[must_use = "use the returned payload (if any) to restore state"]
    pub fn cancel_drag(&mut self) -> Option<DragPayload> {
        let state = self.state.take()?;
        self.queue_announcement(Announcement::normal("Drop cancelled"));
        Some(state.payload)
    }

    /// Handle key press during keyboard drag.
    ///
    /// Returns `KeyboardDragAction` indicating what action was triggered.
    pub fn handle_key(&mut self, key: KeyboardDragKey) -> KeyboardDragAction {
        match key {
            KeyboardDragKey::Activate => {
                if self.is_active() {
                    // If navigating with a selected target, complete the drop
                    if let Some(state) = &self.state
                        && state.selected_target_index.is_some()
                    {
                        KeyboardDragAction::Drop
                    } else {
                        // No target selected yet, stay in current state
                        KeyboardDragAction::None
                    }
                } else {
                    // No drag active, signal to pick up
                    KeyboardDragAction::PickUp
                }
            }
            KeyboardDragKey::Cancel => {
                if self.is_active() && self.config.cancel_on_escape {
                    KeyboardDragAction::Cancel
                } else {
                    KeyboardDragAction::None
                }
            }
            KeyboardDragKey::Navigate(dir) => {
                if self.is_active() {
                    KeyboardDragAction::Navigate(dir)
                } else {
                    KeyboardDragAction::None
                }
            }
        }
    }

    /// Advance animation state.
    pub fn tick(&mut self) {
        if let Some(state) = &mut self.state {
            state.tick_animation();
        }
    }

    /// Get and clear pending announcements.
    pub fn drain_announcements(&mut self) -> Vec<Announcement> {
        std::mem::take(&mut self.announcements)
    }

    /// Peek at pending announcements without clearing.
    #[must_use]
    pub fn announcements(&self) -> &[Announcement] {
        &self.announcements
    }

    /// Queue an announcement for screen readers.
    fn queue_announcement(&mut self, announcement: Announcement) {
        if self.announcements.len() >= self.config.max_announcement_queue {
            // Remove lowest priority announcement
            if let Some(pos) = self
                .announcements
                .iter()
                .enumerate()
                .min_by_key(|(_, a)| a.priority)
                .map(|(i, _)| i)
            {
                self.announcements.remove(pos);
            }
        }
        self.announcements.push(announcement);
    }

    /// Render the target highlight overlay.
    pub fn render_highlight(&self, targets: &[DropTargetInfo], frame: &mut Frame) {
        let Some(state) = &self.state else {
            return;
        };
        let Some(target_idx) = state.selected_target_index else {
            return;
        };
        let Some(target) = targets.get(target_idx) else {
            return;
        };

        let style = if target.can_accept(&state.payload.drag_type) {
            &self.config.target_highlight_style
        } else {
            &self.config.invalid_target_style
        };

        let bounds = target.bounds;
        if bounds.is_empty() {
            return;
        }

        // Apply background tint if configured
        if let Some(bg) = style.background {
            // Calculate effective alpha based on pulse
            let alpha = if style.animate_pulse {
                let base_alpha = (bg.0 & 0xFF) as f32 / 255.0;
                let pulsed = base_alpha * (0.5 + 0.5 * state.pulse_intensity());
                (pulsed * 255.0) as u8
            } else {
                (bg.0 & 0xFF) as u8
            };

            let effective_bg = PackedRgba((bg.0 & 0xFFFF_FF00) | alpha as u32);

            // Fill background
            for y in bounds.y..bounds.y.saturating_add(bounds.height) {
                for x in bounds.x..bounds.x.saturating_add(bounds.width) {
                    if let Some(cell) = frame.buffer.get_mut(x, y) {
                        cell.bg = effective_bg;
                    }
                }
            }
        }

        // Draw highlight border
        let fg_style = Style::new().fg(style.border_fg);
        let border_char = style.border_char;

        // Top and bottom borders
        for x in bounds.x..bounds.x.saturating_add(bounds.width) {
            // Top
            if let Some(cell) = frame.buffer.get_mut(x, bounds.y) {
                cell.content = CellContent::from_char(border_char);
                cell.fg = fg_style.fg.unwrap_or(style.border_fg);
            }
            // Bottom
            let bottom_y = bounds.y.saturating_add(bounds.height.saturating_sub(1));
            if bounds.height > 1
                && let Some(cell) = frame.buffer.get_mut(x, bottom_y)
            {
                cell.content = CellContent::from_char(border_char);
                cell.fg = fg_style.fg.unwrap_or(style.border_fg);
            }
        }

        // Left and right borders (excluding corners)
        for y in
            bounds.y.saturating_add(1)..bounds.y.saturating_add(bounds.height.saturating_sub(1))
        {
            // Left
            if let Some(cell) = frame.buffer.get_mut(bounds.x, y) {
                cell.content = CellContent::from_char(border_char);
                cell.fg = fg_style.fg.unwrap_or(style.border_fg);
            }
            // Right
            let right_x = bounds.x.saturating_add(bounds.width.saturating_sub(1));
            if bounds.width > 1
                && let Some(cell) = frame.buffer.get_mut(right_x, y)
            {
                cell.content = CellContent::from_char(border_char);
                cell.fg = fg_style.fg.unwrap_or(style.border_fg);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// KeyboardDragKey
// ---------------------------------------------------------------------------

/// Key events relevant to keyboard drag operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardDragKey {
    /// Activation key (Space or Enter by default).
    Activate,
    /// Cancellation key (Escape by default).
    Cancel,
    /// Navigation key.
    Navigate(Direction),
}

// ---------------------------------------------------------------------------
// KeyboardDragAction
// ---------------------------------------------------------------------------

/// Action resulting from key handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardDragAction {
    /// No action needed.
    None,
    /// Pick up the focused item to start a drag.
    PickUp,
    /// Navigate to next target in direction.
    Navigate(Direction),
    /// Drop on the selected target.
    Drop,
    /// Cancel the drag operation.
    Cancel,
}

// ---------------------------------------------------------------------------
// KeyboardDropResult
// ---------------------------------------------------------------------------

/// Result of a completed keyboard drag-and-drop operation.
#[derive(Debug, Clone)]
pub struct KeyboardDropResult {
    /// The dropped payload.
    pub payload: DragPayload,
    /// Source widget ID.
    pub source_id: WidgetId,
    /// Target widget ID.
    pub target_id: WidgetId,
    /// Target index in the targets list.
    pub target_index: usize,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // === KeyboardDragMode tests ===

    #[test]
    fn mode_is_active() {
        assert!(!KeyboardDragMode::Inactive.is_active());
        assert!(KeyboardDragMode::Holding.is_active());
        assert!(KeyboardDragMode::Navigating.is_active());
    }

    #[test]
    fn mode_as_str() {
        assert_eq!(KeyboardDragMode::Inactive.as_str(), "inactive");
        assert_eq!(KeyboardDragMode::Holding.as_str(), "holding");
        assert_eq!(KeyboardDragMode::Navigating.as_str(), "navigating");
    }

    // === Direction tests ===

    #[test]
    fn direction_opposite() {
        assert_eq!(Direction::Up.opposite(), Direction::Down);
        assert_eq!(Direction::Down.opposite(), Direction::Up);
        assert_eq!(Direction::Left.opposite(), Direction::Right);
        assert_eq!(Direction::Right.opposite(), Direction::Left);
    }

    #[test]
    fn direction_is_vertical() {
        assert!(Direction::Up.is_vertical());
        assert!(Direction::Down.is_vertical());
        assert!(!Direction::Left.is_vertical());
        assert!(!Direction::Right.is_vertical());
    }

    // === DropTargetInfo tests ===

    #[test]
    fn drop_target_info_new() {
        let target = DropTargetInfo::new(WidgetId(1), "Test Target", Rect::new(0, 0, 10, 5));
        assert_eq!(target.id, WidgetId(1));
        assert_eq!(target.name, "Test Target");
        assert!(target.enabled);
        assert!(target.accepted_types.is_empty());
    }

    #[test]
    fn drop_target_info_can_accept_any() {
        let target = DropTargetInfo::new(WidgetId(1), "Any", Rect::new(0, 0, 1, 1));
        // No filter means accept any
        assert!(target.can_accept("text/plain"));
        assert!(target.can_accept("application/json"));
    }

    #[test]
    fn drop_target_info_can_accept_filtered() {
        let target = DropTargetInfo::new(WidgetId(1), "Text Only", Rect::new(0, 0, 1, 1))
            .with_accepted_types(vec!["text/plain".to_string()]);
        assert!(target.can_accept("text/plain"));
        assert!(!target.can_accept("application/json"));
    }

    #[test]
    fn drop_target_info_can_accept_wildcard() {
        let target = DropTargetInfo::new(WidgetId(1), "All Text", Rect::new(0, 0, 1, 1))
            .with_accepted_types(vec!["text/*".to_string()]);
        assert!(target.can_accept("text/plain"));
        assert!(target.can_accept("text/html"));
        assert!(!target.can_accept("application/json"));
    }

    #[test]
    fn drop_target_info_disabled() {
        let target =
            DropTargetInfo::new(WidgetId(1), "Disabled", Rect::new(0, 0, 1, 1)).with_enabled(false);
        assert!(!target.can_accept("text/plain"));
    }

    #[test]
    fn drop_target_info_center() {
        let target = DropTargetInfo::new(WidgetId(1), "Test", Rect::new(10, 20, 10, 6));
        assert_eq!(target.center(), (15, 23));
    }

    // === Announcement tests ===

    #[test]
    fn announcement_normal() {
        let a = Announcement::normal("Test message");
        assert_eq!(a.text, "Test message");
        assert_eq!(a.priority, AnnouncementPriority::Normal);
    }

    #[test]
    fn announcement_high() {
        let a = Announcement::high("Important!");
        assert_eq!(a.priority, AnnouncementPriority::High);
    }

    // === KeyboardDragConfig tests ===

    #[test]
    fn config_defaults() {
        let config = KeyboardDragConfig::default();
        assert!(config.cancel_on_escape);
        assert!(config.wrap_navigation);
        assert_eq!(config.activate_keys.len(), 2);
    }

    // === KeyboardDragState tests ===

    #[test]
    fn drag_state_animation() {
        let payload = DragPayload::text("test");
        let mut state = KeyboardDragState::new(WidgetId(1), payload);

        let initial_tick = state.animation_tick;
        state.tick_animation();
        assert_eq!(state.animation_tick, initial_tick.wrapping_add(1));
    }

    #[test]
    fn drag_state_pulse_intensity() {
        let payload = DragPayload::text("test");
        let state = KeyboardDragState::new(WidgetId(1), payload);

        let intensity = state.pulse_intensity();
        assert!((0.0..=1.0).contains(&intensity));
    }

    // === KeyboardDragManager tests ===

    #[test]
    fn manager_start_drag() {
        let mut manager = KeyboardDragManager::with_defaults();
        assert!(!manager.is_active());

        let payload = DragPayload::text("item");
        assert!(manager.start_drag(WidgetId(1), payload));
        assert!(manager.is_active());
        assert_eq!(manager.mode(), KeyboardDragMode::Holding);
    }

    #[test]
    fn manager_double_start_fails() {
        let mut manager = KeyboardDragManager::with_defaults();

        assert!(manager.start_drag(WidgetId(1), DragPayload::text("first")));
        assert!(!manager.start_drag(WidgetId(2), DragPayload::text("second")));
    }

    #[test]
    fn manager_cancel_drag() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        let payload = manager.cancel_drag();
        assert!(payload.is_some());
        assert!(!manager.is_active());
    }

    #[test]
    fn manager_cancel_inactive() {
        let mut manager = KeyboardDragManager::with_defaults();
        assert!(manager.cancel_drag().is_none());
    }

    #[test]
    fn manager_navigate_targets() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        let targets = vec![
            DropTargetInfo::new(WidgetId(10), "Target A", Rect::new(0, 0, 10, 5)),
            DropTargetInfo::new(WidgetId(11), "Target B", Rect::new(20, 0, 10, 5)),
        ];

        let selected = manager.navigate_targets(Direction::Down, &targets);
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().name, "Target A");
        assert_eq!(manager.mode(), KeyboardDragMode::Navigating);
    }

    #[test]
    fn manager_navigate_empty_targets() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        let targets: Vec<DropTargetInfo> = vec![];
        let selected = manager.navigate_targets(Direction::Down, &targets);
        assert!(selected.is_none());
    }

    #[test]
    fn manager_navigate_wrap() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        let targets = vec![
            DropTargetInfo::new(WidgetId(10), "Target A", Rect::new(0, 0, 10, 5)),
            DropTargetInfo::new(WidgetId(11), "Target B", Rect::new(20, 0, 10, 5)),
        ];

        // Navigate to first
        manager.navigate_targets(Direction::Down, &targets);
        // Navigate to second
        manager.navigate_targets(Direction::Down, &targets);
        // Navigate past end, should wrap to first
        let selected = manager.navigate_targets(Direction::Down, &targets);

        assert_eq!(selected.unwrap().name, "Target A");
    }

    #[test]
    fn manager_complete_drag() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        let targets = vec![DropTargetInfo::new(
            WidgetId(10),
            "Target A",
            Rect::new(0, 0, 10, 5),
        )];

        manager.navigate_targets(Direction::Down, &targets);

        let result = manager.complete_drag();
        assert!(result.is_some());
        let (payload, idx) = result.unwrap();
        assert_eq!(payload.as_text(), Some("item"));
        assert_eq!(idx, 0);
        assert!(!manager.is_active());
    }

    #[test]
    fn manager_complete_without_target() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        // No target selected
        let result = manager.complete_drag();
        assert!(result.is_none());
    }

    #[test]
    fn manager_handle_key_pickup() {
        let mut manager = KeyboardDragManager::with_defaults();
        let action = manager.handle_key(KeyboardDragKey::Activate);
        assert_eq!(action, KeyboardDragAction::PickUp);
    }

    #[test]
    fn manager_handle_key_drop() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        // Select a target
        manager.state_mut().unwrap().selected_target_index = Some(0);

        let action = manager.handle_key(KeyboardDragKey::Activate);
        assert_eq!(action, KeyboardDragAction::Drop);
    }

    #[test]
    fn manager_handle_key_cancel() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        let action = manager.handle_key(KeyboardDragKey::Cancel);
        assert_eq!(action, KeyboardDragAction::Cancel);
    }

    #[test]
    fn manager_handle_key_navigate() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        let action = manager.handle_key(KeyboardDragKey::Navigate(Direction::Down));
        assert_eq!(action, KeyboardDragAction::Navigate(Direction::Down));
    }

    #[test]
    fn manager_announcements() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::text("item"));

        let announcements = manager.drain_announcements();
        assert!(!announcements.is_empty());
        assert!(announcements[0].text.contains("Picked up"));
    }

    #[test]
    fn manager_announcement_queue_limit() {
        let config = KeyboardDragConfig {
            max_announcement_queue: 2,
            ..Default::default()
        };
        let mut manager = KeyboardDragManager::new(config);

        // Fill queue
        manager.start_drag(WidgetId(1), DragPayload::text("item1"));
        manager.cancel_drag();
        manager.start_drag(WidgetId(2), DragPayload::text("item2"));

        // Should have at most 2 announcements
        assert!(manager.announcements().len() <= 2);
    }

    // === Target filtering tests ===

    #[test]
    fn manager_navigate_skips_incompatible() {
        let mut manager = KeyboardDragManager::with_defaults();
        manager.start_drag(WidgetId(1), DragPayload::new("text/plain", vec![]));

        let targets = vec![
            DropTargetInfo::new(WidgetId(10), "Text Target", Rect::new(0, 0, 10, 5))
                .with_accepted_types(vec!["text/plain".to_string()]),
            DropTargetInfo::new(WidgetId(11), "Image Target", Rect::new(20, 0, 10, 5))
                .with_accepted_types(vec!["image/*".to_string()]),
            DropTargetInfo::new(WidgetId(12), "Text Target 2", Rect::new(40, 0, 10, 5))
                .with_accepted_types(vec!["text/plain".to_string()]),
        ];

        // First navigation should select Text Target
        let selected = manager.navigate_targets(Direction::Down, &targets);
        assert_eq!(selected.unwrap().name, "Text Target");

        // Second navigation should skip Image Target and select Text Target 2
        let selected = manager.navigate_targets(Direction::Down, &targets);
        assert_eq!(selected.unwrap().name, "Text Target 2");
    }

    // === Integration tests ===

    #[test]
    fn full_keyboard_drag_lifecycle() {
        let mut manager = KeyboardDragManager::with_defaults();

        // 1. Start drag
        assert!(manager.start_drag(WidgetId(1), DragPayload::text("dragged_item")));
        assert_eq!(manager.mode(), KeyboardDragMode::Holding);

        let targets = vec![
            DropTargetInfo::new(WidgetId(10), "Target A", Rect::new(0, 0, 10, 5)),
            DropTargetInfo::new(WidgetId(11), "Target B", Rect::new(0, 10, 10, 5)),
        ];

        // 2. Navigate to target
        manager.navigate_targets(Direction::Down, &targets);
        assert_eq!(manager.mode(), KeyboardDragMode::Navigating);

        // 3. Navigate to next target
        manager.navigate_targets(Direction::Down, &targets);

        // 4. Complete drop
        let result = manager.drop_on_target(&targets);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.payload.as_text(), Some("dragged_item"));
        assert_eq!(result.target_id, WidgetId(11));
        assert_eq!(result.target_index, 1);

        // 5. Manager is now inactive
        assert!(!manager.is_active());
    }
}
