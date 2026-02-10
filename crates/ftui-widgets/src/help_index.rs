#![forbid(unsafe_code)]

//! Searchable index for help content with fuzzy matching.
//!
//! Provides O(1) token lookup with sub-100ms search response for typical
//! registries. Supports full-text search across `short`, `long`, and
//! `keybindings[].action` fields.
//!
//! # Example
//!
//! ```
//! use ftui_widgets::help_registry::{HelpRegistry, HelpContent, HelpId, Keybinding};
//! use ftui_widgets::help_index::HelpIndex;
//!
//! let mut reg = HelpRegistry::new();
//! reg.register(HelpId(1), HelpContent {
//!     short: "Save the current file".into(),
//!     long: Some("Writes buffer to disk".into()),
//!     keybindings: vec![Keybinding::new("Ctrl+S", "Save file")],
//!     see_also: vec![],
//! });
//! reg.register(HelpId(2), HelpContent {
//!     short: "Open file picker".into(),
//!     long: None,
//!     keybindings: vec![Keybinding::new("Ctrl+O", "Open")],
//!     see_also: vec![],
//! });
//!
//! let index = HelpIndex::build(&reg);
//! let results = index.search("save", 10);
//! assert!(!results.is_empty());
//! assert_eq!(results[0].id, HelpId(1));
//! ```

use std::collections::HashMap;

use crate::help_registry::{HelpId, HelpRegistry};

/// Weight multipliers for different help content fields.
/// Higher weight = higher score for matches in that field.
const WEIGHT_SHORT: f32 = 3.0;
const WEIGHT_LONG: f32 = 1.5;
const WEIGHT_KEYBINDING_ACTION: f32 = 2.0;
const WEIGHT_KEYBINDING_KEY: f32 = 2.5;

/// Maximum edit distance for fuzzy matching (proportion of query length).
const FUZZY_THRESHOLD_RATIO: f32 = 0.35;

/// Minimum query length to enable fuzzy matching.
const MIN_FUZZY_QUERY_LEN: usize = 3;

/// A search result with relevance score.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// The HelpId of the matching entry.
    pub id: HelpId,
    /// Relevance score (higher = more relevant).
    pub score: f32,
    /// Best matching field for context display.
    pub matched_field: MatchedField,
    /// The matched text snippet for highlighting.
    pub matched_text: String,
}

/// Which field contained the best match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedField {
    /// Match in the short description.
    Short,
    /// Match in the long description.
    Long,
    /// Match in a keybinding action.
    KeybindingAction,
    /// Match in a keybinding key combo.
    KeybindingKey,
}

impl core::fmt::Display for MatchedField {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Short => write!(f, "description"),
            Self::Long => write!(f, "details"),
            Self::KeybindingAction => write!(f, "keybinding"),
            Self::KeybindingKey => write!(f, "key"),
        }
    }
}

/// Token occurrence with field and position info for scoring.
#[derive(Debug, Clone)]
struct TokenOccurrence {
    id: HelpId,
    field: MatchedField,
    position: u16,      // Word position in field (0-indexed)
    field_text: String, // The full field text for snippet extraction
}

/// Searchable index across all help content.
///
/// Build once from a [`HelpRegistry`] using [`build`](Self::build),
/// then perform repeated searches using [`search`](Self::search).
///
/// The index only captures *loaded* entries (not lazy providers that
/// haven't been accessed). To include all entries, call `registry.get()`
/// on each ID before building the index.
#[derive(Debug)]
pub struct HelpIndex {
    /// Inverted index: lowercase token → occurrences
    inverted: HashMap<String, Vec<TokenOccurrence>>,
    /// All indexed HelpIds for fuzzy scan fallback
    all_ids: Vec<HelpId>,
    /// Cached content for fuzzy matching (id → indexed text)
    content_cache: HashMap<HelpId, IndexedContent>,
}

/// Cached content for an entry, used during fuzzy search.
#[derive(Debug, Clone)]
struct IndexedContent {
    short: String,
    long: Option<String>,
    keybindings: Vec<(String, String)>, // (key, action)
}

impl HelpIndex {
    /// Build an index from all loaded entries in the registry.
    ///
    /// Only entries that have been loaded (not lazy) are indexed.
    /// Use `registry.peek()` to check if an entry is loaded.
    #[must_use]
    pub fn build(registry: &HelpRegistry) -> Self {
        let mut inverted: HashMap<String, Vec<TokenOccurrence>> = HashMap::new();
        let mut all_ids = Vec::new();
        let mut content_cache = HashMap::new();

        for id in registry.ids() {
            // Use peek() to avoid forcing lazy providers
            let Some(content) = registry.peek(id) else {
                continue;
            };
            all_ids.push(id);

            // Cache content for fuzzy matching
            let cached = IndexedContent {
                short: content.short.clone(),
                long: content.long.clone(),
                keybindings: content
                    .keybindings
                    .iter()
                    .map(|kb| (kb.key.clone(), kb.action.clone()))
                    .collect(),
            };
            content_cache.insert(id, cached);

            // Index short description
            Self::index_text(
                &mut inverted,
                &content.short,
                id,
                MatchedField::Short,
                &content.short,
            );

            // Index long description
            if let Some(ref long) = content.long {
                Self::index_text(&mut inverted, long, id, MatchedField::Long, long);
            }

            // Index keybindings
            for kb in &content.keybindings {
                Self::index_text(
                    &mut inverted,
                    &kb.action,
                    id,
                    MatchedField::KeybindingAction,
                    &kb.action,
                );
                Self::index_text(
                    &mut inverted,
                    &kb.key,
                    id,
                    MatchedField::KeybindingKey,
                    &kb.key,
                );
            }
        }

        Self {
            inverted,
            all_ids,
            content_cache,
        }
    }

    /// Index text by tokenizing and adding to inverted index.
    fn index_text(
        inverted: &mut HashMap<String, Vec<TokenOccurrence>>,
        text: &str,
        id: HelpId,
        field: MatchedField,
        field_text: &str,
    ) {
        for (pos, token) in Self::tokenize(text).enumerate() {
            let occurrence = TokenOccurrence {
                id,
                field,
                position: pos as u16,
                field_text: field_text.to_string(),
            };
            inverted
                .entry(token.to_lowercase())
                .or_default()
                .push(occurrence);
        }
    }

    /// Tokenize text into searchable tokens.
    fn tokenize(text: &str) -> impl Iterator<Item = &str> {
        text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .filter(|s| !s.is_empty())
    }

    /// Search for entries matching the query.
    ///
    /// Returns up to `limit` results sorted by relevance score (highest first).
    ///
    /// # Search Behavior
    ///
    /// - Queries are tokenized and matched against indexed content
    /// - Exact token matches score higher than fuzzy matches
    /// - Matches in `short` descriptions score higher than `long`
    /// - Earlier positions in text score slightly higher
    /// - Multiple matching tokens boost the score
    ///
    /// # Performance
    ///
    /// Designed for sub-100ms response with typical registry sizes (< 10k entries).
    /// Fuzzy matching is only enabled for queries ≥ 3 characters.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        if query.is_empty() {
            return Vec::new();
        }

        let query_lower = query.to_lowercase();
        let query_tokens: Vec<&str> = Self::tokenize(&query_lower).collect();

        if query_tokens.is_empty() {
            return Vec::new();
        }

        // Aggregate scores by HelpId
        let mut scores: HashMap<HelpId, (f32, MatchedField, String)> = HashMap::new();

        // Phase 1: Exact and prefix token matches
        for token in &query_tokens {
            // Exact match
            if let Some(occurrences) = self.inverted.get(*token) {
                for occ in occurrences {
                    let field_weight = Self::field_weight(occ.field);
                    let position_bonus = 1.0 / (1.0 + occ.position as f32 * 0.1);
                    let score = field_weight * position_bonus;

                    let entry =
                        scores
                            .entry(occ.id)
                            .or_insert((0.0, occ.field, occ.field_text.clone()));
                    entry.0 += score;
                    // Keep the highest-scoring field
                    if field_weight > Self::field_weight(entry.1) {
                        entry.1 = occ.field;
                        entry.2 = occ.field_text.clone();
                    }
                }
            }

            // Prefix match (for partial queries)
            if token.len() >= 2 {
                for (indexed_token, occurrences) in &self.inverted {
                    if indexed_token.starts_with(*token) && indexed_token != *token {
                        for occ in occurrences {
                            let field_weight = Self::field_weight(occ.field);
                            // Prefix matches score lower than exact
                            let prefix_penalty = 0.7;
                            let position_bonus = 1.0 / (1.0 + occ.position as f32 * 0.1);
                            let score = field_weight * prefix_penalty * position_bonus;

                            let entry = scores.entry(occ.id).or_insert((
                                0.0,
                                occ.field,
                                occ.field_text.clone(),
                            ));
                            entry.0 += score;
                        }
                    }
                }
            }
        }

        // Phase 2: Fuzzy matching (only for longer queries with no/few results)
        let enable_fuzzy =
            query_lower.chars().count() >= MIN_FUZZY_QUERY_LEN && scores.len() < limit;

        if enable_fuzzy {
            self.fuzzy_search(&query_lower, &mut scores);
        }

        // Phase 3: Substring matching in cached content
        self.substring_search(&query_lower, &mut scores);

        // Convert to results and sort
        let mut results: Vec<SearchResult> = scores
            .into_iter()
            .map(|(id, (score, field, text))| SearchResult {
                id,
                score,
                matched_field: field,
                matched_text: text,
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results.truncate(limit);
        results
    }

    /// Perform fuzzy matching against all cached content.
    fn fuzzy_search(&self, query: &str, scores: &mut HashMap<HelpId, (f32, MatchedField, String)>) {
        let max_distance = ((query.chars().count() as f32) * FUZZY_THRESHOLD_RATIO).ceil() as usize;
        let max_distance = max_distance.max(1);

        for (id, content) in &self.content_cache {
            // Check each field for fuzzy matches
            for token in Self::tokenize(&content.short.to_lowercase()) {
                if let Some(dist) = Self::levenshtein_bounded(query, token, max_distance) {
                    let score = Self::fuzzy_score(dist, token.len(), WEIGHT_SHORT);
                    let entry = scores.entry(*id).or_insert((
                        0.0,
                        MatchedField::Short,
                        content.short.clone(),
                    ));
                    entry.0 += score;
                }
            }

            if let Some(ref long) = content.long {
                for token in Self::tokenize(&long.to_lowercase()) {
                    if let Some(dist) = Self::levenshtein_bounded(query, token, max_distance) {
                        let score = Self::fuzzy_score(dist, token.len(), WEIGHT_LONG);
                        let entry =
                            scores
                                .entry(*id)
                                .or_insert((0.0, MatchedField::Long, long.clone()));
                        if entry.1 == MatchedField::Long
                            || Self::field_weight(MatchedField::Long) > Self::field_weight(entry.1)
                        {
                            entry.0 += score;
                        }
                    }
                }
            }

            for (key, action) in &content.keybindings {
                for token in Self::tokenize(&action.to_lowercase()) {
                    if let Some(dist) = Self::levenshtein_bounded(query, token, max_distance) {
                        let score = Self::fuzzy_score(dist, token.len(), WEIGHT_KEYBINDING_ACTION);
                        let entry = scores.entry(*id).or_insert((
                            0.0,
                            MatchedField::KeybindingAction,
                            action.clone(),
                        ));
                        entry.0 += score;
                    }
                }
                for token in Self::tokenize(&key.to_lowercase()) {
                    if let Some(dist) = Self::levenshtein_bounded(query, token, max_distance) {
                        let score = Self::fuzzy_score(dist, token.len(), WEIGHT_KEYBINDING_KEY);
                        let entry = scores.entry(*id).or_insert((
                            0.0,
                            MatchedField::KeybindingKey,
                            key.clone(),
                        ));
                        entry.0 += score;
                    }
                }
            }
        }
    }

    /// Search for substring matches in cached content.
    fn substring_search(
        &self,
        query: &str,
        scores: &mut HashMap<HelpId, (f32, MatchedField, String)>,
    ) {
        for (id, content) in &self.content_cache {
            if content.short.to_lowercase().contains(query) {
                let entry =
                    scores
                        .entry(*id)
                        .or_insert((0.0, MatchedField::Short, content.short.clone()));
                entry.0 += WEIGHT_SHORT * 0.5; // Substring matches score lower
            }

            if let Some(ref long) = content.long
                && long.to_lowercase().contains(query)
            {
                let entry = scores
                    .entry(*id)
                    .or_insert((0.0, MatchedField::Long, long.clone()));
                entry.0 += WEIGHT_LONG * 0.5;
            }

            for (key, action) in &content.keybindings {
                if action.to_lowercase().contains(query) {
                    let entry = scores.entry(*id).or_insert((
                        0.0,
                        MatchedField::KeybindingAction,
                        action.clone(),
                    ));
                    entry.0 += WEIGHT_KEYBINDING_ACTION * 0.5;
                }
                if key.to_lowercase().contains(query) {
                    let entry = scores.entry(*id).or_insert((
                        0.0,
                        MatchedField::KeybindingKey,
                        key.clone(),
                    ));
                    entry.0 += WEIGHT_KEYBINDING_KEY * 0.5;
                }
            }
        }
    }

    /// Calculate score for a fuzzy match based on edit distance.
    fn fuzzy_score(distance: usize, token_len: usize, field_weight: f32) -> f32 {
        let similarity = 1.0 - (distance as f32 / token_len.max(1) as f32);
        field_weight * similarity * 0.5 // Fuzzy matches are penalized
    }

    /// Bounded Levenshtein distance. Returns None if distance exceeds max.
    fn levenshtein_bounded(a: &str, b: &str, max: usize) -> Option<usize> {
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        let m = a_chars.len();
        let n = b_chars.len();

        // Quick length check
        if m.abs_diff(n) > max {
            return None;
        }

        // Handle edge cases
        if m == 0 {
            return if n <= max { Some(n) } else { None };
        }
        if n == 0 {
            return if m <= max { Some(m) } else { None };
        }

        // Use two-row optimization for memory efficiency
        let mut prev: Vec<usize> = (0..=n).collect();
        let mut curr = vec![0; n + 1];

        for i in 1..=m {
            curr[0] = i;
            let mut min_in_row = curr[0];

            for j in 1..=n {
                let cost = if a_chars[i - 1] == b_chars[j - 1] {
                    0
                } else {
                    1
                };
                curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
                min_in_row = min_in_row.min(curr[j]);
            }

            // Early termination if minimum exceeds threshold
            if min_in_row > max {
                return None;
            }

            std::mem::swap(&mut prev, &mut curr);
        }

        if prev[n] <= max { Some(prev[n]) } else { None }
    }

    /// Get field weight for scoring.
    fn field_weight(field: MatchedField) -> f32 {
        match field {
            MatchedField::Short => WEIGHT_SHORT,
            MatchedField::Long => WEIGHT_LONG,
            MatchedField::KeybindingAction => WEIGHT_KEYBINDING_ACTION,
            MatchedField::KeybindingKey => WEIGHT_KEYBINDING_KEY,
        }
    }

    /// Number of indexed entries.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.all_ids.len()
    }

    /// Whether the index is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.all_ids.is_empty()
    }

    /// All indexed HelpIds.
    pub fn ids(&self) -> impl Iterator<Item = HelpId> + '_ {
        self.all_ids.iter().copied()
    }

    /// Number of unique tokens in the index.
    #[must_use]
    pub fn token_count(&self) -> usize {
        self.inverted.len()
    }

    /// Jump to the most relevant widget for a search query.
    ///
    /// Returns the [`HelpId`] of the top result, or `None` if no matches.
    /// This is a convenience method for quick navigation scenarios.
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(id) = index.jump_to("save") {
    ///     focus_manager.focus(id.into());
    /// }
    /// ```
    #[must_use = "use the returned id (if any) to navigate"]
    pub fn jump_to(&self, query: &str) -> Option<HelpId> {
        self.search(query, 1).first().map(|r| r.id)
    }

    /// Search and return a single best match with its content.
    ///
    /// Useful for "I'm feeling lucky" style searches where you want
    /// the top result along with display information.
    #[must_use = "use the returned result (if any)"]
    pub fn best_match(&self, query: &str) -> Option<SearchResult> {
        self.search(query, 1).into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help_registry::{HelpContent, Keybinding};

    fn sample_registry() -> HelpRegistry {
        let mut reg = HelpRegistry::new();
        reg.register(
            HelpId(1),
            HelpContent {
                short: "Save the current file".into(),
                long: Some(
                    "Writes the buffer contents to disk, creating the file if needed.".into(),
                ),
                keybindings: vec![Keybinding::new("Ctrl+S", "Save file to disk")],
                see_also: vec![],
            },
        );
        reg.register(
            HelpId(2),
            HelpContent {
                short: "Open file picker".into(),
                long: Some("Opens a file browser to select files.".into()),
                keybindings: vec![Keybinding::new("Ctrl+O", "Open file")],
                see_also: vec![],
            },
        );
        reg.register(
            HelpId(3),
            HelpContent {
                short: "Undo last action".into(),
                long: None,
                keybindings: vec![Keybinding::new("Ctrl+Z", "Undo")],
                see_also: vec![],
            },
        );
        reg.register(
            HelpId(4),
            HelpContent {
                short: "Navigate to definition".into(),
                long: Some("Jump to the definition of the symbol under cursor.".into()),
                keybindings: vec![
                    Keybinding::new("F12", "Go to definition"),
                    Keybinding::new("Ctrl+Click", "Navigate to symbol"),
                ],
                see_also: vec![HelpId(5)],
            },
        );
        reg.register(
            HelpId(5),
            HelpContent {
                short: "Find references".into(),
                long: Some("Find all references to the symbol under cursor.".into()),
                keybindings: vec![Keybinding::new("Shift+F12", "Find all references")],
                see_also: vec![HelpId(4)],
            },
        );
        reg
    }

    // ── Basic search ────────────────────────────────────────────────

    #[test]
    fn search_exact_match() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let results = index.search("save", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, HelpId(1));
    }

    #[test]
    fn search_case_insensitive() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let results = index.search("SAVE", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, HelpId(1));
    }

    #[test]
    fn search_prefix_match() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let results = index.search("nav", 10);
        assert!(!results.is_empty());
        // Should find "Navigate to definition"
        assert!(results.iter().any(|r| r.id == HelpId(4)));
    }

    #[test]
    fn search_keybinding_key() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let results = index.search("ctrl", 10);
        assert!(results.len() >= 3); // Multiple entries have Ctrl bindings
    }

    #[test]
    fn search_keybinding_action() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let results = index.search("undo", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, HelpId(3));
    }

    #[test]
    fn search_empty_query() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let results = index.search("", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn search_no_match() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let results = index.search("xyznonexistent", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn search_limit_respected() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let results = index.search("file", 2);
        assert!(results.len() <= 2);
    }

    // ── Fuzzy matching ──────────────────────────────────────────────

    #[test]
    fn fuzzy_match_typo() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        // "definiton" (typo) should still find "definition"
        let results = index.search("definiton", 10);
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.id == HelpId(4)));
    }

    #[test]
    fn fuzzy_match_partial() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        // "refernce" should find "references"
        let results = index.search("refernce", 10);
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.id == HelpId(5)));
    }

    // ── Scoring ─────────────────────────────────────────────────────

    #[test]
    fn short_matches_rank_higher() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        // "file" appears in short desc for HelpId(1) and (2), should rank high
        let results = index.search("file", 10);
        assert!(!results.is_empty());
        // Both file-related entries should be in top results
        let top_ids: Vec<_> = results.iter().take(2).map(|r| r.id.0).collect();
        assert!(top_ids.contains(&1) || top_ids.contains(&2));
    }

    #[test]
    fn multiple_token_matches_boost_score() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        // "save file" should boost HelpId(1) even higher
        let results = index.search("save file", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, HelpId(1));
    }

    // ── Index properties ────────────────────────────────────────────

    #[test]
    fn build_indexes_all_loaded() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        assert_eq!(index.len(), 5);
        assert!(!index.is_empty());
        assert!(index.token_count() > 0);
    }

    #[test]
    fn ids_returns_all_indexed() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let ids: Vec<_> = index.ids().collect();
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn empty_registry_produces_empty_index() {
        let reg = HelpRegistry::new();
        let index = HelpIndex::build(&reg);

        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert_eq!(index.token_count(), 0);
    }

    #[test]
    fn lazy_entries_not_indexed() {
        let mut reg = HelpRegistry::new();
        reg.register(HelpId(1), HelpContent::short("Loaded entry"));
        reg.register_lazy(HelpId(2), || HelpContent::short("Lazy entry"));

        let index = HelpIndex::build(&reg);

        // Only the loaded entry should be indexed
        assert_eq!(index.len(), 1);

        let results = index.search("lazy", 10);
        assert!(results.is_empty());
    }

    // ── Substring matching ──────────────────────────────────────────

    #[test]
    fn substring_match_in_long() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        // "buffer" appears in long desc of HelpId(1)
        let results = index.search("buffer", 10);
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.id == HelpId(1)));
    }

    // ── Levenshtein distance ────────────────────────────────────────

    #[test]
    fn levenshtein_exact() {
        assert_eq!(HelpIndex::levenshtein_bounded("abc", "abc", 2), Some(0));
    }

    #[test]
    fn levenshtein_one_edit() {
        assert_eq!(HelpIndex::levenshtein_bounded("abc", "abd", 2), Some(1));
        assert_eq!(HelpIndex::levenshtein_bounded("abc", "ab", 2), Some(1));
        assert_eq!(HelpIndex::levenshtein_bounded("abc", "abcd", 2), Some(1));
    }

    #[test]
    fn levenshtein_exceeds_max() {
        assert_eq!(HelpIndex::levenshtein_bounded("abc", "xyz", 1), None);
    }

    #[test]
    fn levenshtein_empty_strings() {
        assert_eq!(HelpIndex::levenshtein_bounded("", "", 2), Some(0));
        assert_eq!(HelpIndex::levenshtein_bounded("abc", "", 3), Some(3));
        assert_eq!(HelpIndex::levenshtein_bounded("", "abc", 3), Some(3));
        assert_eq!(HelpIndex::levenshtein_bounded("abc", "", 2), None);
    }

    // ── Matched field tracking ──────────────────────────────────────

    #[test]
    fn matched_field_display() {
        assert_eq!(format!("{}", MatchedField::Short), "description");
        assert_eq!(format!("{}", MatchedField::Long), "details");
        assert_eq!(format!("{}", MatchedField::KeybindingAction), "keybinding");
        assert_eq!(format!("{}", MatchedField::KeybindingKey), "key");
    }

    #[test]
    fn result_contains_matched_text() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let results = index.search("save", 10);
        assert!(!results.is_empty());
        // Matched text should contain relevant content
        assert!(!results[0].matched_text.is_empty());
    }

    // ── Jump to widget ───────────────────────────────────────────────

    #[test]
    fn jump_to_returns_top_result() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let id = index.jump_to("save");
        assert_eq!(id, Some(HelpId(1)));
    }

    #[test]
    fn jump_to_no_match_returns_none() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let id = index.jump_to("xyznonexistent");
        assert_eq!(id, None);
    }

    #[test]
    fn jump_to_empty_query_returns_none() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let id = index.jump_to("");
        assert_eq!(id, None);
    }

    #[test]
    fn best_match_returns_single_result() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let result = index.best_match("undo");
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.id, HelpId(3));
        assert!(result.score > 0.0);
    }

    #[test]
    fn best_match_no_match_returns_none() {
        let reg = sample_registry();
        let index = HelpIndex::build(&reg);

        let result = index.best_match("xyznonexistent");
        assert!(result.is_none());
    }
}
