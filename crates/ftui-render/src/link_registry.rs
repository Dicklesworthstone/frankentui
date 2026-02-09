#![forbid(unsafe_code)]

//! OSC 8 hyperlink registry.
//!
//! The `LinkRegistry` maps link IDs to URLs. This allows cells to store
//! compact 24-bit link IDs instead of full URL strings.
//!
//! # Usage
//!
//! ```
//! use ftui_render::link_registry::LinkRegistry;
//!
//! let mut registry = LinkRegistry::new();
//! let id = registry.register("https://example.com");
//! assert_eq!(registry.get(id), Some("https://example.com"));
//! ```

use std::collections::HashMap;

const MAX_LINK_ID: u32 = 0x00FF_FFFF;

/// Registry for OSC 8 hyperlink URLs.
#[derive(Debug, Clone, Default)]
pub struct LinkRegistry {
    /// Link slots indexed by ID (0 reserved for "no link").
    links: Vec<Option<String>>,
    /// URL to ID lookup for deduplication.
    lookup: HashMap<String, u32>,
    /// Reusable IDs from removed links.
    free_list: Vec<u32>,
}

impl LinkRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            links: vec![None],
            lookup: HashMap::new(),
            free_list: Vec::new(),
        }
    }

    /// Register a URL and return its link ID.
    ///
    /// If the URL is already registered, returns the existing ID.
    pub fn register(&mut self, url: &str) -> u32 {
        if let Some(&id) = self.lookup.get(url) {
            return id;
        }

        let id = if let Some(id) = self.free_list.pop() {
            id
        } else {
            let id = self.links.len() as u32;
            debug_assert!(id <= MAX_LINK_ID, "link id overflow");
            if id > MAX_LINK_ID {
                return 0;
            }
            self.links.push(None);
            id
        };

        if id == 0 || id > MAX_LINK_ID {
            return 0;
        }

        self.links[id as usize] = Some(url.to_string());
        self.lookup.insert(url.to_string(), id);
        id
    }

    /// Get the URL for a link ID.
    pub fn get(&self, id: u32) -> Option<&str> {
        self.links
            .get(id as usize)
            .and_then(|slot| slot.as_ref())
            .map(|s| s.as_str())
    }

    /// Unregister a link by ID.
    pub fn unregister(&mut self, id: u32) {
        if id == 0 {
            return;
        }

        let Some(slot) = self.links.get_mut(id as usize) else {
            return;
        };

        if let Some(url) = slot.take() {
            self.lookup.remove(&url);
            self.free_list.push(id);
        }
    }

    /// Clear all links.
    pub fn clear(&mut self) {
        self.links.clear();
        self.links.push(None);
        self.lookup.clear();
        self.free_list.clear();
    }

    /// Number of registered links.
    pub fn len(&self) -> usize {
        self.links.iter().filter(|slot| slot.is_some()).count()
    }

    /// Check if the registry is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Check if the registry contains a link ID.
    #[inline]
    pub fn contains(&self, id: u32) -> bool {
        self.get(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_get() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com");
        assert_eq!(registry.get(id), Some("https://example.com"));
    }

    #[test]
    fn deduplication() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://example.com");
        let id2 = registry.register("https://example.com");
        assert_eq!(id1, id2);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn multiple_urls() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://one.com");
        let id2 = registry.register("https://two.com");
        assert_ne!(id1, id2);
        assert_eq!(registry.get(id1), Some("https://one.com"));
        assert_eq!(registry.get(id2), Some("https://two.com"));
    }

    #[test]
    fn unregister_reuses_id() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com");
        assert!(registry.contains(id));
        registry.unregister(id);
        assert!(!registry.contains(id));
        let reused = registry.register("https://new.com");
        assert_eq!(reused, id);
    }

    #[test]
    fn clear() {
        let mut registry = LinkRegistry::new();
        registry.register("https://one.com");
        registry.register("https://two.com");
        assert_eq!(registry.len(), 2);
        registry.clear();
        assert!(registry.is_empty());
    }

    // --- Edge case tests ---

    #[test]
    fn id_zero_is_reserved() {
        let registry = LinkRegistry::new();
        assert_eq!(registry.get(0), None);
    }

    #[test]
    fn unregister_zero_is_noop() {
        let mut registry = LinkRegistry::new();
        registry.register("https://example.com");
        registry.unregister(0);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn get_out_of_bounds_returns_none() {
        let registry = LinkRegistry::new();
        assert_eq!(registry.get(999), None);
        assert_eq!(registry.get(u32::MAX), None);
    }

    #[test]
    fn unregister_out_of_bounds_is_safe() {
        let mut registry = LinkRegistry::new();
        registry.unregister(999);
        registry.unregister(u32::MAX);
        // No panic, no effect
        assert!(registry.is_empty());
    }

    #[test]
    fn unregister_twice_is_safe() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com");
        registry.unregister(id);
        registry.unregister(id); // Second call is no-op
        assert!(registry.is_empty());
    }

    #[test]
    fn register_returns_nonzero() {
        let mut registry = LinkRegistry::new();
        for i in 0..20 {
            let id = registry.register(&format!("https://example.com/{i}"));
            assert_ne!(id, 0, "register must never return id 0");
        }
    }

    #[test]
    fn contains_after_unregister() {
        let mut registry = LinkRegistry::new();
        let id = registry.register("https://example.com");
        assert!(registry.contains(id));
        registry.unregister(id);
        assert!(!registry.contains(id));
    }

    #[test]
    fn contains_invalid_id() {
        let registry = LinkRegistry::new();
        assert!(!registry.contains(0));
        assert!(!registry.contains(999));
    }

    #[test]
    fn dedup_after_unregister_gets_new_id() {
        let mut registry = LinkRegistry::new();
        let id1 = registry.register("https://example.com");
        registry.unregister(id1);
        // Re-register same URL — lookup cleared, so gets new (reused) id
        let id2 = registry.register("https://example.com");
        assert_eq!(id2, id1); // Reuses freed slot
        assert_eq!(registry.get(id2), Some("https://example.com"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn free_list_lifo_order() {
        let mut registry = LinkRegistry::new();
        let a = registry.register("https://a.com");
        let b = registry.register("https://b.com");
        let c = registry.register("https://c.com");

        // Free in order a, b, c — free_list is [a, b, c]
        registry.unregister(a);
        registry.unregister(b);
        registry.unregister(c);

        // LIFO: next alloc pops c, then b, then a
        let new1 = registry.register("https://new1.com");
        assert_eq!(new1, c);
        let new2 = registry.register("https://new2.com");
        assert_eq!(new2, b);
        let new3 = registry.register("https://new3.com");
        assert_eq!(new3, a);
    }

    #[test]
    fn len_tracks_operations() {
        let mut registry = LinkRegistry::new();
        assert_eq!(registry.len(), 0);

        let id1 = registry.register("https://one.com");
        assert_eq!(registry.len(), 1);

        let id2 = registry.register("https://two.com");
        assert_eq!(registry.len(), 2);

        // Dedup doesn't increase len
        registry.register("https://one.com");
        assert_eq!(registry.len(), 2);

        registry.unregister(id1);
        assert_eq!(registry.len(), 1);

        registry.unregister(id2);
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
    }

    #[test]
    fn register_after_clear_works() {
        let mut registry = LinkRegistry::new();
        registry.register("https://one.com");
        registry.register("https://two.com");
        registry.clear();

        let id = registry.register("https://fresh.com");
        assert_ne!(id, 0);
        assert_eq!(registry.get(id), Some("https://fresh.com"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn many_registrations() {
        let mut registry = LinkRegistry::new();
        let mut ids = Vec::new();
        for i in 0..100 {
            let url = format!("https://example.com/{i}");
            ids.push(registry.register(&url));
        }
        assert_eq!(registry.len(), 100);

        // All IDs unique and non-zero
        for (i, &id) in ids.iter().enumerate() {
            assert_ne!(id, 0);
            let url = format!("https://example.com/{i}");
            assert_eq!(registry.get(id), Some(url.as_str()));
        }

        // All IDs distinct
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    mod property {
        use super::*;
        use proptest::prelude::*;

        fn arb_url() -> impl Strategy<Value = String> {
            "[a-z]{3,12}".prop_map(|s| format!("https://{s}.com"))
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// Register/get roundtrip always returns the original URL.
            #[test]
            fn register_get_roundtrip(url in arb_url()) {
                let mut registry = LinkRegistry::new();
                let id = registry.register(&url);
                prop_assert_ne!(id, 0);
                prop_assert_eq!(registry.get(id), Some(url.as_str()));
            }

            /// Duplicate registration returns the same ID.
            #[test]
            fn dedup_same_id(url in arb_url()) {
                let mut registry = LinkRegistry::new();
                let id1 = registry.register(&url);
                let id2 = registry.register(&url);
                prop_assert_eq!(id1, id2);
                prop_assert_eq!(registry.len(), 1);
            }

            /// Distinct URLs produce distinct IDs.
            #[test]
            fn distinct_urls_distinct_ids(count in 2usize..20) {
                let mut registry = LinkRegistry::new();
                let mut ids = Vec::new();
                for i in 0..count {
                    ids.push(registry.register(&format!("https://u{i}.com")));
                }
                for i in 0..ids.len() {
                    for j in (i + 1)..ids.len() {
                        prop_assert_ne!(ids[i], ids[j]);
                    }
                }
            }

            /// len tracks correctly through register/unregister cycles.
            #[test]
            fn len_invariant(n_register in 1usize..15, n_unregister in 0usize..15) {
                let mut registry = LinkRegistry::new();
                let mut ids = Vec::new();
                for i in 0..n_register {
                    ids.push(registry.register(&format!("https://r{i}.com")));
                }
                prop_assert_eq!(registry.len(), n_register);

                let actual_unreg = n_unregister.min(n_register);
                for id in &ids[..actual_unreg] {
                    registry.unregister(*id);
                }
                prop_assert_eq!(registry.len(), n_register - actual_unreg);
            }

            /// Unregister + re-register reuses the freed slot.
            #[test]
            fn slot_reuse(url1 in arb_url(), url2 in arb_url()) {
                let mut registry = LinkRegistry::new();
                let id1 = registry.register(&url1);
                registry.unregister(id1);
                let id2 = registry.register(&url2);
                prop_assert_eq!(id1, id2);
                prop_assert_eq!(registry.get(id2), Some(url2.as_str()));
            }

            /// Clear resets everything; old IDs return None.
            #[test]
            fn clear_resets(count in 1usize..15) {
                let mut registry = LinkRegistry::new();
                let mut ids = Vec::new();
                for i in 0..count {
                    ids.push(registry.register(&format!("https://c{i}.com")));
                }
                registry.clear();
                prop_assert!(registry.is_empty());
                for id in &ids {
                    prop_assert_eq!(registry.get(*id), None);
                }
            }
        }
    }
}
