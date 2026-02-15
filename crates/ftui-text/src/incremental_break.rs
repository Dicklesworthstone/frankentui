//! Incremental Knuth-Plass line-break optimizer.
//!
//! Wraps the existing [`wrap_optimal`] DP algorithm with paragraph-level
//! caching and dirty-region tracking, so only edited/resized paragraphs are
//! recomputed during reflow.
//!
//! # Incrementality model
//!
//! A document is a sequence of paragraphs separated by hard line breaks (`\n`).
//! The Knuth-Plass DP runs independently per paragraph, so:
//!
//! - A text edit affects at most one paragraph (unless it inserts/removes `\n`).
//! - A width change invalidates all paragraphs.
//! - Cached solutions are keyed by `(text_hash, width)` for staleness detection.
//!
//! For very long documents, only dirty paragraphs are re-broken, providing
//! bounded latency proportional to the edited paragraph's length rather than
//! the total document length.
//!
//! # Usage
//!
//! ```
//! use ftui_text::incremental_break::IncrementalBreaker;
//! use ftui_text::wrap::ParagraphObjective;
//!
//! let mut breaker = IncrementalBreaker::new(80, ParagraphObjective::terminal());
//! let result = breaker.reflow("Hello world. This is a test paragraph.");
//! assert!(!result.lines.is_empty());
//! ```

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::wrap::{ParagraphObjective, display_width, wrap_optimal};

// =========================================================================
// BreakSolution
// =========================================================================

/// Cached break solution for a single paragraph.
#[derive(Debug, Clone)]
struct BreakSolution {
    /// The wrapped lines for this paragraph.
    lines: Vec<String>,
    /// Total cost of the solution.
    total_cost: u64,
    /// Per-line badness values (retained for diagnostics).
    #[allow(dead_code)]
    line_badness: Vec<u64>,
    /// Width used for this solution (for staleness detection).
    width: usize,
    /// Hash of the paragraph text (for staleness detection).
    text_hash: u64,
}

impl BreakSolution {
    /// Check if this cached solution is still valid for the given text and width.
    fn is_valid(&self, text_hash: u64, width: usize) -> bool {
        self.text_hash == text_hash && self.width == width
    }
}

/// Hash a paragraph text deterministically.
fn hash_paragraph(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

// =========================================================================
// EditEvent
// =========================================================================

/// Describes a text edit for incremental reflow.
///
/// The breaker uses this to determine which paragraphs need recomputation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditEvent {
    /// Byte offset where the edit starts.
    pub offset: usize,
    /// Number of bytes deleted from the old text.
    pub deleted: usize,
    /// Number of bytes inserted in the new text.
    pub inserted: usize,
}

// =========================================================================
// ReflowResult
// =========================================================================

/// Result of an incremental reflow operation.
#[derive(Debug, Clone)]
pub struct ReflowResult {
    /// All lines of the document after reflow.
    pub lines: Vec<String>,
    /// Indices of paragraphs that were recomputed (not cache hits).
    pub recomputed: Vec<usize>,
    /// Total cost across all paragraphs (sum of per-paragraph costs).
    pub total_cost: u64,
    /// Number of paragraphs in the document.
    pub paragraph_count: usize,
}

// =========================================================================
// BreakerSnapshot
// =========================================================================

/// Diagnostic snapshot of the incremental breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakerSnapshot {
    /// Target line width.
    pub width: usize,
    /// Number of cached paragraph solutions.
    pub cached_paragraphs: usize,
    /// Number of dirty paragraphs pending reflow.
    pub dirty_paragraphs: usize,
    /// Generation counter.
    pub generation: u64,
    /// Total reflow operations performed.
    pub total_reflows: u64,
    /// Total cache hits across all reflows.
    pub cache_hits: u64,
    /// Total cache misses (recomputations) across all reflows.
    pub cache_misses: u64,
}

// =========================================================================
// IncrementalBreaker
// =========================================================================

/// Incremental Knuth-Plass line-break optimizer with paragraph-level caching.
///
/// Maintains cached break solutions per paragraph and only recomputes
/// paragraphs that have been modified or whose line width has changed.
#[derive(Debug, Clone)]
pub struct IncrementalBreaker {
    /// Target line width in cells.
    width: usize,
    /// Paragraph objective configuration.
    objective: ParagraphObjective,
    /// Cached solutions per paragraph index.
    solutions: Vec<Option<BreakSolution>>,
    /// Generation counter (incremented on any change).
    generation: u64,
    /// Total reflow operations.
    total_reflows: u64,
    /// Total cache hits.
    cache_hits: u64,
    /// Total cache misses.
    cache_misses: u64,
}

impl IncrementalBreaker {
    /// Create a new incremental breaker with the given line width and objective.
    #[must_use]
    pub fn new(width: usize, objective: ParagraphObjective) -> Self {
        Self {
            width,
            objective,
            solutions: Vec::new(),
            generation: 0,
            total_reflows: 0,
            cache_hits: 0,
            cache_misses: 0,
        }
    }

    /// Create a breaker with terminal-optimized defaults.
    #[must_use]
    pub fn terminal(width: usize) -> Self {
        Self::new(width, ParagraphObjective::terminal())
    }

    /// Current line width.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Current generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Current paragraph objective.
    #[must_use]
    pub fn objective(&self) -> &ParagraphObjective {
        &self.objective
    }

    /// Update the target line width.
    ///
    /// All cached solutions are invalidated (width change affects every paragraph).
    pub fn set_width(&mut self, width: usize) {
        if self.width != width {
            self.width = width;
            self.generation += 1;
            // Don't clear solutions — they'll be detected as stale via width mismatch.
        }
    }

    /// Update the paragraph objective.
    ///
    /// All cached solutions are invalidated.
    pub fn set_objective(&mut self, objective: ParagraphObjective) {
        self.objective = objective;
        self.generation += 1;
        self.solutions.clear();
    }

    /// Invalidate all cached solutions, forcing full recomputation on next reflow.
    pub fn invalidate_all(&mut self) {
        self.generation += 1;
        self.solutions.clear();
    }

    /// Invalidate a specific paragraph by index.
    ///
    /// Safe to call with out-of-bounds index (no-op).
    pub fn invalidate_paragraph(&mut self, paragraph_idx: usize) {
        if paragraph_idx < self.solutions.len() {
            self.solutions[paragraph_idx] = None;
            self.generation += 1;
        }
    }

    /// Notify the breaker of a text edit.
    ///
    /// This determines which paragraph(s) are affected by the edit and
    /// invalidates their cached solutions. The caller must provide the
    /// *old* text so the breaker can locate paragraph boundaries.
    pub fn notify_edit(&mut self, old_text: &str, event: &EditEvent) {
        let paragraphs: Vec<&str> = old_text.split('\n').collect();

        // Find which paragraph(s) the edit range overlaps.
        let mut byte_offset = 0;
        for (idx, para) in paragraphs.iter().enumerate() {
            let para_end = byte_offset + para.len();
            // Check if the edit overlaps this paragraph
            let edit_end = event.offset + event.deleted;
            if event.offset <= para_end && edit_end >= byte_offset {
                self.invalidate_paragraph(idx);
            }
            byte_offset = para_end + 1; // +1 for the '\n'
        }

        // If the edit introduces or removes newlines, invalidate everything
        // after the edit point (paragraph structure changed).
        if event.deleted != event.inserted {
            let mut byte_offset = 0;
            for (idx, para) in paragraphs.iter().enumerate() {
                let para_end = byte_offset + para.len();
                if para_end >= event.offset {
                    self.invalidate_paragraph(idx);
                }
                byte_offset = para_end + 1;
            }
        }
    }

    /// Reflow the document text, reusing cached solutions where possible.
    ///
    /// This is the primary entry point. It splits the text into paragraphs,
    /// checks each against cached solutions, and only recomputes dirty ones.
    pub fn reflow(&mut self, text: &str) -> ReflowResult {
        self.total_reflows += 1;

        let paragraphs: Vec<&str> = text.split('\n').collect();
        let para_count = paragraphs.len();

        // Resize solution cache to match paragraph count.
        self.solutions.resize_with(para_count, || None);
        // Truncate excess if text has fewer paragraphs now.
        self.solutions.truncate(para_count);

        let mut all_lines = Vec::new();
        let mut recomputed = Vec::new();
        let mut total_cost = 0u64;

        for (idx, paragraph) in paragraphs.iter().enumerate() {
            let text_hash = hash_paragraph(paragraph);

            // Check cache validity.
            let cached_valid = self.solutions[idx]
                .as_ref()
                .is_some_and(|sol| sol.is_valid(text_hash, self.width));

            if cached_valid {
                let sol = self.solutions[idx].as_ref().unwrap();
                all_lines.extend(sol.lines.iter().cloned());
                total_cost = total_cost.saturating_add(sol.total_cost);
                self.cache_hits += 1;
            } else {
                // Recompute this paragraph.
                let sol = self.break_paragraph(paragraph, text_hash);
                all_lines.extend(sol.lines.iter().cloned());
                total_cost = total_cost.saturating_add(sol.total_cost);
                self.solutions[idx] = Some(sol);
                recomputed.push(idx);
                self.cache_misses += 1;
            }
        }

        ReflowResult {
            lines: all_lines,
            recomputed,
            total_cost,
            paragraph_count: para_count,
        }
    }

    /// Reflow with forced full recomputation (no caching).
    pub fn reflow_full(&mut self, text: &str) -> ReflowResult {
        self.invalidate_all();
        self.reflow(text)
    }

    /// Break a single paragraph using the Knuth-Plass algorithm.
    fn break_paragraph(&self, paragraph: &str, text_hash: u64) -> BreakSolution {
        if paragraph.is_empty() {
            return BreakSolution {
                lines: vec![String::new()],
                total_cost: 0,
                line_badness: vec![0],
                width: self.width,
                text_hash,
            };
        }

        let result = wrap_optimal(paragraph, self.width);

        // Apply widow/orphan demerits from the objective.
        let mut adjusted_cost = result.total_cost;
        if result.lines.len() > 1 {
            if let Some(last) = result.lines.last() {
                let last_chars = display_width(last);
                adjusted_cost =
                    adjusted_cost.saturating_add(self.objective.widow_demerits(last_chars));
            }
            if let Some(first) = result.lines.first() {
                let first_chars = display_width(first);
                adjusted_cost =
                    adjusted_cost.saturating_add(self.objective.orphan_demerits(first_chars));
            }
        }

        BreakSolution {
            lines: result.lines,
            total_cost: adjusted_cost,
            line_badness: result.line_badness,
            width: self.width,
            text_hash,
        }
    }

    /// Diagnostic snapshot of the current state.
    #[must_use]
    pub fn snapshot(&self) -> BreakerSnapshot {
        let cached = self.solutions.iter().filter(|s| s.is_some()).count();
        let dirty = self.solutions.iter().filter(|s| s.is_none()).count();
        BreakerSnapshot {
            width: self.width,
            cached_paragraphs: cached,
            dirty_paragraphs: dirty,
            generation: self.generation,
            total_reflows: self.total_reflows,
            cache_hits: self.cache_hits,
            cache_misses: self.cache_misses,
        }
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn breaker(width: usize) -> IncrementalBreaker {
        IncrementalBreaker::terminal(width)
    }

    // ── Basic reflow ──────────────────────────────────────────────────

    #[test]
    fn reflow_empty_text() {
        let mut b = breaker(80);
        let r = b.reflow("");
        assert_eq!(r.lines, vec![""]);
        assert_eq!(r.paragraph_count, 1);
    }

    #[test]
    fn reflow_single_word() {
        let mut b = breaker(80);
        let r = b.reflow("hello");
        assert_eq!(r.lines, vec!["hello"]);
        assert_eq!(r.total_cost, 0); // last line has zero badness
    }

    #[test]
    fn reflow_fits_in_width() {
        let mut b = breaker(80);
        let r = b.reflow("The quick brown fox jumps over the lazy dog.");
        assert_eq!(r.lines.len(), 1);
        assert_eq!(r.recomputed, vec![0]);
    }

    #[test]
    fn reflow_wraps_long_text() {
        let mut b = breaker(20);
        let text = "The quick brown fox jumps over the lazy dog.";
        let r = b.reflow(text);
        assert!(r.lines.len() > 1);
        // All lines should be within width (except possibly last)
        for line in &r.lines[..r.lines.len() - 1] {
            assert!(
                display_width(line) <= 20,
                "line too wide: {:?} (width {})",
                line,
                display_width(line)
            );
        }
    }

    #[test]
    fn reflow_preserves_paragraphs() {
        let mut b = breaker(80);
        let r = b.reflow("First paragraph.\nSecond paragraph.\nThird.");
        assert_eq!(r.paragraph_count, 3);
        assert_eq!(r.lines.len(), 3);
    }

    // ── Caching ───────────────────────────────────────────────────────

    #[test]
    fn second_reflow_uses_cache() {
        let mut b = breaker(80);
        let text = "Hello world.";
        b.reflow(text);
        let r2 = b.reflow(text);
        assert!(r2.recomputed.is_empty(), "second reflow should be cached");
    }

    #[test]
    fn cache_hit_increments_counter() {
        let mut b = breaker(80);
        let text = "Hello.";
        b.reflow(text);
        b.reflow(text);
        let snap = b.snapshot();
        assert_eq!(snap.cache_hits, 1);
        assert_eq!(snap.cache_misses, 1);
    }

    #[test]
    fn width_change_invalidates_cache() {
        let mut b = breaker(80);
        let text = "Hello world.";
        b.reflow(text);
        b.set_width(40);
        let r = b.reflow(text);
        assert_eq!(r.recomputed, vec![0]);
    }

    #[test]
    fn text_change_invalidates_paragraph() {
        let mut b = breaker(80);
        b.reflow("Hello.\nWorld.");
        let r2 = b.reflow("Hello.\nChanged.");
        // Paragraph 0 should be cached, paragraph 1 recomputed
        assert_eq!(r2.recomputed, vec![1]);
    }

    #[test]
    fn invalidate_all_forces_recomputation() {
        let mut b = breaker(80);
        b.reflow("Hello.\nWorld.");
        b.invalidate_all();
        let r = b.reflow("Hello.\nWorld.");
        assert_eq!(r.recomputed, vec![0, 1]);
    }

    #[test]
    fn invalidate_paragraph_selective() {
        let mut b = breaker(80);
        b.reflow("A.\nB.\nC.");
        b.invalidate_paragraph(1);
        let r = b.reflow("A.\nB.\nC.");
        // Only paragraph 1 should be recomputed
        assert_eq!(r.recomputed, vec![1]);
    }

    #[test]
    fn invalidate_out_of_bounds_is_noop() {
        let mut b = breaker(80);
        b.reflow("Hello.");
        b.invalidate_paragraph(999); // should not panic
    }

    // ── Edit notification ──────────────────────────────────────────────

    #[test]
    fn notify_edit_invalidates_affected_paragraph() {
        let mut b = breaker(80);
        let text = "First.\nSecond.\nThird.";
        b.reflow(text);
        // Edit in "Second." (byte offset 7..14)
        b.notify_edit(
            text,
            &EditEvent {
                offset: 7,
                deleted: 7,
                inserted: 8,
            },
        );
        let r = b.reflow("First.\nChanged!.\nThird.");
        // Paragraphs 1 and 2 should be recomputed (edit + structure change)
        assert!(r.recomputed.contains(&1));
    }

    #[test]
    fn notify_edit_at_start() {
        let mut b = breaker(80);
        let text = "Hello.\nWorld.";
        b.reflow(text);
        b.notify_edit(
            text,
            &EditEvent {
                offset: 0,
                deleted: 5,
                inserted: 3,
            },
        );
        let r = b.reflow("Hi.\nWorld.");
        assert!(r.recomputed.contains(&0));
    }

    // ── Width changes ──────────────────────────────────────────────────

    #[test]
    fn set_width_same_is_noop() {
        let mut b = breaker(80);
        b.reflow("Test.");
        let prev_gen = b.generation();
        b.set_width(80);
        assert_eq!(b.generation(), prev_gen);
    }

    #[test]
    fn set_width_different_bumps_generation() {
        let mut b = breaker(80);
        let prev_gen = b.generation();
        b.set_width(40);
        assert!(b.generation() > prev_gen);
    }

    // ── Objective changes ──────────────────────────────────────────────

    #[test]
    fn set_objective_clears_cache() {
        let mut b = breaker(80);
        b.reflow("Hello world.");
        b.set_objective(ParagraphObjective::typographic());
        let snap = b.snapshot();
        assert_eq!(snap.cached_paragraphs, 0);
    }

    // ── Reflow full ────────────────────────────────────────────────────

    #[test]
    fn reflow_full_recomputes_everything() {
        let mut b = breaker(80);
        b.reflow("A.\nB.");
        let r = b.reflow_full("A.\nB.");
        assert_eq!(r.recomputed, vec![0, 1]);
    }

    // ── Snapshot diagnostics ───────────────────────────────────────────

    #[test]
    fn snapshot_initial_state() {
        let b = breaker(80);
        let snap = b.snapshot();
        assert_eq!(snap.width, 80);
        assert_eq!(snap.cached_paragraphs, 0);
        assert_eq!(snap.dirty_paragraphs, 0);
        assert_eq!(snap.generation, 0);
        assert_eq!(snap.total_reflows, 0);
    }

    #[test]
    fn snapshot_after_reflow() {
        let mut b = breaker(80);
        b.reflow("A.\nB.\nC.");
        let snap = b.snapshot();
        assert_eq!(snap.cached_paragraphs, 3);
        assert_eq!(snap.total_reflows, 1);
        assert_eq!(snap.cache_misses, 3);
    }

    // ── Determinism ────────────────────────────────────────────────────

    #[test]
    fn reflow_deterministic() {
        let mut b1 = breaker(30);
        let mut b2 = breaker(30);
        let text = "The quick brown fox jumps over the lazy dog near the river bank.";
        let r1 = b1.reflow(text);
        let r2 = b2.reflow(text);
        assert_eq!(r1.lines, r2.lines);
        assert_eq!(r1.total_cost, r2.total_cost);
    }

    #[test]
    fn reflow_idempotent() {
        let mut b = breaker(30);
        let text = "The quick brown fox jumps over the lazy dog.";
        let r1 = b.reflow(text);
        let r2 = b.reflow_full(text);
        assert_eq!(r1.lines, r2.lines);
        assert_eq!(r1.total_cost, r2.total_cost);
    }

    // ── Edge cases ─────────────────────────────────────────────────────

    #[test]
    fn reflow_zero_width() {
        let mut b = breaker(0);
        let r = b.reflow("Hello");
        assert!(!r.lines.is_empty());
    }

    #[test]
    fn reflow_very_narrow() {
        let mut b = breaker(1);
        let r = b.reflow("abc");
        // Should not panic; may have forced breaks
        assert!(!r.lines.is_empty());
    }

    #[test]
    fn reflow_only_newlines() {
        let mut b = breaker(80);
        let r = b.reflow("\n\n\n");
        assert_eq!(r.paragraph_count, 4); // 3 newlines = 4 paragraphs
    }

    #[test]
    fn reflow_paragraph_count_changes() {
        let mut b = breaker(80);
        b.reflow("A.\nB.\nC.");
        // Now text has fewer paragraphs
        let r = b.reflow("A.\nB.");
        assert_eq!(r.paragraph_count, 2);
    }

    #[test]
    fn reflow_paragraph_count_grows() {
        let mut b = breaker(80);
        b.reflow("A.\nB.");
        let r = b.reflow("A.\nB.\nC.\nD.");
        assert_eq!(r.paragraph_count, 4);
        // New paragraphs 2,3 should be recomputed
        assert!(r.recomputed.contains(&2));
        assert!(r.recomputed.contains(&3));
    }

    #[test]
    fn multiple_edits_accumulate() {
        let mut b = breaker(80);
        b.reflow("One.\nTwo.\nThree.");
        b.invalidate_paragraph(0);
        b.invalidate_paragraph(2);
        let r = b.reflow("One.\nTwo.\nThree.");
        assert_eq!(r.recomputed, vec![0, 2]);
    }

    #[test]
    fn long_paragraph_performance() {
        let mut b = breaker(60);
        let text = "word ".repeat(500);
        let r = b.reflow(&text);
        assert!(r.lines.len() > 1);
        // Second reflow should be instant (cached)
        let r2 = b.reflow(&text);
        assert!(r2.recomputed.is_empty());
    }

    // ── Constructor variants ───────────────────────────────────────────

    #[test]
    fn terminal_constructor() {
        let b = IncrementalBreaker::terminal(120);
        assert_eq!(b.width(), 120);
        assert_eq!(b.objective().line_penalty, 20); // terminal preset
    }

    #[test]
    fn new_with_default_objective() {
        let b = IncrementalBreaker::new(80, ParagraphObjective::default());
        assert_eq!(b.objective().line_penalty, 10); // TeX default
    }
}
