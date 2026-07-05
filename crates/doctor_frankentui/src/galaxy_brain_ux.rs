//! Galaxy-brain UX: L0-L3 progressive disclosure contracts (bd-3bxhj.10.43).
//!
//! Alien-grade math only helps operators if it is comprehensible and
//! operationally safe. This module elevates the [`crate::galaxy_brain_cards`]
//! artifacts into explicit L0-L3 UX contracts:
//!
//! - **L0**: decision signal + confidence band + risk class.
//! - **L1**: plain-language intuition + dominant evidence contributors.
//! - **L2**: structured evidence terms + guarantee status.
//! - **L3**: the full equation, substituted values, and machine-exportable
//!   artifacts (Unicode / LaTeX / JSON copy-as).
//!
//! Contracts enforced by the fail-closed gate:
//!
//! - **Determinism / diff stability** (AC1): views are sorted by
//!   `(card_id, level)`, every view carries a stable `content_id` +
//!   `content_hash` that the gate RE-DERIVES from the rendered lines, and the
//!   whole report replays byte-identically.
//! - **Hard non-interference** (AC2): views and the scripted interaction
//!   session consume the decision sources immutably; the engine hashes the
//!   sources before and after the full render + interaction pass and the gate
//!   requires the hashes to be identical. Explainability can never feed back
//!   into the decision core.
//! - **Accessibility + performance** (AC3): every line is truncated to a
//!   deterministic width, every view carries an ASCII screen-reader text
//!   alternative, per-level line/char budgets hold even for the adversarial
//!   wide-card stress fixture (rows past the cap collapse into an explicit
//!   `(+N more terms)` line — never silently dropped), and every disclosure
//!   transition is a discrete single-level step (low-motion by construction).
//! - **Traceability** (AC4): every view carries claim / evidence / policy
//!   provenance, and the L3 exports embed the card's content-addressed id.
//!
//! The ledger is float-free (counts + strings only), so it derives [`Eq`] and
//! replays byte-identically. Interaction latency telemetry is modeled as
//! deterministic render units (characters emitted for the target view), which
//! is the budgeted quantity; wall-clock latency would break replay. The
//! pipeline is exposed through the `galaxy-ux` CLI command.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::galaxy_brain_cards::{
    CardFormat, GalaxyBrainCard, Substitution, conformal_interval_card, evalue_fdr_card,
    posterior_core_card, render_card,
};
use crate::guarantee_layer::{ConformalConfig, EProcessConfig, conformal_interval, run_eprocess};
use crate::posterior_core::{ChannelEvidence, EvidenceChannel, PosteriorEngine};

/// Schema version for the in-memory galaxy-brain UX report.
pub const GALAXY_UX_SCHEMA_VERSION: &str = "galaxy-brain-ux-v1";

/// Schema version for the materialized galaxy-brain UX pipeline artifacts.
pub const GALAXY_UX_PIPELINE_SCHEMA_VERSION: &str = "galaxy-brain-ux-pipeline-v1";

// ── Budgets (deterministic render units) ─────────────────────────────────────

/// Maximum rendered line width; longer lines truncate with an ASCII ellipsis.
pub const MAX_LINE_CHARS: usize = 120;

/// Maximum evidence-term rows rendered at L2/L3 before the remainder
/// collapses into an explicit `(+N more terms)` line.
pub const MAX_TERM_ROWS: usize = 16;

/// Per-level line budgets (L0..L3). Presentation stays scannable at every
/// level; the caps are structural, so they hold for adversarial cards too.
pub const LEVEL_LINE_BUDGETS: [usize; 4] = [4, 10, 28, 40];

/// Per-transition render budget in characters (the deterministic latency
/// proxy): rendering any single disclosure level must stay under this.
pub const MAX_TRANSITION_RENDER_UNITS: usize = 8_192;

// ── Hashing helpers ──────────────────────────────────────────────────────────

fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// A progressive-disclosure level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureLevel {
    /// Decision signal + confidence band + risk class.
    L0,
    /// Plain-language intuition + dominant evidence contributors.
    L1,
    /// Structured evidence terms + guarantee status.
    L2,
    /// Full equation, substituted values, machine-exportable artifacts.
    L3,
}

impl DisclosureLevel {
    /// All levels in disclosure order.
    pub const ALL: [DisclosureLevel; 4] = [Self::L0, Self::L1, Self::L2, Self::L3];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::L0 => "l0",
            Self::L1 => "l1",
            Self::L2 => "l2",
            Self::L3 => "l3",
        }
    }

    /// Numeric rank (L0 = 0).
    #[must_use]
    pub fn rank(self) -> usize {
        match self {
            Self::L0 => 0,
            Self::L1 => 1,
            Self::L2 => 2,
            Self::L3 => 3,
        }
    }

    fn from_rank(rank: usize) -> Self {
        match rank {
            0 => Self::L0,
            1 => Self::L1,
            2 => Self::L2,
            _ => Self::L3,
        }
    }

    /// The line budget for this level.
    #[must_use]
    pub fn line_budget(self) -> usize {
        LEVEL_LINE_BUDGETS[self.rank()]
    }
}

/// The operational risk class shown at L0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UxRiskClass {
    /// Reversible with a redeploy.
    Low,
    /// Stateful surface; staged verification.
    Medium,
    /// Operator-critical; holdback + rehearsed rollback.
    High,
}

impl UxRiskClass {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// One decision-core source handed to the UX layer: the transparency card plus
/// the decision metadata L0 must surface and the provenance links (AC4).
///
/// The UX layer only ever borrows this immutably — that is the non-interference
/// contract the gate proves by hashing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UxCardSource {
    /// The transparency card from the decision core.
    pub card: GalaxyBrainCard,
    /// The decision's risk class (L0).
    pub risk_class: UxRiskClass,
    /// The confidence band, pre-formatted by the decision core (L0).
    pub confidence_band: String,
    /// The formal-guarantee status (`holds` / `fallback` / `rejected`) (L2).
    pub guarantee_status: String,
    /// Provenance: the evidence record backing the card (AC4).
    pub evidence_id: String,
    /// Provenance: the policy in force for the gate decision (AC4).
    pub policy_id: String,
}

// ── View building ────────────────────────────────────────────────────────────

/// Truncate a line to [`MAX_LINE_CHARS`] with an ASCII ellipsis.
fn clip_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let head: String = line.chars().take(MAX_LINE_CHARS - 3).collect();
    format!("{head}...")
}

/// Render the evidence-term rows with the deterministic row cap. Rows past
/// [`MAX_TERM_ROWS`] collapse into an explicit `(+N more terms)` line so the
/// truncation is visible, never silent.
fn term_rows(substitutions: &[Substitution]) -> Vec<String> {
    let mut rows: Vec<String> = substitutions
        .iter()
        .take(MAX_TERM_ROWS)
        .map(|s| {
            let note = s
                .note
                .as_deref()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default();
            clip_line(&format!("  {} = {}{}", s.symbol, s.value, note))
        })
        .collect();
    if substitutions.len() > MAX_TERM_ROWS {
        rows.push(format!(
            "  (+{} more terms)",
            substitutions.len() - MAX_TERM_ROWS
        ));
    }
    rows
}

/// An ASCII screen-reader alternative for a view: a plain sentence with no
/// box-drawing, math symbols, or color reliance.
fn text_alternative(source: &UxCardSource, level: DisclosureLevel) -> String {
    let card = &source.card;
    let alt = match level {
        DisclosureLevel::L0 => format!(
            "{} decision card: {}. Confidence {}. Risk class {}.",
            card.kind.as_str().replace('_', " "),
            card.headline,
            source.confidence_band,
            source.risk_class.as_str()
        ),
        DisclosureLevel::L1 => format!(
            "Intuition: {} Dominant contributors: {} evidence terms.",
            card.intuition,
            card.substitutions.len().min(3)
        ),
        DisclosureLevel::L2 => format!(
            "{} evidence terms with concrete values. Guarantee status: {}.",
            card.substitutions.len(),
            source.guarantee_status
        ),
        DisclosureLevel::L3 => format!(
            "Governing equation with {} substituted values. Machine exports available as Unicode, LaTeX, and JSON.",
            card.substitutions.len()
        ),
    };
    // The alternative must be consumable by any screen reader: force ASCII.
    let ascii: String = alt
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect();
    clip_line(&ascii)
}

fn view_lines(source: &UxCardSource, level: DisclosureLevel) -> Vec<String> {
    let card = &source.card;
    let mut lines = vec![
        clip_line(&format!("{}: {}", card.title, card.headline)),
        clip_line(&format!("confidence: {}", source.confidence_band)),
        clip_line(&format!("risk: {}", source.risk_class.as_str())),
    ];
    if level.rank() >= DisclosureLevel::L1.rank() {
        lines.push(clip_line(&format!("intuition: {}", card.intuition)));
        for s in card.substitutions.iter().take(3) {
            lines.push(clip_line(&format!("  key term {} = {}", s.symbol, s.value)));
        }
    }
    if level.rank() >= DisclosureLevel::L2.rank() {
        lines.push(clip_line(&format!(
            "guarantee: {}",
            source.guarantee_status
        )));
        lines.extend(term_rows(&card.substitutions));
    }
    if level.rank() >= DisclosureLevel::L3.rank() {
        lines.push(clip_line(&format!("equation: {}", card.equation)));
        lines.push(clip_line(&format!(
            "exports: unicode | latex | json (card {})",
            card.card_id
        )));
    }
    lines.truncate(level.line_budget());
    lines
}

/// The three machine exports for a card's L3 view (copy-as targets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardExports {
    /// Terminal-ready Unicode rendering.
    pub unicode: String,
    /// LaTeX rendering of the equation + substitutions.
    pub latex: String,
    /// Machine-readable JSON (the card's serde serialization).
    pub json: String,
}

fn build_exports(card: &GalaxyBrainCard) -> CardExports {
    let mut latex = String::new();
    latex.push_str(&format!("% galaxy-brain card {}\n", card.card_id));
    latex.push_str(&format!(
        "\\begin{{aligned}}\n{}\n\\end{{aligned}}\n",
        card.equation
    ));
    for s in &card.substitutions {
        latex.push_str(&format!("% {} = {}\n", s.symbol, s.value));
    }
    CardExports {
        unicode: render_card(card, CardFormat::CliUnicode),
        latex,
        json: card.to_json(),
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One (card, level) view's contract record (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UxViewLedgerEntry {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The source card id (content-addressed by the card kernel).
    pub card_id: String,
    /// Stable content id: `<card_id>/<level>`.
    pub content_id: String,
    /// The disclosure level.
    pub level: DisclosureLevel,
    /// Stable hash of the rendered lines (re-derived by the gate).
    pub content_hash: String,
    /// The rendered lines.
    pub lines: Vec<String>,
    /// Rendered line count.
    pub line_count: usize,
    /// Deterministic render units (characters) — the latency/allocation proxy.
    pub render_units: usize,
    /// Whether line/char budgets hold for this level.
    pub within_budget: bool,
    /// ASCII screen-reader alternative for the view.
    pub text_alternative: String,
    /// Whether the accessibility contract holds (alt text present + ASCII,
    /// every line within width).
    pub accessibility_ok: bool,
    /// Provenance: the claim the card pertains to (AC4).
    pub claim_id: String,
    /// Provenance: the evidence record (AC4).
    pub evidence_id: String,
    /// Provenance: the policy in force (AC4).
    pub policy_id: String,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

fn entry_has_required_fields(e: &UxViewLedgerEntry) -> bool {
    !e.schema_version.is_empty()
        && !e.run_id.is_empty()
        && !e.card_id.is_empty()
        && !e.content_id.is_empty()
        && !e.content_hash.is_empty()
        && !e.lines.is_empty()
        && !e.text_alternative.is_empty()
        && !e.claim_id.is_empty()
        && !e.evidence_id.is_empty()
        && !e.policy_id.is_empty()
        && !e.reproduction_command.is_empty()
}

// ── Interaction model ────────────────────────────────────────────────────────

/// A keyboard-first UX command. The canonical keymap is documented on each
/// variant; the model itself is host-agnostic and pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UxCommand {
    /// Disclose one more level (`Enter` / `Right`). Saturates at L3.
    ExpandOne,
    /// Collapse one level (`Left` / `Esc`). Saturates at L0.
    CollapseOne,
    /// Focus the next card (`j` / `Down`). Saturates at the last card.
    NextCard,
    /// Focus the previous card (`k` / `Up`). Saturates at the first card.
    PrevCard,
    /// Copy the focused card as terminal Unicode (`c u`).
    CopyUnicode,
    /// Copy the focused card as LaTeX (`c l`).
    CopyLatex,
    /// Copy the focused card as JSON (`c j`).
    CopyJson,
}

impl UxCommand {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExpandOne => "expand_one",
            Self::CollapseOne => "collapse_one",
            Self::NextCard => "next_card",
            Self::PrevCard => "prev_card",
            Self::CopyUnicode => "copy_unicode",
            Self::CopyLatex => "copy_latex",
            Self::CopyJson => "copy_json",
        }
    }
}

/// The deterministic interaction state: focused card + disclosure level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UxInteractionState {
    /// Index of the focused card in the (sorted) deck.
    pub card_index: usize,
    /// The focused card's current disclosure level.
    pub level: DisclosureLevel,
}

impl Default for UxInteractionState {
    fn default() -> Self {
        Self {
            card_index: 0,
            level: DisclosureLevel::L0,
        }
    }
}

/// One step of the scripted interaction session (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UxInteractionLogEntry {
    /// 1-based step number.
    pub step: usize,
    /// The command applied.
    pub command: UxCommand,
    /// Focused card id after the step.
    pub card_id: String,
    /// Level before the step.
    pub from_level: DisclosureLevel,
    /// Level after the step.
    pub to_level: DisclosureLevel,
    /// Whether the transition was a discrete single-level step (or a no-op /
    /// focus move) — the low-motion contract.
    pub discrete: bool,
    /// The copy export produced, or `"n/a"`.
    pub copied_format: String,
    /// Deterministic latency proxy: render units of the view (or export)
    /// produced by this step.
    pub latency_units: usize,
}

/// Apply `command` to `state` over a deck of `deck_len` cards. Pure and total:
/// every command saturates at the deck/level edges instead of failing.
#[must_use]
pub fn apply_command(
    state: UxInteractionState,
    command: UxCommand,
    deck_len: usize,
) -> UxInteractionState {
    let mut next = state;
    match command {
        UxCommand::ExpandOne => {
            next.level = DisclosureLevel::from_rank((state.level.rank() + 1).min(3));
        }
        UxCommand::CollapseOne => {
            next.level = DisclosureLevel::from_rank(state.level.rank().saturating_sub(1));
        }
        UxCommand::NextCard => {
            if state.card_index + 1 < deck_len {
                next.card_index = state.card_index + 1;
                next.level = DisclosureLevel::L0;
            }
        }
        UxCommand::PrevCard => {
            if state.card_index > 0 {
                next.card_index = state.card_index - 1;
                next.level = DisclosureLevel::L0;
            }
        }
        UxCommand::CopyUnicode | UxCommand::CopyLatex | UxCommand::CopyJson => {}
    }
    next
}

/// The scripted coverage session driven on every run: exercises expand /
/// collapse / focus moves / every copy-as export at least once.
#[must_use]
pub fn scripted_session() -> Vec<UxCommand> {
    vec![
        UxCommand::ExpandOne,
        UxCommand::ExpandOne,
        UxCommand::ExpandOne,
        UxCommand::CopyUnicode,
        UxCommand::CopyLatex,
        UxCommand::CopyJson,
        UxCommand::CollapseOne,
        UxCommand::CollapseOne,
        UxCommand::NextCard,
        UxCommand::ExpandOne,
        UxCommand::PrevCard,
        UxCommand::CollapseOne,
    ]
}

// ── Summary + report ─────────────────────────────────────────────────────────

/// Machine-readable summary of one galaxy-brain UX run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GalaxyUxSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the deterministic ledger + interaction log.
    pub evidence_checksum: String,
    /// Number of cards in the deck.
    pub total_cards: usize,
    /// Number of (card, level) views.
    pub total_views: usize,
    /// Number of scripted interaction steps.
    pub interaction_steps: usize,
    /// Whether every ledger entry has all mandated fields.
    pub required_fields_complete: bool,
    /// Whether every card has exactly one view per level (AC1).
    pub levels_complete: bool,
    /// Whether every recorded content hash re-derives from the lines (AC1).
    pub hashes_stable: bool,
    /// Whether views are sorted by `(card_id, level)` (diff stability).
    pub ordering_deterministic: bool,
    /// Whether the source hash was identical before/after the full render +
    /// interaction pass (AC2 — hard non-interference).
    pub non_interference_proven: bool,
    /// Whether every view passes the accessibility contract (AC3).
    pub accessibility_pass: bool,
    /// Whether every view + transition fits the render budgets (AC3).
    pub perf_within_budget: bool,
    /// Whether every view carries claim/evidence/policy provenance (AC4).
    pub provenance_complete: bool,
    /// Whether every card's three L3 exports are present and the JSON parses.
    pub copy_exports_complete: bool,
    /// Whether the scripted session exercised every command class and every
    /// disclosure transition was discrete (low-motion).
    pub interaction_coverage: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// The in-memory galaxy-brain UX report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GalaxyUxReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum.
    pub evidence_checksum: String,
    /// The per-view contract ledger, sorted by `(card_id, level)`.
    pub views: Vec<UxViewLedgerEntry>,
    /// The scripted interaction log.
    pub interactions: Vec<UxInteractionLogEntry>,
    /// Per-card L3 exports, keyed in deck order.
    pub exports: Vec<(String, CardExports)>,
    /// Aggregate summary.
    pub summary: GalaxyUxSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
}

impl GalaxyUxReport {
    /// Render the view ledger as JSONL.
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        let mut out = String::new();
        for entry in &self.views {
            match serde_json::to_string(entry) {
                Ok(line) => out.push_str(&line),
                Err(error) => out.push_str(&error.to_string()),
            }
            out.push('\n');
        }
        out
    }

    /// Render the interaction log as JSONL.
    #[must_use]
    pub fn render_interaction_jsonl(&self) -> String {
        let mut out = String::new();
        for entry in &self.interactions {
            match serde_json::to_string(entry) {
                Ok(line) => out.push_str(&line),
                Err(error) => out.push_str(&error.to_string()),
            }
            out.push('\n');
        }
        out
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The deterministic galaxy-brain UX engine.
#[derive(Debug, Clone)]
pub struct GalaxyUx {
    run_id: String,
    label: String,
}

impl GalaxyUx {
    /// Construct an engine with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "galaxy-ux-{}",
            short_hash(&stable_hash(&format!("{GALAXY_UX_SCHEMA_VERSION}|{label}")))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn view_entry(&self, source: &UxCardSource, level: DisclosureLevel) -> UxViewLedgerEntry {
        let card = &source.card;
        let lines = view_lines(source, level);
        let content_id = format!("{}/{}", card.card_id, level.as_str());
        let content_hash = short_hash(&stable_hash(&(&content_id, &lines)));
        let render_units: usize = lines.iter().map(|l| l.chars().count()).sum();
        let within_budget = lines.len() <= level.line_budget()
            && lines.iter().all(|l| l.chars().count() <= MAX_LINE_CHARS)
            && render_units <= MAX_TRANSITION_RENDER_UNITS;
        let alt = text_alternative(source, level);
        let accessibility_ok = !alt.is_empty()
            && alt.is_ascii()
            && lines.iter().all(|l| l.chars().count() <= MAX_LINE_CHARS);

        UxViewLedgerEntry {
            schema_version: GALAXY_UX_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            card_id: card.card_id.clone(),
            content_id,
            level,
            content_hash,
            line_count: lines.len(),
            render_units,
            lines,
            within_budget,
            text_alternative: alt,
            accessibility_ok,
            claim_id: card.claim_id.clone(),
            evidence_id: source.evidence_id.clone(),
            policy_id: source.policy_id.clone(),
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- galaxy-ux --label '{}' # run {} card {}",
                self.label, self.run_id, card.card_id
            ),
        }
    }

    fn run_session(
        &self,
        deck: &[&UxCardSource],
        views: &[UxViewLedgerEntry],
        exports: &[(String, CardExports)],
    ) -> Vec<UxInteractionLogEntry> {
        let view_units = |card_id: &str, level: DisclosureLevel| -> usize {
            views
                .iter()
                .find(|v| v.card_id == card_id && v.level == level)
                .map_or(0, |v| v.render_units)
        };
        let mut state = UxInteractionState::default();
        let mut log = Vec::new();
        for (step, &command) in scripted_session().iter().enumerate() {
            let from_level = state.level;
            let next = apply_command(state, command, deck.len());
            let card_id = deck[next.card_index].card.card_id.clone();
            let (copied_format, latency_units) = match command {
                UxCommand::CopyUnicode => {
                    ("unicode".to_string(), export_units(exports, &card_id, 0))
                }
                UxCommand::CopyLatex => ("latex".to_string(), export_units(exports, &card_id, 1)),
                UxCommand::CopyJson => ("json".to_string(), export_units(exports, &card_id, 2)),
                _ => ("n/a".to_string(), view_units(&card_id, next.level)),
            };
            // Low-motion contract: a disclosure change moves by exactly one
            // level; focus moves reset to L0 (a fresh card, not an animation).
            let level_delta = next.level.rank().abs_diff(from_level.rank());
            let discrete = match command {
                UxCommand::ExpandOne | UxCommand::CollapseOne => level_delta <= 1,
                UxCommand::NextCard | UxCommand::PrevCard => next.level == DisclosureLevel::L0,
                _ => level_delta == 0,
            };
            log.push(UxInteractionLogEntry {
                step: step + 1,
                command,
                card_id,
                from_level,
                to_level: next.level,
                discrete,
                copied_format,
                latency_units,
            });
            state = next;
        }
        log
    }

    /// Build the L0-L3 contract views, drive the scripted interaction session,
    /// and prove non-interference over `sources`.
    #[must_use]
    pub fn run(&self, sources: &[UxCardSource]) -> GalaxyUxReport {
        // Deterministic deck order (diff stability): sort by card_id.
        let mut deck: Vec<&UxCardSource> = sources.iter().collect();
        deck.sort_by(|a, b| a.card.card_id.cmp(&b.card.card_id));

        // AC2: hash the decision sources before any rendering/interaction.
        let sources_hash_before = stable_hash(&deck);

        let mut views: Vec<UxViewLedgerEntry> = Vec::new();
        for source in &deck {
            for level in DisclosureLevel::ALL {
                views.push(self.view_entry(source, level));
            }
        }
        let exports: Vec<(String, CardExports)> = deck
            .iter()
            .map(|s| (s.card.card_id.clone(), build_exports(&s.card)))
            .collect();
        let interactions = self.run_session(&deck, &views, &exports);

        // AC2: the full render + interaction pass must not have perturbed the
        // decision sources in any observable way.
        let sources_hash_after = stable_hash(&deck);
        let non_interference_proven = sources_hash_before == sources_hash_after;

        let evidence_checksum = stable_hash(&(&views, &interactions));
        let report_id = format!(
            "galaxy-ux-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(
            &views,
            &interactions,
            &exports,
            non_interference_proven,
            &report_id,
            &evidence_checksum,
        );
        let gate_passes = summary.gate_passes;

        GalaxyUxReport {
            schema_version: GALAXY_UX_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            views,
            interactions,
            exports,
            summary,
            gate_passes,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn summarize(
        &self,
        views: &[UxViewLedgerEntry],
        interactions: &[UxInteractionLogEntry],
        exports: &[(String, CardExports)],
        non_interference_proven: bool,
        report_id: &str,
        evidence_checksum: &str,
    ) -> GalaxyUxSummary {
        let required_fields_complete =
            !views.is_empty() && views.iter().all(entry_has_required_fields);

        // AC1: every card has exactly one view per level.
        let card_ids: BTreeSet<&str> = views.iter().map(|v| v.card_id.as_str()).collect();
        let levels_complete = !card_ids.is_empty()
            && card_ids.iter().all(|id| {
                let levels: BTreeSet<DisclosureLevel> = views
                    .iter()
                    .filter(|v| v.card_id == *id)
                    .map(|v| v.level)
                    .collect();
                levels.len() == DisclosureLevel::ALL.len()
            })
            && views.len() == card_ids.len() * DisclosureLevel::ALL.len();

        // AC1: re-derive every content hash from the recorded lines.
        let hashes_stable = views.iter().all(|v| {
            v.content_hash == short_hash(&stable_hash(&(&v.content_id, &v.lines)))
                && v.content_id == format!("{}/{}", v.card_id, v.level.as_str())
        });

        // Diff stability: sorted by (card_id, level rank).
        let ordering_deterministic = views.windows(2).all(|w| {
            (w[0].card_id.as_str(), w[0].level.rank()) <= (w[1].card_id.as_str(), w[1].level.rank())
        });

        // AC3.
        let accessibility_pass = views.iter().all(|v| v.accessibility_ok);
        let perf_within_budget = views.iter().all(|v| v.within_budget)
            && interactions
                .iter()
                .all(|i| i.latency_units <= MAX_TRANSITION_RENDER_UNITS * 4);

        // AC4.
        let provenance_complete = views.iter().all(|v| {
            !v.claim_id.is_empty() && !v.evidence_id.is_empty() && !v.policy_id.is_empty()
        });

        // Copy-as exports: all three present per card, JSON parses, and every
        // export embeds the card id (traceability).
        let copy_exports_complete = exports.len() == card_ids.len()
            && exports.iter().all(|(card_id, e)| {
                !e.unicode.is_empty()
                    && e.latex.contains(card_id.as_str())
                    && serde_json::from_str::<serde_json::Value>(&e.json)
                        .ok()
                        .and_then(|v| {
                            v.get("card_id")
                                .and_then(|c| c.as_str())
                                .map(|c| c == card_id)
                        })
                        .unwrap_or(false)
            });

        // Interaction coverage + low motion: every command class exercised,
        // every step discrete.
        let commands_seen: BTreeSet<UxCommand> = interactions.iter().map(|i| i.command).collect();
        let interaction_coverage = !interactions.is_empty()
            && commands_seen.len() == 7
            && interactions.iter().all(|i| i.discrete);

        let gate_passes = required_fields_complete
            && levels_complete
            && hashes_stable
            && ordering_deterministic
            && non_interference_proven
            && accessibility_pass
            && perf_within_budget
            && provenance_complete
            && copy_exports_complete
            && interaction_coverage;

        GalaxyUxSummary {
            schema_version: GALAXY_UX_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_cards: card_ids.len(),
            total_views: views.len(),
            interaction_steps: interactions.len(),
            required_fields_complete,
            levels_complete,
            hashes_stable,
            ordering_deterministic,
            non_interference_proven,
            accessibility_pass,
            perf_within_budget,
            provenance_complete,
            copy_exports_complete,
            interaction_coverage,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- galaxy-ux --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

fn export_units(exports: &[(String, CardExports)], card_id: &str, which: usize) -> usize {
    exports
        .iter()
        .find(|(id, _)| id == card_id)
        .map_or(0, |(_, e)| match which {
            0 => e.unicode.chars().count(),
            1 => e.latex.chars().count(),
            _ => e.json.chars().count(),
        })
}

/// Run the galaxy-brain UX contracts over `sources` with the given label.
#[must_use]
pub fn run_galaxy_ux(label: &str, sources: &[UxCardSource]) -> GalaxyUxReport {
    GalaxyUx::new(label).run(sources)
}

// ── Default corpus ───────────────────────────────────────────────────────────

fn posterior_source() -> UxCardSource {
    let claim = "ux.claim.posterior";
    let evidence: Vec<ChannelEvidence> = EvidenceChannel::ALL
        .iter()
        .map(|c| ChannelEvidence::new(*c, 3.0).claim(claim))
        .collect();
    let report = PosteriorEngine::default().evaluate(&[(claim.to_string(), evidence)]);
    UxCardSource {
        card: posterior_core_card(&report.records[0]),
        risk_class: UxRiskClass::Low,
        confidence_band: "0.86..0.94 (90% credible)".to_string(),
        guarantee_status: "holds".to_string(),
        evidence_id: "ev-ux-posterior".to_string(),
        policy_id: "pol-ux-explain".to_string(),
    }
}

fn conformal_source() -> UxCardSource {
    let cfg = ConformalConfig::default();
    let calibration: Vec<f64> = (0..30).map(|i| f64::from(i) * 0.01).collect();
    let forecast = conformal_interval(&cfg, &calibration, 0.5, &[])
        .expect("well-calibrated conformal forecast");
    UxCardSource {
        card: conformal_interval_card(&forecast, "ux.claim.conformal"),
        risk_class: UxRiskClass::Medium,
        confidence_band: "coverage 0.90 (split conformal)".to_string(),
        guarantee_status: "holds".to_string(),
        evidence_id: "ev-ux-conformal".to_string(),
        policy_id: "pol-ux-explain".to_string(),
    }
}

fn eprocess_source() -> UxCardSource {
    let config = EProcessConfig::default().with_alpha(0.05).with_mu0(0.1);
    let observations: Vec<f64> = vec![0.6; 30];
    let result = run_eprocess(&config, &observations).expect("e-process result");
    UxCardSource {
        card: evalue_fdr_card(&result, "ux.claim.eprocess"),
        risk_class: UxRiskClass::High,
        confidence_band: "e-value past 1/alpha (anytime-valid)".to_string(),
        guarantee_status: "rejected".to_string(),
        evidence_id: "ev-ux-eprocess".to_string(),
        policy_id: "pol-ux-explain".to_string(),
    }
}

/// The adversarial stress fixture: a posterior card inflated with many long
/// evidence terms, proving the truncation + budget contracts non-vacuously.
fn stress_source() -> UxCardSource {
    let mut source = posterior_source();
    source.card.claim_id = "ux.claim.stress".to_string();
    let long_note = "an intentionally verbose clarifying note that describes the provenance, \
                     the calibration procedure, and the guarantee assumptions in full detail"
        .to_string();
    for i in 0..40 {
        source.card.substitutions.push(Substitution::with_note(
            format!("theta_{i:02}"),
            format!("{}.{:06}", i, i * 991),
            long_note.clone(),
        ));
    }
    // Re-address the card body so the id stays content-derived and unique.
    source.card.card_id = format!(
        "galaxy-card-stress-{}",
        short_hash(&stable_hash(&source.card.substitutions))
    );
    source.evidence_id = "ev-ux-stress".to_string();
    source
}

/// The default deck: three real decision-kernel cards spanning the risk
/// classes plus the adversarial wide-card stress fixture.
#[must_use]
pub fn default_ux_sources() -> Vec<UxCardSource> {
    vec![
        posterior_source(),
        conformal_source(),
        eprocess_source(),
        stress_source(),
    ]
}

/// Run the galaxy-brain UX contracts over the default deck.
#[must_use]
pub fn run_default_galaxy_ux(label: &str) -> GalaxyUxReport {
    run_galaxy_ux(label, &default_ux_sources())
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized galaxy-brain UX pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct GalaxyUxPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for GalaxyUxPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "galaxy_ux".to_string(),
            label: "galaxy-ux/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GalaxyUxArtifact {
    /// Logical artifact name.
    pub name: String,
    /// Relative file path within the run directory.
    pub file: String,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// Byte length of the file content.
    pub bytes: u64,
}

/// The outcome of running and materializing the pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GalaxyUxOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL view ledger.
    pub ledger_path: String,
    /// Absolute path to the interaction log JSONL.
    pub interaction_log_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// The machine-readable summary.
    pub summary: GalaxyUxSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<GalaxyUxArtifact>,
}

fn artifact_of(file: &str, content: &str) -> GalaxyUxArtifact {
    GalaxyUxArtifact {
        name: file.replace(['.', '/'], "-"),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the galaxy-brain UX contracts over the default deck and materialize the
/// ledger / interaction log / per-card exports / summary / manifest under
/// `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_galaxy_ux_pipeline(
    run_root: &Path,
    config: &GalaxyUxPipelineConfig,
) -> crate::error::Result<GalaxyUxOutcome> {
    let report = GalaxyUx::new(config.label.as_str()).run(&default_ux_sources());

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let mut artifacts: Vec<GalaxyUxArtifact> = Vec::new();

    // Per-card machine exports (the copy-as artifacts, on disk for CI).
    for (card_id, exports) in &report.exports {
        let card_rel = format!("exports/{card_id}");
        let card_dir = run_dir.join(&card_rel);
        crate::util::ensure_dir(&card_dir)?;
        for (file, content) in [
            ("card.txt", exports.unicode.as_str()),
            ("card.tex", exports.latex.as_str()),
            ("card.json", exports.json.as_str()),
        ] {
            crate::util::write_string(&card_dir.join(file), content)?;
            artifacts.push(artifact_of(&format!("{card_rel}/{file}"), content));
        }
    }

    let ledger_content = report.render_ledger_jsonl();
    let interaction_content = report.render_interaction_jsonl();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let ledger_file = "ux_ledger.jsonl";
    let interaction_file = "interaction_log.jsonl";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&run_dir.join(interaction_file), &interaction_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    artifacts.push(artifact_of(ledger_file, &ledger_content));
    artifacts.push(artifact_of(interaction_file, &interaction_content));
    artifacts.push(artifact_of(summary_file, &summary_content));

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        gate_passes: bool,
        artifacts: &'a [GalaxyUxArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: GALAXY_UX_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(GalaxyUxOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        interaction_log_path: run_dir.join(interaction_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for the `galaxy-ux` command.
#[derive(Debug, clap::Args)]
pub struct GalaxyUxArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(long = "run-root", default_value = "/tmp/doctor_frankentui/galaxy_ux")]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "galaxy_ux")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "galaxy-ux/e2e")]
    pub label: String,
}

/// Run the `galaxy-ux` command: build the L0-L3 contract views over the
/// default deck, drive the scripted interaction session, materialize the
/// pipeline, and apply the fail-closed UX-contract gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the
/// gate fails (a hash drift, a non-interference violation, an accessibility or
/// budget breach, or missing provenance/exports), or an I/O error if artifacts
/// cannot be materialized.
pub fn run_galaxy_ux_command(args: GalaxyUxArgs) -> crate::error::Result<()> {
    let config = GalaxyUxPipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_galaxy_ux_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("galaxy-brain UX contracts"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "cards: {} | views: {} | interaction steps: {}",
            summary.total_cards, summary.total_views, summary.interaction_steps
        ));
        ui.info(&format!(
            "non-interference: {} | accessibility: {} | perf: {} | hashes: {}",
            summary.non_interference_proven,
            summary.accessibility_pass,
            summary.perf_within_budget,
            summary.hashes_stable
        ));
        if summary.gate_passes {
            ui.success("galaxy-ux gate PASSED");
        } else {
            ui.error("galaxy-ux gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "galaxy-ux gate failed: required_fields_complete={}, levels_complete={}, hashes_stable={}, ordering_deterministic={}, non_interference_proven={}, accessibility_pass={}, perf_within_budget={}, provenance_complete={}, copy_exports_complete={}, interaction_coverage={}",
                summary.required_fields_complete,
                summary.levels_complete,
                summary.hashes_stable,
                summary.ordering_deterministic,
                summary.non_interference_proven,
                summary.accessibility_pass,
                summary.perf_within_budget,
                summary.provenance_complete,
                summary.copy_exports_complete,
                summary.interaction_coverage
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn view<'a>(
        report: &'a GalaxyUxReport,
        claim: &str,
        level: DisclosureLevel,
    ) -> &'a UxViewLedgerEntry {
        report
            .views
            .iter()
            .find(|v| v.claim_id == claim && v.level == level)
            .expect("view present")
    }

    #[test]
    fn default_deck_passes_gate() {
        let report = run_default_galaxy_ux("gux/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_cards, 4);
        assert_eq!(report.summary.total_views, 16);
        assert!(report.summary.non_interference_proven);
        assert!(report.summary.accessibility_pass);
        assert!(report.summary.perf_within_budget);
        assert!(report.summary.copy_exports_complete);
        assert!(report.summary.interaction_coverage);
    }

    #[test]
    fn l0_carries_signal_confidence_and_risk() {
        let report = run_default_galaxy_ux("gux/test");
        let l0 = view(&report, "ux.claim.posterior", DisclosureLevel::L0);
        assert_eq!(l0.line_count, 3);
        assert!(l0.lines[1].starts_with("confidence: "));
        assert!(l0.lines[2].starts_with("risk: "));
        assert!(l0.line_count <= DisclosureLevel::L0.line_budget());
    }

    #[test]
    fn levels_disclose_progressively() {
        let report = run_default_galaxy_ux("gux/test");
        let claim = "ux.claim.conformal";
        let counts: Vec<usize> = DisclosureLevel::ALL
            .iter()
            .map(|&l| view(&report, claim, l).line_count)
            .collect();
        // Each level reveals at least as much as the previous one.
        assert!(
            counts.windows(2).all(|w| w[0] <= w[1]),
            "counts: {counts:?}"
        );
        // L1 adds intuition; L2 adds guarantee status; L3 adds the equation.
        assert!(
            view(&report, claim, DisclosureLevel::L1)
                .lines
                .iter()
                .any(|l| l.starts_with("intuition: "))
        );
        assert!(
            view(&report, claim, DisclosureLevel::L2)
                .lines
                .iter()
                .any(|l| l.starts_with("guarantee: "))
        );
        assert!(
            view(&report, claim, DisclosureLevel::L3)
                .lines
                .iter()
                .any(|l| l.starts_with("equation: "))
        );
    }

    #[test]
    fn stress_card_truncates_visibly_within_budgets() {
        let report = run_default_galaxy_ux("gux/test");
        let l2 = view(&report, "ux.claim.stress", DisclosureLevel::L2);
        assert!(l2.within_budget, "stress L2 blew the budget: {l2:?}");
        assert!(l2.lines.iter().all(|l| l.chars().count() <= MAX_LINE_CHARS));
        // Truncation is explicit, never silent.
        assert!(
            l2.lines.iter().any(|l| l.contains("more terms)")),
            "lines: {:?}",
            l2.lines
        );
        let l3 = view(&report, "ux.claim.stress", DisclosureLevel::L3);
        assert!(l3.within_budget);
        assert!(l3.line_count <= DisclosureLevel::L3.line_budget());
    }

    #[test]
    fn content_hashes_are_stable_and_rederivable() {
        let a = run_default_galaxy_ux("gux/test");
        let b = run_default_galaxy_ux("gux/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.views, b.views);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
        assert!(a.summary.hashes_stable);
    }

    #[test]
    fn deck_order_is_input_order_independent() {
        let mut sources = default_ux_sources();
        let forward = run_galaxy_ux("gux/test", &sources);
        sources.reverse();
        let reversed = run_galaxy_ux("gux/test", &sources);
        assert_eq!(forward.views, reversed.views);
        assert_eq!(forward.evidence_checksum, reversed.evidence_checksum);
    }

    #[test]
    fn tampered_hash_fails_the_gate() {
        let engine = GalaxyUx::new("gux/test");
        let mut report = engine.run(&default_ux_sources());
        report.views[0].content_hash = "tampered".to_string();
        let summary = engine.summarize(
            &report.views,
            &report.interactions,
            &report.exports,
            true,
            &report.report_id,
            &report.evidence_checksum,
        );
        assert!(!summary.hashes_stable);
        assert!(!summary.gate_passes);
    }

    #[test]
    fn interaction_model_is_pure_and_saturating() {
        let deck_len = 3;
        let mut state = UxInteractionState::default();
        // Expand past L3 saturates.
        for _ in 0..6 {
            state = apply_command(state, UxCommand::ExpandOne, deck_len);
        }
        assert_eq!(state.level, DisclosureLevel::L3);
        // Collapse past L0 saturates.
        for _ in 0..6 {
            state = apply_command(state, UxCommand::CollapseOne, deck_len);
        }
        assert_eq!(state.level, DisclosureLevel::L0);
        // Focus moves reset the level and saturate at the edges.
        state.level = DisclosureLevel::L2;
        state = apply_command(state, UxCommand::NextCard, deck_len);
        assert_eq!(
            state,
            UxInteractionState {
                card_index: 1,
                level: DisclosureLevel::L0
            }
        );
        for _ in 0..5 {
            state = apply_command(state, UxCommand::PrevCard, deck_len);
        }
        assert_eq!(state.card_index, 0);
        // Copy commands never move state.
        let before = state;
        assert_eq!(apply_command(state, UxCommand::CopyJson, deck_len), before);
    }

    #[test]
    fn scripted_session_covers_every_command_discretely() {
        let report = run_default_galaxy_ux("gux/test");
        let seen: BTreeSet<UxCommand> = report.interactions.iter().map(|i| i.command).collect();
        assert_eq!(seen.len(), 7, "session must exercise every command class");
        assert!(report.interactions.iter().all(|i| i.discrete));
        assert!(report.summary.interaction_coverage);
        // Copy steps report the export size as their latency proxy.
        let copy = report
            .interactions
            .iter()
            .find(|i| i.command == UxCommand::CopyJson)
            .unwrap();
        assert!(copy.latency_units > 0);
        assert_eq!(copy.copied_format, "json");
    }

    #[test]
    fn exports_parse_and_embed_the_card_id() {
        let report = run_default_galaxy_ux("gux/test");
        for (card_id, exports) in &report.exports {
            let value: serde_json::Value =
                serde_json::from_str(&exports.json).expect("JSON export parses");
            assert_eq!(value["card_id"].as_str().unwrap(), card_id);
            assert!(exports.latex.contains(card_id.as_str()));
            assert!(!exports.unicode.is_empty());
        }
    }

    #[test]
    fn text_alternatives_are_ascii_screen_reader_ready() {
        let report = run_default_galaxy_ux("gux/test");
        for v in &report.views {
            assert!(v.text_alternative.is_ascii(), "alt: {}", v.text_alternative);
            assert!(!v.text_alternative.is_empty());
        }
    }

    #[test]
    fn provenance_links_are_complete() {
        let report = run_default_galaxy_ux("gux/test");
        assert!(report.summary.provenance_complete);
        let l3 = view(&report, "ux.claim.eprocess", DisclosureLevel::L3);
        assert_eq!(l3.evidence_id, "ev-ux-eprocess");
        assert_eq!(l3.policy_id, "pol-ux-explain");
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_galaxy_ux_pipeline(dir.path(), &GalaxyUxPipelineConfig::default()).unwrap();
        assert!(outcome.summary.gate_passes);
        // 4 cards x 3 exports + ledger + interaction log + summary
        // (manifest not self-listed).
        assert_eq!(outcome.artifacts.len(), 15);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(
                sha256_hex(&bytes),
                artifact.sha256,
                "file {}",
                artifact.file
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_default_galaxy_ux(&label);
            let second = run_default_galaxy_ux(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(first.render_ledger_jsonl(), second.render_ledger_jsonl());
            prop_assert_eq!(first.render_interaction_jsonl(), second.render_interaction_jsonl());
        }

        #[test]
        fn prop_gate_always_passes_on_default_deck(label in "[a-z]{1,8}") {
            let report = run_default_galaxy_ux(&label);
            prop_assert!(report.gate_passes);
            prop_assert!(report.summary.non_interference_proven);
        }
    }
}
