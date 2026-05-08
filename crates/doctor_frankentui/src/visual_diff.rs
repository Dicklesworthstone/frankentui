//! Visual and terminal-output comparison for migration certification.
//!
//! This module compares normalized terminal frames produced by source and
//! translated runs. It supports strict byte-level checks for command output and
//! cursor-sensitive classes, plus tolerance-based checks for explicitly
//! perceptual classes from the semantic contract.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::semantic_contract::{
    ExpectedLossResult, SemanticEquivalenceContract, TransformationRiskLevel,
    VisualTolerancePolicy, load_builtin_confidence_model, load_builtin_semantic_contract,
};
use crate::trace::{InteractionTrace, TracePayload};

pub const VISUAL_DIFF_VALIDATOR_ID: &str = "visual_diff_validator";
const STRICT_VISUAL_CLAUSE_ID: &str = "VT-001";
const PERCEPTUAL_VISUAL_CLAUSE_ID: &str = "VT-002";
const DEFAULT_STRICT_CLASS: &str = "command_output";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisualDiffVerdict {
    Equivalent,
    WithinTolerance,
    Violation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisualDiffMode {
    StrictBytes,
    Tolerance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualDiffConfig {
    pub mode: VisualDiffMode,
    pub strict_classes: Vec<String>,
    pub perceptual_classes: Vec<String>,
    pub max_perceptual_delta: f32,
}

impl VisualDiffConfig {
    #[must_use]
    pub fn strict() -> Self {
        let contract = load_builtin_semantic_contract().expect("built-in semantic contract parses");
        Self::from_policy(
            VisualDiffMode::StrictBytes,
            &contract.visual_tolerance_policy,
        )
    }

    #[must_use]
    pub fn tolerance() -> Self {
        let contract = load_builtin_semantic_contract().expect("built-in semantic contract parses");
        Self::from_policy(VisualDiffMode::Tolerance, &contract.visual_tolerance_policy)
    }

    #[must_use]
    pub fn from_policy(mode: VisualDiffMode, policy: &VisualTolerancePolicy) -> Self {
        Self {
            mode,
            strict_classes: sorted_unique(policy.strict_classes.clone()),
            perceptual_classes: sorted_unique(policy.perceptual_classes.clone()),
            max_perceptual_delta: policy.max_perceptual_delta,
        }
    }

    #[must_use]
    pub fn is_perceptual_class(&self, class: &str) -> bool {
        self.perceptual_classes
            .iter()
            .any(|candidate| candidate == class)
    }

    #[must_use]
    pub fn is_strict_class(&self, class: &str) -> bool {
        self.strict_classes
            .iter()
            .any(|candidate| candidate == class)
            || !self.is_perceptual_class(class)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalStyle {
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub attrs: Vec<String>,
}

impl TerminalStyle {
    #[must_use]
    pub fn plain() -> Self {
        Self {
            fg: None,
            bg: None,
            attrs: Vec::new(),
        }
    }

    #[must_use]
    pub fn normalized(&self) -> Self {
        Self {
            fg: self.fg.as_ref().map(|value| normalize_color_label(value)),
            bg: self.bg.as_ref().map(|value| normalize_color_label(value)),
            attrs: sorted_unique(self.attrs.clone()),
        }
    }

    #[must_use]
    pub fn canonical_string(&self) -> String {
        let normalized = self.normalized();
        format!(
            "fg={};bg={};attrs={}",
            normalized.fg.as_deref().unwrap_or("none"),
            normalized.bg.as_deref().unwrap_or("none"),
            normalized.attrs.join("+")
        )
    }
}

impl Default for TerminalStyle {
    fn default() -> Self {
        Self::plain()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalCell {
    pub grapheme: String,
    pub style: TerminalStyle,
    pub semantic_class: Option<String>,
}

impl TerminalCell {
    #[must_use]
    pub fn new(grapheme: impl Into<String>) -> Self {
        Self {
            grapheme: grapheme.into(),
            style: TerminalStyle::plain(),
            semantic_class: None,
        }
    }

    #[must_use]
    pub fn blank() -> Self {
        Self::new(" ")
    }

    #[must_use]
    pub fn with_style(mut self, style: TerminalStyle) -> Self {
        self.style = style;
        self
    }

    #[must_use]
    pub fn with_semantic_class(mut self, semantic_class: impl Into<String>) -> Self {
        self.semantic_class = Some(semantic_class.into());
        self
    }

    #[must_use]
    pub fn normalized(&self) -> Self {
        Self {
            grapheme: self.grapheme.clone(),
            style: self.style.normalized(),
            semantic_class: self
                .semantic_class
                .as_ref()
                .map(|class| class.trim().to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CursorPosition {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalRegion {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl TerminalRegion {
    #[must_use]
    pub fn cell(x: u16, y: u16) -> Self {
        Self {
            x,
            y,
            width: 1,
            height: 1,
        }
    }

    #[must_use]
    pub fn frame(width: u16, height: u16) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalFrame {
    pub frame_index: u32,
    pub width: u16,
    pub height: u16,
    pub cells: Vec<TerminalCell>,
    pub cursor: Option<CursorPosition>,
    pub raw_bytes: Option<String>,
    pub content_hash: Option<String>,
    pub source_artifact: Option<String>,
}

impl TerminalFrame {
    #[must_use]
    pub fn new(frame_index: u32, width: u16, height: u16, cells: Vec<TerminalCell>) -> Self {
        Self {
            frame_index,
            width,
            height,
            cells,
            cursor: None,
            raw_bytes: None,
            content_hash: None,
            source_artifact: None,
        }
    }

    #[must_use]
    pub fn from_text(frame_index: u32, text: &str) -> Self {
        let normalized = normalize_terminal_bytes(text);
        let lines = split_preserving_empty_final_line(&normalized);
        let width = lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        let height = lines.len();
        let width = u16::try_from(width).unwrap_or(u16::MAX);
        let height = u16::try_from(height).unwrap_or(u16::MAX);
        let mut cells = Vec::with_capacity(usize::from(width) * usize::from(height));

        for line in lines.iter().take(usize::from(height)) {
            let mut chars = line.chars();
            for _x in 0..usize::from(width) {
                let grapheme = chars
                    .next()
                    .map_or_else(|| " ".to_string(), |ch| ch.to_string());
                cells.push(TerminalCell::new(grapheme));
            }
        }

        Self {
            frame_index,
            width,
            height,
            cells,
            cursor: None,
            raw_bytes: Some(normalized.clone()),
            content_hash: Some(sha256_hex(normalized.as_bytes())),
            source_artifact: None,
        }
    }

    #[must_use]
    pub fn digest_only(
        frame_index: u32,
        width: u16,
        height: u16,
        content_hash: impl Into<String>,
    ) -> Self {
        Self {
            frame_index,
            width,
            height,
            cells: Vec::new(),
            cursor: None,
            raw_bytes: None,
            content_hash: Some(content_hash.into()),
            source_artifact: None,
        }
    }

    #[must_use]
    pub fn with_cursor(mut self, cursor: CursorPosition) -> Self {
        self.cursor = Some(cursor);
        self
    }

    #[must_use]
    pub fn with_source_artifact(mut self, source_artifact: impl Into<String>) -> Self {
        self.source_artifact = Some(source_artifact.into());
        self
    }

    #[must_use]
    pub fn normalized_cells(&self) -> Vec<TerminalCell> {
        let expected_len = usize::from(self.width) * usize::from(self.height);
        self.cells
            .iter()
            .take(expected_len)
            .map(|cell| cell.normalized())
            .chain(
                std::iter::repeat_with(TerminalCell::blank)
                    .take(expected_len.saturating_sub(self.cells.len())),
            )
            .collect()
    }

    #[must_use]
    pub fn text_excerpt(&self) -> String {
        if let Some(raw) = &self.raw_bytes {
            return normalize_terminal_bytes(raw);
        }

        let cells = self.normalized_cells();
        let width = usize::from(self.width);
        if width == 0 {
            return String::new();
        }

        cells
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.grapheme.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut canonical = format!(
            "frame={};size={}x{}\n",
            self.frame_index, self.width, self.height
        );

        if let Some(raw) = &self.raw_bytes {
            canonical.push_str("raw:\n");
            canonical.push_str(&normalize_terminal_bytes(raw));
            canonical.push('\n');
        } else {
            canonical.push_str("cells:\n");
            let width = usize::from(self.width);
            for (index, cell) in self.normalized_cells().iter().enumerate() {
                canonical.push_str(&cell.grapheme);
                canonical.push('{');
                canonical.push_str(&cell.style.canonical_string());
                canonical.push('}');
                if width > 0 && (index + 1) % width == 0 {
                    canonical.push('\n');
                }
            }
        }

        if let Some(hash) = &self.content_hash {
            canonical.push_str("hash=");
            canonical.push_str(hash);
            canonical.push('\n');
        }

        if let Some(cursor) = self.cursor {
            canonical.push_str(&format!(
                "cursor={},{},{}\n",
                cursor.x, cursor.y, cursor.visible
            ));
        }

        canonical.into_bytes()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalOutputRun {
    pub run_id: String,
    pub trace_id: Option<String>,
    pub replay_command: Option<String>,
    pub frames: Vec<TerminalFrame>,
}

impl TerminalOutputRun {
    #[must_use]
    pub fn new(run_id: impl Into<String>, frames: Vec<TerminalFrame>) -> Self {
        Self {
            run_id: run_id.into(),
            trace_id: None,
            replay_command: None,
            frames: canonicalize_frames(frames),
        }
    }

    #[must_use]
    pub fn from_trace_render_captures(trace: &InteractionTrace) -> Self {
        let frames = trace
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                TracePayload::RenderCapture {
                    frame_index,
                    content_hash,
                } => Some(TerminalFrame::digest_only(
                    *frame_index,
                    trace.initial_viewport.width,
                    trace.initial_viewport.height,
                    content_hash.clone(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();

        Self {
            run_id: trace.run_id.clone(),
            trace_id: Some(trace.trace_id.clone()),
            replay_command: trace.metadata.get("replay_command").cloned(),
            frames: canonicalize_frames(frames),
        }
    }

    #[must_use]
    pub fn with_replay_command(mut self, replay_command: impl Into<String>) -> Self {
        self.replay_command = Some(replay_command.into());
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisualDifferenceKind {
    StrictByteMismatch,
    FrameHashMismatch,
    MissingFrame,
    UnexpectedFrame,
    DimensionMismatch,
    ContentMismatch,
    StyleMismatch,
    CursorMismatch,
    PerceptualDeltaExceeded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StyleDelta {
    pub x: u16,
    pub y: u16,
    pub property: String,
    pub source_value: Option<String>,
    pub translated_value: Option<String>,
    pub perceptual_delta: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualDifference {
    pub difference_kind: VisualDifferenceKind,
    pub frame_index: u32,
    pub region: TerminalRegion,
    pub semantic_class: String,
    pub source_value: Option<String>,
    pub translated_value: Option<String>,
    pub source_style: Option<TerminalStyle>,
    pub translated_style: Option<TerminalStyle>,
    pub style_deltas: Vec<StyleDelta>,
    pub perceptual_delta: Option<f32>,
    pub clause_ids: Vec<String>,
    pub risk_level: TransformationRiskLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualDiffArtifactFile {
    pub path: String,
    pub sha256: String,
    pub byte_len: usize,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualDiffArtifactBundle {
    pub bundle_id: String,
    pub replay_command: String,
    pub files: Vec<VisualDiffArtifactFile>,
    pub bundle_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualDiffReport {
    pub validator_id: String,
    pub contract_id: String,
    pub source_run_id: String,
    pub translated_run_id: String,
    pub mode: VisualDiffMode,
    pub verdict: VisualDiffVerdict,
    pub frames_compared: usize,
    pub cells_compared: usize,
    pub differences: Vec<VisualDifference>,
    pub covered_clause_ids: Vec<String>,
    pub violated_clause_ids: Vec<String>,
    pub risk_level: TransformationRiskLevel,
    pub risk_score: f64,
    pub expected_loss: ExpectedLossResult,
    pub artifact_bundle: Option<VisualDiffArtifactBundle>,
}

#[must_use]
pub fn compare_trace_render_captures(
    source_trace: &InteractionTrace,
    translated_trace: &InteractionTrace,
    config: &VisualDiffConfig,
) -> VisualDiffReport {
    let source_run = TerminalOutputRun::from_trace_render_captures(source_trace);
    let translated_run = TerminalOutputRun::from_trace_render_captures(translated_trace);
    compare_terminal_runs(&source_run, &translated_run, config)
}

#[must_use]
pub fn compare_terminal_runs(
    source_run: &TerminalOutputRun,
    translated_run: &TerminalOutputRun,
    config: &VisualDiffConfig,
) -> VisualDiffReport {
    let contract = load_builtin_semantic_contract().expect("built-in semantic contract parses");
    compare_terminal_runs_with_contract(source_run, translated_run, config, &contract)
}

#[must_use]
pub fn compare_terminal_runs_with_contract(
    source_run: &TerminalOutputRun,
    translated_run: &TerminalOutputRun,
    config: &VisualDiffConfig,
    contract: &SemanticEquivalenceContract,
) -> VisualDiffReport {
    let clause_risks = clause_risk_map(contract);
    let source_frames = frame_map(&source_run.frames);
    let translated_frames = frame_map(&translated_run.frames);
    let mut frame_indexes = source_frames.keys().copied().collect::<BTreeSet<_>>();
    frame_indexes.extend(translated_frames.keys().copied());

    let mut differences = Vec::new();
    let mut covered_clause_ids = BTreeSet::new();
    let mut violated_clause_ids = BTreeSet::new();
    let mut successes = 0_u32;
    let mut weighted_failures = 0_u32;
    let mut cells_compared = 0_usize;
    let mut tolerated_differences = 0_u32;

    for frame_index in frame_indexes {
        match (
            source_frames.get(&frame_index),
            translated_frames.get(&frame_index),
        ) {
            (Some(source), Some(translated)) => {
                let frame_result = compare_frame_pair(
                    source,
                    translated,
                    config,
                    &clause_risks,
                    &mut covered_clause_ids,
                );
                successes = successes.saturating_add(frame_result.successes);
                weighted_failures =
                    weighted_failures.saturating_add(frame_result.weighted_failures);
                cells_compared = cells_compared.saturating_add(frame_result.cells_compared);
                tolerated_differences =
                    tolerated_differences.saturating_add(frame_result.tolerated_differences);
                violated_clause_ids.extend(
                    frame_result
                        .differences
                        .iter()
                        .flat_map(|diff| diff.clause_ids.iter().cloned()),
                );
                differences.extend(frame_result.differences);
            }
            (Some(source), None) => {
                let diff = missing_or_unexpected_frame(
                    VisualDifferenceKind::MissingFrame,
                    source,
                    None,
                    &clause_risks,
                );
                weighted_failures =
                    weighted_failures.saturating_add(failure_weight(diff.risk_level));
                violated_clause_ids.extend(diff.clause_ids.iter().cloned());
                differences.push(diff);
            }
            (None, Some(translated)) => {
                let diff = missing_or_unexpected_frame(
                    VisualDifferenceKind::UnexpectedFrame,
                    translated,
                    Some("translated run emitted an extra frame".to_string()),
                    &clause_risks,
                );
                weighted_failures =
                    weighted_failures.saturating_add(failure_weight(diff.risk_level));
                violated_clause_ids.extend(diff.clause_ids.iter().cloned());
                differences.push(diff);
            }
            (None, None) => {}
        }
    }

    let verdict = if differences.is_empty() {
        if tolerated_differences > 0 {
            VisualDiffVerdict::WithinTolerance
        } else {
            VisualDiffVerdict::Equivalent
        }
    } else {
        VisualDiffVerdict::Violation
    };
    let risk_level = differences
        .iter()
        .map(|diff| diff.risk_level)
        .max()
        .unwrap_or(TransformationRiskLevel::Low);
    let risk_score = risk_score(successes, weighted_failures);
    let first_violated_clause = violated_clause_ids.iter().next().cloned();
    let expected_loss = expected_loss(successes, weighted_failures, first_violated_clause);
    let artifact_bundle = if differences.is_empty() {
        None
    } else {
        Some(build_artifact_bundle(
            source_run,
            translated_run,
            config.mode,
            &differences,
            &contract.contract_id,
        ))
    };

    VisualDiffReport {
        validator_id: VISUAL_DIFF_VALIDATOR_ID.to_string(),
        contract_id: contract.contract_id.clone(),
        source_run_id: source_run.run_id.clone(),
        translated_run_id: translated_run.run_id.clone(),
        mode: config.mode,
        verdict,
        frames_compared: source_frames.len().max(translated_frames.len()),
        cells_compared,
        differences,
        covered_clause_ids: covered_clause_ids.into_iter().collect(),
        violated_clause_ids: violated_clause_ids.into_iter().collect(),
        risk_level,
        risk_score,
        expected_loss,
        artifact_bundle,
    }
}

#[derive(Default)]
struct FrameCompareResult {
    differences: Vec<VisualDifference>,
    successes: u32,
    weighted_failures: u32,
    cells_compared: usize,
    tolerated_differences: u32,
}

fn compare_frame_pair(
    source: &TerminalFrame,
    translated: &TerminalFrame,
    config: &VisualDiffConfig,
    clause_risks: &BTreeMap<String, TransformationRiskLevel>,
    covered_clause_ids: &mut BTreeSet<String>,
) -> FrameCompareResult {
    let mut result = FrameCompareResult::default();

    if source.width != translated.width || source.height != translated.height {
        let diff = dimension_mismatch(source, translated, clause_risks);
        result.weighted_failures = result
            .weighted_failures
            .saturating_add(failure_weight(diff.risk_level));
        result.differences.push(diff);
        return result;
    }

    if let (Some(source_hash), Some(translated_hash)) =
        (&source.content_hash, &translated.content_hash)
        && source_hash != translated_hash
        && source.cells.is_empty()
        && translated.cells.is_empty()
    {
        let diff = frame_hash_mismatch(
            source,
            translated,
            source_hash,
            translated_hash,
            clause_risks,
        );
        result.weighted_failures = result
            .weighted_failures
            .saturating_add(failure_weight(diff.risk_level));
        result.differences.push(diff);
        return result;
    }

    let strict_bytes_differ = config.mode == VisualDiffMode::StrictBytes
        && source.canonical_bytes() != translated.canonical_bytes();
    let before_cell_diffs = result.differences.len();

    if source.cursor != translated.cursor {
        let diff = cursor_mismatch(source, translated, clause_risks);
        result.weighted_failures = result
            .weighted_failures
            .saturating_add(failure_weight(diff.risk_level));
        result.differences.push(diff);
    } else if source.cursor.is_some() {
        covered_clause_ids.insert(STRICT_VISUAL_CLAUSE_ID.to_string());
        result.successes = result.successes.saturating_add(1);
    }

    let source_cells = source.normalized_cells();
    let translated_cells = translated.normalized_cells();
    let width = usize::from(source.width);

    for (index, (source_cell, translated_cell)) in
        source_cells.iter().zip(translated_cells.iter()).enumerate()
    {
        let x = u16::try_from(index.checked_rem(width).unwrap_or(0)).unwrap_or(u16::MAX);
        let y = u16::try_from(index.checked_div(width).unwrap_or(0)).unwrap_or(u16::MAX);
        let class = cell_semantic_class(source_cell, translated_cell);
        let clause_ids = clause_ids_for_class(config, &class);
        result.cells_compared = result.cells_compared.saturating_add(1);
        let cell_context = CellDiffContext {
            frame: source,
            source_cell,
            translated_cell,
            x,
            y,
            semantic_class: &class,
            clause_ids: &clause_ids,
            clause_risks,
        };

        match compare_cells(source_cell, translated_cell, config, &class) {
            CellComparison::Equal => {
                result.successes = result.successes.saturating_add(1);
                covered_clause_ids.extend(clause_ids);
            }
            CellComparison::Tolerated => {
                result.successes = result.successes.saturating_add(1);
                result.tolerated_differences = result.tolerated_differences.saturating_add(1);
                covered_clause_ids.extend(clause_ids);
            }
            CellComparison::ContentMismatch => {
                let diff = content_mismatch(&cell_context);
                result.weighted_failures = result
                    .weighted_failures
                    .saturating_add(failure_weight(diff.risk_level));
                result.differences.push(diff);
            }
            CellComparison::StyleMismatch(style_deltas) => {
                let diff = style_mismatch(&cell_context, style_deltas);
                result.weighted_failures = result
                    .weighted_failures
                    .saturating_add(failure_weight(diff.risk_level));
                result.differences.push(diff);
            }
            CellComparison::PerceptualDeltaExceeded(delta) => {
                let diff = perceptual_delta_exceeded(&cell_context, delta);
                result.weighted_failures = result
                    .weighted_failures
                    .saturating_add(failure_weight(diff.risk_level));
                result.differences.push(diff);
            }
        }
    }

    if strict_bytes_differ && result.differences.len() == before_cell_diffs {
        let diff = strict_byte_mismatch(source, translated, clause_risks);
        result.weighted_failures = result
            .weighted_failures
            .saturating_add(failure_weight(diff.risk_level));
        result.differences.push(diff);
    }

    result
}

enum CellComparison {
    Equal,
    Tolerated,
    ContentMismatch,
    StyleMismatch(Vec<StyleDelta>),
    PerceptualDeltaExceeded(f32),
}

struct CellDiffContext<'a> {
    frame: &'a TerminalFrame,
    source_cell: &'a TerminalCell,
    translated_cell: &'a TerminalCell,
    x: u16,
    y: u16,
    semantic_class: &'a str,
    clause_ids: &'a [String],
    clause_risks: &'a BTreeMap<String, TransformationRiskLevel>,
}

fn compare_cells(
    source: &TerminalCell,
    translated: &TerminalCell,
    config: &VisualDiffConfig,
    semantic_class: &str,
) -> CellComparison {
    if source.grapheme == translated.grapheme && source.style == translated.style {
        return CellComparison::Equal;
    }

    if config.mode == VisualDiffMode::StrictBytes || config.is_strict_class(semantic_class) {
        if source.grapheme != translated.grapheme {
            return CellComparison::ContentMismatch;
        }
        return CellComparison::StyleMismatch(style_deltas(source, translated));
    }

    if semantic_class == "spacing"
        && source.grapheme.chars().all(char::is_whitespace)
        && translated.grapheme.chars().all(char::is_whitespace)
    {
        return CellComparison::Tolerated;
    }

    if semantic_class == "nonsemantic_animation" {
        return CellComparison::Tolerated;
    }

    if source.grapheme != translated.grapheme {
        return CellComparison::ContentMismatch;
    }

    if semantic_class == "decorative_color" {
        let delta = style_perceptual_delta(&source.style, &translated.style);
        if attrs_equal(&source.style, &translated.style) && delta <= config.max_perceptual_delta {
            return CellComparison::Tolerated;
        }
        return CellComparison::PerceptualDeltaExceeded(delta);
    }

    CellComparison::StyleMismatch(style_deltas(source, translated))
}

fn strict_byte_mismatch(
    source: &TerminalFrame,
    translated: &TerminalFrame,
    clause_risks: &BTreeMap<String, TransformationRiskLevel>,
) -> VisualDifference {
    let clause_ids = vec![STRICT_VISUAL_CLAUSE_ID.to_string()];
    let source_hash = sha256_hex(&source.canonical_bytes());
    let translated_hash = sha256_hex(&translated.canonical_bytes());
    VisualDifference {
        difference_kind: VisualDifferenceKind::StrictByteMismatch,
        frame_index: source.frame_index,
        region: TerminalRegion::frame(
            source.width.max(translated.width),
            source.height.max(translated.height),
        ),
        semantic_class: DEFAULT_STRICT_CLASS.to_string(),
        source_value: Some(source_hash),
        translated_value: Some(translated_hash),
        source_style: None,
        translated_style: None,
        style_deltas: Vec::new(),
        perceptual_delta: None,
        risk_level: risk_for_clauses(&clause_ids, clause_risks),
        clause_ids,
        message: format!(
            "strict canonical terminal bytes differ for frame {}",
            source.frame_index
        ),
    }
}

fn frame_hash_mismatch(
    source: &TerminalFrame,
    translated: &TerminalFrame,
    source_hash: &str,
    translated_hash: &str,
    clause_risks: &BTreeMap<String, TransformationRiskLevel>,
) -> VisualDifference {
    let clause_ids = vec![STRICT_VISUAL_CLAUSE_ID.to_string()];
    VisualDifference {
        difference_kind: VisualDifferenceKind::FrameHashMismatch,
        frame_index: source.frame_index,
        region: TerminalRegion::frame(
            source.width.max(translated.width),
            source.height.max(translated.height),
        ),
        semantic_class: DEFAULT_STRICT_CLASS.to_string(),
        source_value: Some(source_hash.to_string()),
        translated_value: Some(translated_hash.to_string()),
        source_style: None,
        translated_style: None,
        style_deltas: Vec::new(),
        perceptual_delta: None,
        risk_level: risk_for_clauses(&clause_ids, clause_risks),
        clause_ids,
        message: format!(
            "render capture hash differs for frame {}: source {source_hash} translated {translated_hash}",
            source.frame_index
        ),
    }
}

fn missing_or_unexpected_frame(
    kind: VisualDifferenceKind,
    frame: &TerminalFrame,
    message: Option<String>,
    clause_risks: &BTreeMap<String, TransformationRiskLevel>,
) -> VisualDifference {
    let clause_ids = vec![STRICT_VISUAL_CLAUSE_ID.to_string()];
    VisualDifference {
        difference_kind: kind,
        frame_index: frame.frame_index,
        region: TerminalRegion::frame(frame.width, frame.height),
        semantic_class: DEFAULT_STRICT_CLASS.to_string(),
        source_value: frame.content_hash.clone(),
        translated_value: None,
        source_style: None,
        translated_style: None,
        style_deltas: Vec::new(),
        perceptual_delta: None,
        risk_level: risk_for_clauses(&clause_ids, clause_risks),
        clause_ids,
        message: message.unwrap_or_else(|| {
            format!(
                "translated run is missing source frame {}",
                frame.frame_index
            )
        }),
    }
}

fn dimension_mismatch(
    source: &TerminalFrame,
    translated: &TerminalFrame,
    clause_risks: &BTreeMap<String, TransformationRiskLevel>,
) -> VisualDifference {
    let clause_ids = vec![STRICT_VISUAL_CLAUSE_ID.to_string()];
    VisualDifference {
        difference_kind: VisualDifferenceKind::DimensionMismatch,
        frame_index: source.frame_index,
        region: TerminalRegion::frame(
            source.width.max(translated.width),
            source.height.max(translated.height),
        ),
        semantic_class: DEFAULT_STRICT_CLASS.to_string(),
        source_value: Some(format!("{}x{}", source.width, source.height)),
        translated_value: Some(format!("{}x{}", translated.width, translated.height)),
        source_style: None,
        translated_style: None,
        style_deltas: Vec::new(),
        perceptual_delta: None,
        risk_level: risk_for_clauses(&clause_ids, clause_risks),
        clause_ids,
        message: format!(
            "frame {} dimensions differ: source {}x{} translated {}x{}",
            source.frame_index, source.width, source.height, translated.width, translated.height
        ),
    }
}

fn cursor_mismatch(
    source: &TerminalFrame,
    translated: &TerminalFrame,
    clause_risks: &BTreeMap<String, TransformationRiskLevel>,
) -> VisualDifference {
    let clause_ids = vec![STRICT_VISUAL_CLAUSE_ID.to_string()];
    VisualDifference {
        difference_kind: VisualDifferenceKind::CursorMismatch,
        frame_index: source.frame_index,
        region: TerminalRegion::frame(source.width, source.height),
        semantic_class: "cursor_position".to_string(),
        source_value: source.cursor.map(cursor_value),
        translated_value: translated.cursor.map(cursor_value),
        source_style: None,
        translated_style: None,
        style_deltas: Vec::new(),
        perceptual_delta: None,
        risk_level: risk_for_clauses(&clause_ids, clause_risks),
        clause_ids,
        message: format!("cursor semantics differ for frame {}", source.frame_index),
    }
}

fn content_mismatch(context: &CellDiffContext<'_>) -> VisualDifference {
    let clause_ids = context.clause_ids.to_vec();
    VisualDifference {
        difference_kind: VisualDifferenceKind::ContentMismatch,
        frame_index: context.frame.frame_index,
        region: TerminalRegion::cell(context.x, context.y),
        semantic_class: context.semantic_class.to_string(),
        source_value: Some(context.source_cell.grapheme.clone()),
        translated_value: Some(context.translated_cell.grapheme.clone()),
        source_style: Some(context.source_cell.style.clone()),
        translated_style: Some(context.translated_cell.style.clone()),
        style_deltas: Vec::new(),
        perceptual_delta: None,
        risk_level: risk_for_clauses(&clause_ids, context.clause_risks),
        clause_ids,
        message: format!(
            "frame {} content differs at cell ({},{})",
            context.frame.frame_index, context.x, context.y
        ),
    }
}

fn style_mismatch(
    context: &CellDiffContext<'_>,
    style_deltas: Vec<StyleDelta>,
) -> VisualDifference {
    let clause_ids = context.clause_ids.to_vec();
    let mut style_deltas = style_deltas;
    for delta in &mut style_deltas {
        delta.x = context.x;
        delta.y = context.y;
    }
    VisualDifference {
        difference_kind: VisualDifferenceKind::StyleMismatch,
        frame_index: context.frame.frame_index,
        region: TerminalRegion::cell(context.x, context.y),
        semantic_class: context.semantic_class.to_string(),
        source_value: Some(context.source_cell.grapheme.clone()),
        translated_value: Some(context.translated_cell.grapheme.clone()),
        source_style: Some(context.source_cell.style.clone()),
        translated_style: Some(context.translated_cell.style.clone()),
        style_deltas,
        perceptual_delta: None,
        risk_level: risk_for_clauses(&clause_ids, context.clause_risks),
        clause_ids,
        message: format!(
            "frame {} style differs at cell ({},{})",
            context.frame.frame_index, context.x, context.y
        ),
    }
}

fn perceptual_delta_exceeded(context: &CellDiffContext<'_>, delta: f32) -> VisualDifference {
    let clause_ids = context.clause_ids.to_vec();
    let mut style_deltas = style_deltas(context.source_cell, context.translated_cell);
    for style_delta in &mut style_deltas {
        style_delta.x = context.x;
        style_delta.y = context.y;
    }
    VisualDifference {
        difference_kind: VisualDifferenceKind::PerceptualDeltaExceeded,
        frame_index: context.frame.frame_index,
        region: TerminalRegion::cell(context.x, context.y),
        semantic_class: context.semantic_class.to_string(),
        source_value: Some(context.source_cell.grapheme.clone()),
        translated_value: Some(context.translated_cell.grapheme.clone()),
        source_style: Some(context.source_cell.style.clone()),
        translated_style: Some(context.translated_cell.style.clone()),
        style_deltas,
        perceptual_delta: Some(delta),
        risk_level: risk_for_clauses(&clause_ids, context.clause_risks),
        clause_ids,
        message: format!(
            "frame {} perceptual delta {delta:.6} exceeds tolerance at cell ({},{})",
            context.frame.frame_index, context.x, context.y
        ),
    }
}

fn style_deltas(source: &TerminalCell, translated: &TerminalCell) -> Vec<StyleDelta> {
    let source_style = source.style.normalized();
    let translated_style = translated.style.normalized();
    let mut deltas = Vec::new();

    if source_style.fg != translated_style.fg {
        deltas.push(StyleDelta {
            x: 0,
            y: 0,
            property: "fg".to_string(),
            source_value: source_style.fg.clone(),
            translated_value: translated_style.fg.clone(),
            perceptual_delta: color_delta(
                source_style.fg.as_deref(),
                translated_style.fg.as_deref(),
            ),
        });
    }
    if source_style.bg != translated_style.bg {
        deltas.push(StyleDelta {
            x: 0,
            y: 0,
            property: "bg".to_string(),
            source_value: source_style.bg.clone(),
            translated_value: translated_style.bg.clone(),
            perceptual_delta: color_delta(
                source_style.bg.as_deref(),
                translated_style.bg.as_deref(),
            ),
        });
    }
    if source_style.attrs != translated_style.attrs {
        deltas.push(StyleDelta {
            x: 0,
            y: 0,
            property: "attrs".to_string(),
            source_value: Some(source_style.attrs.join("+")),
            translated_value: Some(translated_style.attrs.join("+")),
            perceptual_delta: None,
        });
    }

    deltas
}

fn style_perceptual_delta(source: &TerminalStyle, translated: &TerminalStyle) -> f32 {
    let fg_delta = color_delta(source.fg.as_deref(), translated.fg.as_deref()).unwrap_or(0.0);
    let bg_delta = color_delta(source.bg.as_deref(), translated.bg.as_deref()).unwrap_or(0.0);
    fg_delta.max(bg_delta)
}

fn color_delta(source: Option<&str>, translated: Option<&str>) -> Option<f32> {
    match (source, translated) {
        (None, None) => Some(0.0),
        (Some(source), Some(translated))
            if normalize_color_label(source) == normalize_color_label(translated) =>
        {
            Some(0.0)
        }
        (Some(source), Some(translated)) => {
            let source_rgb = parse_hex_rgb(source)?;
            let translated_rgb = parse_hex_rgb(translated)?;
            let dr = f32::from(source_rgb.0.abs_diff(translated_rgb.0)) / 255.0;
            let dg = f32::from(source_rgb.1.abs_diff(translated_rgb.1)) / 255.0;
            let db = f32::from(source_rgb.2.abs_diff(translated_rgb.2)) / 255.0;
            Some(((dr * dr) + (dg * dg) + (db * db)).sqrt() / 3.0_f32.sqrt())
        }
        _ => Some(1.0),
    }
}

fn parse_hex_rgb(raw: &str) -> Option<(u8, u8, u8)> {
    let trimmed = raw.trim().trim_start_matches('#');
    if trimmed.len() != 6 {
        return None;
    }
    let red = u8::from_str_radix(&trimmed[0..2], 16).ok()?;
    let green = u8::from_str_radix(&trimmed[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&trimmed[4..6], 16).ok()?;
    Some((red, green, blue))
}

fn attrs_equal(source: &TerminalStyle, translated: &TerminalStyle) -> bool {
    sorted_unique(source.attrs.clone()) == sorted_unique(translated.attrs.clone())
}

fn cell_semantic_class(source: &TerminalCell, translated: &TerminalCell) -> String {
    source
        .semantic_class
        .as_deref()
        .or(translated.semantic_class.as_deref())
        .unwrap_or(DEFAULT_STRICT_CLASS)
        .to_string()
}

fn clause_ids_for_class(config: &VisualDiffConfig, semantic_class: &str) -> Vec<String> {
    if config.mode == VisualDiffMode::Tolerance && config.is_perceptual_class(semantic_class) {
        vec![PERCEPTUAL_VISUAL_CLAUSE_ID.to_string()]
    } else {
        vec![STRICT_VISUAL_CLAUSE_ID.to_string()]
    }
}

fn build_artifact_bundle(
    source_run: &TerminalOutputRun,
    translated_run: &TerminalOutputRun,
    mode: VisualDiffMode,
    differences: &[VisualDifference],
    contract_id: &str,
) -> VisualDiffArtifactBundle {
    let replay_command = replay_command(source_run, translated_run);
    let diff_jsonl = differences
        .iter()
        .map(|diff| serde_json::to_string(diff).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let source_excerpt = source_run
        .frames
        .first()
        .map(TerminalFrame::text_excerpt)
        .unwrap_or_default();
    let translated_excerpt = translated_run
        .frames
        .first()
        .map(TerminalFrame::text_excerpt)
        .unwrap_or_default();
    let summary = serde_json::json!({
        "validator_id": VISUAL_DIFF_VALIDATOR_ID,
        "contract_id": contract_id,
        "source_run_id": source_run.run_id,
        "translated_run_id": translated_run.run_id,
        "mode": mode,
        "difference_count": differences.len(),
        "first_difference": differences.first(),
    })
    .to_string();

    let replay_script = format!("#!/usr/bin/env bash\nset -euo pipefail\n{replay_command}\n");
    let files = vec![
        artifact_file("replay.sh", replay_script),
        artifact_file("diffs.jsonl", diff_jsonl),
        artifact_file("source_excerpt.txt", source_excerpt),
        artifact_file("translated_excerpt.txt", translated_excerpt),
        artifact_file("report_summary.json", summary),
    ];
    let bundle_sha256 = bundle_hash(&files);
    let bundle_id = format!("visual-diff-{}", &bundle_sha256[..16]);

    VisualDiffArtifactBundle {
        bundle_id,
        replay_command,
        files,
        bundle_sha256,
    }
}

fn artifact_file(path: impl Into<String>, content: String) -> VisualDiffArtifactFile {
    let path = path.into();
    VisualDiffArtifactFile {
        path,
        sha256: sha256_hex(content.as_bytes()),
        byte_len: content.len(),
        content,
    }
}

fn bundle_hash(files: &[VisualDiffArtifactFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(file.path.as_bytes());
        hasher.update([0]);
        hasher.update(file.sha256.as_bytes());
        hasher.update([0]);
    }
    hex_encode(&hasher.finalize())
}

fn replay_command(source_run: &TerminalOutputRun, translated_run: &TerminalOutputRun) -> String {
    match (&source_run.replay_command, &translated_run.replay_command) {
        (Some(source), Some(translated)) => format!("{source} && {translated}"),
        (Some(source), None) => source.clone(),
        (None, Some(translated)) => translated.clone(),
        (None, None) => match (&source_run.trace_id, &translated_run.trace_id) {
            (Some(source_trace), Some(translated_trace)) => format!(
                "doctor_frankentui replay --trace-id {source_trace} && doctor_frankentui replay --trace-id {translated_trace}"
            ),
            _ => format!(
                "doctor_frankentui visual-diff --source-run {} --translated-run {}",
                source_run.run_id, translated_run.run_id
            ),
        },
    }
}

fn frame_map(frames: &[TerminalFrame]) -> BTreeMap<u32, TerminalFrame> {
    frames
        .iter()
        .cloned()
        .map(|frame| (frame.frame_index, frame))
        .collect()
}

fn canonicalize_frames(frames: Vec<TerminalFrame>) -> Vec<TerminalFrame> {
    let mut frame_map = frame_map(&frames);
    frame_map.values_mut().for_each(|frame| {
        if !frame.cells.is_empty() {
            frame.cells = frame.normalized_cells();
        }
    });
    frame_map.into_values().collect()
}

fn normalize_terminal_bytes(raw: &str) -> String {
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

fn split_preserving_empty_final_line(normalized: &str) -> Vec<&str> {
    if normalized.is_empty() {
        vec![""]
    } else {
        normalized.split('\n').collect()
    }
}

fn normalize_color_label(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

fn cursor_value(cursor: CursorPosition) -> String {
    format!("x={};y={};visible={}", cursor.x, cursor.y, cursor.visible)
}

fn sorted_unique(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn clause_risk_map(
    contract: &SemanticEquivalenceContract,
) -> BTreeMap<String, TransformationRiskLevel> {
    contract
        .clauses
        .iter()
        .map(|clause| (clause.clause_id.clone(), severity_to_risk(&clause.severity)))
        .collect()
}

fn risk_for_clauses(
    clause_ids: &[String],
    clause_risks: &BTreeMap<String, TransformationRiskLevel>,
) -> TransformationRiskLevel {
    clause_ids
        .iter()
        .map(|clause_id| {
            clause_risks
                .get(clause_id)
                .copied()
                .unwrap_or(TransformationRiskLevel::Critical)
        })
        .max()
        .unwrap_or(TransformationRiskLevel::Medium)
}

fn severity_to_risk(severity: &str) -> TransformationRiskLevel {
    match severity {
        "critical" => TransformationRiskLevel::Critical,
        "high" => TransformationRiskLevel::High,
        "medium" => TransformationRiskLevel::Medium,
        _ => TransformationRiskLevel::Low,
    }
}

fn failure_weight(risk: TransformationRiskLevel) -> u32 {
    match risk {
        TransformationRiskLevel::Low => 1,
        TransformationRiskLevel::Medium => 2,
        TransformationRiskLevel::High => 4,
        TransformationRiskLevel::Critical => 8,
    }
}

fn risk_score(successes: u32, weighted_failures: u32) -> f64 {
    if weighted_failures == 0 {
        return 0.0;
    }
    let total = successes.saturating_add(weighted_failures);
    f64::from(weighted_failures) / f64::from(total)
}

fn expected_loss(
    successes: u32,
    weighted_failures: u32,
    claim_id: Option<String>,
) -> ExpectedLossResult {
    let confidence_model =
        load_builtin_confidence_model().expect("built-in confidence model parses");
    let posterior = confidence_model.compute_posterior(successes, weighted_failures);
    confidence_model.expected_loss_decision(
        &posterior,
        claim_id,
        Some(VISUAL_DIFF_VALIDATOR_ID.to_string()),
    )
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
