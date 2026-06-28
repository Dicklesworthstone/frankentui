// SPDX-License-Identifier: Apache-2.0
//! Galaxy-brain transparency cards: operator-friendly explanations of the
//! advanced math behind every consequential migration decision.
//!
//! The deterministic stack produces verdicts in several modules — a posterior
//! over acceptance ([`crate::posterior_core`]), an expected-loss action policy
//! ([`crate::decision_loss_policy`]), formal guarantees
//! ([`crate::guarantee_layer`]), value-of-information probe selection, and
//! BOCPD/CUSUM regime monitoring ([`crate::drift_monitor`]). Those modules
//! answer *what to decide*; this module answers *why, in human-legible terms*,
//! surfacing the governing equation, the substituted numeric values, a plain
//! English intuition line, and the artifact references a reviewer can re-derive
//! the card from.
//!
//! # Card families
//!
//! | kind | source artifact | equation surfaced |
//! | --- | --- | --- |
//! | [`CardKind::PosteriorCore`] | [`crate::posterior_core::PosteriorRecord`] | log-odds update `logit(P) = logit(prior) + Σ wᵢ·ln BFᵢ` |
//! | [`CardKind::LossDecision`] | [`crate::decision_loss_policy::DecisionLogEntry`] | `a* = argmin_a Σ_s P(s)·L(a,s)` |
//! | [`CardKind::ConformalInterval`] | [`crate::guarantee_layer::ConformalForecast`] | split-conformal radius `q = Q_{⌈(1-α)(n+1)⌉}` |
//! | [`CardKind::EValueFdr`] | [`crate::guarantee_layer::EProcessResult`] | wealth process `Wₜ`, reject iff `sup_t Wₜ ≥ 1/α` (Ville) |
//! | [`CardKind::VoiSelection`] | [`VoiCardInput`] | `net = VOI·value_scale − cost`, run iff `net ≥ 0` |
//! | [`CardKind::HazardRegime`] | [`crate::drift_monitor::DriftMetricSummary`] | BOCPD run-length posterior + CUSUM/e-process firing |
//!
//! # Design properties
//!
//! - **Presentation-only (AC3).** Every builder takes a shared reference to an
//!   already-computed artifact and only *reads* it. Building or rendering a card
//!   cannot change a decision; toggling "explain mode" is simply choosing whether
//!   to call these builders. No builder returns or mutates a decision.
//! - **Reproducible from artifacts (AC2).** A card is a pure function of its
//!   source struct, which is itself a deterministic, serializable artifact.
//!   Re-deriving a card from the stored artifact yields a byte-identical card,
//!   and identical cards render to byte-identical text — so cards diff cleanly
//!   across reruns. All ids are SHA-256 of a canonical serialization; there are
//!   no clocks or randomness, and all floats render at fixed precision.
//! - **Full numeric substitution + rationale (AC1).** Each card carries the
//!   governing `equation`, the concrete `substitutions` that were plugged into
//!   it, a plain-English `intuition`, and `artifact_refs` linking back to the
//!   evidence — so a promoted decision is auditable and debuggable end to end.
//!
//! # Renderers
//!
//! [`render_card`] (and [`GalaxyBrainCardSheet::render`]) emit three formats with
//! deterministic formatting: [`CardFormat::CliUnicode`] (box-drawing for the
//! terminal), [`CardFormat::Markdown`] (report embedding), and
//! [`CardFormat::Json`] (machine output / CI). The JSON form is exactly the
//! serde serialization, so it round-trips.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::DecisionLogEntry;
use crate::drift_monitor::DriftMetricSummary;
use crate::guarantee_layer::{ConformalForecast, EProcessResult};
use crate::posterior_core::PosteriorRecord;
use crate::semantic_contract::MigrationDecision;

// ── Schema version & constants ───────────────────────────────────────────────

/// Schema version for galaxy-brain card artifacts.
pub const GALAXY_BRAIN_CARDS_SCHEMA_VERSION: &str = "galaxy-brain-cards-v1";

/// Width (in cells) of the box-drawing rule used by the CLI renderer.
const CARD_RULE_WIDTH: usize = 78;

// ── Errors ───────────────────────────────────────────────────────────────────

/// Errors raised while assembling a galaxy-brain card sheet.
///
/// Individual card builders are infallible (they read already-validated
/// artifacts); only sheet assembly can fail, and only on a structurally empty
/// request.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GalaxyBrainCardError {
    /// The card-sheet request carried no source artifacts.
    #[error("invalid galaxy-brain card input: {detail}")]
    InvalidInput {
        /// Human-readable explanation.
        detail: String,
    },
}

/// Convenience result alias for this module.
type Result<T> = std::result::Result<T, GalaxyBrainCardError>;

// ── Card taxonomy ────────────────────────────────────────────────────────────

/// The family of math a card explains.
///
/// The canonical [`CardKind::order_index`] gives sheets a stable presentation
/// order regardless of the order callers supply sources in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    /// Closed-form Bayesian posterior over migration acceptance.
    PosteriorCore,
    /// Expected-loss `argmin` action policy.
    LossDecision,
    /// Split-conformal prediction interval.
    ConformalInterval,
    /// Anytime-valid e-process / FDR wealth state.
    EValueFdr,
    /// Value-of-information probe selection.
    VoiSelection,
    /// BOCPD/CUSUM hazard & regime status.
    HazardRegime,
}

impl CardKind {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PosteriorCore => "posterior_core",
            Self::LossDecision => "loss_decision",
            Self::ConformalInterval => "conformal_interval",
            Self::EValueFdr => "e_value_fdr",
            Self::VoiSelection => "voi_selection",
            Self::HazardRegime => "hazard_regime",
        }
    }

    /// Canonical presentation order for sheets.
    #[must_use]
    pub fn order_index(self) -> usize {
        match self {
            Self::PosteriorCore => 0,
            Self::LossDecision => 1,
            Self::ConformalInterval => 2,
            Self::EValueFdr => 3,
            Self::VoiSelection => 4,
            Self::HazardRegime => 5,
        }
    }

    /// Operator-facing card title.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::PosteriorCore => "Posterior Core",
            Self::LossDecision => "Loss-Minimization Decision",
            Self::ConformalInterval => "Conformal Interval",
            Self::EValueFdr => "E-Value / FDR State",
            Self::VoiSelection => "Value-of-Information Selection",
            Self::HazardRegime => "Hazard / Regime Status",
        }
    }
}

// ── Substitution row ─────────────────────────────────────────────────────────

/// One substituted term in a card: a symbol bound to its concrete value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Substitution {
    /// The symbol from the equation (e.g. `P_post`, `α`, `q`).
    pub symbol: String,
    /// The concrete value, pre-formatted at fixed precision.
    pub value: String,
    /// Optional clarifying note.
    pub note: Option<String>,
}

impl Substitution {
    /// A substitution with no note.
    #[must_use]
    pub fn new(symbol: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            value: value.into(),
            note: None,
        }
    }

    /// A substitution carrying a clarifying note.
    #[must_use]
    pub fn with_note(
        symbol: impl Into<String>,
        value: impl Into<String>,
        note: impl Into<String>,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            value: value.into(),
            note: Some(note.into()),
        }
    }
}

// ── Card ─────────────────────────────────────────────────────────────────────

/// A single galaxy-brain transparency card.
///
/// Construct one with a standard builder ([`posterior_core_card`],
/// [`loss_decision_card`], [`conformal_interval_card`], [`evalue_fdr_card`],
/// [`voi_selection_card`], [`hazard_regime_card`]) and render it with
/// [`render_card`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GalaxyBrainCard {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic content-addressed id (SHA-256 of the card body).
    pub card_id: String,
    /// Which math family this card explains.
    pub kind: CardKind,
    /// Operator-facing title.
    pub title: String,
    /// The claim this card pertains to.
    pub claim_id: String,
    /// One-line verdict headline.
    pub headline: String,
    /// The governing equation, in legible notation.
    pub equation: String,
    /// The concrete substitutions plugged into `equation` (AC1).
    pub substitutions: Vec<Substitution>,
    /// Plain-English intuition (AC1).
    pub intuition: String,
    /// References to the source artifacts the card was derived from (AC2).
    pub artifact_refs: Vec<String>,
}

impl GalaxyBrainCard {
    /// Assemble a card, computing its deterministic `card_id` from the body.
    fn assemble(
        kind: CardKind,
        claim_id: impl Into<String>,
        headline: impl Into<String>,
        equation: impl Into<String>,
        substitutions: Vec<Substitution>,
        intuition: impl Into<String>,
        artifact_refs: Vec<String>,
    ) -> Self {
        let claim_id = claim_id.into();
        let headline = headline.into();
        let equation = equation.into();
        let intuition = intuition.into();
        let card_id = format!(
            "galaxy-card-{}",
            short_hash(&stable_hash(&CanonicalCard {
                schema_version: GALAXY_BRAIN_CARDS_SCHEMA_VERSION,
                kind,
                claim_id: &claim_id,
                headline: &headline,
                equation: &equation,
                substitutions: &substitutions,
                intuition: &intuition,
                artifact_refs: &artifact_refs,
            }))
        );
        Self {
            schema_version: GALAXY_BRAIN_CARDS_SCHEMA_VERSION.to_string(),
            card_id,
            kind,
            title: kind.title().to_string(),
            claim_id,
            headline,
            equation,
            substitutions,
            intuition,
            artifact_refs,
        }
    }

    /// Pretty-printed JSON of the card.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|error| error.to_string())
    }

    /// Render this card in `format`.
    #[must_use]
    pub fn render(&self, format: CardFormat) -> String {
        render_card(self, format)
    }
}

/// Canonical, id-free view of a card used for content-addressing.
#[derive(Serialize)]
struct CanonicalCard<'a> {
    schema_version: &'a str,
    kind: CardKind,
    claim_id: &'a str,
    headline: &'a str,
    equation: &'a str,
    substitutions: &'a [Substitution],
    intuition: &'a str,
    artifact_refs: &'a [String],
}

// ── Render formats ───────────────────────────────────────────────────────────

/// Output format for a card or sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardFormat {
    /// Box-drawing unicode for terminal display.
    CliUnicode,
    /// Markdown for report embedding.
    Markdown,
    /// Machine-readable JSON (the serde serialization).
    Json,
}

/// Render `card` in `format` with deterministic formatting.
#[must_use]
pub fn render_card(card: &GalaxyBrainCard, format: CardFormat) -> String {
    match format {
        CardFormat::CliUnicode => render_card_cli(card),
        CardFormat::Markdown => render_card_markdown(card),
        CardFormat::Json => card.to_json(),
    }
}

fn render_card_cli(card: &GalaxyBrainCard) -> String {
    let mut out = String::new();
    out.push_str(&rule_top(&card.title));
    out.push('\n');
    push_cli_line(&mut out, &format!("claim    {}", card.claim_id));
    push_cli_line(&mut out, &format!("verdict  {}", card.headline));
    push_cli_line(&mut out, "");
    push_cli_line(&mut out, "equation");
    push_cli_line(&mut out, &format!("  {}", card.equation));
    push_cli_line(&mut out, "");
    push_cli_line(&mut out, "substitutions");
    let width = card
        .substitutions
        .iter()
        .map(|s| s.symbol.chars().count())
        .max()
        .unwrap_or(0);
    for sub in &card.substitutions {
        let pad = width.saturating_sub(sub.symbol.chars().count());
        let spaces: String = " ".repeat(pad);
        let line = match &sub.note {
            Some(note) => format!("  {}{} = {}  ({note})", sub.symbol, spaces, sub.value),
            None => format!("  {}{} = {}", sub.symbol, spaces, sub.value),
        };
        push_cli_line(&mut out, &line);
    }
    push_cli_line(&mut out, "");
    push_cli_line(&mut out, "intuition");
    push_cli_line(&mut out, &format!("  {}", card.intuition));
    if !card.artifact_refs.is_empty() {
        push_cli_line(&mut out, "");
        push_cli_line(&mut out, "artifacts");
        for art in &card.artifact_refs {
            push_cli_line(&mut out, &format!("  - {art}"));
        }
    }
    out.push_str(&rule_bottom());
    out
}

fn render_card_markdown(card: &GalaxyBrainCard) -> String {
    let mut out = String::new();
    out.push_str(&format!("### {}\n\n", card.title));
    out.push_str(&format!("- **Claim:** `{}`\n", card.claim_id));
    out.push_str(&format!("- **Verdict:** {}\n", card.headline));
    out.push_str(&format!("- **Card ID:** `{}`\n\n", card.card_id));
    out.push_str("**Equation**\n\n");
    out.push_str(&format!("```\n{}\n```\n\n", card.equation));
    out.push_str("**Substitutions**\n\n");
    out.push_str("| Symbol | Value | Note |\n| --- | --- | --- |\n");
    for sub in &card.substitutions {
        let note = sub.note.as_deref().unwrap_or("");
        out.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            sub.symbol, sub.value, note
        ));
    }
    out.push_str(&format!("\n**Intuition**\n\n{}\n", card.intuition));
    if !card.artifact_refs.is_empty() {
        out.push_str("\n**Artifacts**\n\n");
        for art in &card.artifact_refs {
            out.push_str(&format!("- `{art}`\n"));
        }
    }
    out
}

fn rule_top(title: &str) -> String {
    // "┏━ {title} " then fill with ━ to the rule width.
    let prefix = format!("┏━ {title} ");
    let used = prefix.chars().count();
    let fill = CARD_RULE_WIDTH.saturating_sub(used);
    format!("{prefix}{}", "━".repeat(fill))
}

fn rule_bottom() -> String {
    format!("┗{}", "━".repeat(CARD_RULE_WIDTH.saturating_sub(1)))
}

fn push_cli_line(out: &mut String, content: &str) {
    if content.is_empty() {
        out.push_str("┃\n");
    } else {
        out.push_str(&format!("┃ {content}\n"));
    }
}

// ── Standard card builders ───────────────────────────────────────────────────

/// Build the posterior-core card from a [`PosteriorRecord`].
#[must_use]
pub fn posterior_core_card(record: &PosteriorRecord) -> GalaxyBrainCard {
    let mut substitutions = vec![
        Substitution::with_note(
            "logit(prior)",
            fmt4(record.prior_logit),
            "prior log-odds of acceptance",
        ),
        Substitution::with_note(
            "Σ wᵢ·ln BFᵢ",
            fmt4(record.aggregate_log_bayes_factor),
            "aggregate log Bayes factor (present channels)",
        ),
        Substitution::new("logit(P_post)", fmt4(record.posterior_logit)),
        Substitution::with_note(
            "σ_logit",
            fmt4(record.posterior_logit_std),
            "posterior log-odds std",
        ),
        Substitution::with_note("P_post", fmt4(record.posterior_prob), "= σ(logit(P_post))"),
        Substitution::with_note(
            "CI(P)",
            format!(
                "[{}, {}]",
                fmt4(record.credible_lower_prob),
                fmt4(record.credible_upper_prob)
            ),
            "credible interval on acceptance prob",
        ),
        Substitution::new("present_channels", record.present_channel_count.to_string()),
    ];
    let top_contributors = record.top_contributors(1);
    if let Some(top) = top_contributors.first() {
        substitutions.push(Substitution::with_note(
            "top_channel",
            top.channel.as_str().to_string(),
            format!("contributes {} to the logit", fmt4(top.contribution)),
        ));
    }

    let degraded_note = if record.degraded {
        " (degraded/sparse evidence)"
    } else {
        ""
    };
    let mut intuition = format!(
        "Posterior acceptance probability is {} (logit {} ± {}); decision {}{}. ",
        fmt_pct(record.posterior_prob),
        fmt4(record.posterior_logit),
        fmt4(record.posterior_logit_std),
        decision_label(record.decision),
        degraded_note,
    );
    let cf = &record.counterfactual;
    match (cf.nearest_channel, cf.nearest_channel_required_log_bf) {
        (Some(channel), Some(required)) => {
            intuition.push_str(&format!(
                "Nearest flip: move channel {} to ln BF {} to flip the verdict to {}.",
                channel.as_str(),
                fmt4(required),
                decision_label(cf.flips_to),
            ));
        }
        _ => {
            intuition.push_str(&format!(
                "No single-channel change flips the verdict toward {}.",
                decision_label(cf.flips_to),
            ));
        }
    }

    let mut artifact_refs = vec![format!("posterior-core://{}", record.claim_id)];
    for contribution in &record.contributions {
        for artifact in &contribution.artifact_ids {
            if !artifact_refs.contains(artifact) {
                artifact_refs.push(artifact.clone());
            }
        }
    }

    let headline = format!(
        "{} · P={}{}",
        decision_label(record.decision),
        fmt4(record.posterior_prob),
        degraded_note,
    );

    GalaxyBrainCard::assemble(
        CardKind::PosteriorCore,
        record.claim_id.clone(),
        headline,
        "logit(P_post) = logit(prior) + Σ wᵢ·ln BFᵢ ;  P_post = σ(logit(P_post))",
        substitutions,
        intuition,
        artifact_refs,
    )
}

/// Build the loss-minimization decision card from a [`DecisionLogEntry`].
#[must_use]
pub fn loss_decision_card(entry: &DecisionLogEntry) -> GalaxyBrainCard {
    let decision = &entry.decision;
    let mut substitutions = vec![
        Substitution::new("profile", format!("{:?}", entry.profile)),
        Substitution::new("tier", format!("{:?}", entry.tier)),
        Substitution::with_note(
            "a*",
            decision_label(decision.selected).to_string(),
            "argmin",
        ),
        Substitution::new("min E[L]", fmt4(decision.min_expected_loss)),
        Substitution::with_note(
            "runner_up",
            decision
                .runner_up
                .map_or("none".to_string(), |a| decision_label(a).to_string()),
            format!("margin {}", fmt4(decision.margin)),
        ),
        Substitution::new("tie_break", decision.tie_break_applied.to_string()),
    ];
    for action in &decision.per_action {
        substitutions.push(Substitution::with_note(
            format!("E[L|{}]", decision_label(action.action)),
            fmt4(action.expected_loss),
            format!("worst-case {}", fmt4(action.worst_case_loss)),
        ));
    }
    if let Some(prob) = entry.posterior_prob {
        substitutions.push(Substitution::with_note(
            "P_post",
            fmt4(prob),
            "posterior acceptance prob driving the state distribution",
        ));
    }

    let intuition = format!(
        "Under the {:?}/{:?} loss matrix, {} minimizes expected loss ({}); the next-best action {} costs {} more. {}",
        entry.profile,
        entry.tier,
        decision_label(decision.selected),
        fmt4(decision.min_expected_loss),
        decision
            .runner_up
            .map_or("(none)".to_string(), |a| decision_label(a).to_string()),
        fmt4(decision.margin),
        if decision.tie_break_applied {
            "A deterministic minimax/conservatism tie-break was applied."
        } else {
            "The minimum was strict; no tie-break was needed."
        },
    );

    let artifact_refs = vec![
        format!("decision://{}", entry.decision_id),
        format!("policy://{}@{}", entry.policy_id, entry.policy_version),
        format!("matrix://{}", entry.matrix_effective_checksum),
    ];

    let headline = format!(
        "{} · E[L]={} · margin={}",
        decision_label(decision.selected),
        fmt4(decision.min_expected_loss),
        fmt4(decision.margin),
    );

    GalaxyBrainCard::assemble(
        CardKind::LossDecision,
        entry.claim_id.clone(),
        headline,
        "a* = argmin_a Σ_s P(s)·L(a,s)",
        substitutions,
        intuition,
        artifact_refs,
    )
}

/// Build the conformal-interval card from a [`ConformalForecast`].
#[must_use]
pub fn conformal_interval_card(forecast: &ConformalForecast, claim_id: &str) -> GalaxyBrainCard {
    let mut substitutions = vec![
        Substitution::with_note("α", fmt4(forecast.alpha), "target miscoverage"),
        Substitution::with_note("1−α", fmt4(forecast.nominal_coverage), "nominal coverage"),
        Substitution::new("forecast", fmt4(forecast.forecast_point)),
        Substitution::with_note("q", fmt4(forecast.conformal_quantile), "conformal radius"),
        Substitution::new(
            "interval",
            format!("[{}, {}]", fmt4(forecast.lower), fmt4(forecast.upper)),
        ),
        Substitution::with_note(
            "n",
            forecast.calibration_count.to_string(),
            "calibration points",
        ),
        Substitution::with_note(
            "rank",
            forecast.quantile_rank.to_string(),
            "1-indexed order statistic",
        ),
    ];
    match forecast.empirical_coverage {
        Some(cov) => substitutions.push(Substitution::with_note(
            "emp_coverage",
            fmt4(cov),
            format!("on {} held-out points", forecast.validation_count),
        )),
        None => substitutions.push(Substitution::new("emp_coverage", "n/a".to_string())),
    }

    let intuition = if forecast.vacuous {
        format!(
            "The conformal interval is vacuous (calibration count {} below the trust floor); a conservative fallback is recommended.",
            forecast.calibration_count,
        )
    } else {
        let cov = forecast
            .empirical_coverage
            .map_or("n/a".to_string(), fmt_pct);
        format!(
            "With nominal coverage {}, the true migration quality lies in [{}, {}] (radius {}); empirical held-out coverage was {}.{}",
            fmt_pct(forecast.nominal_coverage),
            fmt4(forecast.lower),
            fmt4(forecast.upper),
            fmt4(forecast.conformal_quantile),
            cov,
            if forecast.fallback_recommended {
                " A coverage shortfall flagged a fallback."
            } else {
                ""
            },
        )
    };

    let artifact_refs = vec![
        format!("conformal://{}", forecast.forecast_id),
        format!("claim://{claim_id}"),
    ];

    let headline = if forecast.vacuous {
        "vacuous interval · fallback".to_string()
    } else {
        format!(
            "[{}, {}] @ {} coverage",
            fmt4(forecast.lower),
            fmt4(forecast.upper),
            fmt_pct(forecast.nominal_coverage),
        )
    };

    GalaxyBrainCard::assemble(
        CardKind::ConformalInterval,
        claim_id.to_string(),
        headline,
        "q = Quantile_{⌈(1−α)(n+1)⌉}(nonconformity) ;  interval = forecast ± q",
        substitutions,
        intuition,
        artifact_refs,
    )
}

/// Build the e-value / FDR-state card from an [`EProcessResult`].
#[must_use]
pub fn evalue_fdr_card(result: &EProcessResult, claim_id: &str) -> GalaxyBrainCard {
    let substitutions = vec![
        Substitution::with_note("α", fmt4(result.alpha), "target type-I level"),
        Substitution::with_note("1/α", fmt4(result.threshold), "rejection threshold (Ville)"),
        Substitution::with_note("μ₀", fmt4(result.mu0), "null mean"),
        Substitution::with_note(
            "W_final",
            fmt4(result.final_wealth),
            "wealth at last observation",
        ),
        Substitution::with_note("W_peak", fmt4(result.peak_wealth), "sup_t Wₜ"),
        Substitution::new("observations", result.observation_count.to_string()),
        Substitution::new("rejected", result.rejected.to_string()),
        Substitution::new(
            "stop_index",
            result
                .stopping_index
                .map_or("none".to_string(), |i| i.to_string()),
        ),
    ];

    let intuition = if result.rejected {
        format!(
            "Anytime-valid e-process REJECTED the null: peak wealth {} reached the threshold {} at step {}. The predictable betting fraction keeps this valid under optional stopping — degradation is real, not a peeking artifact.",
            fmt4(result.peak_wealth),
            fmt4(result.threshold),
            result
                .stopping_index
                .map_or_else(|| "?".to_string(), |i| i.to_string()),
        )
    } else {
        format!(
            "Anytime-valid e-process retained the null: peak wealth {} never reached the threshold {} over {} observations. No evidence of quality degradation; valid to check after every observation without inflating type-I error.",
            fmt4(result.peak_wealth),
            fmt4(result.threshold),
            result.observation_count,
        )
    };

    let artifact_refs = vec![
        format!("e-process://{}", result.process_id),
        format!("claim://{claim_id}"),
    ];

    let headline = if result.rejected {
        format!(
            "REJECT (degradation) · W*={}/{}",
            fmt4(result.peak_wealth),
            fmt4(result.threshold),
        )
    } else {
        format!(
            "retain · W*={}/{}",
            fmt4(result.peak_wealth),
            fmt4(result.threshold),
        )
    };

    GalaxyBrainCard::assemble(
        CardKind::EValueFdr,
        claim_id.to_string(),
        headline,
        "Wₜ = Π_i (1 + λᵢ·(Xᵢ − μ₀)) ;  reject iff sup_t Wₜ ≥ 1/α",
        substitutions,
        intuition,
        artifact_refs,
    )
}

/// Input contract for a value-of-information selection card.
///
/// The VOI probe planner (`bd-3bxhj.10.24`) — or any module that scores a probe
/// — populates this view; the card layer presents it. Keeping the input here
/// keeps the card presentation-only and decoupled from the planner internals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiCardInput {
    /// The claim this probe informs.
    pub claim_id: String,
    /// Identifier of the candidate probe.
    pub probe_id: String,
    /// Estimated value of information (expected decision improvement).
    pub voi: f64,
    /// Compute/operational cost of running the probe (same units as net value).
    pub cost: f64,
    /// Scalar converting VOI into the cost's value units.
    pub value_scale: f64,
    /// Net value `voi·value_scale − cost`.
    pub net_value: f64,
    /// Whether the planner selected the probe to run.
    pub selected: bool,
    /// Rationale when the probe was skipped (or kept for the record).
    pub rationale: Option<String>,
}

impl VoiCardInput {
    /// Construct an input, computing `net_value = voi·value_scale − cost`.
    #[must_use]
    pub fn new(
        claim_id: impl Into<String>,
        probe_id: impl Into<String>,
        voi: f64,
        value_scale: f64,
        cost: f64,
        selected: bool,
    ) -> Self {
        Self {
            claim_id: claim_id.into(),
            probe_id: probe_id.into(),
            voi,
            cost,
            value_scale,
            net_value: voi * value_scale - cost,
            selected,
            rationale: None,
        }
    }

    /// Attach a selection/skip rationale.
    #[must_use]
    pub fn with_rationale(mut self, rationale: impl Into<String>) -> Self {
        self.rationale = Some(rationale.into());
        self
    }
}

/// Build the value-of-information selection card from a [`VoiCardInput`].
#[must_use]
pub fn voi_selection_card(input: &VoiCardInput) -> GalaxyBrainCard {
    let substitutions = vec![
        Substitution::new("probe", input.probe_id.clone()),
        Substitution::with_note("VOI", fmt4(input.voi), "expected decision improvement"),
        Substitution::with_note("value_scale", fmt4(input.value_scale), "VOI → cost units"),
        Substitution::with_note(
            "VOI·scale",
            fmt4(input.voi * input.value_scale),
            "expected gain in cost units",
        ),
        Substitution::new("cost", fmt4(input.cost)),
        Substitution::with_note("net", fmt4(input.net_value), "VOI·scale − cost"),
        Substitution::new("selected", input.selected.to_string()),
    ];

    let action = if input.selected { "RUN" } else { "SKIP" };
    let mut intuition = format!(
        "Probe {}: expected decision improvement (VOI {}) × value scale {} = {}; minus cost {} ⇒ net value {}. Decision: {} (run iff net ≥ 0).",
        input.probe_id,
        fmt4(input.voi),
        fmt4(input.value_scale),
        fmt4(input.voi * input.value_scale),
        fmt4(input.cost),
        fmt4(input.net_value),
        action,
    );
    if let Some(rationale) = &input.rationale {
        intuition.push(' ');
        intuition.push_str(rationale);
    }

    let artifact_refs = vec![
        format!("voi-probe://{}", input.probe_id),
        format!("claim://{}", input.claim_id),
    ];

    let headline = format!(
        "{action} {} · net={}",
        input.probe_id,
        fmt4(input.net_value)
    );

    GalaxyBrainCard::assemble(
        CardKind::VoiSelection,
        input.claim_id.clone(),
        headline,
        "net = VOI·value_scale − cost ;  run iff net ≥ 0",
        substitutions,
        intuition,
        artifact_refs,
    )
}

/// Build the hazard / regime-status card from a [`DriftMetricSummary`].
#[must_use]
pub fn hazard_regime_card(summary: &DriftMetricSummary, claim_id: &str) -> GalaxyBrainCard {
    let substitutions = vec![
        Substitution::new("metric", summary.metric_id.clone()),
        Substitution::with_note("μ_baseline", fmt4(summary.baseline_mean), "baseline mean"),
        Substitution::with_note("σ_baseline", fmt4(summary.baseline_std), "baseline std"),
        Substitution::new("final_value", fmt4(summary.final_value)),
        Substitution::with_note(
            "E[run length]",
            fmt4(summary.expected_run_length_final),
            "BOCPD posterior; longer ⇒ stabler regime",
        ),
        Substitution::new("events", summary.event_count.to_string()),
        Substitution::with_note(
            "regressions",
            summary.regression_count.to_string(),
            "CUSUM/e-process firings",
        ),
        Substitution::new("improvements", summary.improvement_count.to_string()),
        Substitution::with_note(
            "max_conf",
            fmt4(summary.max_confidence),
            "peak detector confidence",
        ),
    ];

    let shifted = summary.regression_count > 0;
    let intuition = format!(
        "Metric {}: BOCPD expected run length {} ({}); {} regression / {} improvement event(s) over {} observations, peak detector confidence {}. {}",
        summary.metric_id,
        fmt4(summary.expected_run_length_final),
        if shifted {
            "a short run length signals a recent regime change"
        } else {
            "a growing run length signals a stable regime"
        },
        summary.regression_count,
        summary.improvement_count,
        summary.observation_count,
        fmt4(summary.max_confidence),
        if shifted {
            "Regime shift / regression detected — escalate."
        } else {
            "Regime is stable — no escalation."
        },
    );

    let artifact_refs = vec![
        format!("drift-monitor://{}", summary.metric_id),
        format!("claim://{claim_id}"),
    ];

    let headline = format!(
        "{} · E[run]={} · regr={}",
        if shifted { "REGIME SHIFT" } else { "stable" },
        fmt4(summary.expected_run_length_final),
        summary.regression_count,
    );

    GalaxyBrainCard::assemble(
        CardKind::HazardRegime,
        claim_id.to_string(),
        headline,
        "BOCPD run-length posterior P(rₜ | x_{1:t}) ;  regression iff CUSUM/e-process fires",
        substitutions,
        intuition,
        artifact_refs,
    )
}

// ── Card sheet ───────────────────────────────────────────────────────────────

/// JSON-stats artifact (path / checksum / content), matching the crate idiom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonStatsArtifact {
    /// Suggested artifact path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// The serialized JSON content.
    pub content: String,
}

/// A deterministic bundle of the galaxy-brain cards linked to one decision.
///
/// Cards are stored in canonical [`CardKind::order_index`] order so reruns
/// produce byte-identical sheets regardless of source-supply order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GalaxyBrainCardSheet {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic sheet id (SHA-256 over the claim + card ids).
    pub sheet_id: String,
    /// The claim this sheet pertains to.
    pub claim_id: String,
    /// The cards, in canonical order.
    pub cards: Vec<GalaxyBrainCard>,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: JsonStatsArtifact,
    /// Deterministic replay command.
    pub replay_command: String,
}

impl GalaxyBrainCardSheet {
    /// Assemble a sheet from already-built cards.
    ///
    /// Cards are sorted into canonical kind order (then by `card_id` for stable
    /// tie-breaks); the sheet id, json-stats artifact, and replay command are
    /// all derived deterministically from the contents.
    #[must_use]
    pub fn new(claim_id: impl Into<String>, mut cards: Vec<GalaxyBrainCard>) -> Self {
        let claim_id = claim_id.into();
        cards.sort_by(|a, b| {
            a.kind
                .order_index()
                .cmp(&b.kind.order_index())
                .then_with(|| a.card_id.cmp(&b.card_id))
        });
        let card_ids: Vec<&str> = cards.iter().map(|c| c.card_id.as_str()).collect();
        let sheet_id = format!(
            "galaxy-sheet-{}",
            short_hash(&stable_hash(&CanonicalSheetId {
                schema_version: GALAXY_BRAIN_CARDS_SCHEMA_VERSION,
                claim_id: &claim_id,
                card_ids: &card_ids,
            }))
        );
        let exported_json_stats = export_json_stats(&sheet_id, &claim_id, &cards);
        let replay_command = format!(
            "cargo test -p doctor_frankentui --lib galaxy_brain_cards -- --nocapture # sheet {sheet_id}"
        );
        Self {
            schema_version: GALAXY_BRAIN_CARDS_SCHEMA_VERSION.to_string(),
            sheet_id,
            claim_id,
            cards,
            exported_json_stats,
            replay_command,
        }
    }

    /// Pretty-printed JSON of the sheet.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|error| error.to_string())
    }

    /// The card kinds present, in canonical order.
    #[must_use]
    pub fn card_kinds(&self) -> Vec<CardKind> {
        self.cards.iter().map(|c| c.kind).collect()
    }

    /// Render the whole sheet in `format`.
    ///
    /// For [`CardFormat::Json`] the serde serialization of the sheet is returned;
    /// the textual formats concatenate each card with a header.
    #[must_use]
    pub fn render(&self, format: CardFormat) -> String {
        match format {
            CardFormat::Json => self.to_json(),
            CardFormat::CliUnicode => {
                let mut out = format!(
                    "Galaxy-brain cards — claim {} — sheet {}\n\n",
                    self.claim_id, self.sheet_id
                );
                for (idx, card) in self.cards.iter().enumerate() {
                    if idx > 0 {
                        out.push('\n');
                    }
                    out.push_str(&render_card_cli(card));
                    out.push('\n');
                }
                out
            }
            CardFormat::Markdown => {
                let mut out = format!(
                    "## Galaxy-Brain Cards\n\n- **Claim:** `{}`\n- **Sheet ID:** `{}`\n\n",
                    self.claim_id, self.sheet_id
                );
                for card in &self.cards {
                    out.push_str(&render_card_markdown(card));
                    out.push('\n');
                }
                out
            }
        }
    }
}

#[derive(Serialize)]
struct CanonicalSheetId<'a> {
    schema_version: &'a str,
    claim_id: &'a str,
    card_ids: &'a [&'a str],
}

fn export_json_stats(
    sheet_id: &str,
    claim_id: &str,
    cards: &[GalaxyBrainCard],
) -> JsonStatsArtifact {
    #[derive(Serialize)]
    struct CardStat<'a> {
        card_id: &'a str,
        kind: CardKind,
        headline: &'a str,
        substitution_count: usize,
        artifact_count: usize,
    }
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        sheet_id: &'a str,
        claim_id: &'a str,
        card_count: usize,
        cards: Vec<CardStat<'a>>,
    }
    let payload = Export {
        schema_version: GALAXY_BRAIN_CARDS_SCHEMA_VERSION,
        sheet_id,
        claim_id,
        card_count: cards.len(),
        cards: cards
            .iter()
            .map(|c| CardStat {
                card_id: &c.card_id,
                kind: c.kind,
                headline: &c.headline,
                substitution_count: c.substitutions.len(),
                artifact_count: c.artifact_refs.len(),
            })
            .collect(),
    };
    let content = serde_json::to_string_pretty(&payload).unwrap_or_else(|error| error.to_string());
    JsonStatsArtifact {
        path: format!("{sheet_id}/galaxy_brain_cards.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// Optional source artifacts for a single card sheet.
///
/// Borrows each source so the engine stays presentation-only — nothing is
/// consumed or mutated. Supply only the families that apply to the decision.
#[derive(Debug, Default, Clone, Copy)]
pub struct CardSheetInputs<'a> {
    /// Posterior record source.
    pub posterior: Option<&'a PosteriorRecord>,
    /// Loss-decision log source.
    pub loss: Option<&'a DecisionLogEntry>,
    /// Conformal forecast source.
    pub conformal: Option<&'a ConformalForecast>,
    /// E-process result source.
    pub e_process: Option<&'a EProcessResult>,
    /// VOI selection source.
    pub voi: Option<&'a VoiCardInput>,
    /// Hazard / regime summary source.
    pub hazard: Option<&'a DriftMetricSummary>,
}

impl<'a> CardSheetInputs<'a> {
    /// An empty input set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a posterior record.
    #[must_use]
    pub fn with_posterior(mut self, record: &'a PosteriorRecord) -> Self {
        self.posterior = Some(record);
        self
    }

    /// Attach a loss-decision log entry.
    #[must_use]
    pub fn with_loss(mut self, entry: &'a DecisionLogEntry) -> Self {
        self.loss = Some(entry);
        self
    }

    /// Attach a conformal forecast.
    #[must_use]
    pub fn with_conformal(mut self, forecast: &'a ConformalForecast) -> Self {
        self.conformal = Some(forecast);
        self
    }

    /// Attach an e-process result.
    #[must_use]
    pub fn with_e_process(mut self, result: &'a EProcessResult) -> Self {
        self.e_process = Some(result);
        self
    }

    /// Attach a VOI selection input.
    #[must_use]
    pub fn with_voi(mut self, input: &'a VoiCardInput) -> Self {
        self.voi = Some(input);
        self
    }

    /// Attach a hazard / regime summary.
    #[must_use]
    pub fn with_hazard(mut self, summary: &'a DriftMetricSummary) -> Self {
        self.hazard = Some(summary);
        self
    }
}

/// Assembles galaxy-brain card sheets from deterministic-stack artifacts.
#[derive(Debug, Clone, Copy, Default)]
pub struct GalaxyBrainCardEngine;

impl GalaxyBrainCardEngine {
    /// Construct an engine.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Build a card sheet for `claim_id` from whichever sources are present.
    ///
    /// # Errors
    ///
    /// Returns [`GalaxyBrainCardError::InvalidInput`] if no source artifact was
    /// supplied (a promoted decision must carry at least one card — AC1).
    pub fn build_sheet(
        &self,
        claim_id: impl Into<String>,
        inputs: &CardSheetInputs<'_>,
    ) -> Result<GalaxyBrainCardSheet> {
        let claim_id = claim_id.into();
        let mut cards = Vec::new();
        if let Some(record) = inputs.posterior {
            cards.push(posterior_core_card(record));
        }
        if let Some(entry) = inputs.loss {
            cards.push(loss_decision_card(entry));
        }
        if let Some(forecast) = inputs.conformal {
            cards.push(conformal_interval_card(forecast, &claim_id));
        }
        if let Some(result) = inputs.e_process {
            cards.push(evalue_fdr_card(result, &claim_id));
        }
        if let Some(voi) = inputs.voi {
            cards.push(voi_selection_card(voi));
        }
        if let Some(hazard) = inputs.hazard {
            cards.push(hazard_regime_card(hazard, &claim_id));
        }
        if cards.is_empty() {
            return Err(GalaxyBrainCardError::InvalidInput {
                detail: "no card sources provided".to_string(),
            });
        }
        Ok(GalaxyBrainCardSheet::new(claim_id, cards))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Stable lowercase tag for a migration decision (mirrors the decision-loss
/// policy `action_str` so machine output is consistent across the stack).
fn decision_label(decision: MigrationDecision) -> &'static str {
    match decision {
        MigrationDecision::AutoApprove => "auto_approve",
        MigrationDecision::HumanReview => "human_review",
        MigrationDecision::Reject => "reject",
        MigrationDecision::HardReject => "hard_reject",
        MigrationDecision::Rollback => "rollback",
        MigrationDecision::ConservativeFallback => "conservative_fallback",
    }
}

/// Deterministic fixed-precision formatting with a stable non-finite guard.
fn fmt_f64(value: f64, decimals: usize) -> String {
    if value.is_nan() {
        "nan".to_string()
    } else if value.is_infinite() {
        if value > 0.0 { "inf" } else { "-inf" }.to_string()
    } else {
        format!("{value:.decimals$}")
    }
}

/// Four-decimal fixed format.
fn fmt4(value: f64) -> String {
    fmt_f64(value, 4)
}

/// Percentage with two decimals (e.g. `0.873` → `87.30%`).
fn fmt_pct(value: f64) -> String {
    if value.is_finite() {
        format!("{:.2}%", value * 100.0)
    } else {
        fmt_f64(value, 2)
    }
}

/// SHA-256 (hex) of a canonical JSON serialization.
fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

/// SHA-256 (hex) of raw bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

/// First 16 hex chars of a hash, for compact ids.
fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_loss_policy::{
        DecisionPolicyEngine, LossPolicyManifest, PolicyProfile, RiskTier, compile,
    };
    use crate::drift_monitor::{DriftMetricSummary, MetricDirection};
    use crate::guarantee_layer::{
        ConformalForecast, EProcessLedgerEntry, EProcessResult, GuaranteeEvaluationInput,
        GuaranteeLayerEngine, PacBayesInput,
    };
    use crate::posterior_core::{
        ChannelEvidence, EvidenceChannel, PosteriorCoreConfig, PosteriorEngine, PosteriorRecord,
    };
    use crate::semantic_contract::MigrationDecision;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    fn sample_posterior() -> PosteriorRecord {
        let engine = PosteriorEngine::new(PosteriorCoreConfig::default());
        engine.infer(
            "migration-alpha",
            &[
                ChannelEvidence::new(EvidenceChannel::Semantic, 1.4).artifact("art-sem-1"),
                ChannelEvidence::new(EvidenceChannel::Visual, 0.9).artifact("art-vis-1"),
                ChannelEvidence::new(EvidenceChannel::Performance, 0.5),
                ChannelEvidence::new(EvidenceChannel::Accessibility, 0.3),
                ChannelEvidence::new(EvidenceChannel::Determinism, 0.7),
            ],
        )
    }

    fn sample_decision_entry(record: &PosteriorRecord) -> DecisionLogEntry {
        let manifest = LossPolicyManifest::standard("policy-test", "1.0.0");
        let policy = compile(&manifest, &[]).expect("compile policy");
        let engine = DecisionPolicyEngine::new(policy);
        engine
            .decide_from_posterior(record, PolicyProfile::Balanced, RiskTier::Medium)
            .expect("decide")
    }

    fn sample_guarantee() -> (ConformalForecast, EProcessResult) {
        let engine = GuaranteeLayerEngine::default();
        let input = GuaranteeEvaluationInput::new(
            "migration-alpha",
            0.85,
            PacBayesInput::new(0.05, 0.4, 500),
        )
        .with_calibration((0..40).map(|i| f64::from(i) * 0.01))
        .with_validation((0..20).map(|i| (0.9, 0.85 + f64::from(i) * 0.001)))
        .with_observations(vec![0.1; 30]);
        let report = engine.evaluate(&input).expect("evaluate");
        (report.conformal, report.e_process)
    }

    fn vacuous_conformal() -> ConformalForecast {
        // A literal artifact (the serializable struct) with too-few calibration
        // points: a stand-in for a stored vacuous forecast.
        ConformalForecast {
            schema_version: "guarantee-layer-v1".to_string(),
            forecast_id: "guarantee-conformal-vacuous".to_string(),
            alpha: 0.1,
            nominal_coverage: 0.9,
            forecast_point: 0.8,
            conformal_quantile: f64::INFINITY,
            lower: f64::NEG_INFINITY,
            upper: f64::INFINITY,
            calibration_count: 2,
            quantile_rank: 0,
            empirical_coverage: None,
            validation_count: 0,
            vacuous: true,
            assumption_violations: vec!["insufficient calibration".to_string()],
            fallback_recommended: true,
        }
    }

    fn eproc(rejected: bool, peak: f64) -> EProcessResult {
        EProcessResult {
            schema_version: "guarantee-layer-v1".to_string(),
            process_id: format!(
                "guarantee-eprocess-{}",
                if rejected { "rej" } else { "ret" }
            ),
            alpha: 0.05,
            mu0: 0.1,
            threshold: 20.0,
            ledger: vec![EProcessLedgerEntry {
                index: 0,
                observation: 0.5,
                betting_fraction: 0.25,
                increment_log_wealth: 0.1,
                cumulative_log_wealth: 0.1,
                wealth: 1.1,
                rejected,
            }],
            final_log_wealth: peak.ln(),
            final_wealth: peak,
            peak_wealth: peak,
            rejected,
            stopping_index: if rejected { Some(0) } else { None },
            observation_count: 30,
            assumption_violations: Vec::new(),
            fallback_recommended: rejected,
        }
    }

    fn drift_summary(regressions: usize) -> DriftMetricSummary {
        DriftMetricSummary {
            metric_id: "frame_us_p99".to_string(),
            direction: MetricDirection::LowerIsBetter,
            observation_count: 120,
            baseline_mean: 1000.0,
            baseline_std: 50.0,
            final_value: if regressions > 0 { 1400.0 } else { 1010.0 },
            event_count: regressions,
            regression_count: regressions,
            improvement_count: 0,
            max_confidence: if regressions > 0 { 0.98 } else { 0.10 },
            expected_run_length_final: if regressions > 0 { 3.0 } else { 88.0 },
        }
    }

    fn voi_input(selected: bool) -> VoiCardInput {
        VoiCardInput::new(
            "migration-alpha",
            "stress-replay",
            0.40,
            10.0,
            2.0,
            selected,
        )
        .with_rationale(if selected {
            "net value positive; probe scheduled"
        } else {
            "net value below threshold; probe skipped"
        })
    }

    // ── Taxonomy ─────────────────────────────────────────────────────────────

    #[test]
    fn card_kind_order_indices_are_distinct_and_canonical() {
        let kinds = [
            CardKind::PosteriorCore,
            CardKind::LossDecision,
            CardKind::ConformalInterval,
            CardKind::EValueFdr,
            CardKind::VoiSelection,
            CardKind::HazardRegime,
        ];
        for (i, kind) in kinds.iter().enumerate() {
            assert_eq!(kind.order_index(), i);
            assert!(!kind.as_str().is_empty());
            assert!(!kind.title().is_empty());
        }
    }

    #[test]
    fn card_kind_serde_round_trips() {
        for kind in [
            CardKind::PosteriorCore,
            CardKind::LossDecision,
            CardKind::ConformalInterval,
            CardKind::EValueFdr,
            CardKind::VoiSelection,
            CardKind::HazardRegime,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: CardKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn substitution_new_and_with_note() {
        let plain = Substitution::new("x", "1.0000");
        assert!(plain.note.is_none());
        let noted = Substitution::with_note("y", "2.0000", "the y term");
        assert_eq!(noted.note.as_deref(), Some("the y term"));
    }

    // ── Posterior card ───────────────────────────────────────────────────────

    #[test]
    fn posterior_core_card_has_expected_shape() {
        let record = sample_posterior();
        let card = posterior_core_card(&record);
        assert_eq!(card.kind, CardKind::PosteriorCore);
        assert_eq!(card.claim_id, "migration-alpha");
        assert_eq!(card.schema_version, GALAXY_BRAIN_CARDS_SCHEMA_VERSION);
        assert!(card.equation.contains("logit"));
        assert!(!card.substitutions.is_empty());
        assert!(!card.intuition.is_empty());
        assert!(card.headline.contains(decision_label(record.decision)));
    }

    #[test]
    fn posterior_core_card_id_is_deterministic() {
        let record = sample_posterior();
        let a = posterior_core_card(&record);
        let b = posterior_core_card(&record);
        assert_eq!(a, b);
        assert_eq!(a.card_id, b.card_id);
        assert!(a.card_id.starts_with("galaxy-card-"));
    }

    #[test]
    fn posterior_core_card_id_changes_with_content() {
        let record = sample_posterior();
        let card = posterior_core_card(&record);

        let engine = PosteriorEngine::new(PosteriorCoreConfig::default());
        let other_record = engine.infer(
            "migration-beta",
            &[ChannelEvidence::new(EvidenceChannel::Semantic, -2.0)],
        );
        let other = posterior_core_card(&other_record);
        assert_ne!(card.card_id, other.card_id);
    }

    #[test]
    fn posterior_core_card_collects_artifact_ids() {
        let record = sample_posterior();
        let card = posterior_core_card(&record);
        assert!(card.artifact_refs.iter().any(|r| r.contains("art-sem-1")));
        assert!(card.artifact_refs.iter().any(|r| r.contains("art-vis-1")));
        assert!(
            card.artifact_refs
                .iter()
                .any(|r| r == "posterior-core://migration-alpha")
        );
    }

    #[test]
    fn posterior_core_card_mentions_counterfactual() {
        let record = sample_posterior();
        let card = posterior_core_card(&record);
        // Either a nearest-flip channel or the "no single-channel change" clause.
        assert!(card.intuition.contains("flip") || card.intuition.contains("flips"));
    }

    // ── Loss card ────────────────────────────────────────────────────────────

    #[test]
    fn loss_decision_card_from_entry() {
        let record = sample_posterior();
        let entry = sample_decision_entry(&record);
        let card = loss_decision_card(&entry);
        assert_eq!(card.kind, CardKind::LossDecision);
        assert_eq!(card.claim_id, entry.claim_id);
        assert!(card.equation.contains("argmin"));
        assert!(
            card.headline
                .contains(decision_label(entry.decision.selected))
        );
    }

    #[test]
    fn loss_decision_card_lists_all_actions() {
        let record = sample_posterior();
        let entry = sample_decision_entry(&record);
        let card = loss_decision_card(&entry);
        // Each action in the per-action ledger should have its own E[L| ] row.
        let el_rows = card
            .substitutions
            .iter()
            .filter(|s| s.symbol.starts_with("E[L|"))
            .count();
        assert_eq!(el_rows, entry.decision.per_action.len());
    }

    #[test]
    fn loss_decision_card_references_policy_artifacts() {
        let record = sample_posterior();
        let entry = sample_decision_entry(&record);
        let card = loss_decision_card(&entry);
        assert!(
            card.artifact_refs
                .iter()
                .any(|r| r.contains(&entry.decision_id))
        );
        assert!(card.artifact_refs.iter().any(|r| r.contains("policy://")));
        assert!(card.artifact_refs.iter().any(|r| r.contains("matrix://")));
    }

    // ── Conformal card ───────────────────────────────────────────────────────

    #[test]
    fn conformal_interval_card_brackets_interval() {
        let (forecast, _) = sample_guarantee();
        let card = conformal_interval_card(&forecast, "migration-alpha");
        assert_eq!(card.kind, CardKind::ConformalInterval);
        assert!(!forecast.vacuous);
        assert!(
            card.substitutions
                .iter()
                .any(|s| s.symbol == "interval" && s.value.starts_with('['))
        );
        assert!(card.headline.contains("coverage"));
    }

    #[test]
    fn conformal_interval_card_vacuous_is_flagged() {
        let forecast = vacuous_conformal();
        let card = conformal_interval_card(&forecast, "migration-alpha");
        assert!(card.headline.contains("vacuous"));
        assert!(card.intuition.contains("vacuous"));
    }

    #[test]
    fn conformal_interval_card_references_forecast_id() {
        let (forecast, _) = sample_guarantee();
        let card = conformal_interval_card(&forecast, "migration-alpha");
        assert!(
            card.artifact_refs
                .iter()
                .any(|r| r.contains(&forecast.forecast_id))
        );
    }

    // ── E-value card ─────────────────────────────────────────────────────────

    #[test]
    fn evalue_fdr_card_retain_vs_reject() {
        let retain = evalue_fdr_card(&eproc(false, 4.0), "migration-alpha");
        assert!(retain.headline.starts_with("retain"));
        assert!(retain.intuition.contains("retained"));

        let reject = evalue_fdr_card(&eproc(true, 25.0), "migration-alpha");
        assert!(reject.headline.contains("REJECT"));
        assert!(reject.intuition.contains("REJECTED"));
        assert_ne!(retain.card_id, reject.card_id);
    }

    #[test]
    fn evalue_fdr_card_threshold_is_inverse_alpha() {
        let result = eproc(false, 4.0);
        let card = evalue_fdr_card(&result, "migration-alpha");
        // threshold field == 1/alpha by construction (20.0 == 1/0.05).
        let expected = fmt4(1.0 / result.alpha);
        assert!(
            card.substitutions
                .iter()
                .any(|s| s.symbol == "1/α" && s.value == expected)
        );
    }

    #[test]
    fn evalue_fdr_card_references_process_id() {
        let result = eproc(true, 25.0);
        let card = evalue_fdr_card(&result, "migration-alpha");
        assert!(
            card.artifact_refs
                .iter()
                .any(|r| r.contains(&result.process_id))
        );
    }

    // ── VOI card ─────────────────────────────────────────────────────────────

    #[test]
    fn voi_selection_card_run_vs_skip() {
        let run = voi_selection_card(&voi_input(true));
        assert!(run.headline.starts_with("RUN"));
        let skip = voi_selection_card(&voi_input(false));
        assert!(skip.headline.starts_with("SKIP"));
    }

    #[test]
    fn voi_selection_card_net_value_matches_formula() {
        let input = voi_input(true);
        // net = voi*scale - cost = 0.40*10 - 2 = 2.0 (independently computed).
        let expected_net = 0.40_f64 * 10.0 - 2.0;
        assert!((input.net_value - expected_net).abs() < 1e-12);
        let card = voi_selection_card(&input);
        assert!(
            card.substitutions
                .iter()
                .any(|s| s.symbol == "net" && s.value == fmt4(expected_net))
        );
    }

    // ── Hazard card ──────────────────────────────────────────────────────────

    #[test]
    fn hazard_regime_card_stable_vs_shift() {
        let stable = hazard_regime_card(&drift_summary(0), "migration-alpha");
        assert!(stable.headline.contains("stable"));
        assert!(stable.intuition.contains("stable"));

        let shift = hazard_regime_card(&drift_summary(3), "migration-alpha");
        assert!(shift.headline.contains("REGIME SHIFT"));
        assert!(shift.intuition.contains("escalate"));
    }

    #[test]
    fn hazard_regime_card_surfaces_run_length() {
        let summary = drift_summary(0);
        let card = hazard_regime_card(&summary, "migration-alpha");
        assert!(
            card.substitutions
                .iter()
                .any(|s| s.symbol == "E[run length]"
                    && s.value == fmt4(summary.expected_run_length_final))
        );
    }

    // ── Renderers ────────────────────────────────────────────────────────────

    #[test]
    fn render_cli_contains_title_and_equation() {
        let card = posterior_core_card(&sample_posterior());
        let cli = render_card(&card, CardFormat::CliUnicode);
        assert!(cli.contains(&card.title));
        assert!(cli.contains("equation"));
        assert!(cli.contains("┏━"));
        assert!(cli.contains("┗"));
    }

    #[test]
    fn render_cli_is_deterministic() {
        let card = posterior_core_card(&sample_posterior());
        assert_eq!(
            render_card(&card, CardFormat::CliUnicode),
            render_card(&card, CardFormat::CliUnicode)
        );
    }

    #[test]
    fn render_markdown_contains_heading_and_table() {
        let card = posterior_core_card(&sample_posterior());
        let md = render_card(&card, CardFormat::Markdown);
        assert!(md.contains("### Posterior Core"));
        assert!(md.contains("| Symbol | Value | Note |"));
        assert!(md.contains("**Intuition**"));
    }

    #[test]
    fn render_markdown_is_deterministic() {
        let card = posterior_core_card(&sample_posterior());
        assert_eq!(
            render_card(&card, CardFormat::Markdown),
            render_card(&card, CardFormat::Markdown)
        );
    }

    #[test]
    fn render_json_round_trips() {
        let card = posterior_core_card(&sample_posterior());
        let json = render_card(&card, CardFormat::Json);
        let back: GalaxyBrainCard = serde_json::from_str(&json).unwrap();
        assert_eq!(card, back);
    }

    #[test]
    fn fmt_f64_handles_non_finite() {
        assert_eq!(fmt_f64(f64::NAN, 4), "nan");
        assert_eq!(fmt_f64(f64::INFINITY, 4), "inf");
        assert_eq!(fmt_f64(f64::NEG_INFINITY, 4), "-inf");
        assert_eq!(fmt4(1.23456), "1.2346");
        assert_eq!(fmt_pct(0.873), "87.30%");
    }

    #[test]
    fn decision_label_covers_all_variants() {
        let labels = [
            decision_label(MigrationDecision::AutoApprove),
            decision_label(MigrationDecision::HumanReview),
            decision_label(MigrationDecision::Reject),
            decision_label(MigrationDecision::HardReject),
            decision_label(MigrationDecision::Rollback),
            decision_label(MigrationDecision::ConservativeFallback),
        ];
        // All distinct and non-empty.
        for (i, a) in labels.iter().enumerate() {
            assert!(!a.is_empty());
            for b in &labels[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    // ── Sheet ────────────────────────────────────────────────────────────────

    #[test]
    fn sheet_sorts_cards_canonically() {
        let record = sample_posterior();
        let (forecast, eprocess) = sample_guarantee();
        // Supply out of canonical order; expect canonical order out.
        let cards = vec![
            evalue_fdr_card(&eprocess, "migration-alpha"),
            posterior_core_card(&record),
            conformal_interval_card(&forecast, "migration-alpha"),
        ];
        let sheet = GalaxyBrainCardSheet::new("migration-alpha", cards);
        assert_eq!(
            sheet.card_kinds(),
            vec![
                CardKind::PosteriorCore,
                CardKind::ConformalInterval,
                CardKind::EValueFdr,
            ]
        );
    }

    #[test]
    fn sheet_id_is_deterministic_and_order_independent() {
        let record = sample_posterior();
        let (forecast, eprocess) = sample_guarantee();
        let a = GalaxyBrainCardSheet::new(
            "migration-alpha",
            vec![
                posterior_core_card(&record),
                conformal_interval_card(&forecast, "migration-alpha"),
                evalue_fdr_card(&eprocess, "migration-alpha"),
            ],
        );
        let b = GalaxyBrainCardSheet::new(
            "migration-alpha",
            vec![
                evalue_fdr_card(&eprocess, "migration-alpha"),
                conformal_interval_card(&forecast, "migration-alpha"),
                posterior_core_card(&record),
            ],
        );
        assert_eq!(a.sheet_id, b.sheet_id);
        assert_eq!(a, b);
        assert!(a.sheet_id.starts_with("galaxy-sheet-"));
    }

    #[test]
    fn sheet_id_changes_with_cards() {
        let record = sample_posterior();
        let (forecast, _) = sample_guarantee();
        let one = GalaxyBrainCardSheet::new("migration-alpha", vec![posterior_core_card(&record)]);
        let two = GalaxyBrainCardSheet::new(
            "migration-alpha",
            vec![
                posterior_core_card(&record),
                conformal_interval_card(&forecast, "migration-alpha"),
            ],
        );
        assert_ne!(one.sheet_id, two.sheet_id);
    }

    #[test]
    fn sheet_exported_json_stats_sha_matches_content() {
        let record = sample_posterior();
        let sheet =
            GalaxyBrainCardSheet::new("migration-alpha", vec![posterior_core_card(&record)]);
        let recomputed = sha256_hex(sheet.exported_json_stats.content.as_bytes());
        assert_eq!(sheet.exported_json_stats.sha256, recomputed);
        assert!(
            sheet
                .exported_json_stats
                .path
                .ends_with("galaxy_brain_cards.json")
        );
    }

    #[test]
    fn sheet_to_json_round_trips() {
        let record = sample_posterior();
        let entry = sample_decision_entry(&record);
        let (forecast, eprocess) = sample_guarantee();
        let sheet = GalaxyBrainCardSheet::new(
            "migration-alpha",
            vec![
                posterior_core_card(&record),
                loss_decision_card(&entry),
                conformal_interval_card(&forecast, "migration-alpha"),
                evalue_fdr_card(&eprocess, "migration-alpha"),
            ],
        );
        let json = sheet.to_json();
        let back: GalaxyBrainCardSheet = serde_json::from_str(&json).unwrap();
        assert_eq!(sheet, back);
    }

    #[test]
    fn sheet_replay_command_mentions_sheet_id() {
        let record = sample_posterior();
        let sheet =
            GalaxyBrainCardSheet::new("migration-alpha", vec![posterior_core_card(&record)]);
        assert!(sheet.replay_command.contains(&sheet.sheet_id));
        assert!(sheet.replay_command.contains("galaxy_brain_cards"));
    }

    #[test]
    fn sheet_render_includes_all_cards() {
        let record = sample_posterior();
        let (forecast, eprocess) = sample_guarantee();
        let sheet = GalaxyBrainCardSheet::new(
            "migration-alpha",
            vec![
                posterior_core_card(&record),
                conformal_interval_card(&forecast, "migration-alpha"),
                evalue_fdr_card(&eprocess, "migration-alpha"),
            ],
        );
        let cli = sheet.render(CardFormat::CliUnicode);
        assert!(cli.contains("Posterior Core"));
        assert!(cli.contains("Conformal Interval"));
        assert!(cli.contains("E-Value / FDR State"));

        let md = sheet.render(CardFormat::Markdown);
        assert!(md.contains("## Galaxy-Brain Cards"));
        assert!(md.contains("### Posterior Core"));

        let json = sheet.render(CardFormat::Json);
        assert!(json.contains(&sheet.sheet_id));
    }

    // ── Engine ───────────────────────────────────────────────────────────────

    #[test]
    fn engine_builds_sheet_from_all_six_sources() {
        let record = sample_posterior();
        let entry = sample_decision_entry(&record);
        let (forecast, eprocess) = sample_guarantee();
        let voi = voi_input(true);
        let hazard = drift_summary(0);
        let inputs = CardSheetInputs::new()
            .with_posterior(&record)
            .with_loss(&entry)
            .with_conformal(&forecast)
            .with_e_process(&eprocess)
            .with_voi(&voi)
            .with_hazard(&hazard);
        let sheet = GalaxyBrainCardEngine::new()
            .build_sheet("migration-alpha", &inputs)
            .expect("build sheet");
        assert_eq!(
            sheet.card_kinds(),
            vec![
                CardKind::PosteriorCore,
                CardKind::LossDecision,
                CardKind::ConformalInterval,
                CardKind::EValueFdr,
                CardKind::VoiSelection,
                CardKind::HazardRegime,
            ]
        );
    }

    #[test]
    fn engine_builds_sheet_from_partial_sources() {
        let record = sample_posterior();
        let inputs = CardSheetInputs::new().with_posterior(&record);
        let sheet = GalaxyBrainCardEngine::new()
            .build_sheet("migration-alpha", &inputs)
            .expect("build sheet");
        assert_eq!(sheet.cards.len(), 1);
        assert_eq!(sheet.card_kinds(), vec![CardKind::PosteriorCore]);
    }

    #[test]
    fn engine_empty_sources_is_error() {
        let inputs = CardSheetInputs::new();
        let err = GalaxyBrainCardEngine::new()
            .build_sheet("migration-alpha", &inputs)
            .unwrap_err();
        assert!(matches!(err, GalaxyBrainCardError::InvalidInput { .. }));
    }

    // ── AC2: reproducible from stored artifacts ──────────────────────────────

    #[test]
    fn cards_reproducible_from_serialized_artifacts() {
        let record = sample_posterior();
        let original = posterior_core_card(&record);
        // Round-trip the SOURCE artifact through JSON (as it would be stored),
        // then rebuild the card: it must be byte-identical (diffable).
        let stored = serde_json::to_string(&record).unwrap();
        let restored: PosteriorRecord = serde_json::from_str(&stored).unwrap();
        let rebuilt = posterior_core_card(&restored);
        assert_eq!(original, rebuilt);
        assert_eq!(original.card_id, rebuilt.card_id);
        assert_eq!(
            render_card(&original, CardFormat::Markdown),
            render_card(&rebuilt, CardFormat::Markdown)
        );
    }

    // ── AC3: presentation-only ───────────────────────────────────────────────

    #[test]
    fn building_cards_does_not_alter_the_decision() {
        let record = sample_posterior();
        let decision_before = record.decision;
        let _card = posterior_core_card(&record);
        // The source decision is untouched, and the card merely reports it.
        assert_eq!(record.decision, decision_before);
        let card = posterior_core_card(&record);
        assert!(card.headline.contains(decision_label(record.decision)));
    }

    #[test]
    fn all_standard_cards_buildable() {
        let record = sample_posterior();
        let entry = sample_decision_entry(&record);
        let (forecast, eprocess) = sample_guarantee();
        let voi = voi_input(true);
        let hazard = drift_summary(1);
        let cards = vec![
            posterior_core_card(&record),
            loss_decision_card(&entry),
            conformal_interval_card(&forecast, "migration-alpha"),
            evalue_fdr_card(&eprocess, "migration-alpha"),
            voi_selection_card(&voi),
            hazard_regime_card(&hazard, "migration-alpha"),
        ];
        // Every card is non-empty and uniquely identified.
        let mut ids: Vec<&str> = cards.iter().map(|c| c.card_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), cards.len());
        for card in &cards {
            assert!(!card.equation.is_empty());
            assert!(!card.substitutions.is_empty());
            assert!(!card.intuition.is_empty());
            assert!(!card.artifact_refs.is_empty());
        }
    }
}
