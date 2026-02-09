#![forbid(unsafe_code)]

//! Animation group: shared lifecycle management for multiple animations.
//!
//! An [`AnimationGroup`] holds a collection of named [`Animation`] handles
//! that can be controlled together (play all, cancel all) or individually.
//! The group itself implements [`Animation`], reporting the average progress
//! of all members.
//!
//! # Usage
//!
//! ```ignore
//! use std::time::Duration;
//! use ftui_core::animation::{Fade, AnimationGroup};
//!
//! let mut group = AnimationGroup::new()
//!     .add("fade_in", Fade::new(Duration::from_millis(300)))
//!     .add("fade_out", Fade::new(Duration::from_millis(500)));
//!
//! group.start_all();
//! group.tick(Duration::from_millis(100));
//! let fade_in_val = group.get("fade_in").unwrap().value();
//! ```
//!
//! # Invariants
//!
//! 1. Each member has a unique string label; duplicate labels overwrite.
//! 2. `start_all()` / `cancel_all()` affect every member simultaneously.
//! 3. `overall_progress()` returns the mean of all members' `value()`.
//! 4. An empty group has progress 0.0 and is immediately complete.
//! 5. `is_complete()` is true iff every member is complete.
//!
//! # Failure Modes
//!
//! - Empty group: `overall_progress()` returns 0.0, `is_complete()` returns true.
//! - Unknown label in `get()` / `get_mut()`: returns `None`.

use std::time::Duration;

use super::Animation;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A named animation member in the group.
struct GroupMember {
    label: String,
    animation: Box<dyn Animation>,
}

impl std::fmt::Debug for GroupMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupMember")
            .field("label", &self.label)
            .field("value", &self.animation.value())
            .field("complete", &self.animation.is_complete())
            .finish()
    }
}

/// A collection of named animations with shared lifecycle control.
///
/// Implements [`Animation`] — `value()` returns the average progress of all
/// members, and `is_complete()` is true when every member has finished.
pub struct AnimationGroup {
    members: Vec<GroupMember>,
}

impl std::fmt::Debug for AnimationGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnimationGroup")
            .field("count", &self.members.len())
            .field("progress", &self.overall_progress())
            .field("complete", &self.all_complete())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

impl AnimationGroup {
    /// Create an empty animation group.
    #[must_use]
    pub fn new() -> Self {
        Self {
            members: Vec::new(),
        }
    }

    /// Add a named animation to the group (builder pattern).
    ///
    /// If `label` already exists, the previous animation is replaced.
    #[must_use]
    pub fn add(mut self, label: &str, animation: impl Animation + 'static) -> Self {
        self.insert(label, Box::new(animation));
        self
    }

    /// Insert a named animation (mutating).
    ///
    /// If `label` already exists, the previous animation is replaced.
    pub fn insert(&mut self, label: &str, animation: Box<dyn Animation>) {
        if let Some(existing) = self.members.iter_mut().find(|m| m.label == label) {
            existing.animation = animation;
        } else {
            self.members.push(GroupMember {
                label: label.to_string(),
                animation,
            });
        }
    }

    /// Remove a named animation. Returns `true` if found and removed.
    pub fn remove(&mut self, label: &str) -> bool {
        let len_before = self.members.len();
        self.members.retain(|m| m.label != label);
        self.members.len() < len_before
    }
}

impl Default for AnimationGroup {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Lifecycle control
// ---------------------------------------------------------------------------

impl AnimationGroup {
    /// Reset all animations to their initial state.
    pub fn start_all(&mut self) {
        for member in &mut self.members {
            member.animation.reset();
        }
    }

    /// Reset all animations (alias for consistency with "cancel" semantics).
    pub fn cancel_all(&mut self) {
        self.start_all();
    }

    /// Number of animations in the group.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the group is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Whether every animation in the group has completed.
    #[inline]
    #[must_use]
    pub fn all_complete(&self) -> bool {
        self.members.is_empty() || self.members.iter().all(|m| m.animation.is_complete())
    }

    /// Average progress across all animations (0.0–1.0).
    ///
    /// Returns 0.0 for an empty group.
    #[inline]
    #[must_use]
    pub fn overall_progress(&self) -> f32 {
        if self.members.is_empty() {
            return 0.0;
        }
        let sum: f32 = self.members.iter().map(|m| m.animation.value()).sum();
        sum / self.members.len() as f32
    }

    /// Get a reference to a named animation's value.
    #[inline]
    #[must_use]
    pub fn get(&self, label: &str) -> Option<&dyn Animation> {
        self.members
            .iter()
            .find(|m| m.label == label)
            .map(|m| &*m.animation)
    }

    /// Get a mutable reference to a named animation.
    pub fn get_mut(&mut self, label: &str) -> Option<&mut Box<dyn Animation>> {
        for member in &mut self.members {
            if member.label == label {
                return Some(&mut member.animation);
            }
        }
        None
    }

    /// Get a reference to an animation by index.
    #[inline]
    #[must_use]
    pub fn get_at(&self, index: usize) -> Option<&dyn Animation> {
        self.members.get(index).map(|m| &*m.animation)
    }

    /// Iterator over (label, animation) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &dyn Animation)> {
        self.members
            .iter()
            .map(|m| (m.label.as_str(), &*m.animation))
    }

    /// Labels of all animations in the group.
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(|m| m.label.as_str())
    }
}

// ---------------------------------------------------------------------------
// Animation trait implementation
// ---------------------------------------------------------------------------

impl Animation for AnimationGroup {
    fn tick(&mut self, dt: Duration) {
        for member in &mut self.members {
            if !member.animation.is_complete() {
                member.animation.tick(dt);
            }
        }
    }

    fn is_complete(&self) -> bool {
        self.all_complete()
    }

    fn value(&self) -> f32 {
        self.overall_progress()
    }

    fn reset(&mut self) {
        for member in &mut self.members {
            member.animation.reset();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::Fade;

    const MS_100: Duration = Duration::from_millis(100);
    const MS_200: Duration = Duration::from_millis(200);
    const MS_300: Duration = Duration::from_millis(300);
    const MS_500: Duration = Duration::from_millis(500);
    const SEC_1: Duration = Duration::from_secs(1);

    #[test]
    fn empty_group() {
        let group = AnimationGroup::new();
        assert!(group.is_empty());
        assert_eq!(group.len(), 0);
        assert!(group.all_complete());
        assert_eq!(group.overall_progress(), 0.0);
    }

    #[test]
    fn add_and_tick() {
        let mut group = AnimationGroup::new()
            .add("a", Fade::new(MS_500))
            .add("b", Fade::new(SEC_1));

        assert_eq!(group.len(), 2);
        assert!(!group.all_complete());

        group.tick(MS_500);
        // "a" should be complete, "b" at 50%
        assert!(group.get("a").unwrap().is_complete());
        assert!(!group.get("b").unwrap().is_complete());
        assert!((group.get("b").unwrap().value() - 0.5).abs() < 0.02);
    }

    #[test]
    fn overall_progress() {
        let mut group = AnimationGroup::new()
            .add("short", Fade::new(MS_200))
            .add("long", Fade::new(SEC_1));

        group.tick(MS_200);
        // short=1.0, long=0.2 → avg = 0.6
        assert!((group.overall_progress() - 0.6).abs() < 0.02);
    }

    #[test]
    fn all_complete_when_all_done() {
        let mut group = AnimationGroup::new()
            .add("a", Fade::new(MS_100))
            .add("b", Fade::new(MS_200));

        group.tick(MS_200);
        assert!(group.all_complete());
        assert!(group.is_complete());
    }

    #[test]
    fn start_all_resets_everything() {
        let mut group = AnimationGroup::new().add("a", Fade::new(MS_100));

        group.tick(MS_100);
        assert!(group.all_complete());

        group.start_all();
        assert!(!group.all_complete());
        assert!((group.get("a").unwrap().value() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cancel_all_resets() {
        let mut group = AnimationGroup::new().add("a", Fade::new(MS_100));

        group.tick(MS_100);
        group.cancel_all();
        assert!(!group.all_complete());
    }

    #[test]
    fn duplicate_label_replaces() {
        let group = AnimationGroup::new()
            .add("x", Fade::new(MS_100))
            .add("x", Fade::new(SEC_1));

        assert_eq!(group.len(), 1);
        // The second (1s) fade replaced the first (100ms)
    }

    #[test]
    fn remove_animation() {
        let mut group = AnimationGroup::new()
            .add("a", Fade::new(MS_100))
            .add("b", Fade::new(MS_200));

        assert!(group.remove("a"));
        assert_eq!(group.len(), 1);
        assert!(group.get("a").is_none());
        assert!(group.get("b").is_some());

        assert!(!group.remove("nonexistent"));
    }

    #[test]
    fn get_at_index() {
        let group = AnimationGroup::new()
            .add("a", Fade::new(MS_100))
            .add("b", Fade::new(MS_200));

        assert!(group.get_at(0).is_some());
        assert!(group.get_at(1).is_some());
        assert!(group.get_at(2).is_none());
    }

    #[test]
    fn get_mut_allows_individual_tick() {
        let mut group = AnimationGroup::new()
            .add("a", Fade::new(SEC_1))
            .add("b", Fade::new(SEC_1));

        // Tick only "a" individually
        if let Some(a) = group.get_mut("a") {
            a.tick(MS_500);
        }
        assert!((group.get("a").unwrap().value() - 0.5).abs() < 0.02);
        assert!((group.get("b").unwrap().value() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn labels_iterator() {
        let group = AnimationGroup::new()
            .add("alpha", Fade::new(MS_100))
            .add("beta", Fade::new(MS_100));

        let labels: Vec<&str> = group.labels().collect();
        assert_eq!(labels, vec!["alpha", "beta"]);
    }

    #[test]
    fn iter_pairs() {
        let group = AnimationGroup::new()
            .add("a", Fade::new(MS_100))
            .add("b", Fade::new(MS_100));

        let pairs: Vec<_> = group.iter().collect();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "a");
        assert_eq!(pairs[1].0, "b");
    }

    #[test]
    fn animation_trait_reset() {
        let mut group = AnimationGroup::new().add("a", Fade::new(MS_100));

        group.tick(MS_100);
        assert!(group.is_complete());

        group.reset();
        assert!(!group.is_complete());
    }

    #[test]
    fn animation_trait_value_matches_overall() {
        let mut group = AnimationGroup::new()
            .add("a", Fade::new(MS_300))
            .add("b", Fade::new(SEC_1));

        group.tick(MS_300);
        assert!((group.value() - group.overall_progress()).abs() < f32::EPSILON);
    }

    #[test]
    fn skips_completed_on_tick() {
        let mut group = AnimationGroup::new()
            .add("a", Fade::new(MS_100))
            .add("b", Fade::new(SEC_1));

        group.tick(MS_200);
        // "a" completed at 100ms, subsequent ticks should skip it
        let a_val = group.get("a").unwrap().value();
        group.tick(MS_100);
        // "a" value should still be 1.0 (not ticked further)
        assert!((group.get("a").unwrap().value() - a_val).abs() < f32::EPSILON);
    }

    #[test]
    fn debug_format() {
        let group = AnimationGroup::new().add("a", Fade::new(MS_100));

        let dbg = format!("{:?}", group);
        assert!(dbg.contains("AnimationGroup"));
        assert!(dbg.contains("count"));
    }

    #[test]
    fn insert_mutating() {
        let mut group = AnimationGroup::new();
        group.insert("x", Box::new(Fade::new(MS_100)));
        assert_eq!(group.len(), 1);
        assert!(group.get("x").is_some());
    }
}
