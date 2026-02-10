#![forbid(unsafe_code)]

//! Central registry for contextual help content.
//!
//! Maps widget IDs to structured help entries with hierarchical resolution
//! (widget → container → app). Widgets register their help content via
//! [`HelpRegistry::register`], and consumers look up help for a given widget
//! via [`HelpRegistry::get`] or [`HelpRegistry::resolve`] (which walks the
//! hierarchy).
//!
//! # Invariants
//!
//! 1. Each `HelpId` maps to at most one [`HelpContent`] at any given time.
//! 2. [`resolve`](HelpRegistry::resolve) walks parents until it finds content
//!    or reaches the root; it never cycles (parent chains are acyclic by
//!    construction—[`set_parent`](HelpRegistry::set_parent) rejects cycles).
//! 3. Lazy providers are called at most once per lookup; results are cached
//!    in the registry for subsequent lookups.
//!
//! # Example
//!
//! ```
//! use ftui_widgets::help_registry::{HelpRegistry, HelpContent, HelpId, Keybinding};
//!
//! let mut reg = HelpRegistry::new();
//! let widget_id = HelpId(42);
//!
//! reg.register(widget_id, HelpContent {
//!     short: "Save the current file".into(),
//!     long: Some("Writes the buffer to disk, creating the file if needed.".into()),
//!     keybindings: vec![Keybinding::new("Ctrl+S", "Save")],
//!     see_also: vec![],
//! });
//!
//! assert_eq!(reg.get(widget_id).unwrap().short, "Save the current file");
//! ```

use std::collections::HashMap;

/// Unique identifier for a help-registered widget.
///
/// Typically corresponds to a [`FocusId`](crate::focus::FocusId) but is its
/// own type so the help system can be used independently of focus management.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HelpId(pub u64);

impl core::fmt::Display for HelpId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "help:{}", self.0)
    }
}

/// A keyboard shortcut associated with a widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keybinding {
    /// Human-readable key combo (e.g. "Ctrl+S", "↑/k").
    pub key: String,
    /// What the binding does.
    pub action: String,
}

impl Keybinding {
    /// Create a new keybinding.
    #[must_use]
    pub fn new(key: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            action: action.into(),
        }
    }
}

/// Structured help content for a widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpContent {
    /// Short tooltip-length description (one line).
    pub short: String,
    /// Optional longer description shown in a detail panel.
    pub long: Option<String>,
    /// Keybindings available when this widget is focused.
    pub keybindings: Vec<Keybinding>,
    /// Related widget help IDs for "see also" links.
    pub see_also: Vec<HelpId>,
}

impl HelpContent {
    /// Create minimal help content with just a short description.
    #[must_use]
    pub fn short(desc: impl Into<String>) -> Self {
        Self {
            short: desc.into(),
            long: None,
            keybindings: Vec::new(),
            see_also: Vec::new(),
        }
    }
}

/// A lazy provider that produces [`HelpContent`] on demand.
///
/// Providers are called at most once; the result is cached.
type LazyProvider = Box<dyn FnOnce() -> HelpContent + Send>;

/// Internal entry: either already-loaded content or a lazy provider.
enum Entry {
    Loaded(HelpContent),
    Lazy(LazyProvider),
}

impl core::fmt::Debug for Entry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Loaded(c) => f.debug_tuple("Loaded").field(c).finish(),
            Self::Lazy(_) => f.debug_tuple("Lazy").field(&"<fn>").finish(),
        }
    }
}

/// Central registry mapping widget IDs to help content.
///
/// Supports:
/// - Direct registration via [`register`](Self::register)
/// - Lazy/deferred registration via [`register_lazy`](Self::register_lazy)
/// - Hierarchical parent chains via [`set_parent`](Self::set_parent)
/// - Resolution that walks parent chain via [`resolve`](Self::resolve)
#[derive(Debug)]
pub struct HelpRegistry {
    entries: HashMap<HelpId, Entry>,
    /// Parent mapping: child → parent. Used by `resolve` to walk up the
    /// widget tree when a widget has no help of its own.
    parents: HashMap<HelpId, HelpId>,
}

impl Default for HelpRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HelpRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            parents: HashMap::new(),
        }
    }

    /// Register help content for a widget.
    ///
    /// Overwrites any previous content (or lazy provider) for the same ID.
    pub fn register(&mut self, id: HelpId, content: HelpContent) {
        self.entries.insert(id, Entry::Loaded(content));
    }

    /// Register a lazy provider that will be called on first lookup.
    ///
    /// The provider is invoked at most once; its result is cached.
    pub fn register_lazy(
        &mut self,
        id: HelpId,
        provider: impl FnOnce() -> HelpContent + Send + 'static,
    ) {
        self.entries.insert(id, Entry::Lazy(Box::new(provider)));
    }

    /// Remove help content for a widget.
    ///
    /// Returns `true` if content was present.
    pub fn unregister(&mut self, id: HelpId) -> bool {
        self.entries.remove(&id).is_some()
    }

    /// Set the parent of a widget in the help hierarchy.
    ///
    /// When [`resolve`](Self::resolve) is called for `child` and no content
    /// is found, the lookup walks to `parent` (and its ancestors).
    ///
    /// Returns `false` (and does nothing) if setting this parent would create
    /// a cycle.
    pub fn set_parent(&mut self, child: HelpId, parent: HelpId) -> bool {
        // Cycle check: walk from parent upward; if we reach child, it's a cycle.
        if child == parent {
            return false;
        }
        let mut cursor = parent;
        while let Some(&ancestor) = self.parents.get(&cursor) {
            if ancestor == child {
                return false;
            }
            cursor = ancestor;
        }
        self.parents.insert(child, parent);
        true
    }

    /// Remove the parent link for a widget.
    pub fn clear_parent(&mut self, child: HelpId) {
        self.parents.remove(&child);
    }

    /// Get help content for a specific widget (no hierarchy walk).
    ///
    /// Forces lazy providers if present.
    #[must_use = "use the returned help content (if any)"]
    pub fn get(&mut self, id: HelpId) -> Option<&HelpContent> {
        // Force lazy → loaded if needed.
        if matches!(self.entries.get(&id), Some(Entry::Lazy(_)))
            && let Some(Entry::Lazy(provider)) = self.entries.remove(&id)
        {
            let content = provider();
            self.entries.insert(id, Entry::Loaded(content));
        }
        match self.entries.get(&id) {
            Some(Entry::Loaded(c)) => Some(c),
            _ => None,
        }
    }

    /// Peek at help content without forcing lazy providers.
    #[must_use = "use the returned help content (if any)"]
    pub fn peek(&self, id: HelpId) -> Option<&HelpContent> {
        match self.entries.get(&id) {
            Some(Entry::Loaded(c)) => Some(c),
            _ => None,
        }
    }

    /// Resolve help content by walking the parent hierarchy.
    ///
    /// Returns the first content found starting from `id` and walking up
    /// through parents. Returns `None` if no content exists in the chain.
    #[must_use = "use the returned help content (if any)"]
    pub fn resolve(&mut self, id: HelpId) -> Option<&HelpContent> {
        // Collect the chain of IDs to check (avoid borrow issues).
        let chain = self.ancestor_chain(id);
        // Force any lazy entries in the chain.
        for &cid in &chain {
            if matches!(self.entries.get(&cid), Some(Entry::Lazy(_)))
                && let Some(Entry::Lazy(provider)) = self.entries.remove(&cid)
            {
                let content = provider();
                self.entries.insert(cid, Entry::Loaded(content));
            }
        }
        // Now find the first loaded entry.
        for &cid in &chain {
            if let Some(Entry::Loaded(c)) = self.entries.get(&cid) {
                return Some(c);
            }
        }
        None
    }

    /// Check whether any help content is registered for this ID (including lazy).
    #[must_use]
    pub fn contains(&self, id: HelpId) -> bool {
        self.entries.contains_key(&id)
    }

    /// Number of registered entries (loaded + lazy).
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all registered IDs.
    pub fn ids(&self) -> impl Iterator<Item = HelpId> + '_ {
        self.entries.keys().copied()
    }

    /// Build the ancestor chain starting from `id` (inclusive).
    fn ancestor_chain(&self, id: HelpId) -> Vec<HelpId> {
        let mut chain = vec![id];
        let mut cursor = id;
        while let Some(&parent) = self.parents.get(&cursor) {
            chain.push(parent);
            cursor = parent;
        }
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_content(short: &str) -> HelpContent {
        HelpContent {
            short: short.into(),
            long: None,
            keybindings: Vec::new(),
            see_also: Vec::new(),
        }
    }

    // ── Registration and lookup ────────────────────────────────────

    #[test]
    fn register_and_get() {
        let mut reg = HelpRegistry::new();
        let id = HelpId(1);
        reg.register(id, sample_content("tooltip"));
        assert_eq!(reg.get(id).unwrap().short, "tooltip");
    }

    #[test]
    fn missing_key_returns_none() {
        let mut reg = HelpRegistry::new();
        assert!(reg.get(HelpId(999)).is_none());
    }

    #[test]
    fn register_overwrites() {
        let mut reg = HelpRegistry::new();
        let id = HelpId(1);
        reg.register(id, sample_content("old"));
        reg.register(id, sample_content("new"));
        assert_eq!(reg.get(id).unwrap().short, "new");
    }

    #[test]
    fn unregister() {
        let mut reg = HelpRegistry::new();
        let id = HelpId(1);
        reg.register(id, sample_content("x"));
        assert!(reg.unregister(id));
        assert!(reg.get(id).is_none());
        assert!(!reg.unregister(id));
    }

    #[test]
    fn len_and_is_empty() {
        let mut reg = HelpRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        reg.register(HelpId(1), sample_content("a"));
        reg.register(HelpId(2), sample_content("b"));
        assert_eq!(reg.len(), 2);
        assert!(!reg.is_empty());
    }

    #[test]
    fn contains() {
        let mut reg = HelpRegistry::new();
        let id = HelpId(1);
        assert!(!reg.contains(id));
        reg.register(id, sample_content("x"));
        assert!(reg.contains(id));
    }

    #[test]
    fn ids_iteration() {
        let mut reg = HelpRegistry::new();
        reg.register(HelpId(10), sample_content("a"));
        reg.register(HelpId(20), sample_content("b"));
        let mut ids: Vec<_> = reg.ids().collect();
        ids.sort_by_key(|h| h.0);
        assert_eq!(ids, vec![HelpId(10), HelpId(20)]);
    }

    // ── Lazy providers ─────────────────────────────────────────────

    #[test]
    fn lazy_provider_called_on_get() {
        let mut reg = HelpRegistry::new();
        let id = HelpId(1);
        reg.register_lazy(id, || sample_content("lazy"));
        assert!(reg.peek(id).is_none()); // not yet forced
        assert_eq!(reg.get(id).unwrap().short, "lazy");
        assert!(reg.peek(id).is_some()); // now cached
    }

    #[test]
    fn lazy_provider_overwritten_by_register() {
        let mut reg = HelpRegistry::new();
        let id = HelpId(1);
        reg.register_lazy(id, || sample_content("lazy"));
        reg.register(id, sample_content("eager"));
        assert_eq!(reg.get(id).unwrap().short, "eager");
    }

    #[test]
    fn register_overwrites_lazy() {
        let mut reg = HelpRegistry::new();
        let id = HelpId(1);
        reg.register_lazy(id, || sample_content("first"));
        reg.register_lazy(id, || sample_content("second"));
        assert_eq!(reg.get(id).unwrap().short, "second");
    }

    // ── Hierarchy ──────────────────────────────────────────────────

    #[test]
    fn resolve_walks_parents() {
        let mut reg = HelpRegistry::new();
        let child = HelpId(1);
        let parent = HelpId(2);
        let grandparent = HelpId(3);

        reg.register(grandparent, sample_content("app help"));
        reg.set_parent(child, parent);
        reg.set_parent(parent, grandparent);

        // child has no content; resolve walks to grandparent
        assert_eq!(reg.resolve(child).unwrap().short, "app help");
    }

    #[test]
    fn resolve_prefers_nearest() {
        let mut reg = HelpRegistry::new();
        let child = HelpId(1);
        let parent = HelpId(2);
        let grandparent = HelpId(3);

        reg.register(parent, sample_content("container help"));
        reg.register(grandparent, sample_content("app help"));
        reg.set_parent(child, parent);
        reg.set_parent(parent, grandparent);

        // child has no content; resolve finds parent first
        assert_eq!(reg.resolve(child).unwrap().short, "container help");
    }

    #[test]
    fn resolve_returns_own_content_first() {
        let mut reg = HelpRegistry::new();
        let child = HelpId(1);
        let parent = HelpId(2);

        reg.register(child, sample_content("widget help"));
        reg.register(parent, sample_content("container help"));
        reg.set_parent(child, parent);

        assert_eq!(reg.resolve(child).unwrap().short, "widget help");
    }

    #[test]
    fn resolve_no_content_returns_none() {
        let mut reg = HelpRegistry::new();
        let child = HelpId(1);
        let parent = HelpId(2);
        reg.set_parent(child, parent);
        assert!(reg.resolve(child).is_none());
    }

    #[test]
    fn set_parent_rejects_self_cycle() {
        let mut reg = HelpRegistry::new();
        let id = HelpId(1);
        assert!(!reg.set_parent(id, id));
    }

    #[test]
    fn set_parent_rejects_indirect_cycle() {
        let mut reg = HelpRegistry::new();
        let a = HelpId(1);
        let b = HelpId(2);
        let c = HelpId(3);

        assert!(reg.set_parent(a, b));
        assert!(reg.set_parent(b, c));
        // c → a would create cycle c→a→b→c
        assert!(!reg.set_parent(c, a));
    }

    #[test]
    fn clear_parent() {
        let mut reg = HelpRegistry::new();
        let child = HelpId(1);
        let parent = HelpId(2);

        reg.register(parent, sample_content("parent"));
        reg.set_parent(child, parent);
        assert!(reg.resolve(child).is_some());

        reg.clear_parent(child);
        assert!(reg.resolve(child).is_none());
    }

    // ── Keybindings and structured content ─────────────────────────

    #[test]
    fn keybindings_stored() {
        let mut reg = HelpRegistry::new();
        let id = HelpId(1);
        reg.register(
            id,
            HelpContent {
                short: "Editor".into(),
                long: Some("Main text editor".into()),
                keybindings: vec![
                    Keybinding::new("Ctrl+S", "Save"),
                    Keybinding::new("Ctrl+Q", "Quit"),
                ],
                see_also: vec![HelpId(2)],
            },
        );
        let content = reg.get(id).unwrap();
        assert_eq!(content.keybindings.len(), 2);
        assert_eq!(content.keybindings[0].key, "Ctrl+S");
        assert_eq!(content.keybindings[0].action, "Save");
        assert_eq!(content.see_also, vec![HelpId(2)]);
    }

    #[test]
    fn help_content_short_constructor() {
        let c = HelpContent::short("tooltip");
        assert_eq!(c.short, "tooltip");
        assert!(c.long.is_none());
        assert!(c.keybindings.is_empty());
        assert!(c.see_also.is_empty());
    }

    #[test]
    fn help_id_display() {
        assert_eq!(HelpId(42).to_string(), "help:42");
    }

    // ── Lazy in hierarchy ──────────────────────────────────────────

    #[test]
    fn resolve_forces_lazy_in_parent() {
        let mut reg = HelpRegistry::new();
        let child = HelpId(1);
        let parent = HelpId(2);

        reg.register_lazy(parent, || sample_content("lazy parent"));
        reg.set_parent(child, parent);

        assert_eq!(reg.resolve(child).unwrap().short, "lazy parent");
        // Now cached
        assert!(reg.peek(parent).is_some());
    }

    // ── Edge cases ─────────────────────────────────────────────────

    #[test]
    fn empty_registry_resolve() {
        let mut reg = HelpRegistry::new();
        assert!(reg.resolve(HelpId(1)).is_none());
    }

    #[test]
    fn deep_hierarchy() {
        let mut reg = HelpRegistry::new();
        // Chain: 0 → 1 → 2 → 3 → 4 (content at 4)
        for i in 0..4u64 {
            assert!(reg.set_parent(HelpId(i), HelpId(i + 1)));
        }
        reg.register(HelpId(4), sample_content("root"));
        assert_eq!(reg.resolve(HelpId(0)).unwrap().short, "root");
    }

    #[test]
    fn set_parent_allows_reparenting() {
        let mut reg = HelpRegistry::new();
        let child = HelpId(1);
        let p1 = HelpId(2);
        let p2 = HelpId(3);

        reg.register(p1, sample_content("first parent"));
        reg.register(p2, sample_content("second parent"));

        reg.set_parent(child, p1);
        assert_eq!(reg.resolve(child).unwrap().short, "first parent");

        // Reparent
        reg.set_parent(child, p2);
        assert_eq!(reg.resolve(child).unwrap().short, "second parent");
    }

    #[test]
    fn unregister_does_not_remove_parent_link() {
        let mut reg = HelpRegistry::new();
        let child = HelpId(1);
        let parent = HelpId(2);
        let grandparent = HelpId(3);

        reg.register(parent, sample_content("parent"));
        reg.register(grandparent, sample_content("grandparent"));
        reg.set_parent(child, parent);
        reg.set_parent(parent, grandparent);

        // Remove parent content; resolve should walk to grandparent
        reg.unregister(parent);
        assert_eq!(reg.resolve(child).unwrap().short, "grandparent");
    }
}
