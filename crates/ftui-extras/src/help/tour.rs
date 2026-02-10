#![forbid(unsafe_code)]

//! Guided tour system for step-by-step onboarding walkthroughs.
//!
//! # Invariants
//!
//! 1. A tour can only have one active step at a time.
//! 2. Navigation respects step order: back goes to previous, next goes to next.
//! 3. Skipping a tour marks it as incomplete but ends the tour.
//! 4. Completion is tracked persistently via `TourCompletion`.
//!
//! # Example
//!
//! ```ignore
//! use ftui_extras::help::{Tour, TourStep};
//!
//! let tour = Tour::new("onboarding")
//!     .add_step(TourStep::new("Welcome")
//!         .content("Welcome to the app! Let's get started.")
//!         .target_widget(1))
//!     .add_step(TourStep::new("Search")
//!         .content("Use the search bar to find items.")
//!         .target_widget(2));
//! ```

use ftui_core::geometry::Rect;
use std::collections::HashSet;

/// Unique identifier for a widget to highlight.
pub type WidgetId = u32;

/// A single step in a guided tour.
#[derive(Debug, Clone)]
pub struct TourStep {
    /// Step title displayed in the spotlight.
    pub title: String,
    /// Step content/instructions.
    pub content: String,
    /// Target widget ID to highlight (if any).
    pub target_widget: Option<WidgetId>,
    /// Target bounds override (if widget ID not available).
    pub target_bounds: Option<Rect>,
    /// Whether this step requires user action before continuing.
    pub requires_action: bool,
    /// Custom data for the step (e.g., keybinding to demonstrate).
    pub metadata: Option<String>,
}

impl TourStep {
    /// Create a new tour step with the given title.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            content: String::new(),
            target_widget: None,
            target_bounds: None,
            requires_action: false,
            metadata: None,
        }
    }

    /// Set the step content/instructions.
    #[must_use]
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    /// Set the target widget to highlight.
    #[must_use]
    pub fn target_widget(mut self, id: WidgetId) -> Self {
        self.target_widget = Some(id);
        self
    }

    /// Set explicit target bounds (overrides widget lookup).
    #[must_use]
    pub fn target_bounds(mut self, bounds: Rect) -> Self {
        self.target_bounds = Some(bounds);
        self
    }

    /// Mark this step as requiring user action before continuing.
    #[must_use]
    pub fn requires_action(mut self, requires: bool) -> Self {
        self.requires_action = requires;
        self
    }

    /// Set custom metadata for the step.
    #[must_use]
    pub fn metadata(mut self, data: impl Into<String>) -> Self {
        self.metadata = Some(data.into());
        self
    }

    /// Get the effective target bounds for highlighting.
    ///
    /// If `target_bounds` is set, returns that directly.
    /// Otherwise, the caller should look up bounds from the widget ID.
    #[must_use]
    pub fn effective_bounds(&self) -> Option<Rect> {
        self.target_bounds
    }
}

/// A guided tour consisting of multiple steps.
#[derive(Debug, Clone)]
pub struct Tour {
    /// Unique tour identifier.
    pub id: String,
    /// Human-readable tour name.
    pub name: String,
    /// Tour steps in order.
    pub steps: Vec<TourStep>,
    /// Whether the tour can be skipped.
    pub skippable: bool,
}

impl Tour {
    /// Create a new tour with the given ID.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: String::new(),
            steps: Vec::new(),
            skippable: true,
        }
    }

    /// Set the tour name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Add a step to the tour.
    #[must_use]
    pub fn add_step(mut self, step: TourStep) -> Self {
        self.steps.push(step);
        self
    }

    /// Set whether the tour can be skipped.
    #[must_use]
    pub fn skippable(mut self, skippable: bool) -> Self {
        self.skippable = skippable;
        self
    }

    /// Get the number of steps.
    #[must_use]
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// Get a step by index.
    #[must_use]
    pub fn get_step(&self, index: usize) -> Option<&TourStep> {
        self.steps.get(index)
    }
}

/// Completion status for a tour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionStatus {
    /// Tour has not been started.
    NotStarted,
    /// Tour was started but not finished.
    InProgress,
    /// Tour was completed successfully.
    Completed,
    /// Tour was skipped.
    Skipped,
}

/// Tracks completion status for multiple tours.
#[derive(Debug, Clone, Default)]
pub struct TourCompletion {
    /// Set of completed tour IDs.
    completed: HashSet<String>,
    /// Set of skipped tour IDs.
    skipped: HashSet<String>,
}

impl TourCompletion {
    /// Create a new tour completion tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check the completion status of a tour.
    #[must_use]
    pub fn status(&self, tour_id: &str) -> CompletionStatus {
        if self.completed.contains(tour_id) {
            CompletionStatus::Completed
        } else if self.skipped.contains(tour_id) {
            CompletionStatus::Skipped
        } else {
            CompletionStatus::NotStarted
        }
    }

    /// Mark a tour as completed.
    pub fn mark_completed(&mut self, tour_id: impl Into<String>) {
        let id = tour_id.into();
        self.skipped.remove(&id);
        self.completed.insert(id);
    }

    /// Mark a tour as skipped.
    pub fn mark_skipped(&mut self, tour_id: impl Into<String>) {
        let id = tour_id.into();
        self.completed.remove(&id);
        self.skipped.insert(id);
    }

    /// Reset a tour to not started.
    pub fn reset(&mut self, tour_id: &str) {
        self.completed.remove(tour_id);
        self.skipped.remove(tour_id);
    }

    /// Check if a tour has been completed.
    #[must_use]
    pub fn is_completed(&self, tour_id: &str) -> bool {
        self.completed.contains(tour_id)
    }

    /// Get all completed tour IDs.
    pub fn completed_tours(&self) -> impl Iterator<Item = &str> {
        self.completed.iter().map(String::as_str)
    }
}

/// Navigation action in a tour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TourAction {
    /// Move to the next step.
    Next,
    /// Move to the previous step.
    Back,
    /// Skip the entire tour.
    Skip,
    /// Complete the current step's required action.
    Complete,
}

/// Events emitted by the tour state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TourEvent {
    /// Tour started.
    Started { tour_id: String },
    /// Moved to a new step.
    StepChanged { step_index: usize },
    /// Tour was completed successfully.
    Completed { tour_id: String },
    /// Tour was skipped.
    Skipped { tour_id: String },
}

/// Active tour state and navigation.
#[derive(Debug, Clone)]
pub struct TourState {
    /// The active tour (if any).
    tour: Option<Tour>,
    /// Current step index.
    current_step: usize,
    /// Whether the current step's action has been completed.
    action_completed: bool,
    /// Pending event to emit.
    pending_event: Option<TourEvent>,
}

impl Default for TourState {
    fn default() -> Self {
        Self::new()
    }
}

impl TourState {
    /// Create a new tour state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tour: None,
            current_step: 0,
            action_completed: false,
            pending_event: None,
        }
    }

    /// Check if a tour is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.tour.is_some()
    }

    /// Get the active tour.
    #[must_use]
    pub fn tour(&self) -> Option<&Tour> {
        self.tour.as_ref()
    }

    /// Get the current step index.
    #[must_use]
    pub fn current_step_index(&self) -> usize {
        self.current_step
    }

    /// Get the current step.
    #[must_use]
    pub fn current_step(&self) -> Option<&TourStep> {
        self.tour.as_ref()?.get_step(self.current_step)
    }

    /// Get tour progress as (current, total).
    #[must_use]
    pub fn progress(&self) -> (usize, usize) {
        match &self.tour {
            Some(tour) => (self.current_step + 1, tour.step_count()),
            None => (0, 0),
        }
    }

    /// Start a tour.
    pub fn start(&mut self, tour: Tour) {
        let tour_id = tour.id.clone();
        self.tour = Some(tour);
        self.current_step = 0;
        self.action_completed = false;
        self.pending_event = Some(TourEvent::Started { tour_id });
    }

    /// Stop the current tour without completion.
    pub fn stop(&mut self) {
        self.tour = None;
        self.current_step = 0;
        self.action_completed = false;
    }

    /// Navigate the tour based on an action.
    ///
    /// Returns `true` if the action was handled.
    pub fn navigate(&mut self, action: TourAction) -> bool {
        let Some(tour) = &self.tour else {
            return false;
        };

        match action {
            TourAction::Next => {
                let step = tour.get_step(self.current_step);
                if let Some(s) = step
                    && s.requires_action
                    && !self.action_completed
                {
                    // Cannot proceed without completing the action.
                    return false;
                }

                if self.current_step + 1 < tour.step_count() {
                    self.current_step += 1;
                    self.action_completed = false;
                    self.pending_event = Some(TourEvent::StepChanged {
                        step_index: self.current_step,
                    });
                    true
                } else {
                    // Tour complete.
                    let tour_id = tour.id.clone();
                    self.pending_event = Some(TourEvent::Completed { tour_id });
                    self.tour = None;
                    self.current_step = 0;
                    true
                }
            }
            TourAction::Back => {
                if self.current_step > 0 {
                    self.current_step -= 1;
                    self.action_completed = false;
                    self.pending_event = Some(TourEvent::StepChanged {
                        step_index: self.current_step,
                    });
                    true
                } else {
                    false
                }
            }
            TourAction::Skip => {
                if tour.skippable {
                    let tour_id = tour.id.clone();
                    self.pending_event = Some(TourEvent::Skipped { tour_id });
                    self.tour = None;
                    self.current_step = 0;
                    true
                } else {
                    false
                }
            }
            TourAction::Complete => {
                self.action_completed = true;
                true
            }
        }
    }

    /// Mark the current step's action as completed.
    pub fn complete_action(&mut self) {
        self.action_completed = true;
    }

    /// Check if the current step's action is completed.
    #[must_use]
    pub fn is_action_completed(&self) -> bool {
        self.action_completed
    }

    /// Take the pending event (if any).
    pub fn take_event(&mut self) -> Option<TourEvent> {
        self.pending_event.take()
    }

    /// Check if we can go to the next step.
    #[must_use]
    pub fn can_go_next(&self) -> bool {
        let Some(tour) = &self.tour else {
            return false;
        };
        let step = tour.get_step(self.current_step);
        if let Some(s) = step
            && s.requires_action
            && !self.action_completed
        {
            return false;
        }
        true
    }

    /// Check if we can go back.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.tour.is_some() && self.current_step > 0
    }

    /// Check if we can skip.
    #[must_use]
    pub fn can_skip(&self) -> bool {
        self.tour.as_ref().map(|t| t.skippable).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tour() -> Tour {
        Tour::new("test-tour")
            .name("Test Tour")
            .add_step(TourStep::new("Step 1").content("First step"))
            .add_step(TourStep::new("Step 2").content("Second step"))
            .add_step(TourStep::new("Step 3").content("Third step"))
    }

    // ── Tour construction ────────────────────────────────────────────────

    #[test]
    fn tour_construction() {
        let tour = sample_tour();
        assert_eq!(tour.id, "test-tour");
        assert_eq!(tour.name, "Test Tour");
        assert_eq!(tour.step_count(), 3);
    }

    #[test]
    fn step_construction() {
        let step = TourStep::new("Welcome")
            .content("Hello!")
            .target_widget(42)
            .requires_action(true)
            .metadata("press Enter");

        assert_eq!(step.title, "Welcome");
        assert_eq!(step.content, "Hello!");
        assert_eq!(step.target_widget, Some(42));
        assert!(step.requires_action);
        assert_eq!(step.metadata, Some("press Enter".into()));
    }

    // ── TourState navigation ─────────────────────────────────────────────

    #[test]
    fn state_start_tour() {
        let mut state = TourState::new();
        assert!(!state.is_active());

        state.start(sample_tour());
        assert!(state.is_active());
        assert_eq!(state.current_step_index(), 0);
        assert_eq!(state.progress(), (1, 3));

        let event = state.take_event();
        assert_eq!(
            event,
            Some(TourEvent::Started {
                tour_id: "test-tour".into()
            })
        );
    }

    #[test]
    fn state_navigate_next() {
        let mut state = TourState::new();
        state.start(sample_tour());
        state.take_event(); // Clear start event

        assert!(state.navigate(TourAction::Next));
        assert_eq!(state.current_step_index(), 1);
        assert_eq!(
            state.take_event(),
            Some(TourEvent::StepChanged { step_index: 1 })
        );
    }

    #[test]
    fn state_navigate_back() {
        let mut state = TourState::new();
        state.start(sample_tour());
        state.navigate(TourAction::Next);
        state.take_event();

        assert!(state.navigate(TourAction::Back));
        assert_eq!(state.current_step_index(), 0);

        // Cannot go back from first step
        assert!(!state.navigate(TourAction::Back));
    }

    #[test]
    fn state_navigate_to_completion() {
        let mut state = TourState::new();
        state.start(sample_tour());
        state.take_event();

        state.navigate(TourAction::Next);
        state.navigate(TourAction::Next);
        let completed = state.navigate(TourAction::Next);

        assert!(completed);
        assert!(!state.is_active());
        assert_eq!(
            state.take_event(),
            Some(TourEvent::Completed {
                tour_id: "test-tour".into()
            })
        );
    }

    #[test]
    fn state_skip_tour() {
        let mut state = TourState::new();
        state.start(sample_tour());
        state.take_event();

        assert!(state.navigate(TourAction::Skip));
        assert!(!state.is_active());
        assert_eq!(
            state.take_event(),
            Some(TourEvent::Skipped {
                tour_id: "test-tour".into()
            })
        );
    }

    #[test]
    fn state_skip_disabled() {
        let mut state = TourState::new();
        state.start(sample_tour().skippable(false));
        state.take_event();

        assert!(!state.navigate(TourAction::Skip));
        assert!(state.is_active());
    }

    #[test]
    fn state_requires_action() {
        let tour = Tour::new("action-tour").add_step(
            TourStep::new("Action Step")
                .content("Do something")
                .requires_action(true),
        );

        let mut state = TourState::new();
        state.start(tour);
        state.take_event();

        // Cannot proceed without completing action
        assert!(!state.can_go_next());
        assert!(!state.navigate(TourAction::Next));

        // Complete the action
        state.complete_action();
        assert!(state.can_go_next());
        assert!(state.navigate(TourAction::Next));
    }

    // ── TourCompletion tracking ──────────────────────────────────────────

    #[test]
    fn completion_tracking() {
        let mut completion = TourCompletion::new();

        assert_eq!(completion.status("tour1"), CompletionStatus::NotStarted);

        completion.mark_completed("tour1");
        assert_eq!(completion.status("tour1"), CompletionStatus::Completed);
        assert!(completion.is_completed("tour1"));

        completion.mark_skipped("tour1");
        assert_eq!(completion.status("tour1"), CompletionStatus::Skipped);
        assert!(!completion.is_completed("tour1"));

        completion.reset("tour1");
        assert_eq!(completion.status("tour1"), CompletionStatus::NotStarted);
    }

    #[test]
    fn completion_iterator() {
        let mut completion = TourCompletion::new();
        completion.mark_completed("tour1");
        completion.mark_completed("tour2");
        completion.mark_skipped("tour3");

        let completed: Vec<_> = completion.completed_tours().collect();
        assert_eq!(completed.len(), 2);
        assert!(completed.contains(&"tour1"));
        assert!(completed.contains(&"tour2"));
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[test]
    fn navigate_no_active_tour() {
        let mut state = TourState::new();
        assert!(!state.navigate(TourAction::Next));
        assert!(!state.navigate(TourAction::Back));
        assert!(!state.navigate(TourAction::Skip));
    }

    #[test]
    fn empty_tour() {
        let tour = Tour::new("empty");
        assert_eq!(tour.step_count(), 0);
        assert!(tour.get_step(0).is_none());

        let mut state = TourState::new();
        state.start(tour);
        assert!(state.current_step().is_none());

        // Immediate completion on next
        assert!(state.navigate(TourAction::Next));
        assert!(!state.is_active());
    }

    #[test]
    fn step_bounds_override() {
        let bounds = Rect::new(10, 20, 30, 40);
        let step = TourStep::new("Test").target_widget(5).target_bounds(bounds);

        assert_eq!(step.target_widget, Some(5));
        assert_eq!(step.effective_bounds(), Some(bounds));
    }

    #[test]
    fn step_defaults_no_target_or_bounds() {
        let step = TourStep::new("Plain");
        assert_eq!(step.title, "Plain");
        assert!(step.content.is_empty());
        assert!(step.target_widget.is_none());
        assert!(step.target_bounds.is_none());
        assert!(!step.requires_action);
        assert!(step.metadata.is_none());
        assert!(step.effective_bounds().is_none());
    }

    #[test]
    fn tour_is_skippable_by_default() {
        let tour = Tour::new("id");
        assert!(tour.skippable);
    }

    #[test]
    fn tour_state_stop_ends_tour() {
        let mut state = TourState::new();
        state.start(sample_tour());
        state.take_event();
        assert!(state.is_active());

        state.stop();
        assert!(!state.is_active());
        assert_eq!(state.current_step_index(), 0);
        assert!(state.take_event().is_none());
    }

    #[test]
    fn progress_no_active_tour_returns_zero() {
        let state = TourState::new();
        assert_eq!(state.progress(), (0, 0));
    }

    #[test]
    fn can_go_back_no_tour() {
        let state = TourState::new();
        assert!(!state.can_go_back());
    }

    #[test]
    fn can_skip_no_tour() {
        let state = TourState::new();
        assert!(!state.can_skip());
    }

    #[test]
    fn completion_mark_completed_idempotent() {
        let mut completion = TourCompletion::new();
        completion.mark_completed("t");
        completion.mark_completed("t");
        assert_eq!(completion.status("t"), CompletionStatus::Completed);
        assert_eq!(completion.completed_tours().count(), 1);
    }

    #[test]
    fn navigate_complete_sets_flag() {
        let tour = Tour::new("a").add_step(TourStep::new("S").requires_action(true));
        let mut state = TourState::new();
        state.start(tour);
        state.take_event();

        assert!(!state.is_action_completed());
        assert!(state.navigate(TourAction::Complete));
        assert!(state.is_action_completed());
    }

    // --- Additional edge case tests (bd-m3gnl) ---

    #[test]
    fn tour_step_debug_clone() {
        let step = TourStep::new("Dbg").content("body").metadata("meta");
        let cloned = step.clone();
        assert_eq!(cloned.title, "Dbg");
        assert_eq!(cloned.metadata, Some("meta".into()));
        assert!(!format!("{:?}", step).is_empty());
    }

    #[test]
    fn tour_debug_clone() {
        let tour = sample_tour();
        let cloned = tour.clone();
        assert_eq!(cloned.id, "test-tour");
        assert_eq!(cloned.step_count(), 3);
        assert!(!format!("{:?}", tour).is_empty());
    }

    #[test]
    fn tour_get_step_valid_and_oob() {
        let tour = sample_tour();
        assert!(tour.get_step(0).is_some());
        assert_eq!(tour.get_step(0).unwrap().title, "Step 1");
        assert!(tour.get_step(2).is_some());
        assert!(tour.get_step(3).is_none());
        assert!(tour.get_step(usize::MAX).is_none());
    }

    #[test]
    fn completion_status_debug_clone_copy_eq() {
        let status = CompletionStatus::Completed;
        let copied = status;
        assert_eq!(status, copied);
        assert_ne!(CompletionStatus::Completed, CompletionStatus::Skipped);
        assert_ne!(CompletionStatus::InProgress, CompletionStatus::NotStarted);
        assert!(!format!("{:?}", status).is_empty());
    }

    #[test]
    fn tour_action_debug_clone_copy_eq() {
        let action = TourAction::Next;
        let copied = action;
        assert_eq!(action, copied);
        assert_ne!(TourAction::Next, TourAction::Back);
        assert_ne!(TourAction::Skip, TourAction::Complete);
        assert!(!format!("{:?}", action).is_empty());
    }

    #[test]
    fn tour_event_debug_clone_eq() {
        let event = TourEvent::Started {
            tour_id: "t".into(),
        };
        let cloned = event.clone();
        assert_eq!(event, cloned);
        assert_ne!(
            TourEvent::Started {
                tour_id: "a".into()
            },
            TourEvent::Completed {
                tour_id: "a".into()
            }
        );
        assert!(!format!("{:?}", event).is_empty());
    }

    #[test]
    fn tour_completion_debug_clone_default() {
        let comp = TourCompletion::default();
        let cloned = comp.clone();
        assert_eq!(cloned.status("x"), CompletionStatus::NotStarted);
        assert!(!format!("{:?}", comp).is_empty());
    }

    #[test]
    fn tour_state_default_equals_new() {
        let a = TourState::default();
        let b = TourState::new();
        assert!(!a.is_active());
        assert!(!b.is_active());
        assert_eq!(a.current_step_index(), b.current_step_index());
    }

    #[test]
    fn tour_state_debug_clone() {
        let mut state = TourState::new();
        state.start(sample_tour());
        let cloned = state.clone();
        assert!(cloned.is_active());
        assert!(!format!("{:?}", state).is_empty());
    }

    #[test]
    fn tour_state_tour_returns_ref() {
        let mut state = TourState::new();
        assert!(state.tour().is_none());
        state.start(sample_tour());
        let tour_ref = state.tour().unwrap();
        assert_eq!(tour_ref.id, "test-tour");
    }

    #[test]
    fn current_step_at_different_indices() {
        let mut state = TourState::new();
        state.start(sample_tour());
        assert_eq!(state.current_step().unwrap().title, "Step 1");

        state.navigate(TourAction::Next);
        assert_eq!(state.current_step().unwrap().title, "Step 2");

        state.navigate(TourAction::Next);
        assert_eq!(state.current_step().unwrap().title, "Step 3");
    }

    #[test]
    fn can_go_next_no_action_required() {
        let mut state = TourState::new();
        state.start(sample_tour());
        assert!(
            state.can_go_next(),
            "should be able to go next without action requirement"
        );
    }

    #[test]
    fn can_go_next_no_tour_returns_false() {
        let state = TourState::new();
        assert!(!state.can_go_next());
    }

    #[test]
    fn action_completed_resets_on_step_change() {
        let tour = Tour::new("t")
            .add_step(TourStep::new("S1").requires_action(true))
            .add_step(TourStep::new("S2").requires_action(true));
        let mut state = TourState::new();
        state.start(tour);
        state.take_event();

        state.complete_action();
        assert!(state.is_action_completed());

        state.navigate(TourAction::Next);
        assert!(
            !state.is_action_completed(),
            "action_completed should reset on step change"
        );
    }

    #[test]
    fn single_step_tour_completes_on_next() {
        let tour = Tour::new("single").add_step(TourStep::new("Only"));
        let mut state = TourState::new();
        state.start(tour);
        state.take_event();

        assert!(state.navigate(TourAction::Next));
        assert!(!state.is_active());
        assert_eq!(
            state.take_event(),
            Some(TourEvent::Completed {
                tour_id: "single".into()
            })
        );
    }

    #[test]
    fn mark_completed_removes_from_skipped() {
        let mut comp = TourCompletion::new();
        comp.mark_skipped("t");
        assert_eq!(comp.status("t"), CompletionStatus::Skipped);

        comp.mark_completed("t");
        assert_eq!(comp.status("t"), CompletionStatus::Completed);
    }

    #[test]
    fn reset_nonexistent_tour_is_noop() {
        let mut comp = TourCompletion::new();
        comp.reset("nonexistent"); // Should not panic
        assert_eq!(comp.status("nonexistent"), CompletionStatus::NotStarted);
    }

    #[test]
    fn restart_replaces_active_tour() {
        let mut state = TourState::new();
        state.start(sample_tour());
        state.navigate(TourAction::Next);

        // Start a different tour
        let tour2 = Tour::new("tour2").add_step(TourStep::new("New"));
        state.start(tour2);
        assert_eq!(state.current_step_index(), 0);
        assert_eq!(state.tour().unwrap().id, "tour2");
    }

    #[test]
    fn take_event_clears_pending() {
        let mut state = TourState::new();
        state.start(sample_tour());
        assert!(state.take_event().is_some());
        assert!(state.take_event().is_none(), "event should be consumed");
    }
}
