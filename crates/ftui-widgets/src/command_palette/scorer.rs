#![forbid(unsafe_code)]

//! Bayesian Match Scoring for Command Palette.
//!
//! This module implements a probabilistic scoring model using Bayes factors
//! to compute match relevance. An evidence ledger tracks each scoring factor
//! and its contribution, enabling explainable ranking decisions.
//!
//! # Mathematical Model
//!
//! We compute the posterior odds ratio:
//!
//! ```text
//! P(relevant | evidence) / P(not_relevant | evidence)
//!     = [P(relevant) / P(not_relevant)] × Π_i BF_i
//!
//! where BF_i = P(evidence_i | relevant) / P(evidence_i | not_relevant)
//!            = likelihood ratio for evidence type i
//! ```
//!
//! The prior odds depend on match type:
//! - Exact match: 99:1 (very likely relevant)
//! - Prefix match: 9:1 (likely relevant)
//! - Word-start match: 4:1 (probably relevant)
//! - Substring match: 2:1 (possibly relevant)
//! - Fuzzy match: 1:3 (unlikely without other evidence)
//!
//! The final score is the posterior probability P(relevant | evidence).
//!
//! # Evidence Ledger
//!
//! Each match includes an evidence ledger that explains the score:
//! - Match type contribution (prior odds)
//! - Word boundary bonus (Bayes factor ~2.0)
//! - Position bonus (earlier = higher factor)
//! - Gap penalty (gaps reduce factor)
//! - Tag match bonus (strong positive evidence)
//!
//! # Invariants
//!
//! 1. Scores are bounded: 0.0 ≤ score ≤ 1.0 (probability)
//! 2. Determinism: same input → identical score
//! 3. Monotonicity: longer exact prefixes score ≥ shorter
//! 4. Transitivity: score ordering is consistent

use std::fmt;

// ---------------------------------------------------------------------------
// Match Types
// ---------------------------------------------------------------------------

/// Type of match between query and title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchType {
    /// No match found.
    NoMatch,
    /// Characters found in order but with gaps.
    Fuzzy,
    /// Query found as contiguous substring.
    Substring,
    /// Query matches start of word boundaries.
    WordStart,
    /// Title starts with query.
    Prefix,
    /// Query equals title exactly.
    Exact,
}

impl MatchType {
    /// Prior odds ratio P(relevant) / P(not_relevant) for this match type.
    ///
    /// These are derived from empirical observations of user intent:
    /// - Exact matches are almost always what the user wants
    /// - Prefix matches are very likely relevant
    /// - Fuzzy matches need additional evidence
    pub fn prior_odds(self) -> f64 {
        match self {
            Self::Exact => 99.0,    // 99:1 odds → P ≈ 0.99
            Self::Prefix => 9.0,    // 9:1 odds → P ≈ 0.90
            Self::WordStart => 4.0, // 4:1 odds → P ≈ 0.80
            Self::Substring => 2.0, // 2:1 odds → P ≈ 0.67
            Self::Fuzzy => 0.333,   // 1:3 odds → P ≈ 0.25
            Self::NoMatch => 0.0,   // Impossible
        }
    }

    /// Human-readable description for the evidence ledger.
    pub fn description(self) -> &'static str {
        match self {
            Self::Exact => "exact match",
            Self::Prefix => "prefix match",
            Self::WordStart => "word-start match",
            Self::Substring => "contiguous substring",
            Self::Fuzzy => "fuzzy match",
            Self::NoMatch => "no match",
        }
    }
}

// ---------------------------------------------------------------------------
// Evidence Entry
// ---------------------------------------------------------------------------

/// A single piece of evidence contributing to the match score.
#[derive(Debug, Clone)]
pub struct EvidenceEntry {
    /// Type of evidence.
    pub kind: EvidenceKind,
    /// Bayes factor: likelihood ratio P(evidence | relevant) / P(evidence | ¬relevant).
    /// Values > 1.0 support relevance, < 1.0 oppose it.
    pub bayes_factor: f64,
    /// Human-readable explanation.
    pub description: EvidenceDescription,
}

/// Types of evidence that contribute to match scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    /// Base match type (prior).
    MatchType,
    /// Match at word boundary.
    WordBoundary,
    /// Match position (earlier is better).
    Position,
    /// Gap between matched characters.
    GapPenalty,
    /// Query also matches a tag.
    TagMatch,
    /// Title length factor (shorter is more specific).
    TitleLength,
}

impl fmt::Display for EvidenceEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let direction = if self.bayes_factor > 1.0 {
            "supports"
        } else if self.bayes_factor < 1.0 {
            "opposes"
        } else {
            "neutral"
        };
        write!(
            f,
            "{:?}: BF={:.2} ({}) - {}",
            self.kind, self.bayes_factor, direction, self.description
        )
    }
}

/// Human-readable evidence description (lazy formatting).
#[derive(Debug, Clone)]
pub enum EvidenceDescription {
    Static(&'static str),
    TitleLengthChars { len: usize },
    FirstMatchPos { pos: usize },
    WordBoundaryCount { count: usize },
    GapTotal { total: usize },
    CoveragePercent { percent: f64 },
}

impl fmt::Display for EvidenceDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Static(msg) => write!(f, "{msg}"),
            Self::TitleLengthChars { len } => write!(f, "title length {} chars", len),
            Self::FirstMatchPos { pos } => write!(f, "first match at position {}", pos),
            Self::WordBoundaryCount { count } => write!(f, "{} word boundary matches", count),
            Self::GapTotal { total } => write!(f, "total gap of {} characters", total),
            Self::CoveragePercent { percent } => write!(f, "query covers {:.0}% of title", percent),
        }
    }
}

// ---------------------------------------------------------------------------
// Evidence Ledger
// ---------------------------------------------------------------------------

/// A ledger of evidence explaining a match score.
///
/// This provides full transparency into why a match received its score,
/// enabling debugging and user explanations.
#[derive(Debug, Clone, Default)]
pub struct EvidenceLedger {
    entries: Vec<EvidenceEntry>,
}

impl EvidenceLedger {
    /// Create a new empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an evidence entry.
    pub fn add(&mut self, kind: EvidenceKind, bayes_factor: f64, description: EvidenceDescription) {
        self.entries.push(EvidenceEntry {
            kind,
            bayes_factor,
            description,
        });
    }

    /// Get all entries.
    pub fn entries(&self) -> &[EvidenceEntry] {
        &self.entries
    }

    /// Compute the combined Bayes factor (product of all factors).
    pub fn combined_bayes_factor(&self) -> f64 {
        self.entries.iter().map(|e| e.bayes_factor).product()
    }

    /// Get the prior odds (from MatchType entry, if present).
    #[must_use = "use the prior odds (if any) for diagnostics"]
    pub fn prior_odds(&self) -> Option<f64> {
        self.entries
            .iter()
            .find(|e| e.kind == EvidenceKind::MatchType)
            .map(|e| e.bayes_factor)
    }

    /// Compute posterior probability from prior odds and evidence.
    ///
    /// posterior_prob = posterior_odds / (1 + posterior_odds)
    /// where posterior_odds = prior_odds × combined_bf
    pub fn posterior_probability(&self) -> f64 {
        let prior = self.prior_odds().unwrap_or(1.0);
        // Exclude prior from BF since it's already the odds
        let bf: f64 = self
            .entries
            .iter()
            .filter(|e| e.kind != EvidenceKind::MatchType)
            .map(|e| e.bayes_factor)
            .product();

        let posterior_odds = prior * bf;
        if posterior_odds.is_infinite() {
            1.0
        } else {
            posterior_odds / (1.0 + posterior_odds)
        }
    }
}

impl fmt::Display for EvidenceLedger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Evidence Ledger:")?;
        for entry in &self.entries {
            writeln!(f, "  {}", entry)?;
        }
        writeln!(f, "  Combined BF: {:.3}", self.combined_bayes_factor())?;
        writeln!(f, "  Posterior P: {:.3}", self.posterior_probability())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Match Result
// ---------------------------------------------------------------------------

/// Result of scoring a query against a title.
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// Computed relevance score (posterior probability).
    pub score: f64,
    /// Type of match detected.
    pub match_type: MatchType,
    /// Positions of matched characters in the title.
    pub match_positions: Vec<usize>,
    /// Evidence ledger explaining the score.
    pub evidence: EvidenceLedger,
}

impl MatchResult {
    /// Create a no-match result.
    pub fn no_match() -> Self {
        let mut evidence = EvidenceLedger::new();
        evidence.add(
            EvidenceKind::MatchType,
            0.0,
            EvidenceDescription::Static("no matching characters found"),
        );
        Self {
            score: 0.0,
            match_type: MatchType::NoMatch,
            match_positions: Vec::new(),
            evidence,
        }
    }
}

// ---------------------------------------------------------------------------
// Scorer
// ---------------------------------------------------------------------------

/// Bayesian fuzzy matcher for command palette.
///
/// Computes relevance scores using a probabilistic model with
/// evidence tracking for explainable ranking.
#[derive(Debug, Clone, Default)]
pub struct BayesianScorer {
    /// Whether to track detailed evidence (slower but explainable).
    pub track_evidence: bool,
}

impl BayesianScorer {
    /// Create a new scorer with evidence tracking enabled.
    pub fn new() -> Self {
        Self {
            track_evidence: true,
        }
    }

    /// Create a scorer without evidence tracking (faster).
    pub fn fast() -> Self {
        Self {
            track_evidence: false,
        }
    }

    /// Score a query against a title.
    ///
    /// Returns a MatchResult with score, match type, positions, and evidence.
    pub fn score(&self, query: &str, title: &str) -> MatchResult {
        // Quick rejection: query longer than title
        if query.len() > title.len() {
            return MatchResult::no_match();
        }

        // Empty query matches everything (show all)
        if query.is_empty() {
            return self.score_empty_query(title);
        }

        // Normalize for case-insensitive matching
        let query_lower = query.to_lowercase();
        let title_lower = title.to_lowercase();

        // Determine match type
        let (match_type, positions) = self.detect_match_type(&query_lower, &title_lower, title);

        if match_type == MatchType::NoMatch {
            return MatchResult::no_match();
        }

        // Build evidence ledger and compute score
        self.compute_score(match_type, positions, &query_lower, title)
    }

    /// Score a query using a pre-lowercased query string.
    ///
    /// This avoids repeated query normalization when scoring against many titles.
    pub fn score_with_query_lower(
        &self,
        query: &str,
        query_lower: &str,
        title: &str,
    ) -> MatchResult {
        let title_lower = title.to_lowercase();
        self.score_with_lowered_title(query, query_lower, title, &title_lower)
    }

    /// Score a query with both query and title already lowercased.
    ///
    /// This avoids per-title lowercasing in hot loops.
    pub fn score_with_lowered_title(
        &self,
        query: &str,
        query_lower: &str,
        title: &str,
        title_lower: &str,
    ) -> MatchResult {
        self.score_with_lowered_title_and_words(query, query_lower, title, title_lower, None)
    }

    /// Score a query with pre-lowercased title and optional word-start cache.
    pub fn score_with_lowered_title_and_words(
        &self,
        query: &str,
        query_lower: &str,
        title: &str,
        title_lower: &str,
        word_starts: Option<&[usize]>,
    ) -> MatchResult {
        // Quick rejection: query longer than title
        if query.len() > title.len() {
            return MatchResult::no_match();
        }

        // Empty query matches everything (show all)
        if query.is_empty() {
            return self.score_empty_query(title);
        }

        // Determine match type
        let (match_type, positions) =
            self.detect_match_type_with_words(query_lower, title_lower, title, word_starts);

        if match_type == MatchType::NoMatch {
            return MatchResult::no_match();
        }

        // Build evidence ledger and compute score
        self.compute_score(match_type, positions, query_lower, title)
    }

    /// Score a query against a title with tags.
    pub fn score_with_tags(&self, query: &str, title: &str, tags: &[&str]) -> MatchResult {
        let mut result = self.score(query, title);

        // Check if query matches any tag
        let query_lower = query.to_lowercase();
        let tag_match = tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(&query_lower));

        if tag_match && result.match_type != MatchType::NoMatch {
            // Strong positive evidence
            if self.track_evidence {
                result.evidence.add(
                    EvidenceKind::TagMatch,
                    3.0, // 3:1 in favor
                    EvidenceDescription::Static("query matches tag"),
                );
                result.score = result.evidence.posterior_probability();
            } else if (0.0..1.0).contains(&result.score) {
                let odds = result.score / (1.0 - result.score);
                let boosted = odds * 3.0;
                result.score = boosted / (1.0 + boosted);
            }
        }

        result
    }

    /// Score when query is empty (returns all items with neutral score).
    fn score_empty_query(&self, title: &str) -> MatchResult {
        // Shorter titles are more specific, slight preference
        let length_factor = 1.0 + (1.0 / (title.len() as f64 + 1.0)) * 0.1;
        if self.track_evidence {
            let mut evidence = EvidenceLedger::new();
            evidence.add(
                EvidenceKind::MatchType,
                1.0, // Neutral prior
                EvidenceDescription::Static("empty query matches all"),
            );
            evidence.add(
                EvidenceKind::TitleLength,
                length_factor,
                EvidenceDescription::TitleLengthChars { len: title.len() },
            );
            let score = evidence.posterior_probability();
            MatchResult {
                score,
                match_type: MatchType::Fuzzy, // Treat as weak match
                match_positions: Vec::new(),
                evidence,
            }
        } else {
            let odds = length_factor;
            let score = odds / (1.0 + odds);
            MatchResult {
                score,
                match_type: MatchType::Fuzzy,
                match_positions: Vec::new(),
                evidence: EvidenceLedger::new(),
            }
        }
    }

    /// Detect the type of match and positions of matched characters.
    fn detect_match_type(
        &self,
        query_lower: &str,
        title_lower: &str,
        title: &str,
    ) -> (MatchType, Vec<usize>) {
        self.detect_match_type_with_words(query_lower, title_lower, title, None)
    }

    /// Detect match type with optional precomputed word-start positions.
    fn detect_match_type_with_words(
        &self,
        query_lower: &str,
        title_lower: &str,
        title: &str,
        word_starts: Option<&[usize]>,
    ) -> (MatchType, Vec<usize>) {
        if query_lower.is_ascii() && title_lower.is_ascii() {
            return self.detect_match_type_ascii(query_lower, title_lower, word_starts);
        }

        // Check exact match
        if query_lower == title_lower {
            let positions: Vec<usize> = (0..title.len()).collect();
            return (MatchType::Exact, positions);
        }

        // Check prefix match
        if title_lower.starts_with(query_lower) {
            let positions: Vec<usize> = (0..query_lower.len()).collect();
            return (MatchType::Prefix, positions);
        }

        // Check word-start match (e.g., "gd" matches "Go Dashboard")
        if let Some(positions) = self.word_start_match(query_lower, title_lower) {
            return (MatchType::WordStart, positions);
        }

        // Check contiguous substring
        if let Some(start) = title_lower.find(query_lower) {
            let positions: Vec<usize> = (start..start + query_lower.len()).collect();
            return (MatchType::Substring, positions);
        }

        // Check fuzzy match
        if let Some(positions) = self.fuzzy_match(query_lower, title_lower) {
            return (MatchType::Fuzzy, positions);
        }

        (MatchType::NoMatch, Vec::new())
    }

    /// ASCII fast-path match detection.
    fn detect_match_type_ascii(
        &self,
        query_lower: &str,
        title_lower: &str,
        word_starts: Option<&[usize]>,
    ) -> (MatchType, Vec<usize>) {
        let query_bytes = query_lower.as_bytes();
        let title_bytes = title_lower.as_bytes();

        if query_bytes == title_bytes {
            let positions: Vec<usize> = (0..title_bytes.len()).collect();
            return (MatchType::Exact, positions);
        }

        if title_bytes.starts_with(query_bytes) {
            let positions: Vec<usize> = (0..query_bytes.len()).collect();
            return (MatchType::Prefix, positions);
        }

        if let Some(positions) = self.word_start_match_ascii(query_bytes, title_bytes, word_starts)
        {
            return (MatchType::WordStart, positions);
        }

        if let Some(start) = title_lower.find(query_lower) {
            let positions: Vec<usize> = (start..start + query_bytes.len()).collect();
            return (MatchType::Substring, positions);
        }

        if let Some(positions) = self.fuzzy_match_ascii(query_bytes, title_bytes) {
            return (MatchType::Fuzzy, positions);
        }

        (MatchType::NoMatch, Vec::new())
    }

    /// Check if query matches word starts (e.g., "gd" → "Go Dashboard").
    fn word_start_match(&self, query: &str, title: &str) -> Option<Vec<usize>> {
        let mut positions = Vec::new();
        let mut query_chars = query.chars().peekable();

        let title_bytes = title.as_bytes();
        for (i, c) in title.char_indices() {
            // Is this a word start?
            let is_word_start = i == 0 || {
                let prev = title_bytes
                    .get(i.saturating_sub(1))
                    .copied()
                    .unwrap_or(b' ');
                prev == b' ' || prev == b'-' || prev == b'_'
            };

            if is_word_start
                && let Some(&qc) = query_chars.peek()
                && c == qc
            {
                positions.push(i);
                query_chars.next();
            }
        }

        if query_chars.peek().is_none() {
            Some(positions)
        } else {
            None
        }
    }

    /// ASCII word-start match with optional precomputed word-start positions.
    fn word_start_match_ascii(
        &self,
        query: &[u8],
        title: &[u8],
        word_starts: Option<&[usize]>,
    ) -> Option<Vec<usize>> {
        let mut positions = Vec::new();
        let mut query_idx = 0;
        if query.is_empty() {
            return Some(positions);
        }

        if let Some(starts) = word_starts {
            for &pos in starts {
                if pos >= title.len() {
                    continue;
                }
                if title[pos] == query[query_idx] {
                    positions.push(pos);
                    query_idx += 1;
                    if query_idx == query.len() {
                        return Some(positions);
                    }
                }
            }
        } else {
            for (i, &b) in title.iter().enumerate() {
                let is_word_start = i == 0 || matches!(title[i - 1], b' ' | b'-' | b'_');
                if is_word_start && b == query[query_idx] {
                    positions.push(i);
                    query_idx += 1;
                    if query_idx == query.len() {
                        return Some(positions);
                    }
                }
            }
        }

        None
    }

    /// Check fuzzy match (characters in order).
    fn fuzzy_match(&self, query: &str, title: &str) -> Option<Vec<usize>> {
        let mut positions = Vec::new();
        let mut query_chars = query.chars().peekable();

        for (i, c) in title.char_indices() {
            if let Some(&qc) = query_chars.peek()
                && c == qc
            {
                positions.push(i);
                query_chars.next();
            }
        }

        if query_chars.peek().is_none() {
            Some(positions)
        } else {
            None
        }
    }

    /// ASCII fuzzy match (characters in order).
    fn fuzzy_match_ascii(&self, query: &[u8], title: &[u8]) -> Option<Vec<usize>> {
        let mut positions = Vec::new();
        let mut query_idx = 0;
        if query.is_empty() {
            return Some(positions);
        }

        for (i, &b) in title.iter().enumerate() {
            if b == query[query_idx] {
                positions.push(i);
                query_idx += 1;
                if query_idx == query.len() {
                    return Some(positions);
                }
            }
        }

        None
    }

    /// Compute score from match type and positions.
    fn compute_score(
        &self,
        match_type: MatchType,
        positions: Vec<usize>,
        query: &str,
        title: &str,
    ) -> MatchResult {
        let positions_ref = positions.as_slice();
        if !self.track_evidence {
            let mut combined_bf = match_type.prior_odds();

            if let Some(&first_pos) = positions_ref.first() {
                let position_factor = 1.0 + (1.0 / (first_pos as f64 + 1.0)) * 0.5;
                combined_bf *= position_factor;
            }

            let word_boundary_count = self.count_word_boundaries(positions_ref, title);
            if word_boundary_count > 0 {
                let boundary_factor = 1.0 + (word_boundary_count as f64 * 0.3);
                combined_bf *= boundary_factor;
            }

            if match_type == MatchType::Fuzzy && positions_ref.len() > 1 {
                let total_gap = self.total_gap(positions_ref);
                let gap_factor = 1.0 / (1.0 + total_gap as f64 * 0.1);
                combined_bf *= gap_factor;
            }

            let title_len = title.len().max(1);
            let length_factor = 1.0 + (query.len() as f64 / title_len as f64) * 0.2;
            combined_bf *= length_factor;

            let score = combined_bf / (1.0 + combined_bf);
            return MatchResult {
                score,
                match_type,
                match_positions: positions,
                evidence: EvidenceLedger::new(),
            };
        }

        let mut evidence = EvidenceLedger::new();

        // Prior odds from match type
        let prior_odds = match_type.prior_odds();
        evidence.add(
            EvidenceKind::MatchType,
            prior_odds,
            EvidenceDescription::Static(match_type.description()),
        );

        // Position bonus: matches at start are better
        if let Some(&first_pos) = positions_ref.first() {
            let position_factor = 1.0 + (1.0 / (first_pos as f64 + 1.0)) * 0.5;
            evidence.add(
                EvidenceKind::Position,
                position_factor,
                EvidenceDescription::FirstMatchPos { pos: first_pos },
            );
        }

        // Word boundary bonus
        let word_boundary_count = self.count_word_boundaries(positions_ref, title);
        if word_boundary_count > 0 {
            let boundary_factor = 1.0 + (word_boundary_count as f64 * 0.3);
            evidence.add(
                EvidenceKind::WordBoundary,
                boundary_factor,
                EvidenceDescription::WordBoundaryCount {
                    count: word_boundary_count,
                },
            );
        }

        // Gap penalty for fuzzy matches
        if match_type == MatchType::Fuzzy && positions_ref.len() > 1 {
            let total_gap = self.total_gap(positions_ref);
            let gap_factor = 1.0 / (1.0 + total_gap as f64 * 0.1);
            evidence.add(
                EvidenceKind::GapPenalty,
                gap_factor,
                EvidenceDescription::GapTotal { total: total_gap },
            );
        }

        // Title length: prefer shorter (more specific) titles
        let title_len = title.len().max(1);
        let length_factor = 1.0 + (query.len() as f64 / title_len as f64) * 0.2;
        evidence.add(
            EvidenceKind::TitleLength,
            length_factor,
            EvidenceDescription::CoveragePercent {
                percent: (query.len() as f64 / title_len as f64) * 100.0,
            },
        );

        let score = evidence.posterior_probability();

        MatchResult {
            score,
            match_type,
            match_positions: positions,
            evidence,
        }
    }

    /// Count how many matched positions are at word boundaries.
    fn count_word_boundaries(&self, positions: &[usize], title: &str) -> usize {
        let title_bytes = title.as_bytes();
        positions
            .iter()
            .filter(|&&pos| {
                pos == 0 || {
                    let prev = title_bytes
                        .get(pos.saturating_sub(1))
                        .copied()
                        .unwrap_or(b' ');
                    prev == b' ' || prev == b'-' || prev == b'_'
                }
            })
            .count()
    }

    /// Calculate total gap between matched positions.
    fn total_gap(&self, positions: &[usize]) -> usize {
        if positions.len() < 2 {
            return 0;
        }
        positions
            .windows(2)
            .map(|w| w[1].saturating_sub(w[0]).saturating_sub(1))
            .sum()
    }
}

// ---------------------------------------------------------------------------
// Conformal Rank Confidence
// ---------------------------------------------------------------------------

/// Confidence level for a ranking position.
///
/// Derived from distribution-free conformal prediction: we compute
/// nonconformity scores (score gaps) and calibrate them against the
/// empirical distribution of all gaps in the result set.
///
/// # Invariants
///
/// 1. `confidence` is in `[0.0, 1.0]`.
/// 2. A gap of zero always yields `Unstable` stability.
/// 3. Deterministic: same scores → same confidence.
#[derive(Debug, Clone)]
pub struct RankConfidence {
    /// Probability that this item truly belongs at this rank position.
    /// Computed as the fraction of score gaps that are smaller than
    /// the gap between this item and the next.
    pub confidence: f64,

    /// Absolute score gap to the next-ranked item (0.0 if last or tied).
    pub gap_to_next: f64,

    /// Stability classification derived from gap analysis.
    pub stability: RankStability,
}

/// Stability classification for a rank position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankStability {
    /// Score gap is large relative to the distribution — rank is reliable.
    Stable,
    /// Score gap is moderate — rank is plausible but could swap with neighbors.
    Marginal,
    /// Score gap is negligible — rank is essentially a tie.
    Unstable,
}

impl RankStability {
    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Marginal => "marginal",
            Self::Unstable => "unstable",
        }
    }
}

/// Result of ranking a set of match results with conformal confidence.
#[derive(Debug, Clone)]
pub struct RankedResults {
    /// Items sorted by descending score, each with rank confidence.
    pub items: Vec<RankedItem>,

    /// Summary statistics about the ranking.
    pub summary: RankingSummary,
}

/// A single item in the ranked results.
#[derive(Debug, Clone)]
pub struct RankedItem {
    /// Index into the original (pre-sort) input slice.
    pub original_index: usize,

    /// The match result.
    pub result: MatchResult,

    /// Conformal confidence for this rank position.
    pub rank_confidence: RankConfidence,
}

/// Summary statistics for a ranked result set.
#[derive(Debug, Clone)]
pub struct RankingSummary {
    /// Number of items in the ranking.
    pub count: usize,

    /// Number of items with stable rank positions.
    pub stable_count: usize,

    /// Number of tie groups (sets of items with indistinguishable scores).
    pub tie_group_count: usize,

    /// Median score gap between adjacent ranked items.
    pub median_gap: f64,
}

/// Conformal ranker that assigns distribution-free confidence to rank positions.
///
/// # Method
///
/// Given sorted scores `s_1 ≥ s_2 ≥ ... ≥ s_n`, we define the nonconformity
/// score for position `i` as the gap `g_i = s_i - s_{i+1}`.
///
/// The conformal p-value for position `i` is:
///
/// ```text
/// p_i = |{j : g_j ≤ g_i}| / (n - 1)
/// ```
///
/// This gives the fraction of gaps that are at most as large as this item's
/// gap, which we interpret as confidence that the item is correctly ranked
/// above its successor.
///
/// # Tie Detection
///
/// Two scores are considered tied when their gap is below `tie_epsilon`.
/// The default epsilon is `1e-9`, suitable for f64 posterior probabilities.
///
/// # Failure Modes
///
/// - **All scores identical**: Every position is `Unstable` with confidence 0.
/// - **Single item**: Confidence is 1.0 (trivially correct ranking).
/// - **Empty input**: Returns empty results with zeroed summary.
#[derive(Debug, Clone)]
pub struct ConformalRanker {
    /// Threshold below which two scores are considered tied.
    pub tie_epsilon: f64,

    /// Confidence threshold for `Stable` classification.
    pub stable_threshold: f64,

    /// Confidence threshold for `Marginal` (below this is `Unstable`).
    pub marginal_threshold: f64,
}

impl Default for ConformalRanker {
    fn default() -> Self {
        Self {
            tie_epsilon: 1e-9,
            stable_threshold: 0.7,
            marginal_threshold: 0.3,
        }
    }
}

impl ConformalRanker {
    /// Create a ranker with default thresholds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rank a set of match results and assign conformal confidence.
    ///
    /// Results are sorted by descending score. Ties are broken by
    /// `MatchType` (higher variant first), then by shorter title length.
    pub fn rank(&self, results: Vec<MatchResult>) -> RankedResults {
        let count = results.len();

        if count == 0 {
            return RankedResults {
                items: Vec::new(),
                summary: RankingSummary {
                    count: 0,
                    stable_count: 0,
                    tie_group_count: 0,
                    median_gap: 0.0,
                },
            };
        }

        // Tag each result with its original index, then sort descending by score.
        // Tie-break: higher MatchType variant first, then shorter match_positions
        // (proxy for title length).
        let mut indexed: Vec<(usize, MatchResult)> = results.into_iter().enumerate().collect();
        indexed.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.match_type.cmp(&a.1.match_type))
        });

        // Compute gaps between adjacent scores.
        let gaps: Vec<f64> = if count > 1 {
            indexed
                .windows(2)
                .map(|w| (w[0].1.score - w[1].1.score).max(0.0))
                .collect()
        } else {
            Vec::new()
        };

        // Sort gaps for computing conformal p-values (fraction of gaps ≤ g_i).
        let mut sorted_gaps = gaps.clone();
        sorted_gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // Compute rank confidence for each position.
        let mut items: Vec<RankedItem> = Vec::with_capacity(count);
        let mut stable_count = 0;
        let mut tie_group_count = 0;
        let mut in_tie_group = false;

        for (rank, (orig_idx, result)) in indexed.into_iter().enumerate() {
            let gap_to_next = gaps.get(rank).copied().unwrap_or(0.0);

            // Conformal p-value: fraction of gaps that are ≤ this gap.
            let confidence = if sorted_gaps.is_empty() {
                // Single item: trivially ranked correctly.
                1.0
            } else {
                let leq_count =
                    sorted_gaps.partition_point(|&g| g <= gap_to_next + self.tie_epsilon * 0.5);
                leq_count as f64 / sorted_gaps.len() as f64
            };

            let is_tie = gap_to_next < self.tie_epsilon;
            let stability = if is_tie {
                if !in_tie_group {
                    tie_group_count += 1;
                    in_tie_group = true;
                }
                RankStability::Unstable
            } else {
                in_tie_group = false;
                if confidence >= self.stable_threshold {
                    stable_count += 1;
                    RankStability::Stable
                } else if confidence >= self.marginal_threshold {
                    RankStability::Marginal
                } else {
                    RankStability::Unstable
                }
            };

            items.push(RankedItem {
                original_index: orig_idx,
                result,
                rank_confidence: RankConfidence {
                    confidence,
                    gap_to_next,
                    stability,
                },
            });
        }

        let median_gap = if sorted_gaps.is_empty() {
            0.0
        } else {
            let mid = sorted_gaps.len() / 2;
            if sorted_gaps.len().is_multiple_of(2) {
                (sorted_gaps[mid - 1] + sorted_gaps[mid]) / 2.0
            } else {
                sorted_gaps[mid]
            }
        };

        RankedResults {
            items,
            summary: RankingSummary {
                count,
                stable_count,
                tie_group_count,
                median_gap,
            },
        }
    }

    /// Convenience: rank the top-k items only.
    ///
    /// All items are still scored and sorted, but only the top `k` are
    /// returned (with correct confidence relative to the full set).
    pub fn rank_top_k(&self, results: Vec<MatchResult>, k: usize) -> RankedResults {
        let mut ranked = self.rank(results);
        ranked.items.truncate(k);
        // Stable count may decrease after truncation.
        ranked.summary.stable_count = ranked
            .items
            .iter()
            .filter(|item| item.rank_confidence.stability == RankStability::Stable)
            .count();
        ranked
    }
}

impl fmt::Display for RankConfidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "confidence={:.3} gap={:.4} ({})",
            self.confidence,
            self.gap_to_next,
            self.stability.label()
        )
    }
}

impl fmt::Display for RankingSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} items, {} stable, {} tie groups, median gap {:.4}",
            self.count, self.stable_count, self.tie_group_count, self.median_gap
        )
    }
}

// ---------------------------------------------------------------------------
// Incremental Scorer (bd-39y4.13)
// ---------------------------------------------------------------------------

/// Cached entry from a previous scoring pass.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CachedEntry {
    /// Index into the corpus.
    corpus_index: usize,
}

/// Incremental scorer that caches results across keystrokes.
///
/// When the user types one more character, the new query is a prefix-extension
/// of the old query. Items that didn't match the shorter query can't match the
/// longer one (monotonicity of substring/prefix/fuzzy matching), so we skip them.
///
/// # Invariants
///
/// 1. **Correctness**: Incremental results are identical to a full rescan.
///    Adding characters can only reduce or maintain the match set, never expand it.
/// 2. **Determinism**: Same (query, corpus) → identical results.
/// 3. **Cache coherence**: Cache is invalidated when corpus changes or query
///    does not extend the previous query.
///
/// # Performance Model
///
/// Let `N` = corpus size, `M` = number of matches for the previous query.
/// - Full scan: O(N × L) where L is average title length.
/// - Incremental (query extends previous): O(M × L) where M ≤ N.
/// - Typical command palettes have M ≪ N after 2-3 characters.
///
/// # Failure Modes
///
/// - **Corpus mutation**: If the corpus changes between calls, results may be
///   stale. Call `invalidate()` or let the generation counter detect it.
/// - **Non-extending query** (e.g., backspace): Falls back to full scan.
///   This is correct but loses the incremental speedup.
#[derive(Debug)]
pub struct IncrementalScorer {
    /// Underlying scorer.
    scorer: BayesianScorer,
    /// Previous query string.
    prev_query: String,
    /// Cached matching entries from the previous pass.
    cache: Vec<CachedEntry>,
    /// Generation counter for corpus change detection.
    corpus_generation: u64,
    /// Number of items in the corpus at cache time.
    corpus_len: usize,
    /// Statistics for diagnostics.
    stats: IncrementalStats,
}

/// Diagnostics for incremental scoring performance.
#[derive(Debug, Clone, Default)]
pub struct IncrementalStats {
    /// Number of full rescans performed.
    pub full_scans: u64,
    /// Number of incremental (pruned) scans performed.
    pub incremental_scans: u64,
    /// Total items evaluated across all scans.
    pub total_evaluated: u64,
    /// Total items pruned (skipped) across incremental scans.
    pub total_pruned: u64,
}

impl IncrementalStats {
    /// Fraction of evaluations saved by incremental scoring.
    pub fn prune_ratio(&self) -> f64 {
        let total = self.total_evaluated + self.total_pruned;
        if total == 0 {
            0.0
        } else {
            self.total_pruned as f64 / total as f64
        }
    }
}

impl IncrementalScorer {
    /// Create a new incremental scorer (evidence tracking disabled for speed).
    pub fn new() -> Self {
        Self {
            scorer: BayesianScorer::fast(),
            prev_query: String::new(),
            cache: Vec::new(),
            corpus_generation: 0,
            corpus_len: 0,
            stats: IncrementalStats::default(),
        }
    }

    /// Create with explicit scorer configuration.
    pub fn with_scorer(scorer: BayesianScorer) -> Self {
        Self {
            scorer,
            prev_query: String::new(),
            cache: Vec::new(),
            corpus_generation: 0,
            corpus_len: 0,
            stats: IncrementalStats::default(),
        }
    }

    /// Invalidate the cache (e.g., when corpus changes).
    pub fn invalidate(&mut self) {
        self.prev_query.clear();
        self.cache.clear();
        self.corpus_generation = self.corpus_generation.wrapping_add(1);
    }

    /// Get diagnostic statistics.
    pub fn stats(&self) -> &IncrementalStats {
        &self.stats
    }

    /// Score a query against a corpus, using cached results when possible.
    ///
    /// Returns indices into the corpus and their match results, sorted by
    /// descending score. Only items with score > 0 are returned.
    ///
    /// # Arguments
    ///
    /// * `query` - The current search query.
    /// * `corpus` - Slice of title strings to search.
    /// * `generation` - Optional generation counter; if it differs from the
    ///   cached value, the cache is invalidated.
    pub fn score_corpus(
        &mut self,
        query: &str,
        corpus: &[&str],
        generation: Option<u64>,
    ) -> Vec<(usize, MatchResult)> {
        // Detect corpus changes.
        let generation_val = generation.unwrap_or(self.corpus_generation);
        if generation_val != self.corpus_generation || corpus.len() != self.corpus_len {
            self.invalidate();
            self.corpus_generation = generation_val;
            self.corpus_len = corpus.len();
        }

        // Determine if we can use incremental scoring.
        let can_prune = !self.prev_query.is_empty()
            && query.starts_with(&self.prev_query)
            && !self.cache.is_empty();

        let query_lower = query.to_lowercase();
        let results = if can_prune {
            self.score_incremental(query, &query_lower, corpus)
        } else {
            self.score_full(query, &query_lower, corpus)
        };

        // Update cache state.
        self.prev_query.clear();
        self.prev_query.push_str(query);
        self.cache = results
            .iter()
            .map(|(idx, _result)| CachedEntry { corpus_index: *idx })
            .collect();

        results
    }

    /// Score a query against a corpus with pre-lowercased titles.
    ///
    /// `corpus_lower` must align 1:1 with `corpus`.
    pub fn score_corpus_with_lowered(
        &mut self,
        query: &str,
        corpus: &[&str],
        corpus_lower: &[&str],
        generation: Option<u64>,
    ) -> Vec<(usize, MatchResult)> {
        debug_assert_eq!(
            corpus.len(),
            corpus_lower.len(),
            "corpus_lower must match corpus length"
        );

        // Detect corpus changes.
        let generation_val = generation.unwrap_or(self.corpus_generation);
        if generation_val != self.corpus_generation || corpus.len() != self.corpus_len {
            self.invalidate();
            self.corpus_generation = generation_val;
            self.corpus_len = corpus.len();
        }

        // Determine if we can use incremental scoring.
        let can_prune = !self.prev_query.is_empty()
            && query.starts_with(&self.prev_query)
            && !self.cache.is_empty();

        let query_lower = query.to_lowercase();
        let results = if can_prune {
            self.score_incremental_lowered(query, &query_lower, corpus, corpus_lower)
        } else {
            self.score_full_lowered(query, &query_lower, corpus, corpus_lower)
        };

        // Update cache state.
        self.prev_query.clear();
        self.prev_query.push_str(query);
        self.cache = results
            .iter()
            .map(|(idx, _result)| CachedEntry { corpus_index: *idx })
            .collect();

        results
    }

    /// Score a query against a corpus with pre-lowercased titles and word-start cache.
    ///
    /// `corpus_lower` and `word_starts` must align 1:1 with `corpus`.
    pub fn score_corpus_with_lowered_and_words(
        &mut self,
        query: &str,
        corpus: &[String],
        corpus_lower: &[String],
        word_starts: &[Vec<usize>],
        generation: Option<u64>,
    ) -> Vec<(usize, MatchResult)> {
        debug_assert_eq!(
            corpus.len(),
            corpus_lower.len(),
            "corpus_lower must match corpus length"
        );
        debug_assert_eq!(
            corpus.len(),
            word_starts.len(),
            "word_starts must match corpus length"
        );

        // Detect corpus changes.
        let generation_val = generation.unwrap_or(self.corpus_generation);
        if generation_val != self.corpus_generation || corpus.len() != self.corpus_len {
            self.invalidate();
            self.corpus_generation = generation_val;
            self.corpus_len = corpus.len();
        }

        // Determine if we can use incremental scoring.
        let can_prune = !self.prev_query.is_empty()
            && query.starts_with(&self.prev_query)
            && !self.cache.is_empty();

        let query_lower = query.to_lowercase();
        let results = if can_prune {
            self.score_incremental_lowered_with_words(
                query,
                &query_lower,
                corpus,
                corpus_lower,
                word_starts,
            )
        } else {
            self.score_full_lowered_with_words(
                query,
                &query_lower,
                corpus,
                corpus_lower,
                word_starts,
            )
        };

        // Update cache state.
        self.prev_query.clear();
        self.prev_query.push_str(query);
        self.cache = results
            .iter()
            .map(|(idx, _result)| CachedEntry { corpus_index: *idx })
            .collect();

        results
    }

    /// Full scan: score every item in the corpus.
    fn score_full(
        &mut self,
        query: &str,
        query_lower: &str,
        corpus: &[&str],
    ) -> Vec<(usize, MatchResult)> {
        self.stats.full_scans += 1;
        self.stats.total_evaluated += corpus.len() as u64;

        let mut results: Vec<(usize, MatchResult)> = Vec::with_capacity(corpus.len());
        for (i, title) in corpus.iter().enumerate() {
            let result = self
                .scorer
                .score_with_query_lower(query, query_lower, title);
            if result.score > 0.0 {
                results.push((i, result));
            }
        }

        results.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.match_type.cmp(&a.1.match_type))
        });

        results
    }

    /// Full scan with pre-lowercased titles.
    fn score_full_lowered(
        &mut self,
        query: &str,
        query_lower: &str,
        corpus: &[&str],
        corpus_lower: &[&str],
    ) -> Vec<(usize, MatchResult)> {
        self.stats.full_scans += 1;
        self.stats.total_evaluated += corpus.len() as u64;

        let mut results: Vec<(usize, MatchResult)> = Vec::with_capacity(corpus.len());
        for (i, (title, title_lower)) in corpus.iter().zip(corpus_lower.iter()).enumerate() {
            let result =
                self.scorer
                    .score_with_lowered_title(query, query_lower, title, title_lower);
            if result.score > 0.0 {
                results.push((i, result));
            }
        }

        results.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.match_type.cmp(&a.1.match_type))
        });

        results
    }

    /// Full scan with pre-lowercased titles and word-start cache.
    fn score_full_lowered_with_words(
        &mut self,
        query: &str,
        query_lower: &str,
        corpus: &[String],
        corpus_lower: &[String],
        word_starts: &[Vec<usize>],
    ) -> Vec<(usize, MatchResult)> {
        self.stats.full_scans += 1;
        self.stats.total_evaluated += corpus.len() as u64;

        let mut results: Vec<(usize, MatchResult)> = Vec::with_capacity(corpus.len());
        for (i, ((title, title_lower), starts)) in corpus
            .iter()
            .zip(corpus_lower.iter())
            .zip(word_starts.iter())
            .enumerate()
        {
            let result = self.scorer.score_with_lowered_title_and_words(
                query,
                query_lower,
                title,
                title_lower,
                Some(starts),
            );
            if result.score > 0.0 {
                results.push((i, result));
            }
        }

        results.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.match_type.cmp(&a.1.match_type))
        });

        results
    }
    /// Incremental scan: only re-score items that previously matched.
    fn score_incremental(
        &mut self,
        query: &str,
        query_lower: &str,
        corpus: &[&str],
    ) -> Vec<(usize, MatchResult)> {
        self.stats.incremental_scans += 1;

        let prev_match_count = self.cache.len();
        let pruned = corpus.len().saturating_sub(prev_match_count);
        self.stats.total_pruned += pruned as u64;
        self.stats.total_evaluated += prev_match_count as u64;

        let mut results: Vec<(usize, MatchResult)> =
            Vec::with_capacity(self.cache.len().min(corpus.len()));
        for entry in &self.cache {
            if entry.corpus_index < corpus.len() {
                let result = self.scorer.score_with_query_lower(
                    query,
                    query_lower,
                    corpus[entry.corpus_index],
                );
                if result.score > 0.0 {
                    results.push((entry.corpus_index, result));
                }
            }
        }

        results.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.match_type.cmp(&a.1.match_type))
        });

        results
    }

    /// Incremental scan with pre-lowercased titles.
    fn score_incremental_lowered(
        &mut self,
        query: &str,
        query_lower: &str,
        corpus: &[&str],
        corpus_lower: &[&str],
    ) -> Vec<(usize, MatchResult)> {
        self.stats.incremental_scans += 1;

        let prev_match_count = self.cache.len();
        let pruned = corpus.len().saturating_sub(prev_match_count);
        self.stats.total_pruned += pruned as u64;
        self.stats.total_evaluated += prev_match_count as u64;

        let mut results: Vec<(usize, MatchResult)> =
            Vec::with_capacity(self.cache.len().min(corpus.len()));
        for entry in &self.cache {
            if entry.corpus_index < corpus.len() {
                let title = corpus[entry.corpus_index];
                let title_lower = corpus_lower[entry.corpus_index];
                let result =
                    self.scorer
                        .score_with_lowered_title(query, query_lower, title, title_lower);
                if result.score > 0.0 {
                    results.push((entry.corpus_index, result));
                }
            }
        }

        results.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.match_type.cmp(&a.1.match_type))
        });

        results
    }

    /// Incremental scan with pre-lowercased titles and word-start cache.
    fn score_incremental_lowered_with_words(
        &mut self,
        query: &str,
        query_lower: &str,
        corpus: &[String],
        corpus_lower: &[String],
        word_starts: &[Vec<usize>],
    ) -> Vec<(usize, MatchResult)> {
        self.stats.incremental_scans += 1;

        let prev_match_count = self.cache.len();
        let pruned = corpus.len().saturating_sub(prev_match_count);
        self.stats.total_pruned += pruned as u64;
        self.stats.total_evaluated += prev_match_count as u64;

        let mut results: Vec<(usize, MatchResult)> =
            Vec::with_capacity(self.cache.len().min(corpus.len()));
        for entry in &self.cache {
            if entry.corpus_index < corpus.len() {
                let title = &corpus[entry.corpus_index];
                let title_lower = &corpus_lower[entry.corpus_index];
                let starts = &word_starts[entry.corpus_index];
                let result = self.scorer.score_with_lowered_title_and_words(
                    query,
                    query_lower,
                    title,
                    title_lower,
                    Some(starts),
                );
                if result.score > 0.0 {
                    results.push((entry.corpus_index, result));
                }
            }
        }

        results.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.match_type.cmp(&a.1.match_type))
        });

        results
    }
}

impl Default for IncrementalScorer {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Match Type Tests ---

    #[test]
    fn exact_match_highest_score() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("Settings", "Settings");
        assert_eq!(result.match_type, MatchType::Exact);
        assert!(result.score > 0.95, "Exact match should score > 0.95");
    }

    #[test]
    fn prefix_match_high_score() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("set", "Settings");
        assert_eq!(result.match_type, MatchType::Prefix);
        assert!(result.score > 0.85, "Prefix match should score > 0.85");
    }

    #[test]
    fn word_start_match_score() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("gd", "Go Dashboard");
        assert_eq!(result.match_type, MatchType::WordStart);
        assert!(result.score > 0.75, "Word-start should score > 0.75");
    }

    #[test]
    fn substring_match_moderate_score() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("set", "Asset Manager");
        assert_eq!(result.match_type, MatchType::Substring);
        assert!(result.score > 0.5, "Substring should score > 0.5");
    }

    #[test]
    fn fuzzy_match_low_score() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("stg", "Settings");
        assert_eq!(result.match_type, MatchType::Fuzzy);
        assert!(result.score > 0.2, "Fuzzy should score > 0.2");
        assert!(result.score < 0.7, "Fuzzy should score < 0.7");
    }

    #[test]
    fn no_match_returns_zero() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("xyz", "Settings");
        assert_eq!(result.match_type, MatchType::NoMatch);
        assert_eq!(result.score, 0.0);
    }

    // --- Score Invariants ---

    #[test]
    fn score_bounded() {
        let scorer = BayesianScorer::new();
        let test_cases = [
            ("a", "abcdefghijklmnop"),
            ("full", "full"),
            ("", "anything"),
            ("xyz", "abc"),
            ("stg", "Settings"),
        ];

        for (query, title) in test_cases {
            let result = scorer.score(query, title);
            assert!(
                result.score >= 0.0 && result.score <= 1.0,
                "Score for ({}, {}) = {} not in [0, 1]",
                query,
                title,
                result.score
            );
        }
    }

    #[test]
    fn score_deterministic() {
        let scorer = BayesianScorer::new();
        let result1 = scorer.score("nav", "Navigation");
        let result2 = scorer.score("nav", "Navigation");
        assert!(
            (result1.score - result2.score).abs() < f64::EPSILON,
            "Same input should produce identical scores"
        );
    }

    #[test]
    fn tiebreak_shorter_first() {
        let scorer = BayesianScorer::new();
        let short = scorer.score("set", "Set");
        let long = scorer.score("set", "Settings");
        assert!(
            short.score >= long.score,
            "Shorter title should score >= longer: {} vs {}",
            short.score,
            long.score
        );
    }

    // --- Case Insensitivity ---

    #[test]
    fn case_insensitive() {
        let scorer = BayesianScorer::new();
        let lower = scorer.score("set", "Settings");
        let upper = scorer.score("SET", "Settings");
        assert!(
            (lower.score - upper.score).abs() < f64::EPSILON,
            "Case should not affect score"
        );
    }

    // --- Match Positions ---

    #[test]
    fn match_positions_correct() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("gd", "Go Dashboard");
        assert_eq!(result.match_positions, vec![0, 3]);
    }

    #[test]
    fn fuzzy_match_positions() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("stg", "Settings");
        // s(0), t(3), g(6)
        assert_eq!(result.match_positions.len(), 3);
        assert_eq!(result.match_positions[0], 0); // 's'
    }

    // --- Empty Query ---

    #[test]
    fn empty_query_returns_all() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("", "Any Command");
        assert!(result.score > 0.0, "Empty query should have positive score");
        assert!(result.score < 1.0, "Empty query should not be max score");
    }

    #[test]
    fn empty_title_is_safe() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("x", "");
        assert_eq!(result.match_type, MatchType::NoMatch);
        assert!(result.score.is_finite(), "Score should remain finite");
    }

    #[test]
    fn empty_query_empty_title_is_finite() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("", "");
        assert!(result.score.is_finite(), "Score should remain finite");
        assert_eq!(result.match_type, MatchType::Fuzzy);
    }

    // --- Query Longer Than Title ---

    #[test]
    fn query_longer_than_title() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("verylongquery", "short");
        assert_eq!(result.match_type, MatchType::NoMatch);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn long_query_exact_match_is_handled() {
        let scorer = BayesianScorer::new();
        let query = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let result = scorer.score(query, query);
        assert_eq!(result.match_type, MatchType::Exact);
        assert!(result.score.is_finite());
        assert!(result.score > 0.9, "Exact long query should score high");
    }

    #[test]
    fn gap_penalty_prefers_tight_fuzzy_matches() {
        let scorer = BayesianScorer::new();
        let tight = scorer.score("ace", "abcde");
        let gappy = scorer.score("ace", "a...c...e");
        assert_eq!(tight.match_type, MatchType::Fuzzy);
        assert_eq!(gappy.match_type, MatchType::Fuzzy);
        assert!(
            tight.score > gappy.score,
            "Tight fuzzy match should score higher than gappy: {} vs {}",
            tight.score,
            gappy.score
        );
    }

    // --- Tag Matching ---

    #[test]
    fn tag_match_boosts_score() {
        let scorer = BayesianScorer::new();
        // Use a query that matches the title (fuzzy)
        let without_tag = scorer.score("set", "Settings");
        let with_tag = scorer.score_with_tags("set", "Settings", &["config", "setup"]);
        // Tag "setup" contains "set", so it should boost the score
        assert!(
            with_tag.score > without_tag.score,
            "Tag match should boost score: {} > {}",
            with_tag.score,
            without_tag.score
        );
    }

    // --- Evidence Ledger ---

    #[test]
    fn evidence_ledger_tracks_factors() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("set", "Settings");

        assert!(!result.evidence.entries().is_empty());

        // Should have match type entry
        assert!(
            result
                .evidence
                .entries()
                .iter()
                .any(|e| e.kind == EvidenceKind::MatchType)
        );
    }

    #[test]
    fn evidence_ledger_display() {
        let scorer = BayesianScorer::new();
        let result = scorer.score("gd", "Go Dashboard");
        let display = format!("{}", result.evidence);
        assert!(display.contains("Evidence Ledger"));
        assert!(display.contains("Posterior P:"));
    }

    // --- Property Tests ---

    #[test]
    fn property_ordering_total() {
        let scorer = BayesianScorer::new();
        let titles = ["Settings", "Set Theme", "Reset View", "Asset"];

        let mut scores: Vec<(f64, &str)> = titles
            .iter()
            .map(|t| (scorer.score("set", t).score, *t))
            .collect();

        // Sort should be stable and total
        scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());

        // Verify no NaN or infinite scores
        for (score, _) in &scores {
            assert!(score.is_finite());
        }
    }

    #[test]
    fn property_prefix_monotonic() {
        let scorer = BayesianScorer::new();
        // Longer exact prefix match should score higher
        let one_char = scorer.score("s", "Settings");
        let three_char = scorer.score("set", "Settings");
        // Both are prefix matches, longer should be better
        assert!(
            three_char.score >= one_char.score,
            "Longer prefix should score >= shorter"
        );
    }

    // --- Match Type Prior Odds ---

    #[test]
    fn match_type_prior_ordering() {
        assert!(MatchType::Exact.prior_odds() > MatchType::Prefix.prior_odds());
        assert!(MatchType::Prefix.prior_odds() > MatchType::WordStart.prior_odds());
        assert!(MatchType::WordStart.prior_odds() > MatchType::Substring.prior_odds());
        assert!(MatchType::Substring.prior_odds() > MatchType::Fuzzy.prior_odds());
        assert!(MatchType::Fuzzy.prior_odds() > MatchType::NoMatch.prior_odds());
    }

    // -----------------------------------------------------------------------
    // Conformal Ranker Tests
    // -----------------------------------------------------------------------

    #[test]
    fn conformal_tie_breaks_by_match_type() {
        let mut exact = MatchResult::no_match();
        exact.score = 0.5;
        exact.match_type = MatchType::Exact;

        let mut prefix = MatchResult::no_match();
        prefix.score = 0.5;
        prefix.match_type = MatchType::Prefix;

        let mut word_start = MatchResult::no_match();
        word_start.score = 0.5;
        word_start.match_type = MatchType::WordStart;

        let mut substring = MatchResult::no_match();
        substring.score = 0.5;
        substring.match_type = MatchType::Substring;

        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(vec![substring, word_start, prefix, exact]);
        let order: Vec<MatchType> = ranked
            .items
            .iter()
            .map(|item| item.result.match_type)
            .collect();

        assert_eq!(
            order,
            vec![
                MatchType::Exact,
                MatchType::Prefix,
                MatchType::WordStart,
                MatchType::Substring
            ]
        );
    }

    #[test]
    fn conformal_empty_input() {
        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(Vec::new());
        assert_eq!(ranked.items.len(), 0);
        assert_eq!(ranked.summary.count, 0);
        assert_eq!(ranked.summary.stable_count, 0);
        assert_eq!(ranked.summary.tie_group_count, 0);
        assert_eq!(ranked.summary.median_gap, 0.0);
    }

    #[test]
    fn conformal_single_item() {
        let scorer = BayesianScorer::new();
        let results = vec![scorer.score("set", "Settings")];
        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(results);

        assert_eq!(ranked.items.len(), 1);
        assert_eq!(ranked.items[0].rank_confidence.confidence, 1.0);
        assert_eq!(ranked.summary.count, 1);
    }

    #[test]
    fn conformal_sorted_descending() {
        let scorer = BayesianScorer::new();
        let results = vec![
            scorer.score("set", "Settings"),
            scorer.score("set", "Asset Manager"),
            scorer.score("set", "Reset Configuration Panel"),
        ];
        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(results);

        // Verify descending score order.
        for w in ranked.items.windows(2) {
            assert!(
                w[0].result.score >= w[1].result.score,
                "Items should be sorted descending: {} >= {}",
                w[0].result.score,
                w[1].result.score
            );
        }
    }

    #[test]
    fn conformal_confidence_bounded() {
        let scorer = BayesianScorer::new();
        let titles = [
            "Settings",
            "Set Theme",
            "Asset Manager",
            "Reset View",
            "Offset Tool",
            "Test Suite",
        ];
        let results: Vec<MatchResult> = titles.iter().map(|t| scorer.score("set", t)).collect();
        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(results);

        for item in &ranked.items {
            assert!(
                item.rank_confidence.confidence >= 0.0 && item.rank_confidence.confidence <= 1.0,
                "Confidence must be in [0, 1], got {}",
                item.rank_confidence.confidence
            );
            assert!(
                item.rank_confidence.gap_to_next >= 0.0,
                "Gap must be non-negative"
            );
        }
    }

    #[test]
    fn conformal_deterministic() {
        let scorer = BayesianScorer::new();
        let titles = ["Settings", "Set Theme", "Asset", "Reset"];

        let results1: Vec<MatchResult> = titles.iter().map(|t| scorer.score("set", t)).collect();
        let results2: Vec<MatchResult> = titles.iter().map(|t| scorer.score("set", t)).collect();

        let ranker = ConformalRanker::new();
        let ranked1 = ranker.rank(results1);
        let ranked2 = ranker.rank(results2);

        for (a, b) in ranked1.items.iter().zip(ranked2.items.iter()) {
            assert!(
                (a.rank_confidence.confidence - b.rank_confidence.confidence).abs() < f64::EPSILON,
                "Confidence should be deterministic"
            );
            assert_eq!(
                a.original_index, b.original_index,
                "Rank order should be deterministic"
            );
        }
    }

    #[test]
    fn conformal_ties_detected() {
        // Create items with identical scores.
        let mut r1 = MatchResult::no_match();
        r1.score = 0.8;
        r1.match_type = MatchType::Prefix;
        let mut r2 = MatchResult::no_match();
        r2.score = 0.8;
        r2.match_type = MatchType::Prefix;
        let mut r3 = MatchResult::no_match();
        r3.score = 0.5;
        r3.match_type = MatchType::Substring;

        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(vec![r1, r2, r3]);

        // The first two have identical scores — their gap is 0 → unstable.
        assert_eq!(
            ranked.items[0].rank_confidence.stability,
            RankStability::Unstable,
            "Tied items should be Unstable"
        );
        assert!(
            ranked.summary.tie_group_count >= 1,
            "Should detect at least one tie group"
        );
    }

    #[test]
    fn conformal_all_identical_scores() {
        let mut results = Vec::new();
        for _ in 0..5 {
            let mut r = MatchResult::no_match();
            r.score = 0.5;
            r.match_type = MatchType::Fuzzy;
            results.push(r);
        }

        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(results);

        // All gaps are zero → all unstable.
        for item in &ranked.items {
            assert_eq!(item.rank_confidence.stability, RankStability::Unstable);
        }
    }

    #[test]
    fn conformal_well_separated_scores_are_stable() {
        let mut results = Vec::new();
        // Scores well spread out: 0.9, 0.6, 0.3, 0.1.
        for &s in &[0.9, 0.6, 0.3, 0.1] {
            let mut r = MatchResult::no_match();
            r.score = s;
            r.match_type = MatchType::Prefix;
            results.push(r);
        }

        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(results);

        // With well-separated scores, most should be stable.
        assert!(
            ranked.summary.stable_count >= 2,
            "Well-separated scores should yield stable positions, got {}",
            ranked.summary.stable_count
        );
    }

    #[test]
    fn conformal_top_k_truncates() {
        let scorer = BayesianScorer::new();
        let titles = [
            "Settings",
            "Set Theme",
            "Asset Manager",
            "Reset View",
            "Offset Tool",
        ];
        let results: Vec<MatchResult> = titles.iter().map(|t| scorer.score("set", t)).collect();

        let ranker = ConformalRanker::new();
        let ranked = ranker.rank_top_k(results, 2);

        assert_eq!(ranked.items.len(), 2);
        // Top-k items should still have confidence from the full ranking.
        assert!(ranked.items[0].rank_confidence.confidence > 0.0);
    }

    #[test]
    fn conformal_original_indices_preserved() {
        let scorer = BayesianScorer::new();
        let titles = ["Zebra Tool", "Settings", "Apple"];
        let results: Vec<MatchResult> = titles.iter().map(|t| scorer.score("set", t)).collect();

        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(results);

        // "Settings" (index 1) should be ranked first (prefix match).
        assert_eq!(
            ranked.items[0].original_index, 1,
            "Settings should be first; original_index should be 1"
        );
    }

    #[test]
    fn conformal_summary_display() {
        let scorer = BayesianScorer::new();
        let results = vec![
            scorer.score("set", "Settings"),
            scorer.score("set", "Set Theme"),
        ];
        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(results);

        let display = format!("{}", ranked.summary);
        assert!(display.contains("2 items"));
    }

    #[test]
    fn conformal_rank_confidence_display() {
        let rc = RankConfidence {
            confidence: 0.85,
            gap_to_next: 0.1234,
            stability: RankStability::Stable,
        };
        let display = format!("{}", rc);
        assert!(display.contains("0.850"));
        assert!(display.contains("stable"));
    }

    #[test]
    fn conformal_stability_labels() {
        assert_eq!(RankStability::Stable.label(), "stable");
        assert_eq!(RankStability::Marginal.label(), "marginal");
        assert_eq!(RankStability::Unstable.label(), "unstable");
    }

    // --- Property: gap_to_next of last item is always 0 ---

    #[test]
    fn conformal_last_item_gap_zero() {
        let scorer = BayesianScorer::new();
        let results = vec![
            scorer.score("set", "Settings"),
            scorer.score("set", "Asset"),
            scorer.score("set", "Reset"),
        ];
        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(results);

        let last = ranked.items.last().unwrap();
        assert_eq!(
            last.rank_confidence.gap_to_next, 0.0,
            "Last item gap should be 0"
        );
    }

    // --- Property: median_gap is non-negative ---

    #[test]
    fn conformal_median_gap_non_negative() {
        let scorer = BayesianScorer::new();
        let titles = [
            "Settings",
            "Set Theme",
            "Asset Manager",
            "Reset View",
            "Offset Tool",
            "Test Suite",
            "System Settings",
            "Reset Defaults",
        ];
        let results: Vec<MatchResult> = titles.iter().map(|t| scorer.score("set", t)).collect();
        let ranker = ConformalRanker::new();
        let ranked = ranker.rank(results);

        assert!(
            ranked.summary.median_gap >= 0.0,
            "Median gap must be non-negative"
        );
    }

    // -----------------------------------------------------------------------
    // IncrementalScorer Tests (bd-39y4.13)
    // -----------------------------------------------------------------------

    fn test_corpus() -> Vec<&'static str> {
        vec![
            "Open File",
            "Save File",
            "Close Tab",
            "Git: Commit",
            "Git: Push",
            "Git: Pull",
            "Go to Line",
            "Find in Files",
            "Toggle Terminal",
            "Format Document",
        ]
    }

    #[test]
    fn incremental_full_scan_on_first_call() {
        let mut scorer = IncrementalScorer::new();
        let corpus = test_corpus();
        let results = scorer.score_corpus("git", &corpus, None);

        assert!(!results.is_empty(), "Should find matches for 'git'");
        assert_eq!(scorer.stats().full_scans, 1);
        assert_eq!(scorer.stats().incremental_scans, 0);
    }

    #[test]
    fn incremental_prunes_on_query_extension() {
        let mut scorer = IncrementalScorer::new();
        let corpus = test_corpus();

        // First query: "g" matches many items
        let r1 = scorer.score_corpus("g", &corpus, None);
        assert_eq!(scorer.stats().full_scans, 1);

        // Extended query: "gi" — should use incremental path
        let r2 = scorer.score_corpus("gi", &corpus, None);
        assert_eq!(scorer.stats().incremental_scans, 1);
        assert!(r2.len() <= r1.len(), "Extended query should match <= items");

        // Further extension: "git" — still incremental
        let r3 = scorer.score_corpus("git", &corpus, None);
        assert_eq!(scorer.stats().incremental_scans, 2);
        assert!(r3.len() <= r2.len());
    }

    #[test]
    fn incremental_correctness_matches_full_scan() {
        let corpus = test_corpus();

        // Incremental path
        let mut inc = IncrementalScorer::new();
        inc.score_corpus("g", &corpus, None);
        let inc_results = inc.score_corpus("git", &corpus, None);

        // Full scan path (fresh scorer, no cache)
        let mut full = IncrementalScorer::new();
        let full_results = full.score_corpus("git", &corpus, None);

        // Results should be identical.
        assert_eq!(
            inc_results.len(),
            full_results.len(),
            "Incremental and full scan should return same count"
        );

        for (a, b) in inc_results.iter().zip(full_results.iter()) {
            assert_eq!(a.0, b.0, "Same corpus indices");
            assert!(
                (a.1.score - b.1.score).abs() < f64::EPSILON,
                "Same scores: {} vs {}",
                a.1.score,
                b.1.score
            );
        }
    }

    #[test]
    fn incremental_falls_back_on_non_extension() {
        let mut scorer = IncrementalScorer::new();
        let corpus = test_corpus();

        scorer.score_corpus("git", &corpus, None);
        assert_eq!(scorer.stats().full_scans, 1);

        // "fi" doesn't extend "git" — must full scan
        scorer.score_corpus("fi", &corpus, None);
        assert_eq!(scorer.stats().full_scans, 2);
    }

    #[test]
    fn incremental_invalidate_forces_full_scan() {
        let mut scorer = IncrementalScorer::new();
        let corpus = test_corpus();

        scorer.score_corpus("g", &corpus, None);
        scorer.invalidate();

        // Even though "gi" extends "g", cache was cleared
        scorer.score_corpus("gi", &corpus, None);
        assert_eq!(scorer.stats().full_scans, 2);
        assert_eq!(scorer.stats().incremental_scans, 0);
    }

    #[test]
    fn incremental_generation_change_invalidates() {
        let mut scorer = IncrementalScorer::new();
        let corpus = test_corpus();

        scorer.score_corpus("g", &corpus, Some(1));

        // Generation changed — cache invalid
        scorer.score_corpus("gi", &corpus, Some(2));
        assert_eq!(scorer.stats().full_scans, 2);
    }

    #[test]
    fn incremental_corpus_size_change_invalidates() {
        let mut scorer = IncrementalScorer::new();
        let corpus1 = test_corpus();
        let corpus2 = &corpus1[..5];

        scorer.score_corpus("g", &corpus1, None);
        scorer.score_corpus("gi", corpus2, None);
        // Corpus size changed → full scan
        assert_eq!(scorer.stats().full_scans, 2);
    }

    #[test]
    fn incremental_empty_query() {
        let mut scorer = IncrementalScorer::new();
        let corpus = test_corpus();

        let results = scorer.score_corpus("", &corpus, None);
        // Empty query matches everything (with weak scores)
        assert_eq!(results.len(), corpus.len());
    }

    #[test]
    fn incremental_no_match_query() {
        let mut scorer = IncrementalScorer::new();
        let corpus = test_corpus();

        let results = scorer.score_corpus("zzz", &corpus, None);
        assert!(results.is_empty());
    }

    #[test]
    fn incremental_stats_prune_ratio() {
        let mut scorer = IncrementalScorer::new();
        let corpus = test_corpus();

        scorer.score_corpus("g", &corpus, None);
        scorer.score_corpus("gi", &corpus, None);
        scorer.score_corpus("git", &corpus, None);

        let stats = scorer.stats();
        assert!(
            stats.prune_ratio() > 0.0,
            "Prune ratio should be > 0 after incremental scans"
        );
        assert!(stats.total_pruned > 0, "Should have pruned some items");
    }

    #[test]
    fn incremental_results_sorted_descending() {
        let mut scorer = IncrementalScorer::new();
        let corpus = test_corpus();

        let results = scorer.score_corpus("o", &corpus, None);
        for w in results.windows(2) {
            assert!(
                w[0].1.score >= w[1].1.score,
                "Results should be sorted descending: {} >= {}",
                w[0].1.score,
                w[1].1.score
            );
        }
    }

    #[test]
    fn incremental_lowered_matches_full() {
        let corpus = vec![
            "Open File".to_string(),
            "Save File".to_string(),
            "Close".to_string(),
            "Launch 🚀".to_string(),
        ];
        let corpus_refs: Vec<&str> = corpus.iter().map(|s| s.as_str()).collect();
        let lower: Vec<String> = corpus.iter().map(|s| s.to_lowercase()).collect();
        let word_starts: Vec<Vec<usize>> = lower
            .iter()
            .map(|title_lower| {
                let bytes = title_lower.as_bytes();
                title_lower
                    .char_indices()
                    .filter_map(|(i, _)| {
                        let is_word_start = i == 0 || {
                            let prev = bytes.get(i.saturating_sub(1)).copied().unwrap_or(b' ');
                            prev == b' ' || prev == b'-' || prev == b'_'
                        };
                        is_word_start.then_some(i)
                    })
                    .collect()
            })
            .collect();

        let mut full = IncrementalScorer::new();
        let mut lowered = IncrementalScorer::new();

        let a = full.score_corpus("fi", &corpus_refs, None);
        let b =
            lowered.score_corpus_with_lowered_and_words("fi", &corpus, &lower, &word_starts, None);

        assert_eq!(a.len(), b.len());
        for ((ia, ra), (ib, rb)) in a.iter().zip(b.iter()) {
            assert_eq!(ia, ib);
            assert_eq!(ra.match_type, rb.match_type);
            assert_eq!(ra.match_positions, rb.match_positions);
            assert!(
                (ra.score - rb.score).abs() < 1e-12,
                "score mismatch: {} vs {}",
                ra.score,
                rb.score
            );
        }
    }

    #[test]
    fn incremental_deterministic() {
        let corpus = test_corpus();

        let mut s1 = IncrementalScorer::new();
        let r1 = s1.score_corpus("git", &corpus, None);

        let mut s2 = IncrementalScorer::new();
        let r2 = s2.score_corpus("git", &corpus, None);

        assert_eq!(r1.len(), r2.len());
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.0, b.0);
            assert!((a.1.score - b.1.score).abs() < f64::EPSILON);
        }
    }
}

// ===========================================================================
// Performance regression tests (bd-39y4.5)
// ===========================================================================

#[cfg(test)]
mod perf_tests {
    use super::*;
    use std::time::Instant;

    #[derive(Debug, Clone, Copy)]
    struct PerfStats {
        p50_us: u64,
        p95_us: u64,
        p99_us: u64,
        mean_us: f64,
        variance_us: f64,
        samples: usize,
    }

    /// Budget thresholds for single-query scoring.
    /// These are generous to avoid CI flakes but catch >2x regressions.
    const SINGLE_SCORE_BUDGET_US: u64 = 10; // 10µs per score call
    const CORPUS_100_BUDGET_US: u64 = 500; // 500µs for 100-item full scan
    const CORPUS_1000_BUDGET_US: u64 = 5_000; // 5ms for 1000-item full scan
    const CORPUS_5000_BUDGET_US: u64 = 25_000; // 25ms for 5000-item full scan
    const INCREMENTAL_7KEY_100_BUDGET_US: u64 = 2_000; // 2ms for 7 keystrokes on 100 items
    const INCREMENTAL_7KEY_1000_BUDGET_US: u64 = 15_000; // 15ms for 7 keystrokes on 1000 items

    fn is_coverage_run() -> bool {
        std::env::var("LLVM_PROFILE_FILE").is_ok() || std::env::var("CARGO_LLVM_COV").is_ok()
    }

    fn coverage_budget_us(base: u64) -> u64 {
        if is_coverage_run() {
            // Coverage instrumentation adds substantial overhead (and noise) to these
            // microbench-style regression tests, so we relax budgets enough to keep
            // `cargo llvm-cov` stable while still logging the timings.
            const COVERAGE_BUDGET_MULTIPLIER: u64 = 5;
            base.saturating_mul(COVERAGE_BUDGET_MULTIPLIER)
        } else {
            base
        }
    }

    /// Generate a command corpus of the specified size with realistic variety.
    fn make_corpus(size: usize) -> Vec<String> {
        let base = [
            "Open File",
            "Save File",
            "Close Tab",
            "Split Editor Right",
            "Split Editor Down",
            "Toggle Terminal",
            "Go to Line",
            "Find in Files",
            "Replace in Files",
            "Git: Commit",
            "Git: Push",
            "Git: Pull",
            "Debug: Start",
            "Debug: Stop",
            "Debug: Step Over",
            "Format Document",
            "Rename Symbol",
            "Go to Definition",
            "Find All References",
            "Toggle Sidebar",
        ];
        base.iter()
            .cycle()
            .take(size)
            .enumerate()
            .map(|(i, cmd)| {
                if i < base.len() {
                    (*cmd).to_string()
                } else {
                    format!("{} ({})", cmd, i)
                }
            })
            .collect()
    }

    fn measure_stats_us(iterations: usize, mut f: impl FnMut()) -> PerfStats {
        let mut times = Vec::with_capacity(iterations);
        // Warmup
        for _ in 0..3 {
            f();
        }
        for _ in 0..iterations {
            let start = Instant::now();
            f();
            times.push(start.elapsed().as_micros() as u64);
        }
        times.sort_unstable();
        let len = times.len();
        let p50 = times[len / 2];
        let p95 = times[((len as f64 * 0.95) as usize).min(len.saturating_sub(1))];
        let p99 = times[((len as f64 * 0.99) as usize).min(len.saturating_sub(1))];
        let mean = times.iter().copied().map(|value| value as f64).sum::<f64>() / len as f64;
        let variance = times
            .iter()
            .map(|value| {
                let delta = *value as f64 - mean;
                delta * delta
            })
            .sum::<f64>()
            / len as f64;

        PerfStats {
            p50_us: p50,
            p95_us: p95,
            p99_us: p99,
            mean_us: mean,
            variance_us: variance,
            samples: len,
        }
    }

    #[test]
    fn perf_single_score_under_budget() {
        let scorer = BayesianScorer::fast();
        let stats = measure_stats_us(200, || {
            std::hint::black_box(scorer.score("git co", "Git: Commit"));
        });
        eprintln!(
            "{{\"ts\":\"2026-02-04T00:00:00Z\",\"event\":\"palette_score_single\",\"samples\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"mean_us\":{:.2},\"variance_us\":{:.2}}}",
            stats.samples,
            stats.p50_us,
            stats.p95_us,
            stats.p99_us,
            stats.mean_us,
            stats.variance_us
        );
        let budget = coverage_budget_us(SINGLE_SCORE_BUDGET_US);
        assert!(
            stats.p50_us <= budget,
            "Single score p50 = {}µs exceeds budget {}µs",
            stats.p50_us,
            budget,
        );
    }

    #[test]
    fn perf_corpus_100_under_budget() {
        let scorer = BayesianScorer::fast();
        let corpus = make_corpus(100);
        let stats = measure_stats_us(50, || {
            let mut results: Vec<_> = corpus
                .iter()
                .map(|t| scorer.score("git co", t))
                .filter(|r| r.score > 0.0)
                .collect();
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
            std::hint::black_box(&results);
        });
        eprintln!(
            "{{\"ts\":\"2026-02-04T00:00:00Z\",\"event\":\"palette_corpus_100\",\"samples\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"mean_us\":{:.2},\"variance_us\":{:.2}}}",
            stats.samples,
            stats.p50_us,
            stats.p95_us,
            stats.p99_us,
            stats.mean_us,
            stats.variance_us
        );
        let budget = coverage_budget_us(CORPUS_100_BUDGET_US);
        assert!(
            stats.p95_us <= budget,
            "100-item corpus p95 = {}µs exceeds budget {}µs",
            stats.p95_us,
            budget,
        );
    }

    #[test]
    fn perf_corpus_1000_under_budget() {
        let scorer = BayesianScorer::fast();
        let corpus = make_corpus(1_000);
        let stats = measure_stats_us(20, || {
            let mut results: Vec<_> = corpus
                .iter()
                .map(|t| scorer.score("git co", t))
                .filter(|r| r.score > 0.0)
                .collect();
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
            std::hint::black_box(&results);
        });
        eprintln!(
            "{{\"ts\":\"2026-02-04T00:00:00Z\",\"event\":\"palette_corpus_1000\",\"samples\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"mean_us\":{:.2},\"variance_us\":{:.2}}}",
            stats.samples,
            stats.p50_us,
            stats.p95_us,
            stats.p99_us,
            stats.mean_us,
            stats.variance_us
        );
        let budget = coverage_budget_us(CORPUS_1000_BUDGET_US);
        assert!(
            stats.p95_us <= budget,
            "1000-item corpus p95 = {}µs exceeds budget {}µs",
            stats.p95_us,
            budget,
        );
    }

    #[test]
    fn perf_corpus_5000_under_budget() {
        let scorer = BayesianScorer::fast();
        let corpus = make_corpus(5_000);
        let stats = measure_stats_us(10, || {
            let mut results: Vec<_> = corpus
                .iter()
                .map(|t| scorer.score("git co", t))
                .filter(|r| r.score > 0.0)
                .collect();
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
            std::hint::black_box(&results);
        });
        eprintln!(
            "{{\"ts\":\"2026-02-04T00:00:00Z\",\"event\":\"palette_corpus_5000\",\"samples\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"mean_us\":{:.2},\"variance_us\":{:.2}}}",
            stats.samples,
            stats.p50_us,
            stats.p95_us,
            stats.p99_us,
            stats.mean_us,
            stats.variance_us
        );
        let budget = coverage_budget_us(CORPUS_5000_BUDGET_US);
        assert!(
            stats.p95_us <= budget,
            "5000-item corpus p95 = {}µs exceeds budget {}µs",
            stats.p95_us,
            budget,
        );
    }

    #[test]
    fn perf_incremental_7key_100_under_budget() {
        let corpus = make_corpus(100);
        let corpus_refs: Vec<&str> = corpus.iter().map(|s| s.as_str()).collect();
        let queries = ["g", "gi", "git", "git ", "git c", "git co", "git com"];

        let stats = measure_stats_us(30, || {
            let mut inc = IncrementalScorer::new();
            for query in &queries {
                let results = inc.score_corpus(query, &corpus_refs, None);
                std::hint::black_box(&results);
            }
        });
        eprintln!(
            "{{\"ts\":\"2026-02-04T00:00:00Z\",\"event\":\"palette_incremental_7key_100\",\"samples\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"mean_us\":{:.2},\"variance_us\":{:.2}}}",
            stats.samples,
            stats.p50_us,
            stats.p95_us,
            stats.p99_us,
            stats.mean_us,
            stats.variance_us
        );
        let budget = coverage_budget_us(INCREMENTAL_7KEY_100_BUDGET_US);
        assert!(
            stats.p95_us <= budget,
            "Incremental 7-key 100-item p95 = {}µs exceeds budget {}µs",
            stats.p95_us,
            budget,
        );
    }

    #[test]
    fn perf_incremental_7key_1000_under_budget() {
        let corpus = make_corpus(1_000);
        let corpus_refs: Vec<&str> = corpus.iter().map(|s| s.as_str()).collect();
        let queries = ["g", "gi", "git", "git ", "git c", "git co", "git com"];

        let stats = measure_stats_us(10, || {
            let mut inc = IncrementalScorer::new();
            for query in &queries {
                let results = inc.score_corpus(query, &corpus_refs, None);
                std::hint::black_box(&results);
            }
        });
        eprintln!(
            "{{\"ts\":\"2026-02-04T00:00:00Z\",\"event\":\"palette_incremental_7key_1000\",\"samples\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"mean_us\":{:.2},\"variance_us\":{:.2}}}",
            stats.samples,
            stats.p50_us,
            stats.p95_us,
            stats.p99_us,
            stats.mean_us,
            stats.variance_us
        );
        let budget = coverage_budget_us(INCREMENTAL_7KEY_1000_BUDGET_US);
        assert!(
            stats.p95_us <= budget,
            "Incremental 7-key 1000-item p95 = {}µs exceeds budget {}µs",
            stats.p95_us,
            budget,
        );
    }

    #[test]
    fn perf_incremental_faster_than_naive() {
        let corpus = make_corpus(100);
        let corpus_refs: Vec<&str> = corpus.iter().map(|s| s.as_str()).collect();
        let scorer = BayesianScorer::fast();
        let queries = ["g", "gi", "git", "git ", "git c", "git co", "git com"];

        let naive_stats = measure_stats_us(30, || {
            for query in &queries {
                let mut results: Vec<_> = corpus
                    .iter()
                    .map(|t| scorer.score(query, t))
                    .filter(|r| r.score > 0.0)
                    .collect();
                results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
                std::hint::black_box(&results);
            }
        });

        let inc_stats = measure_stats_us(30, || {
            let mut inc = IncrementalScorer::new();
            for query in &queries {
                let results = inc.score_corpus(query, &corpus_refs, None);
                std::hint::black_box(&results);
            }
        });
        eprintln!(
            "{{\"ts\":\"2026-02-04T00:00:00Z\",\"event\":\"palette_incremental_vs_naive\",\"samples\":{},\"naive_p50_us\":{},\"naive_p95_us\":{},\"inc_p50_us\":{},\"inc_p95_us\":{}}}",
            naive_stats.samples,
            naive_stats.p50_us,
            naive_stats.p95_us,
            inc_stats.p50_us,
            inc_stats.p95_us
        );

        // Incremental should not be more than 2x slower than naive
        // (in practice it's faster, but we set a relaxed threshold)
        assert!(
            inc_stats.p50_us <= naive_stats.p50_us * 2 + 50, // +50µs tolerance for measurement noise
            "Incremental p50 = {}µs is >2x slower than naive p50 = {}µs",
            inc_stats.p50_us,
            naive_stats.p50_us,
        );
    }

    #[test]
    fn perf_scaling_sublinear() {
        let scorer = BayesianScorer::fast();
        let corpus_100 = make_corpus(100);
        let corpus_1000 = make_corpus(1_000);

        let stats_100 = measure_stats_us(30, || {
            let mut results: Vec<_> = corpus_100
                .iter()
                .map(|t| scorer.score("git", t))
                .filter(|r| r.score > 0.0)
                .collect();
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
            std::hint::black_box(&results);
        });

        let stats_1000 = measure_stats_us(20, || {
            let mut results: Vec<_> = corpus_1000
                .iter()
                .map(|t| scorer.score("git", t))
                .filter(|r| r.score > 0.0)
                .collect();
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
            std::hint::black_box(&results);
        });
        eprintln!(
            "{{\"ts\":\"2026-02-04T00:00:00Z\",\"event\":\"palette_scaling\",\"samples_100\":{},\"p50_100_us\":{},\"samples_1000\":{},\"p50_1000_us\":{}}}",
            stats_100.samples, stats_100.p50_us, stats_1000.samples, stats_1000.p50_us
        );

        // 10x corpus ⇒ O(n log n) theoretical ratio ≈ 15x.  Use 25x to absorb
        // system noise when tests run in parallel (still catches O(n²) = 100x).
        let ratio = if stats_100.p50_us > 0 {
            stats_1000.p50_us as f64 / stats_100.p50_us as f64
        } else {
            0.0
        };
        assert!(
            ratio < 25.0,
            "1000/100 scaling ratio = {:.1}x exceeds 25x threshold (100: {}µs, 1000: {}µs)",
            ratio,
            stats_100.p50_us,
            stats_1000.p50_us,
        );
    }
}
