#![forbid(unsafe_code)]

//! TableTheme core types and preset definitions.

use crate::color::{Ansi16, Color, ColorProfile};
use crate::{Style, StyleFlags};
use ftui_render::cell::PackedRgba;
use std::hash::{Hash, Hasher};

#[inline]
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let a = a as f32;
    let b = b as f32;
    (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
}

#[inline]
fn lerp_color(a: PackedRgba, b: PackedRgba, t: f32) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    PackedRgba::rgba(
        lerp_u8(a.r(), b.r(), t),
        lerp_u8(a.g(), b.g(), t),
        lerp_u8(a.b(), b.b(), t),
        lerp_u8(a.a(), b.a(), t),
    )
}

/// Built-in TableTheme preset identifiers.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TablePresetId {
    /// Luminous header with cool zebra rows.
    Aurora,
    /// High-contrast graphite palette for dense data.
    Graphite,
    /// Neon accent palette on dark base.
    Neon,
    /// Muted slate tones with soft dividers.
    Slate,
    /// Warm solar tones with bright header.
    Solar,
    /// Orchard-inspired greens and warm highlights.
    Orchard,
    /// Paper-like light theme with crisp borders.
    Paper,
    /// Midnight palette for dark terminals.
    Midnight,
    /// Classic terminal styling (ANSI-friendly).
    TerminalClassic,
}

/// Semantic table sections.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TableSection {
    /// Header row section.
    Header,
    /// Body rows section.
    Body,
    /// Footer rows section.
    Footer,
}

/// Target selection for a table effect.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TableEffectTarget {
    /// Apply to an entire section (header/body/footer).
    Section(TableSection),
    /// Apply to a specific row index.
    Row(usize),
    /// Apply to a row range (inclusive bounds).
    RowRange { start: usize, end: usize },
    /// Apply to a specific column index.
    Column(usize),
    /// Apply to a column range (inclusive bounds).
    ColumnRange { start: usize, end: usize },
    /// Body rows only.
    AllRows,
    /// Header + body.
    AllCells,
}

/// Scope used to resolve table effects without per-cell work.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TableEffectScope {
    /// Section being rendered.
    pub section: TableSection,
    /// Optional row index within the section.
    pub row: Option<usize>,
    /// Optional column index within the section.
    pub column: Option<usize>,
}

impl TableEffectScope {
    /// Scope for a whole section (no row/column specificity).
    #[must_use]
    pub const fn section(section: TableSection) -> Self {
        Self {
            section,
            row: None,
            column: None,
        }
    }

    /// Scope for a specific row within a section.
    #[must_use]
    pub const fn row(section: TableSection, row: usize) -> Self {
        Self {
            section,
            row: Some(row),
            column: None,
        }
    }

    /// Scope for a specific column within a section.
    #[must_use]
    pub const fn column(section: TableSection, column: usize) -> Self {
        Self {
            section,
            row: None,
            column: Some(column),
        }
    }
}

impl TableEffectTarget {
    /// Determine whether this target applies to the given scope.
    #[must_use]
    pub fn matches_scope(&self, scope: TableEffectScope) -> bool {
        match *self {
            TableEffectTarget::Section(section) => scope.section == section,
            TableEffectTarget::Row(row) => scope.row == Some(row),
            TableEffectTarget::RowRange { start, end } => {
                scope.row.is_some_and(|row| row >= start && row <= end)
            }
            TableEffectTarget::Column(column) => scope.column == Some(column),
            TableEffectTarget::ColumnRange { start, end } => scope
                .column
                .is_some_and(|column| column >= start && column <= end),
            TableEffectTarget::AllRows => {
                scope.section == TableSection::Body && scope.row.is_some()
            }
            TableEffectTarget::AllCells => {
                matches!(scope.section, TableSection::Header | TableSection::Body)
                    && (scope.row.is_some() || scope.column.is_some())
            }
        }
    }
}

/// A multi-stop gradient for table effects.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    stops: Vec<(f32, PackedRgba)>,
}

impl Gradient {
    /// Create a new gradient with stops in the range [0, 1].
    pub fn new(stops: Vec<(f32, PackedRgba)>) -> Self {
        let mut stops = stops;
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        Self { stops }
    }

    /// Access the gradient stops (sorted by position).
    #[inline]
    #[must_use]
    pub fn stops(&self) -> &[(f32, PackedRgba)] {
        &self.stops
    }

    /// Sample the gradient at a normalized position in [0, 1].
    #[must_use]
    pub fn sample(&self, t: f32) -> PackedRgba {
        let t = t.clamp(0.0, 1.0);
        let Some(first) = self.stops.first() else {
            return PackedRgba::TRANSPARENT;
        };
        if t <= first.0 {
            return first.1;
        }
        let Some(last) = self.stops.last() else {
            return first.1;
        };
        if t >= last.0 {
            return last.1;
        }

        for window in self.stops.windows(2) {
            let (p0, c0) = window[0];
            let (p1, c1) = window[1];
            if t <= p1 {
                let denom = p1 - p0;
                if denom <= f32::EPSILON {
                    return c1;
                }
                let local = (t - p0) / denom;
                return lerp_color(c0, c1, local);
            }
        }

        last.1
    }
}

/// Effect definitions applied to table styles.
#[derive(Clone, Debug)]
pub enum TableEffect {
    /// Pulse between two foreground/background colors.
    Pulse {
        fg_a: PackedRgba,
        fg_b: PackedRgba,
        bg_a: PackedRgba,
        bg_b: PackedRgba,
        speed: f32,
        phase_offset: f32,
    },
    /// Breathing glow that brightens/dims around a base color.
    BreathingGlow {
        fg: PackedRgba,
        bg: PackedRgba,
        intensity: f32,
        speed: f32,
        phase_offset: f32,
        asymmetry: f32,
    },
    /// Sweep a multi-stop gradient across the target.
    GradientSweep {
        gradient: Gradient,
        speed: f32,
        phase_offset: f32,
    },
}

/// How effect colors blend with the base style.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default)]
pub enum BlendMode {
    #[default]
    Replace,
    Additive,
    Multiply,
    Screen,
}

/// Mask for which style channels effects are allowed to override.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct StyleMask {
    pub fg: bool,
    pub bg: bool,
    pub attrs: bool,
}

impl StyleMask {
    /// Mask that allows only foreground and background changes.
    #[must_use]
    pub const fn fg_bg() -> Self {
        Self {
            fg: true,
            bg: true,
            attrs: false,
        }
    }

    /// Mask that allows all channels.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            fg: true,
            bg: true,
            attrs: true,
        }
    }

    /// Mask that blocks all channels.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            fg: false,
            bg: false,
            attrs: false,
        }
    }
}

impl Default for StyleMask {
    fn default() -> Self {
        Self::fg_bg()
    }
}

/// A single effect rule applied to a table target.
#[derive(Clone, Debug)]
pub struct TableEffectRule {
    /// Target selection (section/row/column/range).
    pub target: TableEffectTarget,
    /// Effect definition to apply.
    pub effect: TableEffect,
    /// Rule priority (higher applies later).
    pub priority: u8,
    /// Blend mode for effect vs base style.
    pub blend_mode: BlendMode,
    /// Mask of style channels the effect can override.
    pub style_mask: StyleMask,
}

impl TableEffectRule {
    /// Create a new effect rule with default blending and masking.
    #[must_use]
    pub fn new(target: TableEffectTarget, effect: TableEffect) -> Self {
        Self {
            target,
            effect,
            priority: 0,
            blend_mode: BlendMode::default(),
            style_mask: StyleMask::default(),
        }
    }

    /// Set rule priority (higher applies later).
    #[must_use]
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Set blend mode.
    #[must_use]
    pub fn blend_mode(mut self, blend_mode: BlendMode) -> Self {
        self.blend_mode = blend_mode;
        self
    }

    /// Set style mask.
    #[must_use]
    pub fn style_mask(mut self, style_mask: StyleMask) -> Self {
        self.style_mask = style_mask;
        self
    }
}

/// Resolve table effects for a given scope and phase.
///
/// The resolver is designed to run once per row/column/section (not per cell).
pub struct TableEffectResolver<'a> {
    theme: &'a TableTheme,
}

impl<'a> TableEffectResolver<'a> {
    /// Create a resolver for a given theme.
    #[must_use]
    pub const fn new(theme: &'a TableTheme) -> Self {
        Self { theme }
    }

    /// Resolve effects for a specific scope at the provided phase.
    #[must_use]
    pub fn resolve(&self, base: Style, scope: TableEffectScope, phase: f32) -> Style {
        resolve_effects_for_scope(self.theme, base, scope, phase)
    }
}

/// Shared theme for all table render paths.
///
/// This controls base styles (border/header/rows), spacing, and optional
/// effect rules that can animate or accent specific rows/columns.
///
/// Determinism guidance: always supply an explicit phase from the caller
/// (e.g., tick count or frame index). Avoid implicit clocks inside themes.
///
/// # Examples
///
/// Apply a preset and add an animated row highlight:
///
/// ```rust,no_run
/// use ftui_style::{
///     TableEffect, TableEffectRule, TableEffectScope, TableEffectTarget, TableSection, TableTheme,
///     Style,
/// };
/// use ftui_render::cell::PackedRgba;
///
/// let theme = TableTheme::aurora().with_effect(TableEffectRule::new(
///     TableEffectTarget::Row(0),
///     TableEffect::Pulse {
///         fg_a: PackedRgba::rgb(240, 245, 255),
///         fg_b: PackedRgba::rgb(255, 255, 255),
///         bg_a: PackedRgba::rgb(28, 36, 54),
///         bg_b: PackedRgba::rgb(60, 90, 140),
///         speed: 1.0,
///         phase_offset: 0.0,
///     },
/// ));
///
/// let resolver = theme.effect_resolver();
/// let phase = 0.25; // caller-supplied (e.g., tick * 0.02)
/// let scope = TableEffectScope::row(TableSection::Body, 0);
/// let _animated = resolver.resolve(theme.row, scope, phase);
/// ```
///
/// Override a preset for custom header + zebra rows:
///
/// ```rust,no_run
/// use ftui_style::{TableTheme, Style};
/// use ftui_render::cell::PackedRgba;
///
/// let theme = TableTheme::terminal_classic()
///     .with_header(Style::new().fg(PackedRgba::rgb(240, 240, 240)).bold())
///     .with_row_alt(Style::new().bg(PackedRgba::rgb(20, 20, 20)))
///     .with_divider(Style::new().fg(PackedRgba::rgb(60, 60, 60)))
///     .with_padding(1)
///     .with_column_gap(2);
/// ```
#[derive(Clone, Debug)]
pub struct TableTheme {
    /// Border style (table outline).
    pub border: Style,
    /// Header row style.
    pub header: Style,
    /// Base body row style.
    pub row: Style,
    /// Alternate row style for zebra striping.
    pub row_alt: Style,
    /// Selected row style.
    pub row_selected: Style,
    /// Hover row style.
    pub row_hover: Style,
    /// Divider/column separator style.
    pub divider: Style,
    /// Cell padding inside each column (in cells).
    pub padding: u8,
    /// Gap between columns (in cells).
    pub column_gap: u8,
    /// Row height in terminal lines.
    pub row_height: u8,
    /// Effect rules resolved per row/column/section.
    pub effects: Vec<TableEffectRule>,
    /// Optional preset identifier for diagnostics.
    pub preset_id: Option<TablePresetId>,
}

/// Diagnostics payload for TableTheme instrumentation.
#[derive(Clone, Debug)]
pub struct TableThemeDiagnostics {
    pub preset_id: Option<TablePresetId>,
    pub style_hash: u64,
    pub effects_hash: u64,
    pub effect_count: usize,
    pub padding: u8,
    pub column_gap: u8,
    pub row_height: u8,
}

/// Serializable spec for exporting/importing table themes.
///
/// This is a pure data representation (no rendering logic) that preserves
/// the full TableTheme surface, including effects.
///
/// Forward-compatibility notes:
/// - Unknown fields are rejected when `serde` is enabled (strict schema).
/// - New fields should be optional with safe defaults to keep older exports valid.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq)]
pub struct TableThemeSpec {
    /// Schema version for forward-compatible parsing.
    pub version: u8,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// Original preset identifier, if derived from a preset.
    pub preset_id: Option<TablePresetId>,
    /// Layout parameters.
    pub padding: u8,
    pub column_gap: u8,
    pub row_height: u8,
    /// Style buckets.
    pub styles: TableThemeStyleSpec,
    /// Effects applied to the theme.
    pub effects: Vec<TableEffectRuleSpec>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq)]
pub struct TableThemeStyleSpec {
    pub border: StyleSpec,
    pub header: StyleSpec,
    pub row: StyleSpec,
    pub row_alt: StyleSpec,
    pub row_selected: StyleSpec,
    pub row_hover: StyleSpec,
    pub divider: StyleSpec,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq)]
pub struct StyleSpec {
    pub fg: Option<RgbaSpec>,
    pub bg: Option<RgbaSpec>,
    pub underline: Option<RgbaSpec>,
    pub attrs: Vec<StyleAttr>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StyleAttr {
    Bold,
    Dim,
    Italic,
    Underline,
    Blink,
    Reverse,
    Hidden,
    Strikethrough,
    DoubleUnderline,
    CurlyUnderline,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RgbaSpec {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl RgbaSpec {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

impl From<PackedRgba> for RgbaSpec {
    fn from(color: PackedRgba) -> Self {
        Self::new(color.r(), color.g(), color.b(), color.a())
    }
}

impl From<RgbaSpec> for PackedRgba {
    fn from(color: RgbaSpec) -> Self {
        PackedRgba::rgba(color.r, color.g, color.b, color.a)
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq)]
pub struct GradientSpec {
    pub stops: Vec<GradientStopSpec>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStopSpec {
    pub pos: f32,
    pub color: RgbaSpec,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq)]
pub enum TableEffectSpec {
    Pulse {
        fg_a: RgbaSpec,
        fg_b: RgbaSpec,
        bg_a: RgbaSpec,
        bg_b: RgbaSpec,
        speed: f32,
        phase_offset: f32,
    },
    BreathingGlow {
        fg: RgbaSpec,
        bg: RgbaSpec,
        intensity: f32,
        speed: f32,
        phase_offset: f32,
        asymmetry: f32,
    },
    GradientSweep {
        gradient: GradientSpec,
        speed: f32,
        phase_offset: f32,
    },
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq)]
pub struct TableEffectRuleSpec {
    pub target: TableEffectTarget,
    pub effect: TableEffectSpec,
    pub priority: u8,
    pub blend_mode: BlendMode,
    pub style_mask: StyleMask,
}

/// Schema version for TableThemeSpec.
pub const TABLE_THEME_SPEC_VERSION: u8 = 1;
const TABLE_THEME_SPEC_MAX_NAME_LEN: usize = 64;
const TABLE_THEME_SPEC_MAX_EFFECTS: usize = 64;
const TABLE_THEME_SPEC_MAX_STYLE_ATTRS: usize = 16;
const TABLE_THEME_SPEC_MAX_GRADIENT_STOPS: usize = 16;
const TABLE_THEME_SPEC_MIN_GRADIENT_STOPS: usize = 1;
const TABLE_THEME_SPEC_MAX_PADDING: u8 = 8;
const TABLE_THEME_SPEC_MAX_COLUMN_GAP: u8 = 8;
const TABLE_THEME_SPEC_MIN_ROW_HEIGHT: u8 = 1;
const TABLE_THEME_SPEC_MAX_ROW_HEIGHT: u8 = 8;
const TABLE_THEME_SPEC_MAX_SPEED: f32 = 10.0;
const TABLE_THEME_SPEC_MAX_PHASE: f32 = 1.0;
const TABLE_THEME_SPEC_MAX_INTENSITY: f32 = 1.0;
const TABLE_THEME_SPEC_MAX_ASYMMETRY: f32 = 0.9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableThemeSpecError {
    pub field: String,
    pub message: String,
}

impl TableThemeSpecError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TableThemeSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for TableThemeSpecError {}

impl TableThemeSpec {
    /// Create a spec snapshot from a TableTheme.
    #[must_use]
    pub fn from_theme(theme: &TableTheme) -> Self {
        Self {
            version: TABLE_THEME_SPEC_VERSION,
            name: None,
            preset_id: theme.preset_id,
            padding: theme.padding,
            column_gap: theme.column_gap,
            row_height: theme.row_height,
            styles: TableThemeStyleSpec {
                border: StyleSpec::from_style(&theme.border),
                header: StyleSpec::from_style(&theme.header),
                row: StyleSpec::from_style(&theme.row),
                row_alt: StyleSpec::from_style(&theme.row_alt),
                row_selected: StyleSpec::from_style(&theme.row_selected),
                row_hover: StyleSpec::from_style(&theme.row_hover),
                divider: StyleSpec::from_style(&theme.divider),
            },
            effects: theme
                .effects
                .iter()
                .map(TableEffectRuleSpec::from_rule)
                .collect(),
        }
    }

    /// Convert this spec into a TableTheme.
    #[must_use]
    pub fn into_theme(self) -> TableTheme {
        TableTheme {
            border: self.styles.border.to_style(),
            header: self.styles.header.to_style(),
            row: self.styles.row.to_style(),
            row_alt: self.styles.row_alt.to_style(),
            row_selected: self.styles.row_selected.to_style(),
            row_hover: self.styles.row_hover.to_style(),
            divider: self.styles.divider.to_style(),
            padding: self.padding,
            column_gap: self.column_gap,
            row_height: self.row_height,
            effects: self
                .effects
                .into_iter()
                .map(|spec| spec.to_rule())
                .collect(),
            preset_id: self.preset_id,
        }
    }

    /// Validate spec ranges and sizes for safe import.
    pub fn validate(&self) -> Result<(), TableThemeSpecError> {
        if self.version != TABLE_THEME_SPEC_VERSION {
            return Err(TableThemeSpecError::new(
                "version",
                format!("unsupported version {}", self.version),
            ));
        }

        if let Some(name) = &self.name
            && name.len() > TABLE_THEME_SPEC_MAX_NAME_LEN
        {
            return Err(TableThemeSpecError::new(
                "name",
                format!(
                    "name length {} exceeds max {}",
                    name.len(),
                    TABLE_THEME_SPEC_MAX_NAME_LEN
                ),
            ));
        }

        validate_u8_range("padding", self.padding, 0, TABLE_THEME_SPEC_MAX_PADDING)?;
        validate_u8_range(
            "column_gap",
            self.column_gap,
            0,
            TABLE_THEME_SPEC_MAX_COLUMN_GAP,
        )?;
        validate_u8_range(
            "row_height",
            self.row_height,
            TABLE_THEME_SPEC_MIN_ROW_HEIGHT,
            TABLE_THEME_SPEC_MAX_ROW_HEIGHT,
        )?;

        validate_style_spec(&self.styles.border, "styles.border")?;
        validate_style_spec(&self.styles.header, "styles.header")?;
        validate_style_spec(&self.styles.row, "styles.row")?;
        validate_style_spec(&self.styles.row_alt, "styles.row_alt")?;
        validate_style_spec(&self.styles.row_selected, "styles.row_selected")?;
        validate_style_spec(&self.styles.row_hover, "styles.row_hover")?;
        validate_style_spec(&self.styles.divider, "styles.divider")?;

        if self.effects.len() > TABLE_THEME_SPEC_MAX_EFFECTS {
            return Err(TableThemeSpecError::new(
                "effects",
                format!(
                    "effect count {} exceeds max {}",
                    self.effects.len(),
                    TABLE_THEME_SPEC_MAX_EFFECTS
                ),
            ));
        }

        for (idx, rule) in self.effects.iter().enumerate() {
            validate_effect_rule(rule, idx)?;
        }

        Ok(())
    }
}

fn validate_u8_range(
    field: impl Into<String>,
    value: u8,
    min: u8,
    max: u8,
) -> Result<(), TableThemeSpecError> {
    if value < min || value > max {
        return Err(TableThemeSpecError::new(
            field,
            format!("value {} outside range [{}..={}]", value, min, max),
        ));
    }
    Ok(())
}

fn validate_style_spec(style: &StyleSpec, field: &str) -> Result<(), TableThemeSpecError> {
    if style.attrs.len() > TABLE_THEME_SPEC_MAX_STYLE_ATTRS {
        return Err(TableThemeSpecError::new(
            format!("{field}.attrs"),
            format!(
                "attr count {} exceeds max {}",
                style.attrs.len(),
                TABLE_THEME_SPEC_MAX_STYLE_ATTRS
            ),
        ));
    }
    Ok(())
}

fn validate_effect_target(
    target: &TableEffectTarget,
    idx: usize,
) -> Result<(), TableThemeSpecError> {
    let base = format!("effects[{idx}].target");
    match *target {
        TableEffectTarget::RowRange { start, end } => {
            if start > end {
                return Err(TableThemeSpecError::new(
                    format!("{base}.row_range"),
                    "start must be <= end",
                ));
            }
        }
        TableEffectTarget::ColumnRange { start, end } => {
            if start > end {
                return Err(TableThemeSpecError::new(
                    format!("{base}.column_range"),
                    "start must be <= end",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_effect_rule(rule: &TableEffectRuleSpec, idx: usize) -> Result<(), TableThemeSpecError> {
    validate_effect_target(&rule.target, idx)?;
    let base = format!("effects[{idx}].effect");
    match &rule.effect {
        TableEffectSpec::Pulse {
            speed,
            phase_offset,
            ..
        } => {
            validate_f32_range(
                format!("{base}.speed"),
                *speed,
                0.0,
                TABLE_THEME_SPEC_MAX_SPEED,
            )?;
            validate_f32_range(
                format!("{base}.phase_offset"),
                *phase_offset,
                0.0,
                TABLE_THEME_SPEC_MAX_PHASE,
            )?;
        }
        TableEffectSpec::BreathingGlow {
            intensity,
            speed,
            phase_offset,
            asymmetry,
            ..
        } => {
            validate_f32_range(
                format!("{base}.intensity"),
                *intensity,
                0.0,
                TABLE_THEME_SPEC_MAX_INTENSITY,
            )?;
            validate_f32_range(
                format!("{base}.speed"),
                *speed,
                0.0,
                TABLE_THEME_SPEC_MAX_SPEED,
            )?;
            validate_f32_range(
                format!("{base}.phase_offset"),
                *phase_offset,
                0.0,
                TABLE_THEME_SPEC_MAX_PHASE,
            )?;
            validate_f32_range(
                format!("{base}.asymmetry"),
                *asymmetry,
                -TABLE_THEME_SPEC_MAX_ASYMMETRY,
                TABLE_THEME_SPEC_MAX_ASYMMETRY,
            )?;
        }
        TableEffectSpec::GradientSweep {
            gradient,
            speed,
            phase_offset,
        } => {
            validate_gradient_spec(gradient, &base)?;
            validate_f32_range(
                format!("{base}.speed"),
                *speed,
                0.0,
                TABLE_THEME_SPEC_MAX_SPEED,
            )?;
            validate_f32_range(
                format!("{base}.phase_offset"),
                *phase_offset,
                0.0,
                TABLE_THEME_SPEC_MAX_PHASE,
            )?;
        }
    }
    Ok(())
}

fn validate_gradient_spec(gradient: &GradientSpec, base: &str) -> Result<(), TableThemeSpecError> {
    let count = gradient.stops.len();
    if !(TABLE_THEME_SPEC_MIN_GRADIENT_STOPS..=TABLE_THEME_SPEC_MAX_GRADIENT_STOPS).contains(&count)
    {
        return Err(TableThemeSpecError::new(
            format!("{base}.gradient.stops"),
            format!(
                "stop count {} outside range [{}..={}]",
                count, TABLE_THEME_SPEC_MIN_GRADIENT_STOPS, TABLE_THEME_SPEC_MAX_GRADIENT_STOPS
            ),
        ));
    }
    for (idx, stop) in gradient.stops.iter().enumerate() {
        validate_f32_range(
            format!("{base}.gradient.stops[{idx}].pos"),
            stop.pos,
            0.0,
            1.0,
        )?;
    }
    Ok(())
}

fn validate_f32_range(
    field: impl Into<String>,
    value: f32,
    min: f32,
    max: f32,
) -> Result<(), TableThemeSpecError> {
    if !value.is_finite() {
        return Err(TableThemeSpecError::new(field, "value must be finite"));
    }
    if value < min || value > max {
        return Err(TableThemeSpecError::new(
            field,
            format!("value {} outside range [{min}..={max}]", value),
        ));
    }
    Ok(())
}

impl StyleSpec {
    #[must_use]
    pub fn from_style(style: &Style) -> Self {
        Self {
            fg: style.fg.map(RgbaSpec::from),
            bg: style.bg.map(RgbaSpec::from),
            underline: style.underline_color.map(RgbaSpec::from),
            attrs: style.attrs.map(attrs_from_flags).unwrap_or_default(),
        }
    }

    #[must_use]
    pub fn to_style(&self) -> Style {
        let mut style = Style::new();
        style.fg = self.fg.map(PackedRgba::from);
        style.bg = self.bg.map(PackedRgba::from);
        style.underline_color = self.underline.map(PackedRgba::from);
        style.attrs = flags_from_attrs(&self.attrs);
        style
    }
}

impl GradientSpec {
    #[must_use]
    pub fn from_gradient(gradient: &Gradient) -> Self {
        Self {
            stops: gradient
                .stops()
                .iter()
                .map(|(pos, color)| GradientStopSpec {
                    pos: *pos,
                    color: RgbaSpec::from(*color),
                })
                .collect(),
        }
    }

    #[must_use]
    pub fn to_gradient(&self) -> Gradient {
        Gradient::new(
            self.stops
                .iter()
                .map(|stop| (stop.pos, PackedRgba::from(stop.color)))
                .collect(),
        )
    }
}

impl TableEffectSpec {
    #[must_use]
    pub fn from_effect(effect: &TableEffect) -> Self {
        match effect {
            TableEffect::Pulse {
                fg_a,
                fg_b,
                bg_a,
                bg_b,
                speed,
                phase_offset,
            } => Self::Pulse {
                fg_a: (*fg_a).into(),
                fg_b: (*fg_b).into(),
                bg_a: (*bg_a).into(),
                bg_b: (*bg_b).into(),
                speed: *speed,
                phase_offset: *phase_offset,
            },
            TableEffect::BreathingGlow {
                fg,
                bg,
                intensity,
                speed,
                phase_offset,
                asymmetry,
            } => Self::BreathingGlow {
                fg: (*fg).into(),
                bg: (*bg).into(),
                intensity: *intensity,
                speed: *speed,
                phase_offset: *phase_offset,
                asymmetry: *asymmetry,
            },
            TableEffect::GradientSweep {
                gradient,
                speed,
                phase_offset,
            } => Self::GradientSweep {
                gradient: GradientSpec::from_gradient(gradient),
                speed: *speed,
                phase_offset: *phase_offset,
            },
        }
    }

    #[must_use]
    pub fn to_effect(&self) -> TableEffect {
        match self {
            TableEffectSpec::Pulse {
                fg_a,
                fg_b,
                bg_a,
                bg_b,
                speed,
                phase_offset,
            } => TableEffect::Pulse {
                fg_a: (*fg_a).into(),
                fg_b: (*fg_b).into(),
                bg_a: (*bg_a).into(),
                bg_b: (*bg_b).into(),
                speed: *speed,
                phase_offset: *phase_offset,
            },
            TableEffectSpec::BreathingGlow {
                fg,
                bg,
                intensity,
                speed,
                phase_offset,
                asymmetry,
            } => TableEffect::BreathingGlow {
                fg: (*fg).into(),
                bg: (*bg).into(),
                intensity: *intensity,
                speed: *speed,
                phase_offset: *phase_offset,
                asymmetry: *asymmetry,
            },
            TableEffectSpec::GradientSweep {
                gradient,
                speed,
                phase_offset,
            } => TableEffect::GradientSweep {
                gradient: gradient.to_gradient(),
                speed: *speed,
                phase_offset: *phase_offset,
            },
        }
    }
}

impl TableEffectRuleSpec {
    #[must_use]
    pub fn from_rule(rule: &TableEffectRule) -> Self {
        Self {
            target: rule.target,
            effect: TableEffectSpec::from_effect(&rule.effect),
            priority: rule.priority,
            blend_mode: rule.blend_mode,
            style_mask: rule.style_mask,
        }
    }

    #[must_use]
    pub fn to_rule(&self) -> TableEffectRule {
        TableEffectRule {
            target: self.target,
            effect: self.effect.to_effect(),
            priority: self.priority,
            blend_mode: self.blend_mode,
            style_mask: self.style_mask,
        }
    }
}

fn attrs_from_flags(flags: StyleFlags) -> Vec<StyleAttr> {
    let mut attrs = Vec::new();
    if flags.contains(StyleFlags::BOLD) {
        attrs.push(StyleAttr::Bold);
    }
    if flags.contains(StyleFlags::DIM) {
        attrs.push(StyleAttr::Dim);
    }
    if flags.contains(StyleFlags::ITALIC) {
        attrs.push(StyleAttr::Italic);
    }
    if flags.contains(StyleFlags::UNDERLINE) {
        attrs.push(StyleAttr::Underline);
    }
    if flags.contains(StyleFlags::BLINK) {
        attrs.push(StyleAttr::Blink);
    }
    if flags.contains(StyleFlags::REVERSE) {
        attrs.push(StyleAttr::Reverse);
    }
    if flags.contains(StyleFlags::HIDDEN) {
        attrs.push(StyleAttr::Hidden);
    }
    if flags.contains(StyleFlags::STRIKETHROUGH) {
        attrs.push(StyleAttr::Strikethrough);
    }
    if flags.contains(StyleFlags::DOUBLE_UNDERLINE) {
        attrs.push(StyleAttr::DoubleUnderline);
    }
    if flags.contains(StyleFlags::CURLY_UNDERLINE) {
        attrs.push(StyleAttr::CurlyUnderline);
    }
    attrs
}

fn flags_from_attrs(attrs: &[StyleAttr]) -> Option<StyleFlags> {
    if attrs.is_empty() {
        return None;
    }
    let mut flags = StyleFlags::NONE;
    for attr in attrs {
        match attr {
            StyleAttr::Bold => flags.insert(StyleFlags::BOLD),
            StyleAttr::Dim => flags.insert(StyleFlags::DIM),
            StyleAttr::Italic => flags.insert(StyleFlags::ITALIC),
            StyleAttr::Underline => flags.insert(StyleFlags::UNDERLINE),
            StyleAttr::Blink => flags.insert(StyleFlags::BLINK),
            StyleAttr::Reverse => flags.insert(StyleFlags::REVERSE),
            StyleAttr::Hidden => flags.insert(StyleFlags::HIDDEN),
            StyleAttr::Strikethrough => flags.insert(StyleFlags::STRIKETHROUGH),
            StyleAttr::DoubleUnderline => flags.insert(StyleFlags::DOUBLE_UNDERLINE),
            StyleAttr::CurlyUnderline => flags.insert(StyleFlags::CURLY_UNDERLINE),
        }
    }
    if flags.is_empty() { None } else { Some(flags) }
}

struct ThemeStyles {
    border: Style,
    header: Style,
    row: Style,
    row_alt: Style,
    row_selected: Style,
    row_hover: Style,
    divider: Style,
}

impl TableTheme {
    /// Create a resolver that applies this theme's effects.
    #[must_use]
    pub const fn effect_resolver(&self) -> TableEffectResolver<'_> {
        TableEffectResolver::new(self)
    }

    /// Build a theme from a preset identifier.
    #[must_use]
    pub fn preset(preset: TablePresetId) -> Self {
        match preset {
            TablePresetId::Aurora => Self::aurora(),
            TablePresetId::Graphite => Self::graphite(),
            TablePresetId::Neon => Self::neon(),
            TablePresetId::Slate => Self::slate(),
            TablePresetId::Solar => Self::solar(),
            TablePresetId::Orchard => Self::orchard(),
            TablePresetId::Paper => Self::paper(),
            TablePresetId::Midnight => Self::midnight(),
            TablePresetId::TerminalClassic => Self::terminal_classic(),
        }
    }

    /// Set the border style.
    #[must_use]
    pub fn with_border(mut self, border: Style) -> Self {
        self.border = border;
        self
    }

    /// Set the header style.
    #[must_use]
    pub fn with_header(mut self, header: Style) -> Self {
        self.header = header;
        self
    }

    /// Set the base row style.
    #[must_use]
    pub fn with_row(mut self, row: Style) -> Self {
        self.row = row;
        self
    }

    /// Set the alternate row style.
    #[must_use]
    pub fn with_row_alt(mut self, row_alt: Style) -> Self {
        self.row_alt = row_alt;
        self
    }

    /// Set the selected row style.
    #[must_use]
    pub fn with_row_selected(mut self, row_selected: Style) -> Self {
        self.row_selected = row_selected;
        self
    }

    /// Set the hover row style.
    #[must_use]
    pub fn with_row_hover(mut self, row_hover: Style) -> Self {
        self.row_hover = row_hover;
        self
    }

    /// Set the divider style.
    #[must_use]
    pub fn with_divider(mut self, divider: Style) -> Self {
        self.divider = divider;
        self
    }

    /// Set table padding (cells inset).
    #[must_use]
    pub fn with_padding(mut self, padding: u8) -> Self {
        self.padding = padding;
        self
    }

    /// Set column gap in cells.
    #[must_use]
    pub fn with_column_gap(mut self, column_gap: u8) -> Self {
        self.column_gap = column_gap;
        self
    }

    /// Set row height in lines.
    #[must_use]
    pub fn with_row_height(mut self, row_height: u8) -> Self {
        self.row_height = row_height;
        self
    }

    /// Replace effect rules.
    #[must_use]
    pub fn with_effects(mut self, effects: Vec<TableEffectRule>) -> Self {
        self.effects = effects;
        self
    }

    /// Append a single effect rule.
    #[must_use]
    pub fn with_effect(mut self, effect: TableEffectRule) -> Self {
        self.effects.push(effect);
        self
    }

    /// Remove all effect rules.
    #[must_use]
    pub fn clear_effects(mut self) -> Self {
        self.effects.clear();
        self
    }

    /// Override the preset identifier (used for diagnostics).
    #[must_use]
    pub fn with_preset_id(mut self, preset_id: Option<TablePresetId>) -> Self {
        self.preset_id = preset_id;
        self
    }

    /// Luminous header with cool zebra rows.
    #[must_use]
    pub fn aurora() -> Self {
        Self::build(
            TablePresetId::Aurora,
            ThemeStyles {
                border: Style::new().fg(PackedRgba::rgb(130, 170, 210)),
                header: Style::new()
                    .fg(PackedRgba::rgb(250, 250, 255))
                    .bg(PackedRgba::rgb(70, 100, 140))
                    .bold(),
                row: Style::new().fg(PackedRgba::rgb(230, 235, 245)),
                row_alt: Style::new()
                    .fg(PackedRgba::rgb(230, 235, 245))
                    .bg(PackedRgba::rgb(28, 36, 54)),
                row_selected: Style::new()
                    .fg(PackedRgba::rgb(255, 255, 255))
                    .bg(PackedRgba::rgb(50, 90, 140))
                    .bold(),
                row_hover: Style::new()
                    .fg(PackedRgba::rgb(240, 245, 255))
                    .bg(PackedRgba::rgb(40, 70, 110)),
                divider: Style::new().fg(PackedRgba::rgb(90, 120, 160)),
            },
        )
    }

    /// Monochrome, maximum legibility at dense data.
    #[must_use]
    pub fn graphite() -> Self {
        Self::build(
            TablePresetId::Graphite,
            ThemeStyles {
                border: Style::new().fg(PackedRgba::rgb(140, 140, 140)),
                header: Style::new()
                    .fg(PackedRgba::rgb(240, 240, 240))
                    .bg(PackedRgba::rgb(70, 70, 70))
                    .bold(),
                row: Style::new().fg(PackedRgba::rgb(220, 220, 220)),
                row_alt: Style::new()
                    .fg(PackedRgba::rgb(220, 220, 220))
                    .bg(PackedRgba::rgb(35, 35, 35)),
                row_selected: Style::new()
                    .fg(PackedRgba::rgb(255, 255, 255))
                    .bg(PackedRgba::rgb(90, 90, 90)),
                row_hover: Style::new()
                    .fg(PackedRgba::rgb(245, 245, 245))
                    .bg(PackedRgba::rgb(60, 60, 60)),
                divider: Style::new().fg(PackedRgba::rgb(120, 120, 120)),
            },
        )
    }

    /// Neon accent header with vivid highlights.
    #[must_use]
    pub fn neon() -> Self {
        Self::build(
            TablePresetId::Neon,
            ThemeStyles {
                border: Style::new().fg(PackedRgba::rgb(120, 255, 230)),
                header: Style::new()
                    .fg(PackedRgba::rgb(10, 10, 15))
                    .bg(PackedRgba::rgb(0, 255, 200))
                    .bold(),
                row: Style::new().fg(PackedRgba::rgb(210, 255, 245)),
                row_alt: Style::new()
                    .fg(PackedRgba::rgb(210, 255, 245))
                    .bg(PackedRgba::rgb(10, 20, 30)),
                row_selected: Style::new()
                    .fg(PackedRgba::rgb(5, 5, 10))
                    .bg(PackedRgba::rgb(255, 0, 200))
                    .bold(),
                row_hover: Style::new()
                    .fg(PackedRgba::rgb(0, 10, 15))
                    .bg(PackedRgba::rgb(0, 200, 255)),
                divider: Style::new().fg(PackedRgba::rgb(80, 220, 200)),
            },
        )
    }

    /// Subtle slate tones for neutral dashboards.
    #[must_use]
    pub fn slate() -> Self {
        Self::build(
            TablePresetId::Slate,
            ThemeStyles {
                border: Style::new().fg(PackedRgba::rgb(120, 130, 140)),
                header: Style::new()
                    .fg(PackedRgba::rgb(230, 235, 240))
                    .bg(PackedRgba::rgb(60, 70, 80))
                    .bold(),
                row: Style::new().fg(PackedRgba::rgb(210, 215, 220)),
                row_alt: Style::new()
                    .fg(PackedRgba::rgb(210, 215, 220))
                    .bg(PackedRgba::rgb(30, 35, 40)),
                row_selected: Style::new()
                    .fg(PackedRgba::rgb(255, 255, 255))
                    .bg(PackedRgba::rgb(80, 90, 110)),
                row_hover: Style::new()
                    .fg(PackedRgba::rgb(235, 240, 245))
                    .bg(PackedRgba::rgb(50, 60, 70)),
                divider: Style::new().fg(PackedRgba::rgb(110, 120, 130)),
            },
        )
    }

    /// Warm, sunlight-forward palette.
    #[must_use]
    pub fn solar() -> Self {
        Self::build(
            TablePresetId::Solar,
            ThemeStyles {
                border: Style::new().fg(PackedRgba::rgb(200, 170, 120)),
                header: Style::new()
                    .fg(PackedRgba::rgb(30, 25, 10))
                    .bg(PackedRgba::rgb(255, 200, 90))
                    .bold(),
                row: Style::new().fg(PackedRgba::rgb(240, 220, 180)),
                row_alt: Style::new()
                    .fg(PackedRgba::rgb(240, 220, 180))
                    .bg(PackedRgba::rgb(60, 40, 20)),
                row_selected: Style::new()
                    .fg(PackedRgba::rgb(20, 10, 0))
                    .bg(PackedRgba::rgb(255, 140, 60))
                    .bold(),
                row_hover: Style::new()
                    .fg(PackedRgba::rgb(20, 10, 0))
                    .bg(PackedRgba::rgb(220, 120, 40)),
                divider: Style::new().fg(PackedRgba::rgb(170, 140, 90)),
            },
        )
    }

    /// Orchard greens with soft depth.
    #[must_use]
    pub fn orchard() -> Self {
        Self::build(
            TablePresetId::Orchard,
            ThemeStyles {
                border: Style::new().fg(PackedRgba::rgb(140, 180, 120)),
                header: Style::new()
                    .fg(PackedRgba::rgb(20, 40, 20))
                    .bg(PackedRgba::rgb(120, 200, 120))
                    .bold(),
                row: Style::new().fg(PackedRgba::rgb(210, 235, 210)),
                row_alt: Style::new()
                    .fg(PackedRgba::rgb(210, 235, 210))
                    .bg(PackedRgba::rgb(30, 60, 40)),
                row_selected: Style::new()
                    .fg(PackedRgba::rgb(15, 30, 15))
                    .bg(PackedRgba::rgb(160, 230, 140))
                    .bold(),
                row_hover: Style::new()
                    .fg(PackedRgba::rgb(15, 30, 15))
                    .bg(PackedRgba::rgb(130, 210, 120)),
                divider: Style::new().fg(PackedRgba::rgb(100, 150, 100)),
            },
        )
    }

    /// Light, paper-like styling for documentation tables.
    #[must_use]
    pub fn paper() -> Self {
        Self::build(
            TablePresetId::Paper,
            ThemeStyles {
                border: Style::new().fg(PackedRgba::rgb(120, 110, 100)),
                header: Style::new()
                    .fg(PackedRgba::rgb(30, 30, 30))
                    .bg(PackedRgba::rgb(230, 220, 200))
                    .bold(),
                row: Style::new()
                    .fg(PackedRgba::rgb(40, 40, 40))
                    .bg(PackedRgba::rgb(245, 240, 230)),
                row_alt: Style::new()
                    .fg(PackedRgba::rgb(40, 40, 40))
                    .bg(PackedRgba::rgb(235, 230, 220)),
                row_selected: Style::new()
                    .fg(PackedRgba::rgb(10, 10, 10))
                    .bg(PackedRgba::rgb(255, 245, 210))
                    .bold(),
                row_hover: Style::new()
                    .fg(PackedRgba::rgb(20, 20, 20))
                    .bg(PackedRgba::rgb(245, 235, 205)),
                divider: Style::new().fg(PackedRgba::rgb(140, 130, 120)),
            },
        )
    }

    /// Deep, nocturnal palette with high contrast accents.
    #[must_use]
    pub fn midnight() -> Self {
        Self::build(
            TablePresetId::Midnight,
            ThemeStyles {
                border: Style::new().fg(PackedRgba::rgb(80, 100, 130)),
                header: Style::new()
                    .fg(PackedRgba::rgb(220, 230, 255))
                    .bg(PackedRgba::rgb(30, 40, 70))
                    .bold(),
                row: Style::new().fg(PackedRgba::rgb(200, 210, 230)),
                row_alt: Style::new()
                    .fg(PackedRgba::rgb(200, 210, 230))
                    .bg(PackedRgba::rgb(15, 20, 35)),
                row_selected: Style::new()
                    .fg(PackedRgba::rgb(255, 255, 255))
                    .bg(PackedRgba::rgb(60, 80, 120))
                    .bold(),
                row_hover: Style::new()
                    .fg(PackedRgba::rgb(240, 240, 255))
                    .bg(PackedRgba::rgb(45, 60, 90)),
                divider: Style::new().fg(PackedRgba::rgb(100, 120, 150)),
            },
        )
    }

    /// ANSI-16 baseline with richer palettes on 256/truecolor terminals.
    #[must_use]
    pub fn terminal_classic() -> Self {
        Self::terminal_classic_for(ColorProfile::detect())
    }

    /// ANSI-16 baseline with richer palettes on 256/truecolor terminals.
    #[must_use]
    pub fn terminal_classic_for(profile: ColorProfile) -> Self {
        let border = classic_color(profile, (160, 160, 160), Ansi16::BrightBlack);
        let header_fg = classic_color(profile, (245, 245, 245), Ansi16::BrightWhite);
        let header_bg = classic_color(profile, (0, 90, 140), Ansi16::Blue);
        let row_fg = classic_color(profile, (230, 230, 230), Ansi16::White);
        let row_alt_bg = classic_color(profile, (30, 30, 30), Ansi16::Black);
        let selected_bg = classic_color(profile, (160, 90, 10), Ansi16::Yellow);
        let hover_bg = classic_color(profile, (70, 70, 70), Ansi16::BrightBlack);
        let divider = classic_color(profile, (120, 120, 120), Ansi16::BrightBlack);

        Self::build(
            TablePresetId::TerminalClassic,
            ThemeStyles {
                border: Style::new().fg(border),
                header: Style::new().fg(header_fg).bg(header_bg).bold(),
                row: Style::new().fg(row_fg),
                row_alt: Style::new().fg(row_fg).bg(row_alt_bg),
                row_selected: Style::new().fg(PackedRgba::BLACK).bg(selected_bg).bold(),
                row_hover: Style::new().fg(PackedRgba::WHITE).bg(hover_bg),
                divider: Style::new().fg(divider),
            },
        )
    }

    fn build(preset_id: TablePresetId, styles: ThemeStyles) -> Self {
        Self {
            border: styles.border,
            header: styles.header,
            row: styles.row,
            row_alt: styles.row_alt,
            row_selected: styles.row_selected,
            row_hover: styles.row_hover,
            divider: styles.divider,
            padding: 1,
            column_gap: 1,
            row_height: 1,
            effects: Vec::new(),
            preset_id: Some(preset_id),
        }
    }

    /// Produce a deterministic diagnostics summary for logging or tests.
    #[must_use]
    pub fn diagnostics(&self) -> TableThemeDiagnostics {
        TableThemeDiagnostics {
            preset_id: self.preset_id,
            style_hash: self.style_hash(),
            effects_hash: self.effects_hash(),
            effect_count: self.effects.len(),
            padding: self.padding,
            column_gap: self.column_gap,
            row_height: self.row_height,
        }
    }

    /// Stable hash of base styles + layout parameters.
    #[must_use]
    pub fn style_hash(&self) -> u64 {
        let mut hasher = StableHasher::new();
        hash_style(&self.border, &mut hasher);
        hash_style(&self.header, &mut hasher);
        hash_style(&self.row, &mut hasher);
        hash_style(&self.row_alt, &mut hasher);
        hash_style(&self.row_selected, &mut hasher);
        hash_style(&self.row_hover, &mut hasher);
        hash_style(&self.divider, &mut hasher);
        hash_u8(self.padding, &mut hasher);
        hash_u8(self.column_gap, &mut hasher);
        hash_u8(self.row_height, &mut hasher);
        hash_preset(self.preset_id, &mut hasher);
        hasher.finish()
    }

    /// Stable hash of effect rules (target + effect + blend + mask).
    #[must_use]
    pub fn effects_hash(&self) -> u64 {
        let mut hasher = StableHasher::new();
        hash_usize(self.effects.len(), &mut hasher);
        for rule in &self.effects {
            hash_effect_rule(rule, &mut hasher);
        }
        hasher.finish()
    }
}

#[derive(Clone, Copy, Debug)]
struct EffectSample {
    fg: Option<PackedRgba>,
    bg: Option<PackedRgba>,
    alpha: f32,
}

#[inline]
fn resolve_effects_for_scope(
    theme: &TableTheme,
    base: Style,
    scope: TableEffectScope,
    phase: f32,
) -> Style {
    if theme.effects.is_empty() {
        return base;
    }

    let mut min_priority = u8::MAX;
    let mut max_priority = 0;
    for rule in &theme.effects {
        min_priority = min_priority.min(rule.priority);
        max_priority = max_priority.max(rule.priority);
    }
    if min_priority == u8::MAX {
        return base;
    }

    let mut resolved = base;
    for priority in min_priority..=max_priority {
        for rule in &theme.effects {
            if rule.priority != priority {
                continue;
            }
            if !rule.target.matches_scope(scope) {
                continue;
            }
            resolved = apply_effect_rule(resolved, rule, phase);
        }
    }

    resolved
}

#[inline]
fn apply_effect_rule(mut base: Style, rule: &TableEffectRule, phase: f32) -> Style {
    let sample = sample_effect(&rule.effect, phase);
    let alpha = sample.alpha.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return base;
    }

    if rule.style_mask.fg {
        base.fg = apply_channel(base.fg, sample.fg, alpha, rule.blend_mode);
    }
    if rule.style_mask.bg {
        base.bg = apply_channel(base.bg, sample.bg, alpha, rule.blend_mode);
    }
    base
}

#[inline]
fn apply_channel(
    base: Option<PackedRgba>,
    effect: Option<PackedRgba>,
    alpha: f32,
    blend_mode: BlendMode,
) -> Option<PackedRgba> {
    let effect = effect?;
    let alpha = alpha.clamp(0.0, 1.0);
    let result = match base {
        Some(base) => blend_with_alpha(base, effect, alpha, blend_mode),
        None => with_alpha(effect, alpha),
    };
    Some(result)
}

#[inline]
fn blend_with_alpha(
    base: PackedRgba,
    effect: PackedRgba,
    alpha: f32,
    blend_mode: BlendMode,
) -> PackedRgba {
    let alpha = alpha.clamp(0.0, 1.0);
    match blend_mode {
        BlendMode::Replace => lerp_color(base, effect, alpha),
        BlendMode::Additive => blend_additive(with_alpha(effect, alpha), base),
        BlendMode::Multiply => blend_multiply(with_alpha(effect, alpha), base),
        BlendMode::Screen => blend_screen(with_alpha(effect, alpha), base),
    }
}

#[inline]
fn sample_effect(effect: &TableEffect, phase: f32) -> EffectSample {
    match *effect {
        TableEffect::Pulse {
            fg_a,
            fg_b,
            bg_a,
            bg_b,
            speed,
            phase_offset,
        } => {
            let t = normalize_phase(phase * speed + phase_offset);
            let alpha = pulse_curve(t);
            EffectSample {
                fg: Some(lerp_color(fg_a, fg_b, alpha)),
                bg: Some(lerp_color(bg_a, bg_b, alpha)),
                alpha: 1.0,
            }
        }
        TableEffect::BreathingGlow {
            fg,
            bg,
            intensity,
            speed,
            phase_offset,
            asymmetry,
        } => {
            let t = normalize_phase(phase * speed + phase_offset);
            let alpha = (breathing_curve(t, asymmetry) * intensity).clamp(0.0, 1.0);
            EffectSample {
                fg: Some(fg),
                bg: Some(bg),
                alpha,
            }
        }
        TableEffect::GradientSweep {
            ref gradient,
            speed,
            phase_offset,
        } => {
            let t = normalize_phase(phase * speed + phase_offset);
            let color = gradient.sample(t);
            EffectSample {
                fg: Some(color),
                bg: Some(color),
                alpha: 1.0,
            }
        }
    }
}

#[inline]
fn normalize_phase(phase: f32) -> f32 {
    phase.rem_euclid(1.0)
}

#[inline]
fn pulse_curve(t: f32) -> f32 {
    0.5 - 0.5 * (std::f32::consts::TAU * t).cos()
}

#[inline]
fn breathing_curve(t: f32, asymmetry: f32) -> f32 {
    let t = skew_phase(t, asymmetry);
    0.5 - 0.5 * (std::f32::consts::TAU * t).cos()
}

#[inline]
fn skew_phase(t: f32, asymmetry: f32) -> f32 {
    let skew = asymmetry.clamp(-0.9, 0.9);
    if skew == 0.0 {
        return t;
    }
    if skew > 0.0 {
        t.powf(1.0 + skew * 2.0)
    } else {
        1.0 - (1.0 - t).powf(1.0 - skew * 2.0)
    }
}

#[inline]
fn with_alpha(color: PackedRgba, alpha: f32) -> PackedRgba {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    PackedRgba::rgba(color.r(), color.g(), color.b(), a)
}

#[inline]
fn blend_additive(top: PackedRgba, bottom: PackedRgba) -> PackedRgba {
    let ta = top.a() as f32 / 255.0;
    let r = (bottom.r() as f32 + top.r() as f32 * ta).min(255.0) as u8;
    let g = (bottom.g() as f32 + top.g() as f32 * ta).min(255.0) as u8;
    let b = (bottom.b() as f32 + top.b() as f32 * ta).min(255.0) as u8;
    let a = bottom.a().max(top.a());
    PackedRgba::rgba(r, g, b, a)
}

#[inline]
fn blend_multiply(top: PackedRgba, bottom: PackedRgba) -> PackedRgba {
    let ta = top.a() as f32 / 255.0;
    let mr = (top.r() as f32 * bottom.r() as f32 / 255.0) as u8;
    let mg = (top.g() as f32 * bottom.g() as f32 / 255.0) as u8;
    let mb = (top.b() as f32 * bottom.b() as f32 / 255.0) as u8;
    let r = (bottom.r() as f32 * (1.0 - ta) + mr as f32 * ta) as u8;
    let g = (bottom.g() as f32 * (1.0 - ta) + mg as f32 * ta) as u8;
    let b = (bottom.b() as f32 * (1.0 - ta) + mb as f32 * ta) as u8;
    let a = bottom.a().max(top.a());
    PackedRgba::rgba(r, g, b, a)
}

#[inline]
fn blend_screen(top: PackedRgba, bottom: PackedRgba) -> PackedRgba {
    let ta = top.a() as f32 / 255.0;
    let sr = 255 - ((255 - top.r()) as u16 * (255 - bottom.r()) as u16 / 255) as u8;
    let sg = 255 - ((255 - top.g()) as u16 * (255 - bottom.g()) as u16 / 255) as u8;
    let sb = 255 - ((255 - top.b()) as u16 * (255 - bottom.b()) as u16 / 255) as u8;
    let r = (bottom.r() as f32 * (1.0 - ta) + sr as f32 * ta) as u8;
    let g = (bottom.g() as f32 * (1.0 - ta) + sg as f32 * ta) as u8;
    let b = (bottom.b() as f32 * (1.0 - ta) + sb as f32 * ta) as u8;
    let a = bottom.a().max(top.a());
    PackedRgba::rgba(r, g, b, a)
}

impl Default for TableTheme {
    fn default() -> Self {
        Self::graphite()
    }
}

#[inline]
fn classic_color(profile: ColorProfile, rgb: (u8, u8, u8), ansi16: Ansi16) -> PackedRgba {
    let color = match profile {
        ColorProfile::Ansi16 => Color::Ansi16(ansi16),
        _ => Color::rgb(rgb.0, rgb.1, rgb.2).downgrade(profile),
    };
    let rgb = color.to_rgb();
    PackedRgba::rgb(rgb.r, rgb.g, rgb.b)
}

// ---------------------------------------------------------------------------
// Diagnostics hashing (stable, deterministic)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct StableHasher {
    state: u64,
}

impl StableHasher {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    #[must_use]
    const fn new() -> Self {
        Self {
            state: Self::OFFSET,
        }
    }
}

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.state
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.state;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(Self::PRIME);
        }
        self.state = hash;
    }
}

fn hash_u8(value: u8, hasher: &mut StableHasher) {
    hasher.write(&[value]);
}

fn hash_u32(value: u32, hasher: &mut StableHasher) {
    hasher.write(&value.to_le_bytes());
}

fn hash_u64(value: u64, hasher: &mut StableHasher) {
    hasher.write(&value.to_le_bytes());
}

fn hash_usize(value: usize, hasher: &mut StableHasher) {
    hash_u64(value as u64, hasher);
}

fn hash_f32(value: f32, hasher: &mut StableHasher) {
    hash_u32(value.to_bits(), hasher);
}

fn hash_bool(value: bool, hasher: &mut StableHasher) {
    hash_u8(value as u8, hasher);
}

fn hash_style(style: &Style, hasher: &mut StableHasher) {
    style.hash(hasher);
}

fn hash_packed_rgba(color: PackedRgba, hasher: &mut StableHasher) {
    hash_u32(color.0, hasher);
}

fn hash_preset(preset: Option<TablePresetId>, hasher: &mut StableHasher) {
    match preset {
        None => hash_u8(0, hasher),
        Some(id) => {
            hash_u8(1, hasher);
            hash_table_preset(id, hasher);
        }
    }
}

fn hash_table_preset(preset: TablePresetId, hasher: &mut StableHasher) {
    let tag = match preset {
        TablePresetId::Aurora => 1,
        TablePresetId::Graphite => 2,
        TablePresetId::Neon => 3,
        TablePresetId::Slate => 4,
        TablePresetId::Solar => 5,
        TablePresetId::Orchard => 6,
        TablePresetId::Paper => 7,
        TablePresetId::Midnight => 8,
        TablePresetId::TerminalClassic => 9,
    };
    hash_u8(tag, hasher);
}

fn hash_table_section(section: TableSection, hasher: &mut StableHasher) {
    let tag = match section {
        TableSection::Header => 1,
        TableSection::Body => 2,
        TableSection::Footer => 3,
    };
    hash_u8(tag, hasher);
}

fn hash_blend_mode(mode: BlendMode, hasher: &mut StableHasher) {
    let tag = match mode {
        BlendMode::Replace => 1,
        BlendMode::Additive => 2,
        BlendMode::Multiply => 3,
        BlendMode::Screen => 4,
    };
    hash_u8(tag, hasher);
}

fn hash_style_mask(mask: StyleMask, hasher: &mut StableHasher) {
    hash_bool(mask.fg, hasher);
    hash_bool(mask.bg, hasher);
    hash_bool(mask.attrs, hasher);
}

fn hash_effect_target(target: &TableEffectTarget, hasher: &mut StableHasher) {
    match *target {
        TableEffectTarget::Section(section) => {
            hash_u8(1, hasher);
            hash_table_section(section, hasher);
        }
        TableEffectTarget::Row(row) => {
            hash_u8(2, hasher);
            hash_usize(row, hasher);
        }
        TableEffectTarget::RowRange { start, end } => {
            hash_u8(3, hasher);
            hash_usize(start, hasher);
            hash_usize(end, hasher);
        }
        TableEffectTarget::Column(column) => {
            hash_u8(4, hasher);
            hash_usize(column, hasher);
        }
        TableEffectTarget::ColumnRange { start, end } => {
            hash_u8(5, hasher);
            hash_usize(start, hasher);
            hash_usize(end, hasher);
        }
        TableEffectTarget::AllRows => {
            hash_u8(6, hasher);
        }
        TableEffectTarget::AllCells => {
            hash_u8(7, hasher);
        }
    }
}

fn hash_gradient(gradient: &Gradient, hasher: &mut StableHasher) {
    hash_usize(gradient.stops.len(), hasher);
    for (pos, color) in &gradient.stops {
        hash_f32(*pos, hasher);
        hash_packed_rgba(*color, hasher);
    }
}

fn hash_effect(effect: &TableEffect, hasher: &mut StableHasher) {
    match *effect {
        TableEffect::Pulse {
            fg_a,
            fg_b,
            bg_a,
            bg_b,
            speed,
            phase_offset,
        } => {
            hash_u8(1, hasher);
            hash_packed_rgba(fg_a, hasher);
            hash_packed_rgba(fg_b, hasher);
            hash_packed_rgba(bg_a, hasher);
            hash_packed_rgba(bg_b, hasher);
            hash_f32(speed, hasher);
            hash_f32(phase_offset, hasher);
        }
        TableEffect::BreathingGlow {
            fg,
            bg,
            intensity,
            speed,
            phase_offset,
            asymmetry,
        } => {
            hash_u8(2, hasher);
            hash_packed_rgba(fg, hasher);
            hash_packed_rgba(bg, hasher);
            hash_f32(intensity, hasher);
            hash_f32(speed, hasher);
            hash_f32(phase_offset, hasher);
            hash_f32(asymmetry, hasher);
        }
        TableEffect::GradientSweep {
            ref gradient,
            speed,
            phase_offset,
        } => {
            hash_u8(3, hasher);
            hash_gradient(gradient, hasher);
            hash_f32(speed, hasher);
            hash_f32(phase_offset, hasher);
        }
    }
}

fn hash_effect_rule(rule: &TableEffectRule, hasher: &mut StableHasher) {
    hash_effect_target(&rule.target, hasher);
    hash_effect(&rule.effect, hasher);
    hash_u8(rule.priority, hasher);
    hash_blend_mode(rule.blend_mode, hasher);
    hash_style_mask(rule.style_mask, hasher);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{WCAG_AA_LARGE_TEXT, WCAG_AA_NORMAL_TEXT, contrast_ratio_packed};
    #[cfg(feature = "serde")]
    use serde_json;

    fn base_bg(theme: &TableTheme) -> PackedRgba {
        theme
            .row
            .bg
            .or(theme.row_alt.bg)
            .or(theme.header.bg)
            .or(theme.row_selected.bg)
            .or(theme.row_hover.bg)
            .unwrap_or(PackedRgba::BLACK)
    }

    fn expect_fg(preset: TablePresetId, label: &str, style: Style) -> PackedRgba {
        let fg = style.fg;
        assert!(fg.is_some(), "{preset:?} missing fg for {label}");
        fg.unwrap()
    }

    fn expect_bg(preset: TablePresetId, label: &str, style: Style) -> PackedRgba {
        let bg = style.bg;
        assert!(bg.is_some(), "{preset:?} missing bg for {label}");
        bg.unwrap()
    }

    fn assert_contrast(
        preset: TablePresetId,
        label: &str,
        fg: PackedRgba,
        bg: PackedRgba,
        minimum: f64,
    ) {
        let ratio = contrast_ratio_packed(fg, bg);
        assert!(
            ratio >= minimum,
            "{preset:?} {label} contrast {ratio:.2} below {minimum:.2}"
        );
    }

    fn pulse_effect(fg: PackedRgba, bg: PackedRgba) -> TableEffect {
        TableEffect::Pulse {
            fg_a: fg,
            fg_b: fg,
            bg_a: bg,
            bg_b: bg,
            speed: 1.0,
            phase_offset: 0.0,
        }
    }

    fn assert_f32_near(label: &str, value: f32, expected: f32) {
        let delta = (value - expected).abs();
        assert!(delta <= 1e-6, "{label} expected {expected}, got {value}");
    }

    #[test]
    fn style_mask_default_is_fg_bg() {
        let mask = StyleMask::default();
        assert!(mask.fg);
        assert!(mask.bg);
        assert!(!mask.attrs);
    }

    #[test]
    fn effect_target_matches_scope_variants() {
        let row_scope = TableEffectScope::row(TableSection::Body, 2);
        assert!(TableEffectTarget::Section(TableSection::Body).matches_scope(row_scope));
        assert!(!TableEffectTarget::Section(TableSection::Header).matches_scope(row_scope));
        assert!(TableEffectTarget::Row(2).matches_scope(row_scope));
        assert!(!TableEffectTarget::Row(1).matches_scope(row_scope));
        assert!(TableEffectTarget::RowRange { start: 1, end: 3 }.matches_scope(row_scope));
        assert!(!TableEffectTarget::RowRange { start: 3, end: 5 }.matches_scope(row_scope));
        assert!(TableEffectTarget::AllRows.matches_scope(row_scope));
        assert!(TableEffectTarget::AllCells.matches_scope(row_scope));
        assert!(!TableEffectTarget::Column(0).matches_scope(row_scope));

        let col_scope = TableEffectScope::column(TableSection::Header, 1);
        assert!(TableEffectTarget::Column(1).matches_scope(col_scope));
        assert!(TableEffectTarget::ColumnRange { start: 0, end: 2 }.matches_scope(col_scope));
        assert!(!TableEffectTarget::AllRows.matches_scope(col_scope));
        assert!(TableEffectTarget::AllCells.matches_scope(col_scope));

        let footer_scope = TableEffectScope::row(TableSection::Footer, 0);
        assert!(!TableEffectTarget::AllCells.matches_scope(footer_scope));

        let header_section = TableEffectScope::section(TableSection::Header);
        assert!(!TableEffectTarget::AllCells.matches_scope(header_section));
    }

    #[test]
    fn effect_resolver_returns_base_without_effects() {
        let base = Style::new()
            .fg(PackedRgba::rgb(12, 34, 56))
            .bg(PackedRgba::rgb(7, 8, 9));
        let mut theme = TableTheme::aurora();
        theme.effects.clear();

        let resolver = theme.effect_resolver();
        let scope = TableEffectScope::row(TableSection::Body, 0);
        let resolved = resolver.resolve(base, scope, 0.25);
        assert_eq!(resolved, base);
    }

    #[test]
    fn effect_resolver_all_rows_excludes_header() {
        let base = Style::new().fg(PackedRgba::rgb(10, 10, 10));
        let mut theme = TableTheme::aurora();
        // pulse_effect(fg, bg) - fg_a=fg_b=first_param, bg_a=bg_b=second_param
        theme.effects = vec![TableEffectRule::new(
            TableEffectTarget::AllRows,
            pulse_effect(PackedRgba::rgb(200, 0, 0), PackedRgba::rgb(5, 5, 5)),
        )];

        let resolver = theme.effect_resolver();
        let header_scope = TableEffectScope::row(TableSection::Header, 0);
        let body_scope = TableEffectScope::row(TableSection::Body, 0);

        let header = resolver.resolve(base, header_scope, 0.5);
        let body = resolver.resolve(base, body_scope, 0.5);
        assert_eq!(header, base);
        assert_eq!(body.fg, Some(PackedRgba::rgb(200, 0, 0)));
    }

    #[test]
    fn effect_resolver_all_cells_includes_header_rows() {
        let base = Style::new().fg(PackedRgba::rgb(10, 10, 10));
        let mut theme = TableTheme::aurora();
        // pulse_effect(fg, bg) - fg_a=fg_b=first_param, bg_a=bg_b=second_param
        theme.effects = vec![TableEffectRule::new(
            TableEffectTarget::AllCells,
            pulse_effect(PackedRgba::rgb(0, 200, 0), PackedRgba::rgb(5, 5, 5)),
        )];

        let resolver = theme.effect_resolver();
        let header_scope = TableEffectScope::row(TableSection::Header, 0);
        let resolved = resolver.resolve(base, header_scope, 0.5);
        assert_eq!(resolved.fg, Some(PackedRgba::rgb(0, 200, 0)));
    }

    #[test]
    fn normalize_phase_wraps_and_curves_are_deterministic() {
        assert_f32_near("normalize_phase(-0.25)", normalize_phase(-0.25), 0.75);
        assert_f32_near("normalize_phase(1.25)", normalize_phase(1.25), 0.25);
        assert_f32_near("pulse_curve(0.0)", pulse_curve(0.0), 0.0);
        assert_f32_near("pulse_curve(0.5)", pulse_curve(0.5), 1.0);
        assert_f32_near(
            "breathing_curve matches pulse at zero asymmetry",
            breathing_curve(0.25, 0.0),
            pulse_curve(0.25),
        );
    }

    #[test]
    fn lerp_color_clamps_out_of_range_t() {
        let a = PackedRgba::rgb(0, 0, 0);
        let b = PackedRgba::rgb(255, 255, 255);
        assert_eq!(lerp_color(a, b, -1.0), a);
        assert_eq!(lerp_color(a, b, 2.0), b);
    }

    #[test]
    fn effect_resolver_respects_priority_order() {
        let base = Style::new()
            .fg(PackedRgba::rgb(10, 10, 10))
            .bg(PackedRgba::rgb(20, 20, 20));
        let mut theme = TableTheme::aurora();
        theme.effects = vec![
            TableEffectRule::new(
                TableEffectTarget::AllRows,
                pulse_effect(PackedRgba::rgb(200, 0, 0), PackedRgba::rgb(0, 0, 0)),
            )
            .priority(0),
            TableEffectRule::new(
                TableEffectTarget::AllRows,
                pulse_effect(PackedRgba::rgb(0, 0, 200), PackedRgba::rgb(0, 0, 80)),
            )
            .priority(5),
        ];

        let resolver = theme.effect_resolver();
        let scope = TableEffectScope::row(TableSection::Body, 0);
        let resolved = resolver.resolve(base, scope, 0.0);
        assert_eq!(resolved.fg, Some(PackedRgba::rgb(0, 0, 200)));
        assert_eq!(resolved.bg, Some(PackedRgba::rgb(0, 0, 80)));
    }

    #[test]
    fn effect_resolver_applies_same_priority_in_list_order() {
        let base = Style::new().fg(PackedRgba::rgb(5, 5, 5));
        let mut theme = TableTheme::aurora();
        theme.effects = vec![
            TableEffectRule::new(
                TableEffectTarget::Row(0),
                pulse_effect(PackedRgba::rgb(10, 10, 10), PackedRgba::BLACK),
            )
            .priority(1),
            TableEffectRule::new(
                TableEffectTarget::Row(0),
                pulse_effect(PackedRgba::rgb(40, 40, 40), PackedRgba::BLACK),
            )
            .priority(1),
        ];

        let resolver = theme.effect_resolver();
        let scope = TableEffectScope::row(TableSection::Body, 0);
        let resolved = resolver.resolve(base, scope, 0.0);
        assert_eq!(resolved.fg, Some(PackedRgba::rgb(40, 40, 40)));
    }

    #[test]
    fn effect_resolver_respects_style_mask() {
        let base = Style::new()
            .fg(PackedRgba::rgb(10, 20, 30))
            .bg(PackedRgba::rgb(1, 2, 3));
        let mut theme = TableTheme::aurora();
        theme.effects = vec![
            TableEffectRule::new(
                TableEffectTarget::Row(0),
                pulse_effect(PackedRgba::rgb(200, 100, 0), PackedRgba::rgb(9, 9, 9)),
            )
            .style_mask(StyleMask::none()),
        ];

        let resolver = theme.effect_resolver();
        let scope = TableEffectScope::row(TableSection::Body, 0);
        let resolved = resolver.resolve(base, scope, 0.0);
        assert_eq!(resolved, base);

        theme.effects = vec![
            TableEffectRule::new(
                TableEffectTarget::Row(0),
                pulse_effect(PackedRgba::rgb(200, 100, 0), PackedRgba::rgb(9, 9, 9)),
            )
            .style_mask(StyleMask {
                fg: true,
                bg: false,
                attrs: false,
            }),
        ];
        let resolver = theme.effect_resolver();
        let resolved = resolver.resolve(base, scope, 0.0);
        assert_eq!(resolved.fg, Some(PackedRgba::rgb(200, 100, 0)));
        assert_eq!(resolved.bg, base.bg);
    }

    #[test]
    fn effect_resolver_skips_alpha_zero() {
        let base = Style::new()
            .fg(PackedRgba::rgb(10, 10, 10))
            .bg(PackedRgba::rgb(20, 20, 20));
        let mut theme = TableTheme::aurora();
        theme.effects = vec![TableEffectRule::new(
            TableEffectTarget::Row(0),
            TableEffect::BreathingGlow {
                fg: PackedRgba::rgb(200, 200, 200),
                bg: PackedRgba::rgb(10, 10, 10),
                intensity: 0.0,
                speed: 1.0,
                phase_offset: 0.0,
                asymmetry: 0.0,
            },
        )];

        let resolver = theme.effect_resolver();
        let scope = TableEffectScope::row(TableSection::Body, 0);
        let resolved = resolver.resolve(base, scope, 0.5);
        assert_eq!(resolved, base);
    }

    #[test]
    fn presets_set_preset_id() {
        let theme = TableTheme::aurora();
        assert_eq!(theme.preset_id, Some(TablePresetId::Aurora));
    }

    #[test]
    fn terminal_classic_keeps_profile() {
        let theme = TableTheme::terminal_classic_for(ColorProfile::Ansi16);
        assert_eq!(theme.preset_id, Some(TablePresetId::TerminalClassic));
        assert!(theme.column_gap > 0);
    }

    #[test]
    fn style_hash_is_deterministic() {
        let theme = TableTheme::aurora();
        let h1 = theme.style_hash();
        let h2 = theme.style_hash();
        assert_eq!(h1, h2, "style_hash should be stable for identical input");
    }

    #[test]
    fn style_hash_changes_with_layout_params() {
        let mut theme = TableTheme::aurora();
        let base = theme.style_hash();
        theme.padding = theme.padding.saturating_add(1);
        assert_ne!(
            base,
            theme.style_hash(),
            "padding should influence style hash"
        );
    }

    #[test]
    fn effects_hash_changes_with_rules() {
        let mut theme = TableTheme::aurora();
        let base = theme.effects_hash();
        theme.effects.push(TableEffectRule::new(
            TableEffectTarget::AllRows,
            TableEffect::BreathingGlow {
                fg: PackedRgba::rgb(200, 220, 255),
                bg: PackedRgba::rgb(30, 40, 60),
                intensity: 0.6,
                speed: 0.8,
                phase_offset: 0.1,
                asymmetry: 0.2,
            },
        ));
        assert_ne!(
            base,
            theme.effects_hash(),
            "effects hash should change with rules"
        );
    }

    #[test]
    fn presets_meet_wcag_contrast_targets() {
        let presets = [
            TablePresetId::Aurora,
            TablePresetId::Graphite,
            TablePresetId::Neon,
            TablePresetId::Slate,
            TablePresetId::Solar,
            TablePresetId::Orchard,
            TablePresetId::Paper,
            TablePresetId::Midnight,
            TablePresetId::TerminalClassic,
        ];

        for preset in presets {
            let theme = match preset {
                TablePresetId::TerminalClassic => {
                    TableTheme::terminal_classic_for(ColorProfile::Ansi16)
                }
                _ => TableTheme::preset(preset),
            };
            let base = base_bg(&theme);

            let header_fg = expect_fg(preset, "header", theme.header);
            let header_bg = expect_bg(preset, "header", theme.header);
            assert_contrast(preset, "header", header_fg, header_bg, WCAG_AA_NORMAL_TEXT);

            let row_fg = expect_fg(preset, "row", theme.row);
            let row_bg = theme.row.bg.unwrap_or(base);
            assert_contrast(preset, "row", row_fg, row_bg, WCAG_AA_NORMAL_TEXT);

            let row_alt_fg = expect_fg(preset, "row_alt", theme.row_alt);
            let row_alt_bg = expect_bg(preset, "row_alt", theme.row_alt);
            assert_contrast(
                preset,
                "row_alt",
                row_alt_fg,
                row_alt_bg,
                WCAG_AA_NORMAL_TEXT,
            );

            let selected_fg = expect_fg(preset, "row_selected", theme.row_selected);
            let selected_bg = expect_bg(preset, "row_selected", theme.row_selected);
            assert_contrast(
                preset,
                "row_selected",
                selected_fg,
                selected_bg,
                WCAG_AA_NORMAL_TEXT,
            );

            let hover_fg = expect_fg(preset, "row_hover", theme.row_hover);
            let hover_bg = expect_bg(preset, "row_hover", theme.row_hover);
            let hover_min = if preset == TablePresetId::TerminalClassic {
                // ANSI16 hover colors are bounded; accept AA large-text threshold.
                WCAG_AA_LARGE_TEXT
            } else {
                WCAG_AA_NORMAL_TEXT
            };
            assert_contrast(preset, "row_hover", hover_fg, hover_bg, hover_min);

            let border_fg = expect_fg(preset, "border", theme.border);
            assert_contrast(preset, "border", border_fg, base, WCAG_AA_LARGE_TEXT);

            let divider_fg = expect_fg(preset, "divider", theme.divider);
            assert_contrast(preset, "divider", divider_fg, base, WCAG_AA_LARGE_TEXT);
        }
    }

    fn base_spec() -> TableThemeSpec {
        TableThemeSpec::from_theme(&TableTheme::aurora())
    }

    fn sample_rule() -> TableEffectRuleSpec {
        TableEffectRuleSpec {
            target: TableEffectTarget::AllRows,
            effect: TableEffectSpec::Pulse {
                fg_a: RgbaSpec::new(10, 20, 30, 255),
                fg_b: RgbaSpec::new(40, 50, 60, 255),
                bg_a: RgbaSpec::new(5, 5, 5, 255),
                bg_b: RgbaSpec::new(9, 9, 9, 255),
                speed: 1.0,
                phase_offset: 0.0,
            },
            priority: 0,
            blend_mode: BlendMode::Replace,
            style_mask: StyleMask::fg_bg(),
        }
    }

    #[test]
    fn table_theme_spec_validate_accepts_defaults() {
        let spec = base_spec();
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn table_theme_spec_validate_rejects_padding_overflow() {
        let mut spec = base_spec();
        spec.padding = TABLE_THEME_SPEC_MAX_PADDING.saturating_add(1);
        let err = spec.validate().expect_err("expected padding range error");
        assert_eq!(err.field, "padding");
    }

    #[test]
    fn table_theme_spec_validate_rejects_name_length_overflow() {
        let mut spec = base_spec();
        spec.name = Some("x".repeat(TABLE_THEME_SPEC_MAX_NAME_LEN.saturating_add(1)));
        let err = spec.validate().expect_err("expected name length error");
        assert_eq!(err.field, "name");
    }

    #[test]
    fn table_theme_spec_validate_rejects_effect_count_overflow() {
        let mut spec = base_spec();
        spec.effects = vec![sample_rule(); TABLE_THEME_SPEC_MAX_EFFECTS.saturating_add(1)];
        let err = spec.validate().expect_err("expected effects length error");
        assert_eq!(err.field, "effects");
    }

    #[test]
    fn table_theme_spec_validate_rejects_style_attr_overflow() {
        let mut spec = base_spec();
        spec.styles.header.attrs =
            vec![StyleAttr::Bold; TABLE_THEME_SPEC_MAX_STYLE_ATTRS.saturating_add(1)];
        let err = spec
            .validate()
            .expect_err("expected style attr length error");
        assert_eq!(err.field, "styles.header.attrs");
    }

    #[test]
    fn table_theme_spec_validate_rejects_gradient_stop_count_out_of_range() {
        let mut spec = base_spec();
        spec.effects = vec![TableEffectRuleSpec {
            target: TableEffectTarget::AllRows,
            effect: TableEffectSpec::GradientSweep {
                gradient: GradientSpec { stops: Vec::new() },
                speed: 1.0,
                phase_offset: 0.0,
            },
            priority: 0,
            blend_mode: BlendMode::Replace,
            style_mask: StyleMask::fg_bg(),
        }];
        let err = spec
            .validate()
            .expect_err("expected gradient stop count error");
        assert!(
            err.field.contains("gradient.stops"),
            "unexpected field: {}",
            err.field
        );
    }

    #[test]
    fn table_theme_spec_validate_rejects_gradient_stop_out_of_range() {
        let mut spec = base_spec();
        spec.effects = vec![TableEffectRuleSpec {
            target: TableEffectTarget::AllRows,
            effect: TableEffectSpec::GradientSweep {
                gradient: GradientSpec {
                    stops: vec![GradientStopSpec {
                        pos: 1.5,
                        color: RgbaSpec::new(0, 0, 0, 255),
                    }],
                },
                speed: 1.0,
                phase_offset: 0.0,
            },
            priority: 0,
            blend_mode: BlendMode::Replace,
            style_mask: StyleMask::fg_bg(),
        }];
        let err = spec
            .validate()
            .expect_err("expected gradient stop range error");
        assert!(
            err.field.contains("gradient.stops"),
            "unexpected field: {}",
            err.field
        );
    }

    #[test]
    fn table_theme_spec_validate_rejects_inverted_row_range() {
        let mut spec = base_spec();
        let mut rule = sample_rule();
        rule.target = TableEffectTarget::RowRange { start: 3, end: 1 };
        spec.effects = vec![rule];
        let err = spec.validate().expect_err("expected target range error");
        assert!(
            err.field.contains("target"),
            "unexpected field: {}",
            err.field
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn table_theme_spec_json_rejects_unknown_field() {
        let mut value = serde_json::to_value(base_spec()).expect("TableThemeSpec should serialize");
        let obj = value.as_object_mut().expect("spec should be an object");
        obj.insert("unknown_field".to_string(), serde_json::json!(true));
        let err = serde_json::from_value::<TableThemeSpec>(value)
            .expect_err("expected unknown field error");
        assert!(
            err.to_string().contains("unknown field"),
            "unexpected error: {err}"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn table_theme_spec_json_has_canonical_key_order() {
        let json =
            serde_json::to_string_pretty(&base_spec()).expect("TableThemeSpec should serialize");
        let keys = [
            "\"version\"",
            "\"name\"",
            "\"preset_id\"",
            "\"padding\"",
            "\"column_gap\"",
            "\"row_height\"",
            "\"styles\"",
            "\"effects\"",
        ];
        let mut last = 0usize;
        for key in keys {
            let pos = json.find(key);
            assert!(pos.is_some(), "missing key {key}");
            let pos = pos.unwrap();
            assert!(
                pos >= last,
                "key {key} is out of order (pos {pos} < {last})"
            );
            last = pos;
        }
    }

    // =========================================================================
    // Gradient tests
    // =========================================================================

    #[test]
    fn gradient_empty_returns_transparent() {
        let g = Gradient::new(vec![]);
        let c = g.sample(0.5);
        assert_eq!(c, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn gradient_single_stop_returns_that_color() {
        let red = PackedRgba::rgb(255, 0, 0);
        let g = Gradient::new(vec![(0.5, red)]);
        assert_eq!(g.sample(0.0), red);
        assert_eq!(g.sample(0.5), red);
        assert_eq!(g.sample(1.0), red);
    }

    #[test]
    fn gradient_two_stops_interpolates() {
        let black = PackedRgba::rgb(0, 0, 0);
        let white = PackedRgba::rgb(255, 255, 255);
        let g = Gradient::new(vec![(0.0, black), (1.0, white)]);
        let mid = g.sample(0.5);
        // Mid should be roughly 127-128
        assert!(mid.r() > 120 && mid.r() < 135, "mid.r() = {}", mid.r());
    }

    #[test]
    fn gradient_sorts_stops() {
        let a = PackedRgba::rgb(255, 0, 0);
        let b = PackedRgba::rgb(0, 255, 0);
        let g = Gradient::new(vec![(0.8, b), (0.2, a)]);
        let stops = g.stops();
        assert!(stops[0].0 < stops[1].0, "stops should be sorted");
    }

    #[test]
    fn gradient_clamps_t() {
        let red = PackedRgba::rgb(255, 0, 0);
        let blue = PackedRgba::rgb(0, 0, 255);
        let g = Gradient::new(vec![(0.0, red), (1.0, blue)]);
        assert_eq!(g.sample(-1.0), red);
        assert_eq!(g.sample(2.0), blue);
    }

    // =========================================================================
    // lerp_u8 tests
    // =========================================================================

    #[test]
    fn lerp_u8_basic() {
        assert_eq!(lerp_u8(0, 100, 0.0), 0);
        assert_eq!(lerp_u8(0, 100, 1.0), 100);
        assert_eq!(lerp_u8(0, 100, 0.5), 50);
    }

    #[test]
    fn lerp_u8_clamps() {
        // t out of range should still produce valid u8
        assert_eq!(lerp_u8(100, 200, -1.0), 0);
        assert_eq!(lerp_u8(0, 100, 2.0), 200);
    }

    // =========================================================================
    // StyleMask tests
    // =========================================================================

    #[test]
    fn style_mask_all() {
        let mask = StyleMask::all();
        assert!(mask.fg);
        assert!(mask.bg);
        assert!(mask.attrs);
    }

    #[test]
    fn style_mask_none() {
        let mask = StyleMask::none();
        assert!(!mask.fg);
        assert!(!mask.bg);
        assert!(!mask.attrs);
    }

    #[test]
    fn style_mask_fg_bg_no_attrs() {
        let mask = StyleMask::fg_bg();
        assert!(mask.fg);
        assert!(mask.bg);
        assert!(!mask.attrs);
    }

    // =========================================================================
    // TableEffectRule builder tests
    // =========================================================================

    #[test]
    fn effect_rule_defaults() {
        let rule = TableEffectRule::new(
            TableEffectTarget::AllRows,
            pulse_effect(PackedRgba::RED, PackedRgba::BLACK),
        );
        assert_eq!(rule.priority, 0);
        assert_eq!(rule.blend_mode, BlendMode::Replace);
        assert_eq!(rule.style_mask, StyleMask::fg_bg());
    }

    #[test]
    fn effect_rule_builder_chain() {
        let rule = TableEffectRule::new(
            TableEffectTarget::AllRows,
            pulse_effect(PackedRgba::RED, PackedRgba::BLACK),
        )
        .priority(5)
        .blend_mode(BlendMode::Additive)
        .style_mask(StyleMask::all());

        assert_eq!(rule.priority, 5);
        assert_eq!(rule.blend_mode, BlendMode::Additive);
        assert_eq!(rule.style_mask, StyleMask::all());
    }

    // =========================================================================
    // TableTheme builder tests
    // =========================================================================

    #[test]
    fn default_theme_is_graphite() {
        let theme = TableTheme::default();
        assert_eq!(theme.preset_id, Some(TablePresetId::Graphite));
    }

    #[test]
    fn preset_factory_matches_named() {
        let from_preset = TableTheme::preset(TablePresetId::Aurora);
        let from_named = TableTheme::aurora();
        assert_eq!(from_preset.style_hash(), from_named.style_hash());
    }

    #[test]
    fn with_padding_sets_value() {
        let theme = TableTheme::graphite().with_padding(3);
        assert_eq!(theme.padding, 3);
    }

    #[test]
    fn with_column_gap_sets_value() {
        let theme = TableTheme::graphite().with_column_gap(5);
        assert_eq!(theme.column_gap, 5);
    }

    #[test]
    fn with_row_height_sets_value() {
        let theme = TableTheme::graphite().with_row_height(2);
        assert_eq!(theme.row_height, 2);
    }

    #[test]
    fn with_effect_appends() {
        let theme = TableTheme::graphite().with_effect(TableEffectRule::new(
            TableEffectTarget::AllRows,
            pulse_effect(PackedRgba::RED, PackedRgba::BLACK),
        ));
        assert_eq!(theme.effects.len(), 1);
    }

    #[test]
    fn clear_effects_removes_all() {
        let theme = TableTheme::graphite()
            .with_effect(TableEffectRule::new(
                TableEffectTarget::AllRows,
                pulse_effect(PackedRgba::RED, PackedRgba::BLACK),
            ))
            .clear_effects();
        assert!(theme.effects.is_empty());
    }

    #[test]
    fn with_preset_id_overrides() {
        let theme = TableTheme::graphite().with_preset_id(Some(TablePresetId::Neon));
        assert_eq!(theme.preset_id, Some(TablePresetId::Neon));
    }

    // =========================================================================
    // Diagnostics tests
    // =========================================================================

    #[test]
    fn diagnostics_captures_theme_state() {
        let theme = TableTheme::aurora()
            .with_padding(4)
            .with_effect(TableEffectRule::new(
                TableEffectTarget::AllRows,
                pulse_effect(PackedRgba::RED, PackedRgba::BLACK),
            ));
        let diag = theme.diagnostics();
        assert_eq!(diag.preset_id, Some(TablePresetId::Aurora));
        assert_eq!(diag.padding, 4);
        assert_eq!(diag.effect_count, 1);
        assert_ne!(diag.style_hash, 0);
    }

    // =========================================================================
    // TableEffectScope tests
    // =========================================================================

    #[test]
    fn scope_section_has_no_row_or_column() {
        let s = TableEffectScope::section(TableSection::Header);
        assert_eq!(s.section, TableSection::Header);
        assert_eq!(s.row, None);
        assert_eq!(s.column, None);
    }

    #[test]
    fn scope_row_has_row_no_column() {
        let s = TableEffectScope::row(TableSection::Body, 3);
        assert_eq!(s.row, Some(3));
        assert_eq!(s.column, None);
    }

    #[test]
    fn scope_column_has_column_no_row() {
        let s = TableEffectScope::column(TableSection::Footer, 7);
        assert_eq!(s.column, Some(7));
        assert_eq!(s.row, None);
    }

    // =========================================================================
    // Curve function tests
    // =========================================================================

    #[test]
    fn pulse_curve_boundaries() {
        assert_f32_near("pulse(0.0)", pulse_curve(0.0), 0.0);
        assert_f32_near("pulse(0.5)", pulse_curve(0.5), 1.0);
        assert_f32_near("pulse(1.0)", pulse_curve(1.0), 0.0);
    }

    #[test]
    fn breathing_curve_zero_asymmetry_matches_pulse() {
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert_f32_near(
                &format!("breathing({t})"),
                breathing_curve(t, 0.0),
                pulse_curve(t),
            );
        }
    }

    #[test]
    fn skew_phase_zero_is_identity() {
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert_f32_near(&format!("skew({t})"), skew_phase(t, 0.0), t);
        }
    }

    #[test]
    fn skew_phase_positive_slows_start() {
        // With positive asymmetry, early phases should be compressed
        let skewed = skew_phase(0.5, 0.5);
        assert!(
            skewed < 0.5,
            "positive skew should compress early phase: {skewed}"
        );
    }

    #[test]
    fn skew_phase_negative_accelerates_start() {
        let skewed = skew_phase(0.5, -0.5);
        assert!(
            skewed > 0.5,
            "negative skew should expand early phase: {skewed}"
        );
    }

    // =========================================================================
    // Blend mode tests (via effect resolver)
    // =========================================================================

    #[test]
    fn effect_resolver_additive_blend() {
        let base_fg = PackedRgba::rgb(100, 50, 50);
        let effect_fg = PackedRgba::rgb(50, 50, 50);
        let theme = TableTheme::graphite().with_effect(
            TableEffectRule::new(
                TableEffectTarget::AllRows,
                TableEffect::Pulse {
                    fg_a: effect_fg,
                    fg_b: effect_fg,
                    bg_a: PackedRgba::BLACK,
                    bg_b: PackedRgba::BLACK,
                    speed: 1.0,
                    phase_offset: 0.0,
                },
            )
            .blend_mode(BlendMode::Additive),
        );
        let resolver = theme.effect_resolver();
        let base = Style::new().fg(base_fg).bg(PackedRgba::BLACK);
        let resolved = resolver.resolve(base, TableEffectScope::row(TableSection::Body, 0), 0.25);
        // Additive should increase brightness
        let resolved_fg = resolved.fg.unwrap();
        assert!(
            resolved_fg.r() >= base_fg.r(),
            "additive should brighten red"
        );
    }

    #[test]
    fn effect_resolver_multiply_blend() {
        let base_fg = PackedRgba::rgb(200, 200, 200);
        let effect_fg = PackedRgba::rgb(128, 128, 128);
        let theme = TableTheme::graphite().with_effect(
            TableEffectRule::new(
                TableEffectTarget::AllRows,
                TableEffect::Pulse {
                    fg_a: effect_fg,
                    fg_b: effect_fg,
                    bg_a: PackedRgba::BLACK,
                    bg_b: PackedRgba::BLACK,
                    speed: 1.0,
                    phase_offset: 0.0,
                },
            )
            .blend_mode(BlendMode::Multiply),
        );
        let resolver = theme.effect_resolver();
        let base = Style::new().fg(base_fg).bg(PackedRgba::BLACK);
        let resolved = resolver.resolve(base, TableEffectScope::row(TableSection::Body, 0), 0.25);
        let resolved_fg = resolved.fg.unwrap();
        // Multiply should darken
        assert!(resolved_fg.r() <= base_fg.r(), "multiply should darken");
    }

    // =========================================================================
    // Edge-case tests (bd-39605)
    // =========================================================================

    #[test]
    fn gradient_coincident_stops_returns_first() {
        let a = PackedRgba::rgb(255, 0, 0);
        let b = PackedRgba::rgb(0, 0, 255);
        let g = Gradient::new(vec![(0.5, a), (0.5, b)]);
        // t <= first.0 (0.5 <= 0.5) → early-return with first stop
        let c = g.sample(0.5);
        assert_eq!(c, a);
        // Before the coincident position, also returns first stop
        let c_before = g.sample(0.3);
        assert_eq!(c_before, a);
    }

    #[test]
    fn gradient_three_stops_middle_interpolation() {
        let r = PackedRgba::rgb(255, 0, 0);
        let g_color = PackedRgba::rgb(0, 255, 0);
        let b = PackedRgba::rgb(0, 0, 255);
        let g = Gradient::new(vec![(0.0, r), (0.5, g_color), (1.0, b)]);
        // At 0.25, should be between red and green
        let c = g.sample(0.25);
        assert!(c.r() > 100, "should have red component: {}", c.r());
        assert!(c.g() > 100, "should have green component: {}", c.g());
        assert!(c.b() < 10, "should have minimal blue: {}", c.b());
    }

    #[test]
    fn gradient_partial_eq() {
        let a = Gradient::new(vec![(0.0, PackedRgba::RED), (1.0, PackedRgba::BLACK)]);
        let b = Gradient::new(vec![(0.0, PackedRgba::RED), (1.0, PackedRgba::BLACK)]);
        assert_eq!(a, b);
    }

    #[test]
    fn lerp_u8_same_values() {
        assert_eq!(lerp_u8(128, 128, 0.5), 128);
    }

    #[test]
    fn lerp_u8_max_to_max() {
        assert_eq!(lerp_u8(255, 255, 0.5), 255);
    }

    #[test]
    fn lerp_color_exact_midpoint() {
        let a = PackedRgba::rgba(0, 0, 0, 0);
        let b = PackedRgba::rgba(200, 100, 50, 200);
        let mid = lerp_color(a, b, 0.5);
        assert_eq!(mid.r(), 100);
        assert_eq!(mid.g(), 50);
        assert_eq!(mid.b(), 25);
        assert_eq!(mid.a(), 100);
    }

    #[test]
    fn blend_mode_default_is_replace() {
        assert_eq!(BlendMode::default(), BlendMode::Replace);
    }

    #[test]
    fn blend_mode_traits() {
        let mode = BlendMode::Screen;
        let debug = format!("{:?}", mode);
        assert!(debug.contains("Screen"));
        let cloned = mode;
        assert_eq!(mode, cloned);
        assert_ne!(BlendMode::Additive, BlendMode::Multiply);
    }

    #[test]
    fn effect_resolver_screen_blend() {
        let base_fg = PackedRgba::rgb(100, 100, 100);
        let effect_fg = PackedRgba::rgb(128, 128, 128);
        let theme = TableTheme::graphite().with_effect(
            TableEffectRule::new(
                TableEffectTarget::AllRows,
                TableEffect::Pulse {
                    fg_a: effect_fg,
                    fg_b: effect_fg,
                    bg_a: PackedRgba::BLACK,
                    bg_b: PackedRgba::BLACK,
                    speed: 1.0,
                    phase_offset: 0.0,
                },
            )
            .blend_mode(BlendMode::Screen),
        );
        let resolver = theme.effect_resolver();
        let base = Style::new().fg(base_fg).bg(PackedRgba::BLACK);
        let resolved = resolver.resolve(base, TableEffectScope::row(TableSection::Body, 0), 0.25);
        let resolved_fg = resolved.fg.unwrap();
        // Screen should lighten
        assert!(
            resolved_fg.r() >= base_fg.r(),
            "screen should lighten: {} vs {}",
            resolved_fg.r(),
            base_fg.r()
        );
    }

    #[test]
    fn effect_resolver_gradient_sweep() {
        let gradient = Gradient::new(vec![
            (0.0, PackedRgba::rgb(255, 0, 0)),
            (1.0, PackedRgba::rgb(0, 0, 255)),
        ]);
        let theme = TableTheme::graphite().with_effect(TableEffectRule::new(
            TableEffectTarget::AllRows,
            TableEffect::GradientSweep {
                gradient,
                speed: 1.0,
                phase_offset: 0.0,
            },
        ));
        let resolver = theme.effect_resolver();
        let base = Style::new().fg(PackedRgba::BLACK);
        let resolved = resolver.resolve(base, TableEffectScope::row(TableSection::Body, 0), 0.0);
        // At phase 0.0 with speed 1.0, gradient samples at 0.0 → red
        let fg = resolved.fg.unwrap();
        assert_eq!(fg.r(), 255);
        assert_eq!(fg.b(), 0);
    }

    #[test]
    fn effect_resolver_breathing_glow_with_asymmetry() {
        let theme = TableTheme::graphite().with_effect(TableEffectRule::new(
            TableEffectTarget::AllRows,
            TableEffect::BreathingGlow {
                fg: PackedRgba::rgb(255, 255, 255),
                bg: PackedRgba::rgb(50, 50, 50),
                intensity: 1.0,
                speed: 1.0,
                phase_offset: 0.0,
                asymmetry: 0.5,
            },
        ));
        let resolver = theme.effect_resolver();
        let base = Style::new()
            .fg(PackedRgba::rgb(100, 100, 100))
            .bg(PackedRgba::rgb(20, 20, 20));
        // Should not panic with asymmetry
        let _resolved = resolver.resolve(base, TableEffectScope::row(TableSection::Body, 0), 0.25);
    }

    #[test]
    fn apply_channel_none_base_uses_effect() {
        let result = apply_channel(
            None,
            Some(PackedRgba::rgb(100, 200, 50)),
            1.0,
            BlendMode::Replace,
        );
        assert!(result.is_some());
        let c = result.unwrap();
        assert_eq!(c.r(), 100);
        assert_eq!(c.g(), 200);
        assert_eq!(c.b(), 50);
    }

    #[test]
    fn apply_channel_none_effect_returns_none() {
        let result = apply_channel(Some(PackedRgba::RED), None, 1.0, BlendMode::Replace);
        assert!(result.is_none());
    }

    #[test]
    fn spec_round_trip_preserves_theme() {
        let theme = TableTheme::aurora()
            .with_padding(3)
            .with_column_gap(2)
            .with_row_height(2)
            .with_preset_id(Some(TablePresetId::Aurora));
        let spec = TableThemeSpec::from_theme(&theme);
        let restored = spec.into_theme();
        assert_eq!(restored.padding, 3);
        assert_eq!(restored.column_gap, 2);
        assert_eq!(restored.row_height, 2);
        assert_eq!(restored.preset_id, Some(TablePresetId::Aurora));
        assert_eq!(restored.style_hash(), theme.style_hash());
    }

    #[test]
    fn spec_round_trip_with_effects() {
        let theme = TableTheme::neon().with_effect(TableEffectRule::new(
            TableEffectTarget::Row(5),
            TableEffect::Pulse {
                fg_a: PackedRgba::rgb(10, 20, 30),
                fg_b: PackedRgba::rgb(40, 50, 60),
                bg_a: PackedRgba::rgb(1, 2, 3),
                bg_b: PackedRgba::rgb(4, 5, 6),
                speed: 2.0,
                phase_offset: 0.3,
            },
        ));
        let spec = TableThemeSpec::from_theme(&theme);
        let restored = spec.into_theme();
        assert_eq!(restored.effects.len(), 1);
        assert_eq!(restored.effects_hash(), theme.effects_hash());
    }

    #[test]
    fn spec_validate_rejects_bad_version() {
        let mut spec = base_spec();
        spec.version = 99;
        let err = spec.validate().expect_err("expected version error");
        assert_eq!(err.field, "version");
    }

    #[test]
    fn spec_validate_rejects_row_height_zero() {
        let mut spec = base_spec();
        spec.row_height = 0;
        let err = spec.validate().expect_err("expected row_height error");
        assert_eq!(err.field, "row_height");
    }

    #[test]
    fn spec_validate_rejects_column_gap_overflow() {
        let mut spec = base_spec();
        spec.column_gap = TABLE_THEME_SPEC_MAX_COLUMN_GAP + 1;
        let err = spec.validate().expect_err("expected column_gap error");
        assert_eq!(err.field, "column_gap");
    }

    #[test]
    fn spec_validate_rejects_inverted_column_range() {
        let mut spec = base_spec();
        let mut rule = sample_rule();
        rule.target = TableEffectTarget::ColumnRange { start: 5, end: 2 };
        spec.effects = vec![rule];
        let err = spec.validate().expect_err("expected column range error");
        assert!(err.field.contains("target"), "field: {}", err.field);
    }

    #[test]
    fn spec_validate_rejects_nan_speed() {
        let mut spec = base_spec();
        spec.effects = vec![TableEffectRuleSpec {
            target: TableEffectTarget::AllRows,
            effect: TableEffectSpec::Pulse {
                fg_a: RgbaSpec::new(0, 0, 0, 255),
                fg_b: RgbaSpec::new(0, 0, 0, 255),
                bg_a: RgbaSpec::new(0, 0, 0, 255),
                bg_b: RgbaSpec::new(0, 0, 0, 255),
                speed: f32::NAN,
                phase_offset: 0.0,
            },
            priority: 0,
            blend_mode: BlendMode::Replace,
            style_mask: StyleMask::fg_bg(),
        }];
        let err = spec.validate().expect_err("expected NaN error");
        assert!(err.message.contains("finite"), "error msg: {}", err.message);
    }

    #[test]
    fn spec_validate_rejects_inf_intensity() {
        let mut spec = base_spec();
        spec.effects = vec![TableEffectRuleSpec {
            target: TableEffectTarget::AllRows,
            effect: TableEffectSpec::BreathingGlow {
                fg: RgbaSpec::new(0, 0, 0, 255),
                bg: RgbaSpec::new(0, 0, 0, 255),
                intensity: f32::INFINITY,
                speed: 1.0,
                phase_offset: 0.0,
                asymmetry: 0.0,
            },
            priority: 0,
            blend_mode: BlendMode::Replace,
            style_mask: StyleMask::fg_bg(),
        }];
        let err = spec.validate().expect_err("expected Inf error");
        assert!(err.message.contains("finite"), "error msg: {}", err.message);
    }

    #[test]
    fn style_spec_round_trip() {
        let style = Style::new()
            .fg(PackedRgba::rgb(100, 150, 200))
            .bg(PackedRgba::rgb(10, 20, 30))
            .bold()
            .italic();
        let spec = StyleSpec::from_style(&style);
        let restored = spec.to_style();
        assert_eq!(restored.fg, style.fg);
        assert_eq!(restored.bg, style.bg);
        assert!(restored.has_attr(StyleFlags::BOLD));
        assert!(restored.has_attr(StyleFlags::ITALIC));
    }

    #[test]
    fn gradient_spec_round_trip() {
        let gradient = Gradient::new(vec![
            (0.0, PackedRgba::rgb(255, 0, 0)),
            (0.5, PackedRgba::rgb(0, 255, 0)),
            (1.0, PackedRgba::rgb(0, 0, 255)),
        ]);
        let spec = GradientSpec::from_gradient(&gradient);
        let restored = spec.to_gradient();
        assert_eq!(restored.stops().len(), 3);
        assert_eq!(restored.sample(0.0), gradient.sample(0.0));
        assert_eq!(restored.sample(1.0), gradient.sample(1.0));
    }

    #[test]
    fn effect_spec_round_trip_pulse() {
        let effect = TableEffect::Pulse {
            fg_a: PackedRgba::rgb(10, 20, 30),
            fg_b: PackedRgba::rgb(40, 50, 60),
            bg_a: PackedRgba::rgb(1, 2, 3),
            bg_b: PackedRgba::rgb(4, 5, 6),
            speed: 1.5,
            phase_offset: 0.2,
        };
        let spec = TableEffectSpec::from_effect(&effect);
        let _restored = spec.to_effect(); // Should not panic
    }

    #[test]
    fn effect_spec_round_trip_breathing() {
        let effect = TableEffect::BreathingGlow {
            fg: PackedRgba::rgb(200, 200, 200),
            bg: PackedRgba::rgb(10, 10, 10),
            intensity: 0.7,
            speed: 2.0,
            phase_offset: 0.5,
            asymmetry: -0.3,
        };
        let spec = TableEffectSpec::from_effect(&effect);
        let _restored = spec.to_effect();
    }

    #[test]
    fn effect_spec_round_trip_gradient_sweep() {
        let effect = TableEffect::GradientSweep {
            gradient: Gradient::new(vec![(0.0, PackedRgba::RED), (1.0, PackedRgba::BLACK)]),
            speed: 1.0,
            phase_offset: 0.0,
        };
        let spec = TableEffectSpec::from_effect(&effect);
        let _restored = spec.to_effect();
    }

    #[test]
    fn effect_rule_spec_round_trip() {
        let rule = TableEffectRule::new(
            TableEffectTarget::RowRange { start: 1, end: 5 },
            pulse_effect(PackedRgba::RED, PackedRgba::BLACK),
        )
        .priority(3)
        .blend_mode(BlendMode::Screen)
        .style_mask(StyleMask::all());
        let spec = TableEffectRuleSpec::from_rule(&rule);
        let restored = spec.to_rule();
        assert_eq!(restored.priority, 3);
        assert_eq!(restored.blend_mode, BlendMode::Screen);
        assert_eq!(restored.style_mask, StyleMask::all());
    }

    #[test]
    fn attrs_flags_round_trip_all() {
        let flags = StyleFlags::BOLD
            | StyleFlags::DIM
            | StyleFlags::ITALIC
            | StyleFlags::UNDERLINE
            | StyleFlags::BLINK
            | StyleFlags::REVERSE
            | StyleFlags::HIDDEN
            | StyleFlags::STRIKETHROUGH
            | StyleFlags::DOUBLE_UNDERLINE
            | StyleFlags::CURLY_UNDERLINE;
        let attrs = attrs_from_flags(flags);
        assert_eq!(attrs.len(), 10);
        let restored = flags_from_attrs(&attrs);
        assert_eq!(restored, Some(flags));
    }

    #[test]
    fn flags_from_empty_attrs_returns_none() {
        let result = flags_from_attrs(&[]);
        assert!(result.is_none());
    }

    #[test]
    fn table_theme_spec_error_display() {
        let err = TableThemeSpecError::new("test_field", "something went wrong");
        let display = format!("{}", err);
        assert_eq!(display, "test_field: something went wrong");
        let debug = format!("{:?}", err);
        assert!(debug.contains("TableThemeSpecError"));
    }

    #[test]
    fn table_theme_spec_error_is_std_error() {
        let err = TableThemeSpecError::new("f", "m");
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn table_theme_diagnostics_clone_and_debug() {
        let theme = TableTheme::aurora();
        let diag = theme.diagnostics();
        let cloned = diag.clone();
        assert_eq!(cloned.preset_id, diag.preset_id);
        let debug = format!("{:?}", diag);
        assert!(debug.contains("TableThemeDiagnostics"));
    }

    #[test]
    fn rgba_spec_round_trip() {
        let packed = PackedRgba::rgba(10, 20, 30, 40);
        let spec = RgbaSpec::from(packed);
        assert_eq!(spec.r, 10);
        assert_eq!(spec.g, 20);
        assert_eq!(spec.b, 30);
        assert_eq!(spec.a, 40);
        let restored = PackedRgba::from(spec);
        assert_eq!(restored, packed);
    }

    #[test]
    fn with_builders_all_styles() {
        let s = Style::new().fg(PackedRgba::RED);
        let theme = TableTheme::graphite()
            .with_border(s)
            .with_header(s)
            .with_row(s)
            .with_row_alt(s)
            .with_row_selected(s)
            .with_row_hover(s)
            .with_divider(s);
        assert_eq!(theme.border.fg, Some(PackedRgba::RED));
        assert_eq!(theme.header.fg, Some(PackedRgba::RED));
        assert_eq!(theme.row.fg, Some(PackedRgba::RED));
        assert_eq!(theme.row_alt.fg, Some(PackedRgba::RED));
        assert_eq!(theme.row_selected.fg, Some(PackedRgba::RED));
        assert_eq!(theme.row_hover.fg, Some(PackedRgba::RED));
        assert_eq!(theme.divider.fg, Some(PackedRgba::RED));
    }

    #[test]
    fn with_effects_replaces_all() {
        let theme = TableTheme::graphite()
            .with_effect(TableEffectRule::new(
                TableEffectTarget::AllRows,
                pulse_effect(PackedRgba::RED, PackedRgba::BLACK),
            ))
            .with_effects(vec![]);
        assert!(theme.effects.is_empty());
    }

    #[test]
    fn different_presets_have_different_hashes() {
        let aurora = TableTheme::aurora().style_hash();
        let neon = TableTheme::neon().style_hash();
        let graphite = TableTheme::graphite().style_hash();
        assert_ne!(aurora, neon);
        assert_ne!(aurora, graphite);
        assert_ne!(neon, graphite);
    }

    #[test]
    fn all_presets_construct_without_panic() {
        let presets = [
            TablePresetId::Aurora,
            TablePresetId::Graphite,
            TablePresetId::Neon,
            TablePresetId::Slate,
            TablePresetId::Solar,
            TablePresetId::Orchard,
            TablePresetId::Paper,
            TablePresetId::Midnight,
            TablePresetId::TerminalClassic,
        ];
        for id in presets {
            let theme = TableTheme::preset(id);
            assert_eq!(theme.preset_id, Some(id));
            assert_eq!(theme.padding, 1);
            assert_eq!(theme.column_gap, 1);
            assert_eq!(theme.row_height, 1);
            assert!(theme.effects.is_empty());
        }
    }

    #[test]
    fn table_preset_id_traits() {
        let id = TablePresetId::Aurora;
        let debug = format!("{:?}", id);
        assert!(debug.contains("Aurora"));
        let cloned = id;
        assert_eq!(id, cloned);
        // Hash
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut hasher);
    }

    #[test]
    fn table_section_traits() {
        let s = TableSection::Footer;
        let debug = format!("{:?}", s);
        assert!(debug.contains("Footer"));
        assert_eq!(s, TableSection::Footer);
        assert_ne!(s, TableSection::Header);
    }

    #[test]
    fn table_effect_target_traits() {
        let t = TableEffectTarget::AllCells;
        let debug = format!("{:?}", t);
        assert!(debug.contains("AllCells"));
        assert_eq!(t, TableEffectTarget::AllCells);
    }

    #[test]
    fn table_effect_scope_traits() {
        let s = TableEffectScope::section(TableSection::Body);
        let debug = format!("{:?}", s);
        assert!(debug.contains("Body"));
        let cloned = s;
        assert_eq!(s, cloned);
    }

    #[test]
    fn style_mask_traits() {
        let m = StyleMask::all();
        let debug = format!("{:?}", m);
        assert!(debug.contains("StyleMask"));
        let cloned = m;
        assert_eq!(m, cloned);
        // Hash
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        m.hash(&mut hasher);
    }

    #[test]
    fn style_attr_all_variants() {
        let attrs = [
            StyleAttr::Bold,
            StyleAttr::Dim,
            StyleAttr::Italic,
            StyleAttr::Underline,
            StyleAttr::Blink,
            StyleAttr::Reverse,
            StyleAttr::Hidden,
            StyleAttr::Strikethrough,
            StyleAttr::DoubleUnderline,
            StyleAttr::CurlyUnderline,
        ];
        for attr in &attrs {
            let debug = format!("{:?}", attr);
            assert!(!debug.is_empty());
        }
        assert_eq!(attrs[0], StyleAttr::Bold);
        assert_ne!(attrs[0], attrs[1]);
    }

    #[test]
    fn skew_phase_clamps_extreme_asymmetry() {
        // Even with extreme asymmetry, result should be in [0, 1] for t in [0, 1]
        for asym in [-1.5, -0.9, 0.9, 1.5] {
            for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let result = skew_phase(t, asym);
                assert!(
                    (-0.01..=1.01).contains(&result),
                    "skew_phase({t}, {asym}) = {result}"
                );
            }
        }
    }

    #[test]
    fn normalize_phase_negative_large() {
        let result = normalize_phase(-100.7);
        assert!((0.0..1.0).contains(&result), "result: {result}");
    }

    #[test]
    fn table_theme_clone() {
        let theme = TableTheme::aurora().with_effect(TableEffectRule::new(
            TableEffectTarget::AllRows,
            pulse_effect(PackedRgba::RED, PackedRgba::BLACK),
        ));
        let cloned = theme.clone();
        assert_eq!(cloned.style_hash(), theme.style_hash());
        assert_eq!(cloned.effects_hash(), theme.effects_hash());
        assert_eq!(cloned.preset_id, theme.preset_id);
    }
}
