// SPDX-License-Identifier: Apache-2.0
//! Translate style/theme semantics into ftui-style with accessibility-safe upgrades.
//!
//! Consumes a [`MigrationIr`] (style intent: tokens, layouts, themes) and
//! produces a [`TranslatedStyle`] — a structured description of the
//! generated ftui-style code:
//!
//! - **Color mappings**: design tokens → ftui-style Color/PackedRgba
//! - **Typography rules**: font tokens → StyleFlags (Bold, Italic, Underline, etc.)
//! - **Spacing quantization**: px/rem values → terminal cell units
//! - **Border mappings**: border tokens → Block::bordered() with BorderType
//! - **Theme generation**: ThemeDecl → Theme struct with token overrides
//! - **Accessibility upgrades**: contrast fixes, readability improvements
//!
//! Design invariants:
//! - **Deterministic ordering**: all output collections sorted by token name
//!   or IR node id for reproducible output.
//! - **Lossless provenance**: every translated rule links back to its source token.
//! - **Reversible upgrades**: accessibility improvements are annotated and
//!   can be disabled without losing the original mapping.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::migration_ir::{
    IrNodeId, LayoutIntent, LayoutKind, MigrationIr, Provenance, StyleToken, ThemeDecl,
    TokenCategory,
};

// ── Constants ──────────────────────────────────────────────────────────

/// Module version tag.
pub const STYLE_TRANSLATOR_VERSION: &str = "style-translator-v1";

/// Minimum WCAG AA contrast ratio for normal text.
const WCAG_AA_CONTRAST_RATIO: f64 = 4.5;

/// Minimum WCAG AA contrast ratio for large text.
const WCAG_AA_LARGE_TEXT_RATIO: f64 = 3.0;

// ── Core Output Types ──────────────────────────────────────────────────

/// The complete translated style output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatedStyle {
    /// Schema version.
    pub version: String,
    /// Source IR run id.
    pub run_id: String,
    /// Per-token color mappings.
    pub color_mappings: BTreeMap<String, ColorMapping>,
    /// Per-token typography rules.
    pub typography_rules: BTreeMap<String, TypographyRule>,
    /// Per-token spacing quantizations.
    pub spacing_rules: BTreeMap<String, SpacingRule>,
    /// Per-token border mappings.
    pub border_rules: BTreeMap<String, BorderRule>,
    /// Per-node layout translation.
    pub layout_rules: BTreeMap<String, LayoutRule>,
    /// Generated theme structures.
    pub themes: Vec<TranslatedTheme>,
    /// Accessibility upgrades applied.
    pub accessibility_upgrades: Vec<AccessibilityUpgrade>,
    /// Unsupported token categories (Shadow, Animation, etc.).
    pub unsupported_tokens: Vec<UnsupportedToken>,
    /// Diagnostics emitted during translation.
    pub diagnostics: Vec<StyleDiagnostic>,
    /// Statistics.
    pub stats: StyleTranslationStats,
}

/// A translated color mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorMapping {
    /// Source token name.
    pub token_name: String,
    /// Parsed RGB tuple (r, g, b).
    pub rgb: Option<(u8, u8, u8)>,
    /// Generated ftui-style representation.
    pub ftui_repr: String,
    /// Confidence in the mapping (0.0–1.0).
    pub confidence: f64,
    /// Whether this was upgraded for accessibility.
    pub a11y_adjusted: bool,
    /// Original value before any adjustment.
    pub original_value: String,
    /// Source provenance.
    pub provenance: Option<Provenance>,
}

/// A translated typography rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypographyRule {
    /// Source token name.
    pub token_name: String,
    /// Mapped StyleFlags names (e.g. "BOLD", "ITALIC").
    pub flags: Vec<String>,
    /// Properties that cannot be represented in terminal.
    pub lost_properties: Vec<String>,
    /// Confidence in the mapping.
    pub confidence: f64,
    /// Source provenance.
    pub provenance: Option<Provenance>,
}

/// A translated spacing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpacingRule {
    /// Source token name.
    pub token_name: String,
    /// Original value (e.g. "8px", "0.5rem").
    pub original_value: String,
    /// Quantized cell count.
    pub cell_units: u16,
    /// Whether sub-cell precision was lost.
    pub precision_lost: bool,
    /// Confidence in the mapping.
    pub confidence: f64,
    /// Source provenance.
    pub provenance: Option<Provenance>,
}

/// A translated border rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BorderRule {
    /// Source token name.
    pub token_name: String,
    /// Mapped border type name (Plain, Rounded, Double, Thick).
    pub border_type: String,
    /// Whether border color was preserved.
    pub color_preserved: bool,
    /// Confidence in the mapping.
    pub confidence: f64,
    /// Source provenance.
    pub provenance: Option<Provenance>,
}

/// A translated layout rule for a view node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutRule {
    /// Source IR node id.
    pub node_id: String,
    /// Layout kind detected.
    pub source_kind: String,
    /// ftui-layout strategy (e.g. "Flex::vertical", "Flex::horizontal").
    pub ftui_strategy: String,
    /// Direction constraint.
    pub direction: Option<String>,
    /// Alignment constraint.
    pub alignment: Option<String>,
    /// Whether sizing was approximated.
    pub sizing_approximate: bool,
    /// Confidence in the mapping.
    pub confidence: f64,
}

/// A generated theme structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatedTheme {
    /// Theme name.
    pub name: String,
    /// Whether this is the default theme.
    pub is_default: bool,
    /// Color overrides for this theme variant.
    pub color_overrides: BTreeMap<String, ColorMapping>,
    /// Count of tokens that could not be resolved.
    pub unresolved_tokens: usize,
    /// Source provenance.
    pub source_tokens: BTreeSet<String>,
}

/// An accessibility upgrade applied during translation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessibilityUpgrade {
    /// Which token was upgraded.
    pub token_name: String,
    /// Kind of upgrade.
    pub kind: A11yUpgradeKind,
    /// Original value.
    pub original: String,
    /// Upgraded value.
    pub upgraded: String,
    /// Rationale for the upgrade.
    pub rationale: String,
    /// Whether this upgrade is reversible.
    pub reversible: bool,
}

/// Kind of accessibility upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum A11yUpgradeKind {
    /// Foreground contrast increased.
    ContrastBoost,
    /// Minimum font weight enforced.
    MinimumWeight,
    /// Focus indicator added.
    FocusIndicator,
}

/// A token that cannot be represented in terminal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsupportedToken {
    /// Token name.
    pub token_name: String,
    /// Token category.
    pub category: String,
    /// Original value.
    pub value: String,
    /// Suggested workaround.
    pub workaround: Option<String>,
}

/// A diagnostic from style translation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StyleDiagnostic {
    /// Diagnostic code.
    pub code: String,
    /// Severity: info, warning, error.
    pub severity: String,
    /// Diagnostic message.
    pub message: String,
    /// Related token name.
    pub token_name: Option<String>,
    /// Related IR node id.
    pub node_id: Option<String>,
}

/// Statistics for style translation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StyleTranslationStats {
    /// Total tokens processed.
    pub total_tokens: usize,
    /// Colors mapped.
    pub colors_mapped: usize,
    /// Typography rules generated.
    pub typography_rules: usize,
    /// Spacing rules generated.
    pub spacing_rules: usize,
    /// Border rules generated.
    pub border_rules: usize,
    /// Layout rules generated.
    pub layout_rules: usize,
    /// Themes generated.
    pub themes_generated: usize,
    /// Accessibility upgrades applied.
    pub a11y_upgrades: usize,
    /// Tokens that could not be translated.
    pub unsupported_count: usize,
}

// ── Public API ─────────────────────────────────────────────────────────

/// Translate style intent from a migration IR into ftui-style representations.
pub fn translate_style(ir: &MigrationIr) -> TranslatedStyle {
    let mut color_mappings = BTreeMap::new();
    let mut typography_rules = BTreeMap::new();
    let mut spacing_rules = BTreeMap::new();
    let mut border_rules = BTreeMap::new();
    let mut unsupported_tokens = Vec::new();
    let mut diagnostics = Vec::new();
    let mut stats = StyleTranslationStats::default();

    // Process each token by category.
    for (name, token) in &ir.style_intent.tokens {
        stats.total_tokens += 1;
        match token.category {
            TokenCategory::Color => {
                let mapping = translate_color_token(token);
                if mapping.rgb.is_none() {
                    diagnostics.push(StyleDiagnostic {
                        code: "ST001".into(),
                        severity: "warning".into(),
                        message: format!(
                            "Could not parse color value '{}' for token '{}'",
                            token.value, name
                        ),
                        token_name: Some(name.clone()),
                        node_id: None,
                    });
                }
                stats.colors_mapped += 1;
                color_mappings.insert(name.clone(), mapping);
            }
            TokenCategory::Typography => {
                let rule = translate_typography_token(token);
                if !rule.lost_properties.is_empty() {
                    diagnostics.push(StyleDiagnostic {
                        code: "ST002".into(),
                        severity: "info".into(),
                        message: format!(
                            "Typography token '{}' lost properties: {}",
                            name,
                            rule.lost_properties.join(", ")
                        ),
                        token_name: Some(name.clone()),
                        node_id: None,
                    });
                }
                stats.typography_rules += 1;
                typography_rules.insert(name.clone(), rule);
            }
            TokenCategory::Spacing => {
                let rule = translate_spacing_token(token);
                if rule.precision_lost {
                    diagnostics.push(StyleDiagnostic {
                        code: "ST003".into(),
                        severity: "info".into(),
                        message: format!(
                            "Spacing token '{}' lost sub-cell precision: '{}' → {} cell(s)",
                            name, token.value, rule.cell_units
                        ),
                        token_name: Some(name.clone()),
                        node_id: None,
                    });
                }
                stats.spacing_rules += 1;
                spacing_rules.insert(name.clone(), rule);
            }
            TokenCategory::Border => {
                let rule = translate_border_token(token);
                stats.border_rules += 1;
                border_rules.insert(name.clone(), rule);
            }
            TokenCategory::Shadow
            | TokenCategory::Animation
            | TokenCategory::Breakpoint
            | TokenCategory::ZIndex => {
                let workaround = match token.category {
                    TokenCategory::Shadow => {
                        Some("Use dim/reverse attributes for depth cues".into())
                    }
                    TokenCategory::Animation => {
                        Some("Use subscription-based timing for animations".into())
                    }
                    TokenCategory::Breakpoint => {
                        Some("Use terminal size detection for responsive layout".into())
                    }
                    TokenCategory::ZIndex => {
                        Some("Use widget layering order in render tree".into())
                    }
                    _ => None,
                };
                unsupported_tokens.push(UnsupportedToken {
                    token_name: name.clone(),
                    category: format!("{:?}", token.category),
                    value: token.value.clone(),
                    workaround,
                });
                stats.unsupported_count += 1;
            }
        }
    }

    // Translate layout intents.
    let layout_rules = translate_layouts(&ir.style_intent.layouts);
    stats.layout_rules = layout_rules.len();

    // Generate theme structures.
    let themes = translate_themes(&ir.style_intent.themes, &color_mappings);
    stats.themes_generated = themes.len();

    // Apply accessibility upgrades.
    let accessibility_upgrades =
        apply_accessibility_upgrades(&mut color_mappings, &ir.accessibility, &mut diagnostics);
    stats.a11y_upgrades = accessibility_upgrades.len();

    TranslatedStyle {
        version: STYLE_TRANSLATOR_VERSION.to_string(),
        run_id: ir.run_id.clone(),
        color_mappings,
        typography_rules,
        spacing_rules,
        border_rules,
        layout_rules,
        themes,
        accessibility_upgrades,
        unsupported_tokens,
        diagnostics,
        stats,
    }
}

// ── Color Translation ──────────────────────────────────────────────────

/// Translate a color design token into an ftui-style color mapping.
fn translate_color_token(token: &StyleToken) -> ColorMapping {
    let rgb = parse_color_value(&token.value);
    let ftui_repr = match rgb {
        Some((r, g, b)) => format!("Color::Rgb({r}, {g}, {b})"),
        None => format!("/* unparsed: {} */", token.value),
    };
    let confidence = if rgb.is_some() { 0.95 } else { 0.3 };

    ColorMapping {
        token_name: token.name.clone(),
        rgb,
        ftui_repr,
        confidence,
        a11y_adjusted: false,
        original_value: token.value.clone(),
        provenance: token.provenance.clone(),
    }
}

/// Parse a CSS-like color value into (r, g, b).
fn parse_color_value(value: &str) -> Option<(u8, u8, u8)> {
    let trimmed = value.trim();

    // #RRGGBB. Hex digits only: the lengths below count bytes, so a
    // multi-byte character would split a slice (a panic), and
    // `from_str_radix` would take a leading '+' as a sign.
    if let Some(hex) = trimmed
        .strip_prefix('#')
        .filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some((r, g, b));
        }
        // #RGB shorthand
        if hex.len() == 3 {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
            return Some((r, g, b));
        }
    }

    // rgb(R, G, B)
    if let Some(inner) = trimmed
        .strip_prefix("rgb(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 3 {
            let r: u8 = parts[0].trim().parse().ok()?;
            let g: u8 = parts[1].trim().parse().ok()?;
            let b: u8 = parts[2].trim().parse().ok()?;
            return Some((r, g, b));
        }
    }

    // Named colors (common subset).
    match trimmed.to_lowercase().as_str() {
        "black" => Some((0, 0, 0)),
        "white" => Some((255, 255, 255)),
        "red" => Some((255, 0, 0)),
        "green" => Some((0, 128, 0)),
        "blue" => Some((0, 0, 255)),
        "yellow" => Some((255, 255, 0)),
        "cyan" => Some((0, 255, 255)),
        "magenta" => Some((255, 0, 255)),
        "gray" | "grey" => Some((128, 128, 128)),
        "transparent" => Some((0, 0, 0)),
        _ => None,
    }
}

// ── Typography Translation ─────────────────────────────────────────────

/// Translate a typography token into StyleFlags.
fn translate_typography_token(token: &StyleToken) -> TypographyRule {
    let value_lower = token.value.to_lowercase();
    let mut flags = Vec::new();
    let mut lost = Vec::new();

    // Font weight mapping.
    if value_lower.contains("bold")
        || value_lower.contains("700")
        || value_lower.contains("800")
        || value_lower.contains("900")
    {
        flags.push("BOLD".to_string());
    }

    // Font style mapping.
    if value_lower.contains("italic") || value_lower.contains("oblique") {
        flags.push("ITALIC".to_string());
    }

    // Text decoration mapping.
    if value_lower.contains("underline") {
        flags.push("UNDERLINE".to_string());
    }
    if value_lower.contains("line-through") || value_lower.contains("strikethrough") {
        flags.push("STRIKETHROUGH".to_string());
    }

    // Dim/opacity.
    if value_lower.contains("dim") || value_lower.contains("opacity") {
        flags.push("DIM".to_string());
    }

    // Properties that are lost in terminal translation.
    if value_lower.contains("font-family")
        || value_lower.contains("sans-serif")
        || value_lower.contains("serif")
        || value_lower.contains("monospace")
    {
        lost.push("font-family".to_string());
    }
    if value_lower.contains("font-size")
        || value_lower.contains("px")
        || value_lower.contains("rem")
        || value_lower.contains("em")
    {
        lost.push("font-size".to_string());
    }
    if value_lower.contains("letter-spacing") {
        lost.push("letter-spacing".to_string());
    }
    if value_lower.contains("line-height") {
        lost.push("line-height".to_string());
    }

    let confidence = if flags.is_empty() && lost.is_empty() {
        0.5
    } else if flags.is_empty() {
        0.3
    } else {
        0.8
    };

    TypographyRule {
        token_name: token.name.clone(),
        flags,
        lost_properties: lost,
        confidence,
        provenance: token.provenance.clone(),
    }
}

// ── Spacing Translation ────────────────────────────────────────────────

/// Translate a spacing token into cell units.
fn translate_spacing_token(token: &StyleToken) -> SpacingRule {
    let (cell_units, precision_lost) = quantize_to_cells(&token.value);

    let confidence = if precision_lost { 0.7 } else { 0.9 };

    SpacingRule {
        token_name: token.name.clone(),
        original_value: token.value.clone(),
        cell_units,
        precision_lost,
        confidence,
        provenance: token.provenance.clone(),
    }
}

/// Quantize a CSS spacing value to terminal cell units.
/// Returns (cell_count, precision_lost).
fn quantize_to_cells(value: &str) -> (u16, bool) {
    let trimmed = value.trim().to_lowercase();

    // Parse numeric value with unit.
    if let Some(px_str) = trimmed.strip_suffix("px")
        && let Ok(px) = px_str.trim().parse::<f64>()
    {
        let cells = (px / 8.0).round() as u16;
        let precision_lost = (px / 8.0).fract().abs() > 0.01;
        return (cells, precision_lost);
    }

    if let Some(rem_str) = trimmed.strip_suffix("rem")
        && let Ok(rem) = rem_str.trim().parse::<f64>()
    {
        let cells = (rem * 2.0).round() as u16;
        let precision_lost = (rem * 2.0).fract().abs() > 0.01;
        return (cells, precision_lost);
    }

    if let Some(em_str) = trimmed.strip_suffix("em")
        && let Ok(em) = em_str.trim().parse::<f64>()
    {
        let cells = (em * 2.0).round() as u16;
        let precision_lost = (em * 2.0).fract().abs() > 0.01;
        return (cells, precision_lost);
    }

    // Plain number: assume pixels.
    if let Ok(num) = trimmed.parse::<f64>() {
        let cells = (num / 8.0).round() as u16;
        let precision_lost = (num / 8.0).fract().abs() > 0.01;
        return (cells, precision_lost);
    }

    // Unparseable: default to 1 cell, precision lost.
    (1, true)
}

// ── Border Translation ─────────────────────────────────────────────────

/// Translate a border token into an ftui border type.
fn translate_border_token(token: &StyleToken) -> BorderRule {
    let value_lower = token.value.to_lowercase();
    let (border_type, confidence) = classify_border_style(&value_lower);

    // Check if a color component is present.
    let color_preserved = value_lower.contains('#')
        || value_lower.contains("rgb")
        || parse_color_value(&token.value).is_some();

    BorderRule {
        token_name: token.name.clone(),
        border_type,
        color_preserved,
        confidence,
        provenance: token.provenance.clone(),
    }
}

/// Classify a CSS border-style value into an ftui BorderType.
fn classify_border_style(value: &str) -> (String, f64) {
    if value.contains("double") {
        ("Double".to_string(), 0.95)
    } else if value.contains("dashed") || value.contains("dotted") {
        ("Rounded".to_string(), 0.7)
    } else if value.contains("thick") || value.contains("3px") || value.contains("4px") {
        ("Thick".to_string(), 0.85)
    } else if value.contains("rounded") {
        ("Rounded".to_string(), 0.95)
    } else if value.contains("solid") || value.contains("1px") || value.contains("2px") {
        ("Plain".to_string(), 0.95)
    } else if value.contains("none") || value.contains("hidden") {
        ("None".to_string(), 0.95)
    } else {
        ("Plain".to_string(), 0.6)
    }
}

// ── Layout Translation ─────────────────────────────────────────────────

/// Translate layout intents for all nodes.
fn translate_layouts(layouts: &BTreeMap<IrNodeId, LayoutIntent>) -> BTreeMap<String, LayoutRule> {
    let mut rules = BTreeMap::new();
    for (node_id, intent) in layouts {
        let rule = translate_single_layout(node_id, intent);
        rules.insert(node_id.0.clone(), rule);
    }
    rules
}

/// Translate a single layout intent into an ftui-layout strategy.
fn translate_single_layout(node_id: &IrNodeId, intent: &LayoutIntent) -> LayoutRule {
    let (ftui_strategy, confidence) = match intent.kind {
        LayoutKind::Flex => {
            let dir = intent.direction.as_deref().unwrap_or("row");
            if dir.contains("column") {
                ("Flex::vertical()".to_string(), 0.95)
            } else {
                ("Flex::horizontal()".to_string(), 0.95)
            }
        }
        LayoutKind::Grid => ("Grid layout (via nested Flex)".to_string(), 0.7),
        LayoutKind::Absolute => (
            "Absolute positioning (via Rect constraints)".to_string(),
            0.5,
        ),
        LayoutKind::Stack => ("Flex::vertical() with overlap".to_string(), 0.6),
        LayoutKind::Flow => ("Flex::horizontal() with wrap".to_string(), 0.7),
    };

    let sizing_approximate = intent.sizing.is_some();

    LayoutRule {
        node_id: node_id.0.clone(),
        source_kind: format!("{:?}", intent.kind),
        ftui_strategy,
        direction: intent.direction.clone(),
        alignment: intent.alignment.clone(),
        sizing_approximate,
        confidence,
    }
}

// ── Theme Translation ──────────────────────────────────────────────────

/// Translate theme declarations into generated theme structures.
fn translate_themes(
    themes: &[ThemeDecl],
    base_colors: &BTreeMap<String, ColorMapping>,
) -> Vec<TranslatedTheme> {
    themes
        .iter()
        .map(|theme| translate_single_theme(theme, base_colors))
        .collect()
}

/// Translate a single theme declaration.
fn translate_single_theme(
    theme: &ThemeDecl,
    base_colors: &BTreeMap<String, ColorMapping>,
) -> TranslatedTheme {
    let mut color_overrides = BTreeMap::new();
    let mut unresolved = 0;
    let mut source_tokens = BTreeSet::new();

    for (token_name, value) in &theme.tokens {
        source_tokens.insert(token_name.clone());
        let rgb = parse_color_value(value);
        match rgb {
            Some((r, g, b)) => {
                color_overrides.insert(
                    token_name.clone(),
                    ColorMapping {
                        token_name: token_name.clone(),
                        rgb: Some((r, g, b)),
                        ftui_repr: format!("Color::Rgb({r}, {g}, {b})"),
                        confidence: 0.9,
                        a11y_adjusted: false,
                        original_value: value.clone(),
                        provenance: base_colors
                            .get(token_name)
                            .and_then(|m| m.provenance.clone()),
                    },
                );
            }
            None => {
                // Check if it references another token.
                if let Some(base) = base_colors.get(value.as_str()) {
                    color_overrides.insert(token_name.clone(), base.clone());
                } else {
                    unresolved += 1;
                }
            }
        }
    }

    TranslatedTheme {
        name: theme.name.clone(),
        is_default: theme.is_default,
        color_overrides,
        unresolved_tokens: unresolved,
        source_tokens,
    }
}

// ── Accessibility Upgrades ─────────────────────────────────────────────

/// Apply accessibility-safe upgrades to color mappings.
fn apply_accessibility_upgrades(
    color_mappings: &mut BTreeMap<String, ColorMapping>,
    accessibility: &crate::migration_ir::AccessibilityMap,
    diagnostics: &mut Vec<StyleDiagnostic>,
) -> Vec<AccessibilityUpgrade> {
    let mut upgrades = Vec::new();

    // Find foreground/background pairs and check contrast.
    let fg_tokens: Vec<String> = color_mappings
        .keys()
        .filter(|k| k.contains("fg") || k.contains("foreground") || k.contains("text"))
        .cloned()
        .collect();

    let bg_tokens: Vec<String> = color_mappings
        .keys()
        .filter(|k| k.contains("bg") || k.contains("background"))
        .cloned()
        .collect();

    for fg_name in &fg_tokens {
        for bg_name in &bg_tokens {
            if let (Some(fg_map), Some(bg_map)) = (
                color_mappings.get(fg_name).cloned(),
                color_mappings.get(bg_name).cloned(),
            ) && let (Some(fg_rgb), Some(bg_rgb)) = (fg_map.rgb, bg_map.rgb)
            {
                let ratio = contrast_ratio(fg_rgb, bg_rgb);
                let threshold = if !accessibility.entries.is_empty() {
                    WCAG_AA_CONTRAST_RATIO
                } else {
                    WCAG_AA_LARGE_TEXT_RATIO
                };

                if ratio < threshold {
                    // Boost foreground contrast.
                    let boosted = boost_contrast(fg_rgb, bg_rgb, threshold);
                    let upgrade = AccessibilityUpgrade {
                        token_name: fg_name.clone(),
                        kind: A11yUpgradeKind::ContrastBoost,
                        original: format!("rgb({}, {}, {})", fg_rgb.0, fg_rgb.1, fg_rgb.2),
                        upgraded: format!("rgb({}, {}, {})", boosted.0, boosted.1, boosted.2),
                        rationale: format!(
                            "Contrast ratio {ratio:.2} below {threshold:.1}:1 against '{bg_name}'"
                        ),
                        reversible: true,
                    };
                    upgrades.push(upgrade);

                    // Update the mapping.
                    if let Some(mapping) = color_mappings.get_mut(fg_name) {
                        mapping.rgb = Some(boosted);
                        mapping.ftui_repr =
                            format!("Color::Rgb({}, {}, {})", boosted.0, boosted.1, boosted.2);
                        mapping.a11y_adjusted = true;
                    }

                    diagnostics.push(StyleDiagnostic {
                        code: "ST010".into(),
                        severity: "info".into(),
                        message: format!(
                            "Boosted contrast for '{fg_name}' against '{bg_name}': \
                             ratio {ratio:.2} → {:.2}",
                            contrast_ratio(boosted, bg_rgb)
                        ),
                        token_name: Some(fg_name.clone()),
                        node_id: None,
                    });
                }
            }
        }
    }

    upgrades
}

/// Compute WCAG relative luminance for an sRGB color.
fn relative_luminance(r: u8, g: u8, b: u8) -> f64 {
    let to_linear = |c: u8| -> f64 {
        let s = c as f64 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * to_linear(r) + 0.7152 * to_linear(g) + 0.0722 * to_linear(b)
}

/// Compute WCAG contrast ratio between two colors.
fn contrast_ratio(fg: (u8, u8, u8), bg: (u8, u8, u8)) -> f64 {
    let l1 = relative_luminance(fg.0, fg.1, fg.2);
    let l2 = relative_luminance(bg.0, bg.1, bg.2);
    let (lighter, darker) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Boost a foreground color to achieve the target contrast against background.
fn boost_contrast(fg: (u8, u8, u8), bg: (u8, u8, u8), target: f64) -> (u8, u8, u8) {
    let bg_lum = relative_luminance(bg.0, bg.1, bg.2);

    // Determine if we should lighten or darken.
    let lighten = bg_lum < 0.5;

    // Binary search for a multiplier that achieves target contrast.
    let mut best = fg;
    for step in 0..20 {
        let t = step as f64 / 20.0;
        let adjusted = if lighten {
            // Move toward white.
            (
                (fg.0 as f64 + t * (255.0 - fg.0 as f64)).round() as u8,
                (fg.1 as f64 + t * (255.0 - fg.1 as f64)).round() as u8,
                (fg.2 as f64 + t * (255.0 - fg.2 as f64)).round() as u8,
            )
        } else {
            // Move toward black.
            (
                (fg.0 as f64 * (1.0 - t)).round() as u8,
                (fg.1 as f64 * (1.0 - t)).round() as u8,
                (fg.2 as f64 * (1.0 - t)).round() as u8,
            )
        };

        if contrast_ratio(adjusted, bg) >= target {
            best = adjusted;
            break;
        }
        best = adjusted;
    }
    best
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration_ir::IrBuilder;

    fn make_ir_with_colors() -> MigrationIr {
        let mut builder = IrBuilder::new("test-colors".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "primary-fg".into(),
            category: TokenCategory::Color,
            value: "#336699".into(),
            provenance: None,
        });
        builder.add_style_token(StyleToken {
            name: "primary-bg".into(),
            category: TokenCategory::Color,
            value: "#ffffff".into(),
            provenance: None,
        });
        builder.build()
    }

    #[test]
    fn translate_empty_style() {
        let ir = IrBuilder::new("empty".into(), "test".into()).build();
        let result = translate_style(&ir);
        assert_eq!(result.version, STYLE_TRANSLATOR_VERSION);
        assert!(result.color_mappings.is_empty());
        assert!(result.typography_rules.is_empty());
        assert!(result.diagnostics.is_empty());
        assert_eq!(result.stats.total_tokens, 0);
    }

    #[test]
    fn translate_hex_color() {
        let ir = make_ir_with_colors();
        let result = translate_style(&ir);
        assert_eq!(result.color_mappings.len(), 2);

        let fg = &result.color_mappings["primary-fg"];
        assert_eq!(fg.rgb, Some((0x33, 0x66, 0x99)));
        assert_eq!(fg.ftui_repr, "Color::Rgb(51, 102, 153)");
        assert!(fg.confidence > 0.9);
    }

    #[test]
    fn translate_rgb_color() {
        let mut builder = IrBuilder::new("rgb-test".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "accent".into(),
            category: TokenCategory::Color,
            value: "rgb(128, 64, 200)".into(),
            provenance: None,
        });
        let ir = builder.build();
        let result = translate_style(&ir);
        let accent = &result.color_mappings["accent"];
        assert_eq!(accent.rgb, Some((128, 64, 200)));
    }

    #[test]
    fn translate_shorthand_hex_color() {
        let rgb = parse_color_value("#f0c");
        assert_eq!(rgb, Some((255, 0, 204)));
    }

    #[test]
    fn hex_colors_with_non_hex_digits_are_rejected_without_panicking() {
        // Byte-length checks then byte slicing split "é" (a panic), and a
        // leading '+' passed `from_str_radix` as a sign.
        for bad in ["#a\u{e9}123", "#\u{e9}1", "#+f+f+f", "#+ff"] {
            assert_eq!(parse_color_value(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn translate_named_color() {
        let rgb = parse_color_value("red");
        assert_eq!(rgb, Some((255, 0, 0)));
    }

    #[test]
    fn translate_unknown_color_produces_diagnostic() {
        let mut builder = IrBuilder::new("bad-color".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "weird".into(),
            category: TokenCategory::Color,
            value: "var(--custom-color)".into(),
            provenance: None,
        });
        let ir = builder.build();
        let result = translate_style(&ir);
        assert!(result.color_mappings["weird"].rgb.is_none());
        assert!(result.diagnostics.iter().any(|d| d.code == "ST001"));
    }

    #[test]
    fn translate_typography_bold_italic() {
        let mut builder = IrBuilder::new("typo".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "heading".into(),
            category: TokenCategory::Typography,
            value: "bold italic".into(),
            provenance: None,
        });
        let ir = builder.build();
        let result = translate_style(&ir);
        let rule = &result.typography_rules["heading"];
        assert!(rule.flags.contains(&"BOLD".to_string()));
        assert!(rule.flags.contains(&"ITALIC".to_string()));
    }

    #[test]
    fn translate_typography_with_lost_properties() {
        let mut builder = IrBuilder::new("typo-lost".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "body".into(),
            category: TokenCategory::Typography,
            value: "font-family: sans-serif; font-size: 14px".into(),
            provenance: None,
        });
        let ir = builder.build();
        let result = translate_style(&ir);
        let rule = &result.typography_rules["body"];
        assert!(rule.lost_properties.contains(&"font-family".to_string()));
        assert!(rule.lost_properties.contains(&"font-size".to_string()));
        assert!(result.diagnostics.iter().any(|d| d.code == "ST002"));
    }

    #[test]
    fn translate_spacing_px() {
        let (cells, lost) = quantize_to_cells("16px");
        assert_eq!(cells, 2); // 16px / 8px = 2 cells
        assert!(!lost);
    }

    #[test]
    fn translate_spacing_rem() {
        let (cells, lost) = quantize_to_cells("1rem");
        assert_eq!(cells, 2); // 1rem = 2 cells
        assert!(!lost);
    }

    #[test]
    fn translate_spacing_fractional_produces_lost() {
        let (cells, lost) = quantize_to_cells("3px");
        assert_eq!(cells, 0); // 3px / 8 ≈ 0.375 → rounds to 0
        assert!(lost);
    }

    #[test]
    fn translate_border_solid() {
        let (border_type, conf) = classify_border_style("1px solid #000");
        assert_eq!(border_type, "Plain");
        assert!(conf > 0.9);
    }

    #[test]
    fn translate_border_double() {
        let (border_type, _) = classify_border_style("double");
        assert_eq!(border_type, "Double");
    }

    #[test]
    fn translate_border_dashed() {
        let (border_type, _) = classify_border_style("dashed");
        assert_eq!(border_type, "Rounded");
    }

    #[test]
    fn translate_layout_flex_row() {
        let mut layouts = BTreeMap::new();
        let node_id = IrNodeId("node-1".into());
        layouts.insert(
            node_id,
            LayoutIntent {
                kind: LayoutKind::Flex,
                direction: Some("row".into()),
                alignment: None,
                sizing: None,
            },
        );
        let rules = translate_layouts(&layouts);
        assert_eq!(rules["node-1"].ftui_strategy, "Flex::horizontal()");
    }

    #[test]
    fn translate_layout_flex_column() {
        let mut layouts = BTreeMap::new();
        let node_id = IrNodeId("node-2".into());
        layouts.insert(
            node_id,
            LayoutIntent {
                kind: LayoutKind::Flex,
                direction: Some("column".into()),
                alignment: Some("center".into()),
                sizing: None,
            },
        );
        let rules = translate_layouts(&layouts);
        assert_eq!(rules["node-2"].ftui_strategy, "Flex::vertical()");
        assert_eq!(rules["node-2"].alignment.as_deref(), Some("center"));
    }

    #[test]
    fn translate_layout_grid_fallback() {
        let mut layouts = BTreeMap::new();
        layouts.insert(
            IrNodeId("grid-1".into()),
            LayoutIntent {
                kind: LayoutKind::Grid,
                direction: None,
                alignment: None,
                sizing: None,
            },
        );
        let rules = translate_layouts(&layouts);
        assert!(rules["grid-1"].ftui_strategy.contains("Grid"));
        assert!(rules["grid-1"].confidence < 0.8);
    }

    #[test]
    fn translate_unsupported_tokens_shadow() {
        let mut builder = IrBuilder::new("shadow".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "card-shadow".into(),
            category: TokenCategory::Shadow,
            value: "0 2px 4px rgba(0,0,0,0.1)".into(),
            provenance: None,
        });
        let ir = builder.build();
        let result = translate_style(&ir);
        assert_eq!(result.unsupported_tokens.len(), 1);
        assert_eq!(result.unsupported_tokens[0].category, "Shadow");
        assert!(result.unsupported_tokens[0].workaround.is_some());
    }

    #[test]
    fn translate_unsupported_tokens_animation() {
        let mut builder = IrBuilder::new("anim".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "fade-in".into(),
            category: TokenCategory::Animation,
            value: "opacity 0.3s ease".into(),
            provenance: None,
        });
        let ir = builder.build();
        let result = translate_style(&ir);
        assert_eq!(result.unsupported_tokens.len(), 1);
        assert_eq!(result.unsupported_tokens[0].category, "Animation");
    }

    #[test]
    fn translate_theme_with_overrides() {
        let mut builder = IrBuilder::new("themed".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "text-color".into(),
            category: TokenCategory::Color,
            value: "#000000".into(),
            provenance: None,
        });
        builder.add_theme(ThemeDecl {
            name: "dark".into(),
            tokens: {
                let mut m = BTreeMap::new();
                m.insert("text-color".into(), "#ffffff".into());
                m
            },
            is_default: false,
        });
        builder.add_theme(ThemeDecl {
            name: "light".into(),
            tokens: BTreeMap::new(),
            is_default: true,
        });
        let ir = builder.build();
        let result = translate_style(&ir);
        assert_eq!(result.themes.len(), 2);

        let dark = result.themes.iter().find(|t| t.name == "dark").unwrap();
        assert!(!dark.is_default);
        assert!(dark.color_overrides.contains_key("text-color"));
        assert_eq!(
            dark.color_overrides["text-color"].rgb,
            Some((255, 255, 255))
        );

        let light = result.themes.iter().find(|t| t.name == "light").unwrap();
        assert!(light.is_default);
    }

    #[test]
    fn contrast_ratio_black_white() {
        let ratio = contrast_ratio((0, 0, 0), (255, 255, 255));
        assert!(ratio > 20.0); // Should be 21:1.
    }

    #[test]
    fn contrast_ratio_same_color() {
        let ratio = contrast_ratio((128, 128, 128), (128, 128, 128));
        assert!((ratio - 1.0).abs() < 0.001); // Should be 1:1.
    }

    #[test]
    fn accessibility_boost_low_contrast() {
        let mut builder = IrBuilder::new("a11y".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "text-fg".into(),
            category: TokenCategory::Color,
            value: "#cccccc".into(),
            provenance: None,
        });
        builder.add_style_token(StyleToken {
            name: "text-bg".into(),
            category: TokenCategory::Color,
            value: "#ffffff".into(),
            provenance: None,
        });
        let ir = builder.build();
        let result = translate_style(&ir);

        // Contrast between #ccc and #fff is ~1.6:1, below any threshold.
        // So an upgrade should be applied.
        assert!(!result.accessibility_upgrades.is_empty());
        let upgrade = &result.accessibility_upgrades[0];
        assert_eq!(upgrade.kind, A11yUpgradeKind::ContrastBoost);
        assert!(upgrade.reversible);

        // The mapping should be adjusted.
        let fg = &result.color_mappings["text-fg"];
        assert!(fg.a11y_adjusted);
    }

    #[test]
    fn stats_count_all_categories() {
        let mut builder = IrBuilder::new("stats".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "c1".into(),
            category: TokenCategory::Color,
            value: "#ff0000".into(),
            provenance: None,
        });
        builder.add_style_token(StyleToken {
            name: "t1".into(),
            category: TokenCategory::Typography,
            value: "bold".into(),
            provenance: None,
        });
        builder.add_style_token(StyleToken {
            name: "s1".into(),
            category: TokenCategory::Spacing,
            value: "8px".into(),
            provenance: None,
        });
        builder.add_style_token(StyleToken {
            name: "b1".into(),
            category: TokenCategory::Border,
            value: "1px solid".into(),
            provenance: None,
        });
        builder.add_style_token(StyleToken {
            name: "z1".into(),
            category: TokenCategory::ZIndex,
            value: "100".into(),
            provenance: None,
        });
        let ir = builder.build();
        let result = translate_style(&ir);
        assert_eq!(result.stats.total_tokens, 5);
        assert_eq!(result.stats.colors_mapped, 1);
        assert_eq!(result.stats.typography_rules, 1);
        assert_eq!(result.stats.spacing_rules, 1);
        assert_eq!(result.stats.border_rules, 1);
        assert_eq!(result.stats.unsupported_count, 1);
    }

    #[test]
    fn translation_is_deterministic() {
        let ir = make_ir_with_colors();
        let r1 = translate_style(&ir);
        let r2 = translate_style(&ir);

        let j1 = serde_json::to_string(&r1).unwrap();
        let j2 = serde_json::to_string(&r2).unwrap();
        assert_eq!(j1, j2);
    }

    #[test]
    fn theme_with_token_reference() {
        let mut builder = IrBuilder::new("ref-theme".into(), "test".into());
        builder.add_style_token(StyleToken {
            name: "brand".into(),
            category: TokenCategory::Color,
            value: "#3366ff".into(),
            provenance: None,
        });
        builder.add_theme(ThemeDecl {
            name: "branded".into(),
            tokens: {
                let mut m = BTreeMap::new();
                // Reference the token name as the value.
                m.insert("primary".into(), "brand".into());
                m
            },
            is_default: true,
        });
        let ir = builder.build();
        let result = translate_style(&ir);
        let themed = &result.themes[0];
        // "brand" was resolved via base_colors lookup.
        assert!(themed.color_overrides.contains_key("primary"));
    }

    #[test]
    fn relative_luminance_black_is_zero() {
        let lum = relative_luminance(0, 0, 0);
        assert!(lum.abs() < 0.001);
    }

    #[test]
    fn relative_luminance_white_is_one() {
        let lum = relative_luminance(255, 255, 255);
        assert!((lum - 1.0).abs() < 0.001);
    }
}
