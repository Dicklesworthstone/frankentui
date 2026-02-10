//! Opt-in trait for widgets with persistable state.
//!
//! The [`Stateful`] trait defines a contract for widgets that can save and
//! restore their state across sessions or configuration changes. It is
//! orthogonal to [`StatefulWidget`](super::StatefulWidget) — a widget can
//! implement both (render-time state mutation + persistence) or just one.
//!
//! # Design Invariants
//!
//! 1. **Round-trip fidelity**: `restore_state(save_state())` must produce an
//!    equivalent observable state. Fields that are purely derived (e.g., cached
//!    layout) may differ, but user-facing state (scroll position, selection,
//!    expanded nodes) must survive the round trip.
//!
//! 2. **Graceful version mismatch**: When [`VersionedState`] detects a version
//!    mismatch (`stored.version != T::state_version()`), the caller should fall
//!    back to `T::State::default()` rather than panic. Migration logic belongs
//!    in the downstream state migration system (bd-30g1.5).
//!
//! 3. **Key uniqueness**: Two distinct widget instances must produce distinct
//!    [`StateKey`] values. The `(widget_type, instance_id)` pair is the primary
//!    uniqueness invariant.
//!
//! 4. **No side effects**: `save_state` must be a pure read; `restore_state`
//!    must only mutate `self` (no I/O, no global state).
//!
//! # Failure Modes
//!
//! | Failure | Cause | Fallback |
//! |---------|-------|----------|
//! | Deserialization error | Schema drift, corrupt data | Use `Default::default()` |
//! | Version mismatch | Widget upgraded | Use `Default::default()` |
//! | Missing state | First run, key changed | Use `Default::default()` |
//! | Duplicate key | Bug in `state_key()` impl | Last-write-wins (logged) |
//!
//! # Feature Gate
//!
//! This module is always available, but the serde-based [`VersionedState`]
//! wrapper requires the `state-persistence` feature for serialization support.

use core::fmt;
use core::hash::{Hash, Hasher};

/// Unique identifier for a widget's persisted state.
///
/// A `StateKey` is the `(widget_type, instance_id)` pair that maps a widget
/// instance to its stored state blob. Widget type is a `&'static str` (cheap
/// to copy, no allocation) while instance id is an owned `String` to support
/// dynamic widget trees.
///
/// # Construction
///
/// ```
/// # use ftui_widgets::stateful::StateKey;
/// // Explicit
/// let key = StateKey::new("ScrollView", "main-content");
///
/// // From a widget-tree path
/// let key = StateKey::from_path(&["app", "sidebar", "tree"]);
/// assert_eq!(key.instance_id, "app/sidebar/tree");
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateKey {
    /// The widget type name (e.g., `"ScrollView"`, `"TreeView"`).
    pub widget_type: &'static str,
    /// Instance-unique identifier within a widget tree.
    pub instance_id: String,
}

impl StateKey {
    /// Create a new state key from a widget type and instance id.
    #[must_use]
    pub fn new(widget_type: &'static str, id: impl Into<String>) -> Self {
        Self {
            widget_type,
            instance_id: id.into(),
        }
    }

    /// Build a state key from a path of widget-tree segments.
    ///
    /// Segments are joined with `/` to form the instance id.
    /// The widget type is derived from the last segment.
    ///
    /// # Panics
    ///
    /// Panics if `path` is empty.
    #[must_use]
    pub fn from_path(path: &[&str]) -> Self {
        assert!(
            !path.is_empty(),
            "StateKey::from_path requires a non-empty path"
        );
        let widget_type_str = path.last().expect("checked non-empty");
        // We need a &'static str for widget_type. Since the caller passes &str
        // slices that may or may not be 'static, we leak a copy. This is fine
        // because state keys are created once and live for the program lifetime.
        let widget_type: &'static str = Box::leak((*widget_type_str).to_owned().into_boxed_str());
        Self {
            widget_type,
            instance_id: path.join("/"),
        }
    }

    /// Canonical string representation: `"widget_type::instance_id"`.
    #[must_use]
    pub fn canonical(&self) -> String {
        format!("{}::{}", self.widget_type, self.instance_id)
    }
}

impl Hash for StateKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.widget_type.hash(state);
        self.instance_id.hash(state);
    }
}

impl fmt::Display for StateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.widget_type, self.instance_id)
    }
}

/// Opt-in trait for widgets with persistable state.
///
/// Implementing this trait signals that a widget's user-facing state can be
/// serialized, stored, and later restored. This is used by the state registry
/// (bd-30g1.2) to persist widget state across sessions.
///
/// # Relationship to `StatefulWidget`
///
/// - [`StatefulWidget`](super::StatefulWidget): render-time mutable state (scroll clamping, layout cache).
/// - [`Stateful`]: persistence contract (save/restore across sessions).
///
/// A widget can implement both when its render-time state is also worth persisting.
///
/// # Example
///
/// ```ignore
/// use serde::{Serialize, Deserialize};
/// use ftui_widgets::stateful::{Stateful, StateKey};
///
/// #[derive(Serialize, Deserialize, Default)]
/// struct ScrollViewPersist {
///     scroll_offset: u16,
/// }
///
/// impl Stateful for ScrollView {
///     type State = ScrollViewPersist;
///
///     fn state_key(&self) -> StateKey {
///         StateKey::new("ScrollView", &self.id)
///     }
///
///     fn save_state(&self) -> Self::State {
///         ScrollViewPersist { scroll_offset: self.offset }
///     }
///
///     fn restore_state(&mut self, state: Self::State) {
///         self.offset = state.scroll_offset.min(self.max_offset());
///     }
/// }
/// ```
pub trait Stateful: Sized {
    /// The state type that gets persisted.
    ///
    /// Must implement `Default` so missing/corrupt state degrades gracefully.
    type State: Default;

    /// Unique key identifying this widget instance.
    ///
    /// Two distinct widget instances **must** return distinct keys.
    fn state_key(&self) -> StateKey;

    /// Extract current state for persistence.
    ///
    /// This must be a pure read — no side effects, no I/O.
    fn save_state(&self) -> Self::State;

    /// Restore state from persistence.
    ///
    /// Implementations should clamp restored values to valid ranges
    /// (e.g., scroll offset ≤ max offset) rather than trusting stored data.
    fn restore_state(&mut self, state: Self::State);

    /// State schema version for forward-compatible migrations.
    ///
    /// Bump this when the `State` type's serialized form changes in a
    /// backwards-incompatible way. The state registry will discard stored
    /// state with a mismatched version and fall back to `Default`.
    fn state_version() -> u32 {
        1
    }
}

/// Version-tagged wrapper for serialized widget state.
///
/// When persisting state, the registry wraps the raw state in this envelope
/// so it can detect schema version mismatches on restore.
///
/// # Serialization
///
/// With the `state-persistence` feature enabled, `VersionedState` derives
/// `Serialize` and `Deserialize`. Without the feature, it is a plain struct
/// usable for in-memory versioning.
#[derive(Clone, Debug)]
#[cfg_attr(
    feature = "state-persistence",
    derive(serde::Serialize, serde::Deserialize)
)]
pub struct VersionedState<S> {
    /// Schema version (from `Stateful::state_version()`).
    pub version: u32,
    /// The actual state payload.
    pub data: S,
}

impl<S> VersionedState<S> {
    /// Wrap state with its current version tag.
    #[must_use]
    pub fn new(version: u32, data: S) -> Self {
        Self { version, data }
    }

    /// Pack a widget's state into a versioned envelope.
    pub fn pack<W: Stateful<State = S>>(widget: &W) -> Self {
        Self {
            version: W::state_version(),
            data: widget.save_state(),
        }
    }

    /// Attempt to unpack, returning `None` if the version does not match
    /// the widget's current `state_version()`.
    #[must_use = "use the unpacked state (if any)"]
    pub fn unpack<W: Stateful<State = S>>(self) -> Option<S> {
        if self.version == W::state_version() {
            Some(self.data)
        } else {
            None
        }
    }

    /// Unpack with fallback: returns the stored data if versions match,
    /// otherwise returns `S::default()`.
    pub fn unpack_or_default<W: Stateful<State = S>>(self) -> S
    where
        S: Default,
    {
        if self.version == W::state_version() {
            self.data
        } else {
            S::default()
        }
    }
}

impl<S: Default> Default for VersionedState<S> {
    fn default() -> Self {
        Self {
            version: 1,
            data: S::default(),
        }
    }
}

// ============================================================================
// State Migration System (bd-30g1.5)
// ============================================================================

/// Error that can occur during state migration.
#[derive(Debug, Clone)]
pub enum MigrationError {
    /// No migration path exists from source to target version.
    NoPathFound { from: u32, to: u32 },
    /// A migration function returned an error.
    MigrationFailed { from: u32, to: u32, message: String },
    /// Version numbers are invalid (e.g., target < source).
    InvalidVersionRange { from: u32, to: u32 },
}

impl core::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoPathFound { from, to } => {
                write!(f, "no migration path from version {} to {}", from, to)
            }
            Self::MigrationFailed { from, to, message } => {
                write!(f, "migration from {} to {} failed: {}", from, to, message)
            }
            Self::InvalidVersionRange { from, to } => {
                write!(f, "invalid version range: {} to {}", from, to)
            }
        }
    }
}

/// A single-step migration from version N to version N+1.
///
/// Migrations are always forward-only and increment by exactly one version.
/// This ensures a clear, auditable upgrade path.
///
/// # Example
///
/// ```ignore
/// // Migration from v1 ScrollState to v2 ScrollState (adds new field)
/// struct ScrollStateV1ToV2;
///
/// impl StateMigration for ScrollStateV1ToV2 {
///     type OldState = ScrollStateV1;
///     type NewState = ScrollStateV2;
///
///     fn from_version(&self) -> u32 { 1 }
///     fn to_version(&self) -> u32 { 2 }
///
///     fn migrate(&self, old: ScrollStateV1) -> Result<ScrollStateV2, String> {
///         Ok(ScrollStateV2 {
///             scroll_offset: old.scroll_offset,
///             scroll_velocity: 0.0, // New field, default value
///         })
///     }
/// }
/// ```
#[allow(clippy::wrong_self_convention)]
pub trait StateMigration {
    /// The state type before migration.
    type OldState;
    /// The state type after migration.
    type NewState;

    /// Source version this migration transforms from.
    fn from_version(&self) -> u32;

    /// Target version this migration produces.
    /// Must equal `from_version() + 1`.
    fn to_version(&self) -> u32;

    /// Perform the migration.
    ///
    /// Returns `Err` with a message if the migration cannot be performed.
    fn migrate(&self, old: Self::OldState) -> Result<Self::NewState, String>;
}

/// A type-erased migration step for use in migration chains.
///
/// This allows storing migrations with different types in a single collection.
#[allow(clippy::wrong_self_convention)]
pub trait ErasedMigration<S>: Send + Sync {
    /// Source version.
    fn from_version(&self) -> u32;
    /// Target version.
    fn to_version(&self) -> u32;
    /// Perform migration on boxed state, returning boxed result.
    fn migrate_erased(
        &self,
        old: Box<dyn core::any::Any + Send>,
    ) -> Result<Box<dyn core::any::Any + Send>, String>;
}

/// A chain of migrations that can upgrade state through multiple versions.
///
/// # Design
///
/// The migration chain executes migrations in sequence, starting from the
/// stored version and ending at the current version. Each step increments
/// the version by exactly one.
///
/// # Example
///
/// ```ignore
/// let mut chain = MigrationChain::<FinalState>::new();
/// chain.register(Box::new(V1ToV2Migration));
/// chain.register(Box::new(V2ToV3Migration));
///
/// // Migrate from v1 to v3 (current)
/// let result = chain.migrate_to_current(v1_state, 1, 3);
/// ```
pub struct MigrationChain<S> {
    /// Migrations indexed by their `from_version`.
    migrations: std::collections::HashMap<u32, Box<dyn ErasedMigration<S>>>,
}

impl<S: 'static> MigrationChain<S> {
    /// Create an empty migration chain.
    #[must_use]
    pub fn new() -> Self {
        Self {
            migrations: std::collections::HashMap::new(),
        }
    }

    /// Register a migration step.
    ///
    /// # Panics
    ///
    /// Panics if a migration for the same `from_version` is already registered.
    pub fn register(&mut self, migration: Box<dyn ErasedMigration<S>>) {
        let from = migration.from_version();
        let to = migration.to_version();
        assert_eq!(
            to,
            from + 1,
            "migration must increment version by exactly 1 (got {} -> {})",
            from,
            to
        );
        assert!(
            !self.migrations.contains_key(&from),
            "migration for version {} already registered",
            from
        );
        self.migrations.insert(from, migration);
    }

    /// Check if a migration path exists from `from_version` to `to_version`.
    #[must_use]
    pub fn has_path(&self, from_version: u32, to_version: u32) -> bool {
        if from_version >= to_version {
            return from_version == to_version;
        }
        let mut current = from_version;
        while current < to_version {
            if !self.migrations.contains_key(&current) {
                return false;
            }
            current += 1;
        }
        true
    }

    /// Attempt to migrate state from `from_version` to `to_version`.
    ///
    /// Returns `Ok(migrated_state)` on success, or `Err` if migration fails.
    pub fn migrate(
        &self,
        state: Box<dyn core::any::Any + Send>,
        from_version: u32,
        to_version: u32,
    ) -> Result<Box<dyn core::any::Any + Send>, MigrationError> {
        if from_version > to_version {
            return Err(MigrationError::InvalidVersionRange {
                from: from_version,
                to: to_version,
            });
        }
        if from_version == to_version {
            return Ok(state);
        }

        let mut current_state = state;
        let mut current_version = from_version;

        while current_version < to_version {
            let migration =
                self.migrations
                    .get(&current_version)
                    .ok_or(MigrationError::NoPathFound {
                        from: current_version,
                        to: to_version,
                    })?;

            current_state = migration.migrate_erased(current_state).map_err(|msg| {
                MigrationError::MigrationFailed {
                    from: current_version,
                    to: current_version + 1,
                    message: msg,
                }
            })?;

            current_version += 1;
        }

        Ok(current_state)
    }
}

impl<S: 'static> Default for MigrationChain<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of attempting state restoration with migration.
#[derive(Debug)]
pub enum RestoreResult<S> {
    /// State was restored directly (versions matched).
    Direct(S),
    /// State was successfully migrated from an older version.
    Migrated { state: S, from_version: u32 },
    /// Migration failed; falling back to default state.
    DefaultFallback { error: MigrationError, default: S },
}

impl<S> RestoreResult<S> {
    /// Extract the state value, regardless of how it was obtained.
    pub fn into_state(self) -> S {
        match self {
            Self::Direct(s) | Self::Migrated { state: s, .. } => s,
            Self::DefaultFallback { default, .. } => default,
        }
    }

    /// Returns `true` if the state was migrated.
    #[must_use]
    pub fn was_migrated(&self) -> bool {
        matches!(self, Self::Migrated { .. })
    }

    /// Returns `true` if we fell back to default.
    #[must_use]
    pub fn is_fallback(&self) -> bool {
        matches!(self, Self::DefaultFallback { .. })
    }
}

impl<S> VersionedState<S> {
    /// Attempt to unpack with migration support.
    ///
    /// If the stored version doesn't match the current version, attempts to
    /// migrate through the provided chain. Falls back to default on failure.
    ///
    /// # Type Parameters
    ///
    /// - `W`: The widget type that implements `Stateful<State = S>`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let chain = MigrationChain::new();
    /// // ... register migrations ...
    ///
    /// let versioned = load_state_from_disk();
    /// let result = versioned.unpack_with_migration::<MyWidget>(&chain);
    /// let state = result.into_state();
    /// ```
    pub fn unpack_with_migration<W>(self, chain: &MigrationChain<S>) -> RestoreResult<S>
    where
        W: Stateful<State = S>,
        S: Default + 'static + Send,
    {
        let current_version = W::state_version();

        if self.version == current_version {
            return RestoreResult::Direct(self.data);
        }

        // Try migration
        let boxed: Box<dyn core::any::Any + Send> = Box::new(self.data);
        match chain.migrate(boxed, self.version, current_version) {
            Ok(migrated) => {
                if let Ok(state) = migrated.downcast::<S>() {
                    RestoreResult::Migrated {
                        state: *state,
                        from_version: self.version,
                    }
                } else {
                    // Type mismatch after migration (shouldn't happen with correct chain)
                    RestoreResult::DefaultFallback {
                        error: MigrationError::MigrationFailed {
                            from: self.version,
                            to: current_version,
                            message: "type mismatch after migration".to_string(),
                        },
                        default: S::default(),
                    }
                }
            }
            Err(e) => RestoreResult::DefaultFallback {
                error: e,
                default: S::default(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test widget ─────────────────────────────────────────────────

    #[derive(Default)]
    struct TestScrollView {
        id: String,
        offset: u16,
        max: u16,
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    struct ScrollState {
        scroll_offset: u16,
    }

    impl Stateful for TestScrollView {
        type State = ScrollState;

        fn state_key(&self) -> StateKey {
            StateKey::new("ScrollView", &self.id)
        }

        fn save_state(&self) -> ScrollState {
            ScrollState {
                scroll_offset: self.offset,
            }
        }

        fn restore_state(&mut self, state: ScrollState) {
            self.offset = state.scroll_offset.min(self.max);
        }
    }

    // ── Another test widget with version 2 ──────────────────────────

    #[derive(Default)]
    struct TestTreeView {
        id: String,
        expanded: Vec<u32>,
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    struct TreeState {
        expanded_nodes: Vec<u32>,
        collapse_all_on_blur: bool, // added in v2
    }

    impl Stateful for TestTreeView {
        type State = TreeState;

        fn state_key(&self) -> StateKey {
            StateKey::new("TreeView", &self.id)
        }

        fn save_state(&self) -> TreeState {
            TreeState {
                expanded_nodes: self.expanded.clone(),
                collapse_all_on_blur: false,
            }
        }

        fn restore_state(&mut self, state: TreeState) {
            self.expanded = state.expanded_nodes;
        }

        fn state_version() -> u32 {
            2
        }
    }

    // ── StateKey tests ──────────────────────────────────────────────

    #[test]
    fn state_key_new() {
        let key = StateKey::new("ScrollView", "main");
        assert_eq!(key.widget_type, "ScrollView");
        assert_eq!(key.instance_id, "main");
    }

    #[test]
    fn state_key_from_path() {
        let key = StateKey::from_path(&["app", "sidebar", "tree"]);
        assert_eq!(key.instance_id, "app/sidebar/tree");
        assert_eq!(key.widget_type, "tree");
    }

    #[test]
    #[should_panic(expected = "non-empty path")]
    fn state_key_from_empty_path_panics() {
        let _ = StateKey::from_path(&[]);
    }

    #[test]
    fn state_key_uniqueness() {
        let a = StateKey::new("ScrollView", "main");
        let b = StateKey::new("ScrollView", "sidebar");
        let c = StateKey::new("TreeView", "main");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn state_key_equality() {
        let a = StateKey::new("ScrollView", "main");
        let b = StateKey::new("ScrollView", "main");
        assert_eq!(a, b);
    }

    #[test]
    fn state_key_hash_consistency() {
        use std::collections::hash_map::DefaultHasher;

        let a = StateKey::new("ScrollView", "main");
        let b = StateKey::new("ScrollView", "main");

        let hash = |key: &StateKey| {
            let mut h = DefaultHasher::new();
            key.hash(&mut h);
            h.finish()
        };
        assert_eq!(hash(&a), hash(&b));
    }

    #[test]
    fn state_key_display() {
        let key = StateKey::new("ScrollView", "main");
        assert_eq!(key.to_string(), "ScrollView::main");
    }

    #[test]
    fn state_key_canonical() {
        let key = StateKey::new("ScrollView", "main");
        assert_eq!(key.canonical(), "ScrollView::main");
    }

    // ── Save/restore round-trip tests ───────────────────────────────

    #[test]
    fn save_restore_round_trip() {
        let mut widget = TestScrollView {
            id: "content".into(),
            offset: 42,
            max: 100,
        };

        let saved = widget.save_state();
        assert_eq!(saved.scroll_offset, 42);

        widget.offset = 0; // reset
        widget.restore_state(saved);
        assert_eq!(widget.offset, 42);
    }

    #[test]
    fn restore_clamps_to_valid_range() {
        let mut widget = TestScrollView {
            id: "content".into(),
            offset: 0,
            max: 10,
        };

        // Stored state exceeds current max
        widget.restore_state(ScrollState { scroll_offset: 999 });
        assert_eq!(widget.offset, 10);
    }

    #[test]
    fn default_state_on_missing() {
        let mut widget = TestScrollView {
            id: "new".into(),
            offset: 5,
            max: 100,
        };

        widget.restore_state(ScrollState::default());
        assert_eq!(widget.offset, 0);
    }

    // ── Version tests ───────────────────────────────────────────────

    #[test]
    fn default_state_version_is_one() {
        assert_eq!(TestScrollView::state_version(), 1);
    }

    #[test]
    fn custom_state_version() {
        assert_eq!(TestTreeView::state_version(), 2);
    }

    // ── VersionedState tests ────────────────────────────────────────

    #[test]
    fn versioned_state_pack_unpack() {
        let widget = TestScrollView {
            id: "main".into(),
            offset: 77,
            max: 100,
        };

        let packed = VersionedState::pack(&widget);
        assert_eq!(packed.version, 1);
        assert_eq!(packed.data.scroll_offset, 77);

        let unpacked = packed.unpack::<TestScrollView>();
        assert!(unpacked.is_some());
        assert_eq!(unpacked.unwrap().scroll_offset, 77);
    }

    #[test]
    fn versioned_state_version_mismatch_returns_none() {
        // Simulate stored state from version 1, but widget expects version 2
        let stored = VersionedState::<TreeState> {
            version: 1,
            data: TreeState::default(),
        };

        let result = stored.unpack::<TestTreeView>();
        assert!(result.is_none());
    }

    #[test]
    fn versioned_state_unpack_or_default_on_mismatch() {
        let stored = VersionedState::<TreeState> {
            version: 1,
            data: TreeState {
                expanded_nodes: vec![1, 2, 3],
                collapse_all_on_blur: true,
            },
        };

        let result = stored.unpack_or_default::<TestTreeView>();
        // Should return default because version 1 != expected 2
        assert_eq!(result, TreeState::default());
    }

    #[test]
    fn versioned_state_unpack_or_default_on_match() {
        let stored = VersionedState::<ScrollState> {
            version: 1,
            data: ScrollState { scroll_offset: 55 },
        };

        let result = stored.unpack_or_default::<TestScrollView>();
        assert_eq!(result.scroll_offset, 55);
    }

    #[test]
    fn versioned_state_default() {
        let vs = VersionedState::<ScrollState>::default();
        assert_eq!(vs.version, 1);
        assert_eq!(vs.data, ScrollState::default());
    }

    // ── Migration System tests ─────────────────────────────────────────

    #[test]
    fn migration_error_display() {
        let err = MigrationError::NoPathFound { from: 1, to: 3 };
        assert_eq!(err.to_string(), "no migration path from version 1 to 3");

        let err = MigrationError::MigrationFailed {
            from: 2,
            to: 3,
            message: "data corrupt".into(),
        };
        assert_eq!(
            err.to_string(),
            "migration from 2 to 3 failed: data corrupt"
        );

        let err = MigrationError::InvalidVersionRange { from: 5, to: 2 };
        assert_eq!(err.to_string(), "invalid version range: 5 to 2");
    }

    #[test]
    fn migration_chain_new_is_empty() {
        let chain = MigrationChain::<ScrollState>::new();
        assert!(!chain.has_path(1, 2));
    }

    // Test migration from v1 ScrollState (just offset) to v2 (with hypothetical field)
    #[derive(Debug, Clone, Default)]
    struct ScrollStateV1 {
        scroll_offset: u16,
    }

    #[derive(Debug, Clone, Default)]
    struct ScrollStateV2 {
        scroll_offset: u16,
        velocity: f32, // Added in v2
    }

    struct V1ToV2Migration;

    impl ErasedMigration<ScrollStateV2> for V1ToV2Migration {
        fn from_version(&self) -> u32 {
            1
        }
        fn to_version(&self) -> u32 {
            2
        }
        fn migrate_erased(
            &self,
            old: Box<dyn core::any::Any + Send>,
        ) -> Result<Box<dyn core::any::Any + Send>, String> {
            let v1 = old
                .downcast::<ScrollStateV1>()
                .map_err(|_| "invalid state type")?;
            Ok(Box::new(ScrollStateV2 {
                scroll_offset: v1.scroll_offset,
                velocity: 0.0,
            }))
        }
    }

    #[test]
    fn migration_chain_register_and_has_path() {
        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(V1ToV2Migration));

        assert!(chain.has_path(1, 2));
        assert!(chain.has_path(1, 1)); // Same version is valid
        assert!(chain.has_path(2, 2)); // Same version is valid
        assert!(!chain.has_path(1, 3)); // No migration to v3
    }

    #[test]
    #[should_panic(expected = "migration must increment version by exactly 1")]
    fn migration_chain_rejects_non_sequential_migration() {
        struct BadMigration;
        impl ErasedMigration<ScrollStateV2> for BadMigration {
            fn from_version(&self) -> u32 {
                1
            }
            fn to_version(&self) -> u32 {
                3
            } // Skips v2!
            fn migrate_erased(
                &self,
                _: Box<dyn core::any::Any + Send>,
            ) -> Result<Box<dyn core::any::Any + Send>, String> {
                unreachable!()
            }
        }

        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(BadMigration));
    }

    #[test]
    #[should_panic(expected = "migration for version 1 already registered")]
    fn migration_chain_rejects_duplicate_registration() {
        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(V1ToV2Migration));
        chain.register(Box::new(V1ToV2Migration)); // Duplicate!
    }

    #[test]
    fn migration_chain_migrate_success() {
        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(V1ToV2Migration));

        let old_state = Box::new(ScrollStateV1 { scroll_offset: 42 });
        let result = chain.migrate(old_state, 1, 2);

        assert!(result.is_ok());
        let migrated = result
            .unwrap()
            .downcast::<ScrollStateV2>()
            .expect("should be ScrollStateV2");
        assert_eq!(migrated.scroll_offset, 42);
        assert_eq!(migrated.velocity, 0.0);
    }

    #[test]
    fn migration_chain_migrate_same_version() {
        let chain = MigrationChain::<ScrollStateV2>::new();
        let state = Box::new(ScrollStateV2 {
            scroll_offset: 10,
            velocity: 1.5,
        });

        let result = chain.migrate(state, 2, 2);
        assert!(result.is_ok());
    }

    #[test]
    fn migration_chain_migrate_no_path() {
        let chain = MigrationChain::<ScrollStateV2>::new();
        let state: Box<dyn core::any::Any + Send> = Box::new(ScrollStateV1 { scroll_offset: 0 });

        let result = chain.migrate(state, 1, 2);
        assert!(matches!(
            result,
            Err(MigrationError::NoPathFound { from: 1, to: 2 })
        ));
    }

    #[test]
    fn migration_chain_migrate_invalid_range() {
        let chain = MigrationChain::<ScrollStateV2>::new();
        let state: Box<dyn core::any::Any + Send> = Box::new(ScrollStateV2::default());

        let result = chain.migrate(state, 3, 1);
        assert!(matches!(
            result,
            Err(MigrationError::InvalidVersionRange { from: 3, to: 1 })
        ));
    }

    #[test]
    fn restore_result_into_state() {
        let direct = RestoreResult::Direct(ScrollState { scroll_offset: 10 });
        assert_eq!(direct.into_state().scroll_offset, 10);

        let migrated = RestoreResult::Migrated {
            state: ScrollState { scroll_offset: 20 },
            from_version: 1,
        };
        assert_eq!(migrated.into_state().scroll_offset, 20);

        let fallback = RestoreResult::DefaultFallback {
            error: MigrationError::NoPathFound { from: 1, to: 2 },
            default: ScrollState { scroll_offset: 0 },
        };
        assert_eq!(fallback.into_state().scroll_offset, 0);
    }

    #[test]
    fn restore_result_was_migrated() {
        let direct = RestoreResult::Direct(ScrollState::default());
        assert!(!direct.was_migrated());

        let migrated = RestoreResult::Migrated::<ScrollState> {
            state: ScrollState::default(),
            from_version: 1,
        };
        assert!(migrated.was_migrated());

        let fallback = RestoreResult::DefaultFallback::<ScrollState> {
            error: MigrationError::NoPathFound { from: 1, to: 2 },
            default: ScrollState::default(),
        };
        assert!(!fallback.was_migrated());
    }

    #[test]
    fn restore_result_is_fallback() {
        let direct = RestoreResult::Direct(ScrollState::default());
        assert!(!direct.is_fallback());

        let migrated = RestoreResult::Migrated::<ScrollState> {
            state: ScrollState::default(),
            from_version: 1,
        };
        assert!(!migrated.is_fallback());

        let fallback = RestoreResult::DefaultFallback::<ScrollState> {
            error: MigrationError::NoPathFound { from: 1, to: 2 },
            default: ScrollState::default(),
        };
        assert!(fallback.is_fallback());
    }

    // ── Edge-case: StateKey ──────────────────────────────────────────

    #[test]
    fn state_key_from_path_single_segment() {
        let key = StateKey::from_path(&["widget"]);
        assert_eq!(key.widget_type, "widget");
        assert_eq!(key.instance_id, "widget");
    }

    #[test]
    fn state_key_from_path_two_segments() {
        let key = StateKey::from_path(&["parent", "child"]);
        assert_eq!(key.widget_type, "child");
        assert_eq!(key.instance_id, "parent/child");
    }

    #[test]
    fn state_key_empty_instance_id() {
        let key = StateKey::new("Widget", "");
        assert_eq!(key.instance_id, "");
        assert_eq!(key.canonical(), "Widget::");
        assert_eq!(key.to_string(), "Widget::");
    }

    #[test]
    fn state_key_canonical_matches_display() {
        let key = StateKey::new("TreeView", "sidebar/nav");
        assert_eq!(key.canonical(), key.to_string());
    }

    #[test]
    fn state_key_clone() {
        let key = StateKey::new("Scroll", "main");
        let cloned = key.clone();
        assert_eq!(key, cloned);
        assert_eq!(key.widget_type, cloned.widget_type);
        assert_eq!(key.instance_id, cloned.instance_id);
    }

    #[test]
    fn state_key_debug_format() {
        let key = StateKey::new("Foo", "bar");
        let dbg = format!("{:?}", key);
        assert!(dbg.contains("Foo"));
        assert!(dbg.contains("bar"));
    }

    #[test]
    fn state_key_hash_differs_for_different_keys() {
        use std::collections::hash_map::DefaultHasher;

        let hash = |key: &StateKey| {
            let mut h = DefaultHasher::new();
            key.hash(&mut h);
            h.finish()
        };

        let a = StateKey::new("ScrollView", "main");
        let b = StateKey::new("ScrollView", "sidebar");
        let c = StateKey::new("TreeView", "main");

        // Different instance_id → different hash (overwhelmingly likely)
        assert_ne!(hash(&a), hash(&b));
        // Different widget_type → different hash
        assert_ne!(hash(&a), hash(&c));
    }

    #[test]
    fn state_key_usable_as_hashmap_key() {
        use std::collections::HashMap;

        let mut map = HashMap::new();
        let key1 = StateKey::new("Scroll", "a");
        let key2 = StateKey::new("Scroll", "b");

        map.insert(key1.clone(), 1);
        map.insert(key2.clone(), 2);

        assert_eq!(map.get(&key1), Some(&1));
        assert_eq!(map.get(&key2), Some(&2));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn state_key_from_path_with_empty_segments() {
        let key = StateKey::from_path(&["", "child"]);
        assert_eq!(key.instance_id, "/child");
        assert_eq!(key.widget_type, "child");
    }

    // ── Edge-case: Stateful trait ────────────────────────────────────

    #[test]
    fn save_state_on_default_widget() {
        let widget = TestScrollView::default();
        let state = widget.save_state();
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn restore_state_to_zero_max() {
        let mut widget = TestScrollView {
            id: "x".into(),
            offset: 0,
            max: 0,
        };
        widget.restore_state(ScrollState { scroll_offset: 100 });
        // Should clamp to max=0
        assert_eq!(widget.offset, 0);
    }

    #[test]
    fn save_restore_preserves_max_u16() {
        let mut widget = TestScrollView {
            id: "w".into(),
            offset: u16::MAX,
            max: u16::MAX,
        };
        let saved = widget.save_state();
        assert_eq!(saved.scroll_offset, u16::MAX);

        widget.offset = 0;
        widget.restore_state(saved);
        assert_eq!(widget.offset, u16::MAX);
    }

    #[test]
    fn multiple_save_restore_cycles() {
        let mut widget = TestScrollView {
            id: "cycle".into(),
            offset: 10,
            max: 100,
        };

        for i in 0..5 {
            widget.offset = i * 10;
            let saved = widget.save_state();
            widget.offset = 0;
            widget.restore_state(saved);
            assert_eq!(widget.offset, i * 10);
        }
    }

    #[test]
    fn state_key_from_widget() {
        let widget = TestScrollView {
            id: "content-panel".into(),
            offset: 0,
            max: 50,
        };
        let key = widget.state_key();
        assert_eq!(key.widget_type, "ScrollView");
        assert_eq!(key.instance_id, "content-panel");
    }

    #[test]
    fn tree_view_save_restore_round_trip() {
        let mut widget = TestTreeView {
            id: "files".into(),
            expanded: vec![1, 3, 5],
        };
        let saved = widget.save_state();
        assert_eq!(saved.expanded_nodes, vec![1, 3, 5]);
        assert!(!saved.collapse_all_on_blur);

        widget.expanded = vec![];
        widget.restore_state(saved);
        assert_eq!(widget.expanded, vec![1, 3, 5]);
    }

    // ── Edge-case: VersionedState ────────────────────────────────────

    #[test]
    fn versioned_state_new_constructor() {
        let vs = VersionedState::new(42, ScrollState { scroll_offset: 7 });
        assert_eq!(vs.version, 42);
        assert_eq!(vs.data.scroll_offset, 7);
    }

    #[test]
    fn versioned_state_clone() {
        let vs = VersionedState::new(1, ScrollState { scroll_offset: 5 });
        let cloned = vs.clone();
        assert_eq!(cloned.version, 1);
        assert_eq!(cloned.data.scroll_offset, 5);
    }

    #[test]
    fn versioned_state_debug() {
        let vs = VersionedState::new(3, ScrollState { scroll_offset: 10 });
        let dbg = format!("{:?}", vs);
        assert!(dbg.contains("3"));
        assert!(dbg.contains("10"));
    }

    #[test]
    fn versioned_state_unpack_version_match() {
        let vs = VersionedState::new(1, ScrollState { scroll_offset: 42 });
        let result = vs.unpack::<TestScrollView>();
        assert!(result.is_some());
        assert_eq!(result.unwrap().scroll_offset, 42);
    }

    #[test]
    fn versioned_state_unpack_version_zero_mismatch() {
        // Version 0 doesn't match TestScrollView's version 1
        let vs = VersionedState::new(0, ScrollState { scroll_offset: 99 });
        assert!(vs.unpack::<TestScrollView>().is_none());
    }

    #[test]
    fn versioned_state_unpack_future_version() {
        // Version 999 doesn't match TestScrollView's version 1
        let vs = VersionedState::new(999, ScrollState { scroll_offset: 1 });
        assert!(vs.unpack::<TestScrollView>().is_none());
    }

    #[test]
    fn versioned_state_unpack_or_default_version_zero() {
        let vs = VersionedState::new(0, ScrollState { scroll_offset: 50 });
        let result = vs.unpack_or_default::<TestScrollView>();
        assert_eq!(result, ScrollState::default());
    }

    #[test]
    fn versioned_state_default_for_tree_state() {
        let vs = VersionedState::<TreeState>::default();
        assert_eq!(vs.version, 1);
        assert!(vs.data.expanded_nodes.is_empty());
        assert!(!vs.data.collapse_all_on_blur);
    }

    // ── Edge-case: MigrationError ────────────────────────────────────

    #[test]
    fn migration_error_clone() {
        let err = MigrationError::NoPathFound { from: 1, to: 5 };
        let cloned = err.clone();
        assert_eq!(cloned.to_string(), "no migration path from version 1 to 5");

        let err2 = MigrationError::MigrationFailed {
            from: 2,
            to: 3,
            message: "oops".into(),
        };
        let cloned2 = err2.clone();
        assert_eq!(cloned2.to_string(), "migration from 2 to 3 failed: oops");

        let err3 = MigrationError::InvalidVersionRange { from: 10, to: 1 };
        let cloned3 = err3.clone();
        assert_eq!(cloned3.to_string(), "invalid version range: 10 to 1");
    }

    #[test]
    fn migration_error_debug() {
        let err = MigrationError::NoPathFound { from: 1, to: 2 };
        let dbg = format!("{:?}", err);
        assert!(dbg.contains("NoPathFound"));
    }

    // ── Edge-case: MigrationChain ────────────────────────────────────

    #[test]
    fn migration_chain_default() {
        let chain = MigrationChain::<ScrollState>::default();
        assert!(!chain.has_path(1, 2));
    }

    #[test]
    fn migration_chain_has_path_same_version() {
        let chain = MigrationChain::<ScrollState>::new();
        // Same version always returns true even with empty chain
        assert!(chain.has_path(0, 0));
        assert!(chain.has_path(5, 5));
        assert!(chain.has_path(u32::MAX, u32::MAX));
    }

    #[test]
    fn migration_chain_has_path_from_greater_than_to() {
        let chain = MigrationChain::<ScrollState>::new();
        // from > to returns false (not equal)
        assert!(!chain.has_path(3, 1));
        assert!(!chain.has_path(2, 1));
    }

    #[test]
    fn migration_chain_has_path_gap_in_chain() {
        // Register v1→v2, but not v2→v3, then check v1→v3
        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(V1ToV2Migration));
        assert!(chain.has_path(1, 2));
        assert!(!chain.has_path(1, 3)); // gap at v2→v3
    }

    #[test]
    fn migration_chain_migrate_same_version_empty_chain() {
        let chain = MigrationChain::<ScrollState>::new();
        let state: Box<dyn core::any::Any + Send> = Box::new(ScrollState { scroll_offset: 77 });
        let result = chain.migrate(state, 5, 5);
        assert!(result.is_ok());
        let out = result.unwrap().downcast::<ScrollState>().unwrap();
        assert_eq!(out.scroll_offset, 77);
    }

    #[test]
    fn migration_chain_migrate_invalid_range_adjacent() {
        let chain = MigrationChain::<ScrollState>::new();
        let state: Box<dyn core::any::Any + Send> = Box::new(ScrollState::default());
        let result = chain.migrate(state, 2, 1);
        assert!(matches!(
            result,
            Err(MigrationError::InvalidVersionRange { from: 2, to: 1 })
        ));
    }

    // Multi-step migration v1→v2→v3
    #[derive(Debug, Clone, Default)]
    struct ScrollStateV3 {
        scroll_offset: u16,
        velocity: f32,
        smooth_scroll: bool, // Added in v3
    }

    struct V2ToV3Migration;

    impl ErasedMigration<ScrollStateV3> for V2ToV3Migration {
        fn from_version(&self) -> u32 {
            2
        }
        fn to_version(&self) -> u32 {
            3
        }
        fn migrate_erased(
            &self,
            old: Box<dyn core::any::Any + Send>,
        ) -> Result<Box<dyn core::any::Any + Send>, String> {
            let v2 = old
                .downcast::<ScrollStateV2>()
                .map_err(|_| "invalid state type")?;
            Ok(Box::new(ScrollStateV3 {
                scroll_offset: v2.scroll_offset,
                velocity: v2.velocity,
                smooth_scroll: true, // default for new field
            }))
        }
    }

    struct V1ToV2ForV3Migration;

    impl ErasedMigration<ScrollStateV3> for V1ToV2ForV3Migration {
        fn from_version(&self) -> u32 {
            1
        }
        fn to_version(&self) -> u32 {
            2
        }
        fn migrate_erased(
            &self,
            old: Box<dyn core::any::Any + Send>,
        ) -> Result<Box<dyn core::any::Any + Send>, String> {
            let v1 = old
                .downcast::<ScrollStateV1>()
                .map_err(|_| "invalid state type")?;
            Ok(Box::new(ScrollStateV2 {
                scroll_offset: v1.scroll_offset,
                velocity: 0.0,
            }))
        }
    }

    #[test]
    fn migration_chain_multi_step_v1_to_v3() {
        let mut chain = MigrationChain::<ScrollStateV3>::new();
        chain.register(Box::new(V1ToV2ForV3Migration));
        chain.register(Box::new(V2ToV3Migration));

        assert!(chain.has_path(1, 3));
        assert!(chain.has_path(1, 2));
        assert!(chain.has_path(2, 3));

        let old = Box::new(ScrollStateV1 { scroll_offset: 55 });
        let result = chain.migrate(old, 1, 3);
        assert!(result.is_ok());

        let migrated = result.unwrap().downcast::<ScrollStateV3>().unwrap();
        assert_eq!(migrated.scroll_offset, 55);
        assert_eq!(migrated.velocity, 0.0);
        assert!(migrated.smooth_scroll);
    }

    // Failing migration
    struct FailingMigration;

    impl ErasedMigration<ScrollStateV2> for FailingMigration {
        fn from_version(&self) -> u32 {
            1
        }
        fn to_version(&self) -> u32 {
            2
        }
        fn migrate_erased(
            &self,
            _: Box<dyn core::any::Any + Send>,
        ) -> Result<Box<dyn core::any::Any + Send>, String> {
            Err("data corruption detected".into())
        }
    }

    #[test]
    fn migration_chain_migrate_failure() {
        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(FailingMigration));

        let state: Box<dyn core::any::Any + Send> = Box::new(ScrollStateV1 { scroll_offset: 1 });
        let result = chain.migrate(state, 1, 2);
        assert!(result.is_err());
        match result.unwrap_err() {
            MigrationError::MigrationFailed { from, to, message } => {
                assert_eq!(from, 1);
                assert_eq!(to, 2);
                assert_eq!(message, "data corruption detected");
            }
            other => panic!("expected MigrationFailed, got {:?}", other),
        }
    }

    #[test]
    fn migration_chain_type_mismatch_in_migrate_erased() {
        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(V1ToV2Migration));

        // Pass wrong type (String instead of ScrollStateV1)
        let wrong: Box<dyn core::any::Any + Send> = Box::new("not a state".to_string());
        let result = chain.migrate(wrong, 1, 2);
        assert!(result.is_err());
        match result.unwrap_err() {
            MigrationError::MigrationFailed { from: 1, to: 2, .. } => {}
            other => panic!("expected MigrationFailed, got {:?}", other),
        }
    }

    // ── Edge-case: RestoreResult ─────────────────────────────────────

    #[test]
    fn restore_result_debug() {
        let direct = RestoreResult::Direct(ScrollState { scroll_offset: 1 });
        let dbg = format!("{:?}", direct);
        assert!(dbg.contains("Direct"));

        let migrated = RestoreResult::Migrated {
            state: ScrollState { scroll_offset: 2 },
            from_version: 1,
        };
        let dbg = format!("{:?}", migrated);
        assert!(dbg.contains("Migrated"));

        let fallback = RestoreResult::DefaultFallback {
            error: MigrationError::NoPathFound { from: 1, to: 2 },
            default: ScrollState::default(),
        };
        let dbg = format!("{:?}", fallback);
        assert!(dbg.contains("DefaultFallback"));
    }

    #[test]
    fn restore_result_into_state_migrated_with_data() {
        let result = RestoreResult::Migrated {
            state: ScrollState { scroll_offset: 99 },
            from_version: 1,
        };
        assert_eq!(result.into_state().scroll_offset, 99);
    }

    // ── Edge-case: unpack_with_migration ─────────────────────────────

    // Widget for unpack_with_migration tests
    #[derive(Default)]
    struct WidgetV2 {
        data: ScrollStateV2,
    }

    impl Stateful for WidgetV2 {
        type State = ScrollStateV2;

        fn state_key(&self) -> StateKey {
            StateKey::new("WidgetV2", "test")
        }

        fn save_state(&self) -> ScrollStateV2 {
            self.data.clone()
        }

        fn restore_state(&mut self, state: ScrollStateV2) {
            self.data = state;
        }

        fn state_version() -> u32 {
            2
        }
    }

    #[test]
    fn unpack_with_migration_direct_match() {
        let vs = VersionedState::new(
            2,
            ScrollStateV2 {
                scroll_offset: 33,
                velocity: 1.5,
            },
        );
        let chain = MigrationChain::<ScrollStateV2>::new();
        let result = vs.unpack_with_migration::<WidgetV2>(&chain);

        assert!(matches!(&result, RestoreResult::Direct(_)));
        assert!(!result.was_migrated());
        assert!(!result.is_fallback());
        let state = result.into_state();
        assert_eq!(state.scroll_offset, 33);
        assert_eq!(state.velocity, 1.5);
    }

    #[test]
    fn unpack_with_migration_version_mismatch_type_mismatch_falls_back() {
        // VersionedState<S> requires S == Stateful::State (ScrollStateV2 for WidgetV2).
        // When version != current, migrate_erased receives Box<ScrollStateV2> but
        // the V1ToV2Migration expects Box<ScrollStateV1> → downcast fails → fallback.
        let vs = VersionedState::new(1, ScrollStateV2::default());

        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(V1ToV2Migration));

        let result = vs.unpack_with_migration::<WidgetV2>(&chain);
        assert!(result.is_fallback());
        assert!(!result.was_migrated());
    }

    #[test]
    fn unpack_with_migration_no_path_falls_back() {
        let vs = VersionedState::new(
            1,
            ScrollStateV2 {
                scroll_offset: 10,
                velocity: 0.0,
            },
        );
        // Empty chain — no migration path from v1 to v2
        let chain = MigrationChain::<ScrollStateV2>::new();
        let result = vs.unpack_with_migration::<WidgetV2>(&chain);

        assert!(result.is_fallback());
        let state = result.into_state();
        // Should be default
        assert_eq!(state.scroll_offset, 0);
        assert_eq!(state.velocity, 0.0);
    }

    #[test]
    fn unpack_with_migration_failed_migration_falls_back() {
        let vs = VersionedState::new(1, ScrollStateV2::default());

        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(FailingMigration));

        let result = vs.unpack_with_migration::<WidgetV2>(&chain);
        assert!(result.is_fallback());
    }

    #[test]
    fn unpack_with_migration_type_mismatch_after_chain() {
        // Migration succeeds but returns wrong type → DefaultFallback
        struct WrongTypeMigration;

        impl ErasedMigration<ScrollStateV2> for WrongTypeMigration {
            fn from_version(&self) -> u32 {
                1
            }
            fn to_version(&self) -> u32 {
                2
            }
            fn migrate_erased(
                &self,
                _: Box<dyn core::any::Any + Send>,
            ) -> Result<Box<dyn core::any::Any + Send>, String> {
                // Return wrong type (String instead of ScrollStateV2)
                Ok(Box::new("wrong type".to_string()))
            }
        }

        let vs = VersionedState::new(1, ScrollStateV2::default());
        let mut chain = MigrationChain::<ScrollStateV2>::new();
        chain.register(Box::new(WrongTypeMigration));

        let result = vs.unpack_with_migration::<WidgetV2>(&chain);
        assert!(result.is_fallback());
    }

    // ── Edge-case: VersionedState::pack ──────────────────────────────

    #[test]
    fn versioned_state_pack_uses_state_version() {
        let widget = TestTreeView {
            id: "test".into(),
            expanded: vec![1, 2],
        };
        let packed = VersionedState::pack(&widget);
        assert_eq!(packed.version, 2); // TestTreeView::state_version() == 2
        assert_eq!(packed.data.expanded_nodes, vec![1, 2]);
    }

    #[test]
    fn versioned_state_pack_default_version() {
        let widget = TestScrollView {
            id: "test".into(),
            offset: 0,
            max: 100,
        };
        let packed = VersionedState::pack(&widget);
        assert_eq!(packed.version, 1); // default state_version() == 1
    }

    // ── Edge-case: ScrollState trait coverage ────────────────────────

    #[test]
    fn scroll_state_clone() {
        let s = ScrollState { scroll_offset: 42 };
        let cloned = s.clone();
        assert_eq!(s, cloned);
    }

    #[test]
    fn scroll_state_debug() {
        let s = ScrollState { scroll_offset: 10 };
        let dbg = format!("{:?}", s);
        assert!(dbg.contains("ScrollState"));
        assert!(dbg.contains("10"));
    }

    #[test]
    fn tree_state_clone() {
        let s = TreeState {
            expanded_nodes: vec![1, 2, 3],
            collapse_all_on_blur: true,
        };
        let cloned = s.clone();
        assert_eq!(s, cloned);
    }

    #[test]
    fn tree_state_debug() {
        let s = TreeState {
            expanded_nodes: vec![],
            collapse_all_on_blur: false,
        };
        let dbg = format!("{:?}", s);
        assert!(dbg.contains("TreeState"));
    }
}
