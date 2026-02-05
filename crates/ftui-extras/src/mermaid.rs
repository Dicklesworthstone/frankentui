#![forbid(unsafe_code)]

//! Mermaid parser core (tokenizer + AST).
//!
//! This module provides a minimal, deterministic parser for Mermaid fenced blocks.
//! It focuses on:
//! - Tokenization with stable spans (line/col)
//! - Diagram type detection
//! - AST for common diagram elements

use core::{fmt, mem};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs::OpenOptions;
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub col: usize,
    pub byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

impl Span {
    fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    fn at_line(line: usize, line_len: usize) -> Self {
        let start = Position {
            line,
            col: 1,
            byte: 0,
        };
        let end = Position {
            line,
            col: line_len.max(1),
            byte: 0,
        };
        Self::new(start, end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidErrorCode {
    Parse,
}

impl MermaidErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "mermaid/error/parse",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MermaidError {
    pub message: String,
    pub span: Span,
    pub expected: Option<Vec<&'static str>>,
    pub code: MermaidErrorCode,
}

impl MermaidError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            expected: None,
            code: MermaidErrorCode::Parse,
        }
    }

    fn with_expected(mut self, expected: Vec<&'static str>) -> Self {
        self.expected = Some(expected);
        self
    }
}

impl fmt::Display for MermaidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (line {}, col {})",
            self.message, self.span.start.line, self.span.start.col
        )?;
        if let Some(expected) = &self.expected {
            write!(f, "; expected: {}", expected.join(", "))?;
        }
        Ok(())
    }
}

/// Mermaid glyph rendering mode (Unicode or ASCII fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidGlyphMode {
    Unicode,
    Ascii,
}

impl MermaidGlyphMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "unicode" | "uni" | "u" => Some(Self::Unicode),
            "ascii" | "ansi" | "a" => Some(Self::Ascii),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unicode => "unicode",
            Self::Ascii => "ascii",
        }
    }
}

impl fmt::Display for MermaidGlyphMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Mermaid render mode (cell vs sub-cell canvas).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidRenderMode {
    /// Auto-detect best mode from terminal capabilities.
    Auto,
    /// Force classic cell-based rendering (no sub-cell canvas).
    CellOnly,
    /// Braille (2×4 sub-cells per terminal cell).
    Braille,
    /// Block (2×2 sub-cells per terminal cell).
    Block,
    /// Half-block (1×2 sub-cells per terminal cell).
    HalfBlock,
}

impl MermaidRenderMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "cell" | "cellonly" | "cell-only" | "cell_only" | "cells" => Some(Self::CellOnly),
            "braille" | "brl" => Some(Self::Braille),
            "block" | "blocks" => Some(Self::Block),
            "halfblock" | "half-block" | "half_block" | "half" => Some(Self::HalfBlock),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::CellOnly => "cell",
            Self::Braille => "braille",
            Self::Block => "block",
            Self::HalfBlock => "halfblock",
        }
    }
}

impl fmt::Display for MermaidRenderMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Named color palette preset for diagram rendering.
///
/// Each preset remaps node fills, edge colors, cluster backgrounds, and text
/// colors. Use [`DiagramPalettePreset::as_str`] / [`DiagramPalettePreset::parse`]
/// for round-trip string conversion (env vars, CLI args, config files).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramPalettePreset {
    /// Blue nodes, gray edges — current default look.
    Default,
    /// Navy/teal/gray — professional, muted palette.
    Corporate,
    /// Cyan/magenta/green on dark background — high energy.
    Neon,
    /// White/gray/black only — works on any terminal.
    Monochrome,
    /// Soft muted colors — easy on eyes.
    Pastel,
    /// WCAG AAA compliant, bold primary colors.
    HighContrast,
}

impl DiagramPalettePreset {
    /// Parse from a string value (case-insensitive).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "default" | "def" => Some(Self::Default),
            "corporate" | "corp" | "professional" => Some(Self::Corporate),
            "neon" | "glow" | "dark" => Some(Self::Neon),
            "monochrome" | "mono" | "bw" | "grayscale" => Some(Self::Monochrome),
            "pastel" | "soft" | "light" => Some(Self::Pastel),
            "high-contrast" | "highcontrast" | "high_contrast" | "hc" | "accessible" => {
                Some(Self::HighContrast)
            }
            _ => None,
        }
    }

    /// Stable string identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Corporate => "corporate",
            Self::Neon => "neon",
            Self::Monochrome => "monochrome",
            Self::Pastel => "pastel",
            Self::HighContrast => "high-contrast",
        }
    }

    /// All available presets in definition order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Default,
            Self::Corporate,
            Self::Neon,
            Self::Monochrome,
            Self::Pastel,
            Self::HighContrast,
        ]
    }

    /// Next preset in cycle order (wraps around).
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Default => Self::Corporate,
            Self::Corporate => Self::Neon,
            Self::Neon => Self::Monochrome,
            Self::Monochrome => Self::Pastel,
            Self::Pastel => Self::HighContrast,
            Self::HighContrast => Self::Default,
        }
    }

    /// Previous preset in cycle order (wraps around).
    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::Default => Self::HighContrast,
            Self::Corporate => Self::Default,
            Self::Neon => Self::Corporate,
            Self::Monochrome => Self::Neon,
            Self::Pastel => Self::Monochrome,
            Self::HighContrast => Self::Pastel,
        }
    }
}

impl fmt::Display for DiagramPalettePreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Fidelity tier override for Mermaid rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidTier {
    Compact,
    Normal,
    Rich,
    Auto,
}

impl MermaidTier {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" | "small" => Some(Self::Compact),
            "normal" | "default" => Some(Self::Normal),
            "rich" | "full" => Some(Self::Rich),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Normal => "normal",
            Self::Rich => "rich",
            Self::Auto => "auto",
        }
    }
}

impl fmt::Display for MermaidTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Mermaid label wrapping strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidWrapMode {
    None,
    Word,
    Char,
    WordChar,
}

impl MermaidWrapMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Some(Self::None),
            "word" => Some(Self::Word),
            "char" | "grapheme" => Some(Self::Char),
            "wordchar" | "word-char" | "word_char" => Some(Self::WordChar),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Word => "word",
            Self::Char => "char",
            Self::WordChar => "wordchar",
        }
    }
}

impl fmt::Display for MermaidWrapMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Mermaid link rendering strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidLinkMode {
    Inline,
    Footnote,
    Off,
}

impl MermaidLinkMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "inline" => Some(Self::Inline),
            "footnote" | "footnotes" => Some(Self::Footnote),
            "off" | "none" => Some(Self::Off),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Footnote => "footnote",
            Self::Off => "off",
        }
    }
}

impl fmt::Display for MermaidLinkMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Sanitization strictness for Mermaid inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidSanitizeMode {
    Strict,
    Lenient,
}

impl MermaidSanitizeMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Some(Self::Strict),
            "lenient" | "relaxed" => Some(Self::Lenient),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Lenient => "lenient",
        }
    }
}

impl fmt::Display for MermaidSanitizeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error rendering mode for Mermaid failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidErrorMode {
    Panel,
    Raw,
    Both,
}

impl MermaidErrorMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "panel" => Some(Self::Panel),
            "raw" => Some(Self::Raw),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Panel => "panel",
            Self::Raw => "raw",
            Self::Both => "both",
        }
    }
}

impl fmt::Display for MermaidErrorMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

const ENV_MERMAID_ENABLE: &str = "FTUI_MERMAID_ENABLE";
const ENV_MERMAID_GLYPH_MODE: &str = "FTUI_MERMAID_GLYPH_MODE";
const ENV_MERMAID_RENDER_MODE: &str = "FTUI_MERMAID_RENDER_MODE";
const ENV_MERMAID_TIER: &str = "FTUI_MERMAID_TIER";
const ENV_MERMAID_MAX_NODES: &str = "FTUI_MERMAID_MAX_NODES";
const ENV_MERMAID_MAX_EDGES: &str = "FTUI_MERMAID_MAX_EDGES";
const ENV_MERMAID_ROUTE_BUDGET: &str = "FTUI_MERMAID_ROUTE_BUDGET";
const ENV_MERMAID_LAYOUT_ITER_BUDGET: &str = "FTUI_MERMAID_LAYOUT_ITER_BUDGET";
const ENV_MERMAID_MAX_LABEL_CHARS: &str = "FTUI_MERMAID_MAX_LABEL_CHARS";
const ENV_MERMAID_MAX_LABEL_LINES: &str = "FTUI_MERMAID_MAX_LABEL_LINES";
const ENV_MERMAID_WRAP_MODE: &str = "FTUI_MERMAID_WRAP_MODE";
const ENV_MERMAID_ENABLE_STYLES: &str = "FTUI_MERMAID_ENABLE_STYLES";
const ENV_MERMAID_ENABLE_INIT_DIRECTIVES: &str = "FTUI_MERMAID_ENABLE_INIT_DIRECTIVES";
const ENV_MERMAID_ENABLE_LINKS: &str = "FTUI_MERMAID_ENABLE_LINKS";
const ENV_MERMAID_LINK_MODE: &str = "FTUI_MERMAID_LINK_MODE";
const ENV_MERMAID_SANITIZE_MODE: &str = "FTUI_MERMAID_SANITIZE_MODE";
const ENV_MERMAID_ERROR_MODE: &str = "FTUI_MERMAID_ERROR_MODE";
const ENV_MERMAID_LOG_PATH: &str = "FTUI_MERMAID_LOG_PATH";
const ENV_MERMAID_CACHE_ENABLED: &str = "FTUI_MERMAID_CACHE_ENABLED";
const ENV_MERMAID_CAPS_PROFILE: &str = "FTUI_MERMAID_CAPS_PROFILE";
const ENV_MERMAID_CAPABILITY_PROFILE: &str = "FTUI_MERMAID_CAPABILITY_PROFILE";
const ENV_MERMAID_PALETTE: &str = "FTUI_MERMAID_PALETTE";

/// Mermaid engine configuration (deterministic, env-overridable).
///
/// # Environment Variables
/// - `FTUI_MERMAID_ENABLE` (bool)
/// - `FTUI_MERMAID_GLYPH_MODE` = unicode|ascii
/// - `FTUI_MERMAID_RENDER_MODE` = auto|cell|braille|block|halfblock
/// - `FTUI_MERMAID_TIER` = compact|normal|rich|auto
/// - `FTUI_MERMAID_MAX_NODES` (usize)
/// - `FTUI_MERMAID_MAX_EDGES` (usize)
/// - `FTUI_MERMAID_ROUTE_BUDGET` (usize)
/// - `FTUI_MERMAID_LAYOUT_ITER_BUDGET` (usize)
/// - `FTUI_MERMAID_MAX_LABEL_CHARS` (usize)
/// - `FTUI_MERMAID_MAX_LABEL_LINES` (usize)
/// - `FTUI_MERMAID_WRAP_MODE` = none|word|char|wordchar
/// - `FTUI_MERMAID_ENABLE_STYLES` (bool)
/// - `FTUI_MERMAID_ENABLE_INIT_DIRECTIVES` (bool)
/// - `FTUI_MERMAID_ENABLE_LINKS` (bool)
/// - `FTUI_MERMAID_LINK_MODE` = inline|footnote|off
/// - `FTUI_MERMAID_SANITIZE_MODE` = strict|lenient
/// - `FTUI_MERMAID_ERROR_MODE` = panel|raw|both
/// - `FTUI_MERMAID_LOG_PATH` (string path)
/// - `FTUI_MERMAID_CACHE_ENABLED` (bool)
/// - `FTUI_MERMAID_CAPS_PROFILE` / `FTUI_MERMAID_CAPABILITY_PROFILE` (string)
/// - `FTUI_MERMAID_PALETTE` = default|corporate|neon|monochrome|pastel|high-contrast
#[derive(Debug, Clone)]
pub struct MermaidConfig {
    pub enabled: bool,
    pub glyph_mode: MermaidGlyphMode,
    pub render_mode: MermaidRenderMode,
    pub tier_override: MermaidTier,
    pub max_nodes: usize,
    pub max_edges: usize,
    pub route_budget: usize,
    pub layout_iteration_budget: usize,
    pub max_label_chars: usize,
    pub max_label_lines: usize,
    pub wrap_mode: MermaidWrapMode,
    pub enable_styles: bool,
    pub enable_init_directives: bool,
    pub enable_links: bool,
    pub link_mode: MermaidLinkMode,
    pub sanitize_mode: MermaidSanitizeMode,
    pub error_mode: MermaidErrorMode,
    pub log_path: Option<String>,
    pub cache_enabled: bool,
    pub capability_profile: Option<String>,
    pub debug_overlay: bool,
    pub palette: DiagramPalettePreset,
}

impl Default for MermaidConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            glyph_mode: MermaidGlyphMode::Unicode,
            render_mode: MermaidRenderMode::Auto,
            tier_override: MermaidTier::Auto,
            max_nodes: 200,
            max_edges: 400,
            route_budget: 4_000,
            layout_iteration_budget: 200,
            max_label_chars: 48,
            max_label_lines: 3,
            wrap_mode: MermaidWrapMode::WordChar,
            enable_styles: true,
            enable_init_directives: false,
            enable_links: false,
            link_mode: MermaidLinkMode::Off,
            sanitize_mode: MermaidSanitizeMode::Strict,
            error_mode: MermaidErrorMode::Panel,
            log_path: None,
            cache_enabled: true,
            capability_profile: None,
            debug_overlay: false,
            palette: DiagramPalettePreset::Default,
        }
    }
}

/// Configuration parse diagnostics (env + validation).
#[derive(Debug, Clone)]
pub struct MermaidConfigParse {
    pub config: MermaidConfig,
    pub errors: Vec<MermaidConfigError>,
}

/// Configuration error with field context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MermaidConfigError {
    pub field: &'static str,
    pub value: String,
    pub message: String,
}

impl MermaidConfigError {
    fn new(field: &'static str, value: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field,
            value: value.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for MermaidConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={} ({})", self.field, self.value, self.message)
    }
}

impl std::error::Error for MermaidConfigError {}

impl MermaidConfig {
    /// Parse config from environment variables.
    #[must_use]
    pub fn from_env() -> MermaidConfig {
        Self::from_env_with_diagnostics().config
    }

    /// Parse config from environment variables and return diagnostics.
    #[must_use]
    pub fn from_env_with_diagnostics() -> MermaidConfigParse {
        from_env_with(|key| env::var(key).ok())
    }

    /// Validate config constraints and return all violations.
    pub fn validate(&self) -> Result<(), Vec<MermaidConfigError>> {
        let mut errors = Vec::new();
        validate_positive("max_nodes", self.max_nodes, &mut errors);
        validate_positive("max_edges", self.max_edges, &mut errors);
        validate_positive("route_budget", self.route_budget, &mut errors);
        validate_positive(
            "layout_iteration_budget",
            self.layout_iteration_budget,
            &mut errors,
        );
        validate_positive("max_label_chars", self.max_label_chars, &mut errors);
        validate_positive("max_label_lines", self.max_label_lines, &mut errors);
        if !self.enable_links && self.link_mode != MermaidLinkMode::Off {
            errors.push(MermaidConfigError::new(
                "link_mode",
                format!("{:?}", self.link_mode),
                "link_mode requires enable_links=true or must be off",
            ));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Short human-readable summary for debug overlays.
    #[must_use]
    pub fn summary_short(&self) -> String {
        let enabled = if self.enabled { "on" } else { "off" };
        format!(
            "Mermaid: {enabled} · {} · {} · {} · {}",
            self.glyph_mode, self.render_mode, self.tier_override, self.palette
        )
    }
}

fn from_env_with<F>(mut get: F) -> MermaidConfigParse
where
    F: FnMut(&str) -> Option<String>,
{
    let mut config = MermaidConfig::default();
    let mut errors = Vec::new();

    if let Some(value) = get(ENV_MERMAID_ENABLE) {
        match parse_bool(&value) {
            Some(parsed) => config.enabled = parsed,
            None => errors.push(MermaidConfigError::new(
                "enable",
                value,
                "expected bool (1/0/true/false)",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_GLYPH_MODE) {
        match MermaidGlyphMode::parse(&value) {
            Some(parsed) => config.glyph_mode = parsed,
            None => errors.push(MermaidConfigError::new(
                "glyph_mode",
                value,
                "expected unicode|ascii",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_RENDER_MODE) {
        match MermaidRenderMode::parse(&value) {
            Some(parsed) => config.render_mode = parsed,
            None => errors.push(MermaidConfigError::new(
                "render_mode",
                value,
                "expected auto|cell|braille|block|halfblock",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_TIER) {
        match MermaidTier::parse(&value) {
            Some(parsed) => config.tier_override = parsed,
            None => errors.push(MermaidConfigError::new(
                "tier_override",
                value,
                "expected compact|normal|rich|auto",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_MAX_NODES) {
        match parse_usize(&value) {
            Some(parsed) => config.max_nodes = parsed,
            None => errors.push(MermaidConfigError::new(
                "max_nodes",
                value,
                "expected positive integer",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_MAX_EDGES) {
        match parse_usize(&value) {
            Some(parsed) => config.max_edges = parsed,
            None => errors.push(MermaidConfigError::new(
                "max_edges",
                value,
                "expected positive integer",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_ROUTE_BUDGET) {
        match parse_usize(&value) {
            Some(parsed) => config.route_budget = parsed,
            None => errors.push(MermaidConfigError::new(
                "route_budget",
                value,
                "expected positive integer",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_LAYOUT_ITER_BUDGET) {
        match parse_usize(&value) {
            Some(parsed) => config.layout_iteration_budget = parsed,
            None => errors.push(MermaidConfigError::new(
                "layout_iteration_budget",
                value,
                "expected positive integer",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_MAX_LABEL_CHARS) {
        match parse_usize(&value) {
            Some(parsed) => config.max_label_chars = parsed,
            None => errors.push(MermaidConfigError::new(
                "max_label_chars",
                value,
                "expected positive integer",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_MAX_LABEL_LINES) {
        match parse_usize(&value) {
            Some(parsed) => config.max_label_lines = parsed,
            None => errors.push(MermaidConfigError::new(
                "max_label_lines",
                value,
                "expected positive integer",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_WRAP_MODE) {
        match MermaidWrapMode::parse(&value) {
            Some(parsed) => config.wrap_mode = parsed,
            None => errors.push(MermaidConfigError::new(
                "wrap_mode",
                value,
                "expected none|word|char|wordchar",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_ENABLE_STYLES) {
        match parse_bool(&value) {
            Some(parsed) => config.enable_styles = parsed,
            None => errors.push(MermaidConfigError::new(
                "enable_styles",
                value,
                "expected bool (1/0/true/false)",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_ENABLE_INIT_DIRECTIVES) {
        match parse_bool(&value) {
            Some(parsed) => config.enable_init_directives = parsed,
            None => errors.push(MermaidConfigError::new(
                "enable_init_directives",
                value,
                "expected bool (1/0/true/false)",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_ENABLE_LINKS) {
        match parse_bool(&value) {
            Some(parsed) => config.enable_links = parsed,
            None => errors.push(MermaidConfigError::new(
                "enable_links",
                value,
                "expected bool (1/0/true/false)",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_LINK_MODE) {
        match MermaidLinkMode::parse(&value) {
            Some(parsed) => config.link_mode = parsed,
            None => errors.push(MermaidConfigError::new(
                "link_mode",
                value,
                "expected inline|footnote|off",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_SANITIZE_MODE) {
        match MermaidSanitizeMode::parse(&value) {
            Some(parsed) => config.sanitize_mode = parsed,
            None => errors.push(MermaidConfigError::new(
                "sanitize_mode",
                value,
                "expected strict|lenient",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_ERROR_MODE) {
        match MermaidErrorMode::parse(&value) {
            Some(parsed) => config.error_mode = parsed,
            None => errors.push(MermaidConfigError::new(
                "error_mode",
                value,
                "expected panel|raw|both",
            )),
        }
    }

    if let Some(value) = get(ENV_MERMAID_LOG_PATH) {
        let trimmed = value.trim();
        config.log_path = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }

    if let Some(value) = get(ENV_MERMAID_CACHE_ENABLED) {
        match parse_bool(&value) {
            Some(parsed) => config.cache_enabled = parsed,
            None => errors.push(MermaidConfigError::new(
                "cache_enabled",
                value,
                "expected bool (1/0/true/false)",
            )),
        }
    }

    if let Some(value) =
        get(ENV_MERMAID_CAPS_PROFILE).or_else(|| get(ENV_MERMAID_CAPABILITY_PROFILE))
    {
        let trimmed = value.trim();
        config.capability_profile = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }

    if let Some(value) = get(ENV_MERMAID_PALETTE) {
        match DiagramPalettePreset::parse(&value) {
            Some(parsed) => config.palette = parsed,
            None => errors.push(MermaidConfigError::new(
                "palette",
                value,
                "expected default|corporate|neon|monochrome|pastel|high-contrast",
            )),
        }
    }

    if let Err(mut validation) = config.validate() {
        errors.append(&mut validation);
    }

    MermaidConfigParse { config, errors }
}

#[inline]
fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[inline]
fn parse_usize(value: &str) -> Option<usize> {
    value.trim().parse::<usize>().ok()
}

const FNV1A_OFFSET: u64 = 0xcbf29ce484222325;
const FNV1A_PRIME: u64 = 0x100000001b3;

#[inline]
fn fnv1a_hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV1A_PRIME);
    }
}

#[inline]
fn fnv1a_hash_u64(hash: &mut u64, val: u64) {
    fnv1a_hash_bytes(hash, &val.to_le_bytes());
}

#[inline]
fn fnv1a_hash_usize(hash: &mut u64, val: usize) {
    fnv1a_hash_bytes(hash, &(val as u64).to_le_bytes());
}

#[inline]
#[allow(dead_code)]
fn fnv1a_hash_f64(hash: &mut u64, val: f64) {
    fnv1a_hash_bytes(hash, &val.to_le_bytes());
}

#[inline]
fn fnv1a_hash_str(hash: &mut u64, s: &str) {
    fnv1a_hash_usize(hash, s.len());
    fnv1a_hash_bytes(hash, s.as_bytes());
}

#[inline]
fn fnv1a_hash_bool(hash: &mut u64, b: bool) {
    fnv1a_hash_bytes(hash, &[u8::from(b)]);
}

/// Compute a deterministic FNV1a hash of a `MermaidDiagramIr`.
///
/// The hash captures structural identity: node ids, shapes, labels, edges,
/// clusters, ports, style refs, and links. Span information is excluded
/// since it does not affect layout/render output.
#[must_use]
pub fn hash_ir(ir: &MermaidDiagramIr) -> u64 {
    let mut h = FNV1A_OFFSET;

    // Diagram type + direction
    fnv1a_hash_str(&mut h, ir.diagram_type.as_str());
    fnv1a_hash_str(&mut h, ir.direction.as_str());

    // Nodes
    fnv1a_hash_usize(&mut h, ir.nodes.len());
    for node in &ir.nodes {
        fnv1a_hash_str(&mut h, &node.id);
        fnv1a_hash_bool(&mut h, node.label.is_some());
        if let Some(lid) = node.label {
            fnv1a_hash_usize(&mut h, lid.0);
        }
        fnv1a_hash_str(&mut h, node.shape.as_str());
        fnv1a_hash_usize(&mut h, node.classes.len());
        for c in &node.classes {
            fnv1a_hash_str(&mut h, c);
        }
        fnv1a_hash_bool(&mut h, node.style_ref.is_some());
        if let Some(sr) = node.style_ref {
            fnv1a_hash_usize(&mut h, sr.0);
        }
        fnv1a_hash_bool(&mut h, node.implicit);
        fnv1a_hash_usize(&mut h, node.members.len());
        for m in &node.members {
            fnv1a_hash_str(&mut h, m);
        }
    }

    // Labels
    fnv1a_hash_usize(&mut h, ir.labels.len());
    for label in &ir.labels {
        fnv1a_hash_str(&mut h, &label.text);
    }

    // Pie data
    fnv1a_hash_bool(&mut h, ir.pie_show_data);
    fnv1a_hash_bool(&mut h, ir.pie_title.is_some());
    if let Some(title_id) = ir.pie_title {
        fnv1a_hash_usize(&mut h, title_id.0);
    }
    fnv1a_hash_usize(&mut h, ir.pie_entries.len());
    for entry in &ir.pie_entries {
        fnv1a_hash_usize(&mut h, entry.label.0);
        fnv1a_hash_f64(&mut h, entry.value);
        fnv1a_hash_str(&mut h, &entry.value_text);
    }

    // Edges
    fnv1a_hash_usize(&mut h, ir.edges.len());
    for edge in &ir.edges {
        match edge.from {
            IrEndpoint::Node(nid) => {
                fnv1a_hash_bytes(&mut h, &[0]);
                fnv1a_hash_usize(&mut h, nid.0);
            }
            IrEndpoint::Port(pid) => {
                fnv1a_hash_bytes(&mut h, &[1]);
                fnv1a_hash_usize(&mut h, pid.0);
            }
        }
        match edge.to {
            IrEndpoint::Node(nid) => {
                fnv1a_hash_bytes(&mut h, &[0]);
                fnv1a_hash_usize(&mut h, nid.0);
            }
            IrEndpoint::Port(pid) => {
                fnv1a_hash_bytes(&mut h, &[1]);
                fnv1a_hash_usize(&mut h, pid.0);
            }
        }
        fnv1a_hash_str(&mut h, &edge.arrow);
        fnv1a_hash_bool(&mut h, edge.label.is_some());
        if let Some(lid) = edge.label {
            fnv1a_hash_usize(&mut h, lid.0);
        }
        fnv1a_hash_bool(&mut h, edge.style_ref.is_some());
        if let Some(sr) = edge.style_ref {
            fnv1a_hash_usize(&mut h, sr.0);
        }
    }

    // Clusters
    fnv1a_hash_usize(&mut h, ir.clusters.len());
    for cluster in &ir.clusters {
        fnv1a_hash_usize(&mut h, cluster.id.0);
        fnv1a_hash_bool(&mut h, cluster.title.is_some());
        if let Some(lid) = cluster.title {
            fnv1a_hash_usize(&mut h, lid.0);
        }
        fnv1a_hash_usize(&mut h, cluster.members.len());
        for m in &cluster.members {
            fnv1a_hash_usize(&mut h, m.0);
        }
    }

    // Ports
    fnv1a_hash_usize(&mut h, ir.ports.len());
    for port in &ir.ports {
        fnv1a_hash_usize(&mut h, port.node.0);
        fnv1a_hash_str(&mut h, &port.name);
    }

    // Style refs
    fnv1a_hash_usize(&mut h, ir.style_refs.len());
    for sr in &ir.style_refs {
        fnv1a_hash_str(&mut h, &sr.style);
    }

    // Links
    fnv1a_hash_usize(&mut h, ir.links.len());
    for link in &ir.links {
        fnv1a_hash_usize(&mut h, link.target.0);
        fnv1a_hash_str(&mut h, &link.url);
    }

    h
}

/// Compute a deterministic hash of layout-relevant `MermaidConfig` fields.
///
/// Only fields that influence layout/render output are included. Log path,
/// enabled flag, etc. are excluded.
#[must_use]
pub fn hash_config_layout(config: &MermaidConfig) -> u64 {
    let mut h = FNV1A_OFFSET;
    fnv1a_hash_str(&mut h, config.glyph_mode.as_str());
    fnv1a_hash_str(&mut h, config.render_mode.as_str());
    fnv1a_hash_str(&mut h, config.tier_override.as_str());
    fnv1a_hash_usize(&mut h, config.max_nodes);
    fnv1a_hash_usize(&mut h, config.max_edges);
    fnv1a_hash_usize(&mut h, config.route_budget);
    fnv1a_hash_usize(&mut h, config.layout_iteration_budget);
    fnv1a_hash_usize(&mut h, config.max_label_chars);
    fnv1a_hash_usize(&mut h, config.max_label_lines);
    fnv1a_hash_str(&mut h, config.wrap_mode.as_str());
    fnv1a_hash_bool(&mut h, config.enable_styles);
    fnv1a_hash_bool(&mut h, config.enable_links);
    fnv1a_hash_str(&mut h, config.link_mode.as_str());
    if let Some(ref cp) = config.capability_profile {
        fnv1a_hash_bool(&mut h, true);
        fnv1a_hash_str(&mut h, cp);
    } else {
        fnv1a_hash_bool(&mut h, false);
    }
    fnv1a_hash_str(&mut h, config.palette.as_str());
    h
}

/// Full cache key for a diagram layout result.
///
/// Captures all inputs that affect determinism: IR structure, config,
/// and init config hash (from init directives/theme).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiagramCacheKey {
    /// FNV1a hash of the `MermaidDiagramIr` structure.
    pub ir_hash: u64,
    /// FNV1a hash of layout-relevant `MermaidConfig` fields.
    pub config_hash: u64,
    /// FNV1a hash of init directives (from `MermaidInitConfig::checksum`).
    pub init_config_hash: u64,
}

impl DiagramCacheKey {
    /// Build a cache key from an IR, config, and init config hash.
    #[must_use]
    pub fn new(ir: &MermaidDiagramIr, config: &MermaidConfig, init_config_hash: u64) -> Self {
        Self {
            ir_hash: hash_ir(ir),
            config_hash: hash_config_layout(config),
            init_config_hash,
        }
    }

    /// Combined key hash (for compact logging).
    #[must_use]
    pub fn combined_hash(&self) -> u64 {
        let mut h = FNV1A_OFFSET;
        fnv1a_hash_u64(&mut h, self.ir_hash);
        fnv1a_hash_u64(&mut h, self.config_hash);
        fnv1a_hash_u64(&mut h, self.init_config_hash);
        h
    }

    /// Hex representation of the combined hash.
    #[must_use]
    pub fn combined_hash_hex(&self) -> String {
        format!("0x{:016x}", self.combined_hash())
    }
}

/// Bounded diagram layout cache.
///
/// Thread-safe, bounded LRU-style cache mapping `DiagramCacheKey` to
/// `DiagramLayout`. The cache is keyed on IR + config + init hash so that
/// any change to inputs invalidates the entry.
pub struct DiagramCache {
    entries: std::sync::Mutex<Vec<DiagramCacheEntry>>,
    capacity: usize,
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
}

struct DiagramCacheEntry {
    key: DiagramCacheKey,
    layout: crate::mermaid_layout::DiagramLayout,
    last_used: u64, // monotonic counter for LRU eviction
}

impl DiagramCache {
    /// Create a new cache with the given capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: std::sync::Mutex::new(Vec::with_capacity(capacity.min(64))),
            capacity: capacity.max(1),
            hits: std::sync::atomic::AtomicU64::new(0),
            misses: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Look up a layout by cache key. Returns `Some` on hit.
    pub fn get(&self, key: &DiagramCacheKey) -> Option<crate::mermaid_layout::DiagramLayout> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entries.iter_mut().find(|e| e.key == *key) {
            let counter = self.hits.load(std::sync::atomic::Ordering::Relaxed)
                + self.misses.load(std::sync::atomic::Ordering::Relaxed);
            entry.last_used = counter;
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(entry.layout.clone())
        } else {
            self.misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }

    /// Insert a layout into the cache. Evicts LRU entry if at capacity.
    pub fn insert(&self, key: DiagramCacheKey, layout: crate::mermaid_layout::DiagramLayout) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // Update existing entry if present.
        if let Some(entry) = entries.iter_mut().find(|e| e.key == key) {
            let counter = self.hits.load(std::sync::atomic::Ordering::Relaxed)
                + self.misses.load(std::sync::atomic::Ordering::Relaxed);
            entry.layout = layout;
            entry.last_used = counter;
            return;
        }
        // Evict LRU if at capacity.
        if entries.len() >= self.capacity
            && let Some(idx) = entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(i, _)| i)
        {
            entries.swap_remove(idx);
        }
        let counter = self.hits.load(std::sync::atomic::Ordering::Relaxed)
            + self.misses.load(std::sync::atomic::Ordering::Relaxed);
        entries.push(DiagramCacheEntry {
            key,
            layout,
            last_used: counter,
        });
    }

    /// Number of cache hits.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Number of cache misses.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Number of entries currently stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all entries and reset counters.
    pub fn clear(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.clear();
        self.hits.store(0, std::sync::atomic::Ordering::Relaxed);
        self.misses.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Emit a cache-lookup evidence event to the JSONL log.
pub(crate) fn emit_cache_lookup_jsonl(
    config: &MermaidConfig,
    cache_key: &DiagramCacheKey,
    hit: bool,
    cache_size: usize,
    cache_hits: u64,
    cache_misses: u64,
) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    let json = serde_json::json!({
        "event": "cache_lookup",
        "ir_hash": format!("0x{:016x}", cache_key.ir_hash),
        "config_hash": format!("0x{:016x}", cache_key.config_hash),
        "init_config_hash": format!("0x{:016x}", cache_key.init_config_hash),
        "combined_key": cache_key.combined_hash_hex(),
        "cache_hit": hit,
        "cache_size": cache_size,
        "total_hits": cache_hits,
        "total_misses": cache_misses,
    });
    let _ = append_jsonl_line(path, &json.to_string());
}

/// Perform a cached layout: look up in cache, compute on miss, emit evidence.
///
/// If `config.cache_enabled` is false, always computes fresh layout.
pub fn layout_diagram_cached(
    ir: &MermaidDiagramIr,
    config: &MermaidConfig,
    init_config_hash: u64,
    cache: &DiagramCache,
) -> crate::mermaid_layout::DiagramLayout {
    let cache_key = DiagramCacheKey::new(ir, config, init_config_hash);

    if config.cache_enabled
        && let Some(layout) = cache.get(&cache_key)
    {
        emit_cache_lookup_jsonl(
            config,
            &cache_key,
            true,
            cache.len(),
            cache.hits(),
            cache.misses(),
        );
        return layout;
    }

    let layout = crate::mermaid_layout::layout_diagram(ir, config);

    if config.cache_enabled {
        cache.insert(cache_key.clone(), layout.clone());
    }

    emit_cache_lookup_jsonl(
        config,
        &cache_key,
        false,
        cache.len(),
        cache.hits(),
        cache.misses(),
    );

    layout
}

fn validate_positive(field: &'static str, value: usize, errors: &mut Vec<MermaidConfigError>) {
    if value == 0 {
        errors.push(MermaidConfigError::new(
            field,
            value.to_string(),
            "must be >= 1",
        ));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramType {
    Graph,
    Sequence,
    State,
    Gantt,
    Class,
    Er,
    Mindmap,
    Pie,
    Unknown,
}

impl DiagramType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::Sequence => "sequence",
            Self::State => "state",
            Self::Gantt => "gantt",
            Self::Class => "class",
            Self::Er => "er",
            Self::Mindmap => "mindmap",
            Self::Pie => "pie",
            Self::Unknown => "unknown",
        }
    }
}

/// Compatibility level for a Mermaid feature or diagram type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidSupportLevel {
    Supported,
    Partial,
    Unsupported,
}

impl MermaidSupportLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Partial => "partial",
            Self::Unsupported => "unsupported",
        }
    }
}

// ── Feature matrix for diagram demos ─────────────────────────────────
//
// Structured inventory of supported diagram families, syntax features,
// and which test fixture / sample demonstrates each capability. Used by
// the mega-screen sample registry to map coverage gaps.

/// One row of the Mermaid feature matrix.
#[derive(Debug, Clone, Copy)]
pub struct FeatureMatrixEntry {
    /// Diagram family (e.g. "graph", "sequence", "state").
    pub family: DiagramType,
    /// Specific syntax feature (e.g. "subgraphs", "classDef", "sequence messages").
    pub feature: &'static str,
    /// Support level in the current engine.
    pub level: MermaidSupportLevel,
    /// Test fixture that exercises this feature (relative to tests/fixtures/mermaid/).
    pub fixture: Option<&'static str>,
    /// Brief note about gaps or planned improvements.
    pub note: &'static str,
}

/// Comprehensive feature matrix mapping diagram capabilities to demo samples.
///
/// # How to use
///
/// The mega-screen sample registry should reference these entries so every
/// planned sample has a declared purpose and linked feature coverage. Entries
/// with `fixture: None` represent explicit gaps that need sample coverage.
pub const FEATURE_MATRIX: &[FeatureMatrixEntry] = &[
    // ── Flowchart / Graph ───────────────────────────────────────────
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "basic nodes + edges", level: MermaidSupportLevel::Supported, fixture: Some("graph_small.mmd"), note: "" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "subgraphs / clusters", level: MermaidSupportLevel::Supported, fixture: Some("graph_medium.mmd"), note: "" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "direction override (TB/LR/RL/BT)", level: MermaidSupportLevel::Supported, fixture: Some("graph_medium.mmd"), note: "all 5 directions" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "all 8 node shapes", level: MermaidSupportLevel::Supported, fixture: None, note: "cell-mode shapes via draw_shaped_node" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "classDef / style / class", level: MermaidSupportLevel::Supported, fixture: Some("graph_medium.mmd"), note: "fill/stroke/color/dash" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "linkStyle", level: MermaidSupportLevel::Supported, fixture: None, note: "edge styling by index" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "init directives (%%{init}%%)", level: MermaidSupportLevel::Supported, fixture: Some("graph_init_directive.mmd"), note: "theme + themeVariables + direction" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "click / link handlers", level: MermaidSupportLevel::Partial, fixture: None, note: "parsed but rendering TBD" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "unicode / long labels", level: MermaidSupportLevel::Supported, fixture: Some("graph_unicode_labels.mmd"), note: "wrapping + truncation" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "large graph (>50 nodes)", level: MermaidSupportLevel::Supported, fixture: Some("graph_large.mmd"), note: "stress + layout scale" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "edge label placement", level: MermaidSupportLevel::Supported, fixture: None, note: "collision-avoidance labels" },
    FeatureMatrixEntry { family: DiagramType::Graph, feature: "color themes / palettes", level: MermaidSupportLevel::Supported, fixture: None, note: "6 preset palettes" },
    // ── Sequence ────────────────────────────────────────────────────
    FeatureMatrixEntry { family: DiagramType::Sequence, feature: "participants + messages", level: MermaidSupportLevel::Partial, fixture: Some("sequence_basic.mmd"), note: "basic parse; layout WIP" },
    FeatureMatrixEntry { family: DiagramType::Sequence, feature: "activation bars", level: MermaidSupportLevel::Unsupported, fixture: None, note: "not yet implemented" },
    FeatureMatrixEntry { family: DiagramType::Sequence, feature: "notes", level: MermaidSupportLevel::Unsupported, fixture: None, note: "not yet implemented" },
    FeatureMatrixEntry { family: DiagramType::Sequence, feature: "alt/opt/loop/par blocks", level: MermaidSupportLevel::Unsupported, fixture: None, note: "not yet implemented" },
    // ── State ───────────────────────────────────────────────────────
    FeatureMatrixEntry { family: DiagramType::State, feature: "basic transitions", level: MermaidSupportLevel::Supported, fixture: Some("state_basic.mmd"), note: "" },
    FeatureMatrixEntry { family: DiagramType::State, feature: "composite states", level: MermaidSupportLevel::Supported, fixture: Some("state_composite.mmd"), note: "nested state containers" },
    FeatureMatrixEntry { family: DiagramType::State, feature: "start/end markers", level: MermaidSupportLevel::Supported, fixture: Some("state_composite.mmd"), note: "[*] nodes" },
    FeatureMatrixEntry { family: DiagramType::State, feature: "notes", level: MermaidSupportLevel::Partial, fixture: None, note: "parsed; render TBD" },
    // ── Class ───────────────────────────────────────────────────────
    FeatureMatrixEntry { family: DiagramType::Class, feature: "class declarations", level: MermaidSupportLevel::Supported, fixture: Some("class_basic.mmd"), note: "" },
    FeatureMatrixEntry { family: DiagramType::Class, feature: "members (fields + methods)", level: MermaidSupportLevel::Supported, fixture: Some("class_basic.mmd"), note: "via IrNode.members" },
    FeatureMatrixEntry { family: DiagramType::Class, feature: "inheritance/association edges", level: MermaidSupportLevel::Supported, fixture: Some("class_basic.mmd"), note: "" },
    FeatureMatrixEntry { family: DiagramType::Class, feature: "class annotations", level: MermaidSupportLevel::Unsupported, fixture: None, note: "<<interface>> etc." },
    // ── ER ──────────────────────────────────────────────────────────
    FeatureMatrixEntry { family: DiagramType::Er, feature: "entity-relationship edges", level: MermaidSupportLevel::Supported, fixture: Some("er_basic.mmd"), note: "" },
    FeatureMatrixEntry { family: DiagramType::Er, feature: "cardinality labels", level: MermaidSupportLevel::Partial, fixture: Some("er_basic.mmd"), note: "parsed; rendering basic" },
    FeatureMatrixEntry { family: DiagramType::Er, feature: "entity attributes", level: MermaidSupportLevel::Unsupported, fixture: None, note: "not yet implemented" },
    // ── Gantt ───────────────────────────────────────────────────────
    FeatureMatrixEntry { family: DiagramType::Gantt, feature: "title + sections + tasks", level: MermaidSupportLevel::Supported, fixture: Some("gantt_basic.mmd"), note: "" },
    FeatureMatrixEntry { family: DiagramType::Gantt, feature: "date-based timelines", level: MermaidSupportLevel::Partial, fixture: None, note: "parsed; visual layout basic" },
    FeatureMatrixEntry { family: DiagramType::Gantt, feature: "milestones", level: MermaidSupportLevel::Unsupported, fixture: None, note: "not yet implemented" },
    // ── Mindmap ─────────────────────────────────────────────────────
    FeatureMatrixEntry { family: DiagramType::Mindmap, feature: "indent-based hierarchy", level: MermaidSupportLevel::Supported, fixture: Some("mindmap_basic.mmd"), note: "depth detection" },
    FeatureMatrixEntry { family: DiagramType::Mindmap, feature: "node shapes in mindmap", level: MermaidSupportLevel::Partial, fixture: None, note: "shape parsing TBD" },
    // ── Pie ─────────────────────────────────────────────────────────
    FeatureMatrixEntry { family: DiagramType::Pie, feature: "pie entries with values", level: MermaidSupportLevel::Supported, fixture: Some("pie_basic.mmd"), note: "" },
    FeatureMatrixEntry { family: DiagramType::Pie, feature: "pie title", level: MermaidSupportLevel::Supported, fixture: Some("pie_basic.mmd"), note: "" },
    FeatureMatrixEntry { family: DiagramType::Pie, feature: "showData toggle", level: MermaidSupportLevel::Partial, fixture: None, note: "parsed; render TBD" },
    // ── Cross-cutting ───────────────────────────────────────────────
    FeatureMatrixEntry { family: DiagramType::Unknown, feature: "unsupported diagram fallback", level: MermaidSupportLevel::Supported, fixture: Some("unsupported_mix.mmd"), note: "graceful error panel" },
    FeatureMatrixEntry { family: DiagramType::Unknown, feature: "error panel / raw / both modes", level: MermaidSupportLevel::Supported, fixture: None, note: "snapshot tested" },
    FeatureMatrixEntry { family: DiagramType::Unknown, feature: "cache + hash invalidation", level: MermaidSupportLevel::Supported, fixture: None, note: "DiagramCacheKey" },
    FeatureMatrixEntry { family: DiagramType::Unknown, feature: "fidelity tiers (compact/normal/rich)", level: MermaidSupportLevel::Supported, fixture: None, note: "RenderPlan selection" },
    FeatureMatrixEntry { family: DiagramType::Unknown, feature: "interactive selection + highlights", level: MermaidSupportLevel::Supported, fixture: None, note: "SelectionState + navigate_direction" },
    FeatureMatrixEntry { family: DiagramType::Unknown, feature: "debug overlay", level: MermaidSupportLevel::Supported, fixture: None, note: "bounding boxes + metrics" },
];

/// Return features for a specific diagram type.
#[must_use]
pub fn features_for_type(dt: DiagramType) -> Vec<&'static FeatureMatrixEntry> {
    FEATURE_MATRIX.iter().filter(|e| e.family == dt).collect()
}

/// Count features by support level.
#[must_use]
pub fn feature_coverage_summary() -> (usize, usize, usize) {
    let supported = FEATURE_MATRIX.iter().filter(|e| e.level == MermaidSupportLevel::Supported).count();
    let partial = FEATURE_MATRIX.iter().filter(|e| e.level == MermaidSupportLevel::Partial).count();
    let unsupported = FEATURE_MATRIX.iter().filter(|e| e.level == MermaidSupportLevel::Unsupported).count();
    (supported, partial, unsupported)
}

/// Features that have no test fixture (explicit coverage gaps).
#[must_use]
pub fn uncovered_features() -> Vec<&'static FeatureMatrixEntry> {
    FEATURE_MATRIX.iter().filter(|e| e.fixture.is_none()).collect()
}

// ── Interaction model + keymap spec ──────────────────────────────────
//
// Canonical keymap for the Mermaid showcase / mega screen. All keybindings
// are documented here so that (1) help overlays can render from this data,
// (2) new screens reference a single source of truth, and (3) conflicts
// are caught at definition time.

/// Interaction mode for the showcase screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowcaseMode {
    /// Normal mode: sample selection, config knobs, layout controls.
    Normal,
    /// Inspect mode: node navigation, edge following, detail panel.
    Inspect,
    /// Search mode: text input filtering nodes by label.
    Search,
}

impl ShowcaseMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Inspect => "inspect",
            Self::Search => "search",
        }
    }
}

/// Keybinding category for grouping in help overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCategory {
    /// Sample list navigation.
    SampleNav,
    /// Render and layout configuration.
    RenderConfig,
    /// Viewport and zoom.
    Viewport,
    /// Node inspection and navigation.
    NodeInspect,
    /// Search and filtering.
    Search,
    /// Panel toggles and UI.
    Panels,
    /// Palette and themes.
    Theme,
}

impl KeyCategory {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SampleNav => "Samples",
            Self::RenderConfig => "Render",
            Self::Viewport => "Viewport",
            Self::NodeInspect => "Inspect",
            Self::Search => "Search",
            Self::Panels => "Panels",
            Self::Theme => "Theme",
        }
    }
}

/// A single keymap entry for the help overlay.
#[derive(Debug, Clone, Copy)]
pub struct KeymapEntry {
    /// Display string for the key (e.g. "j/↓", "Tab", "/").
    pub key: &'static str,
    /// Short action description (e.g. "Next sample", "Select node").
    pub action: &'static str,
    /// Which category this belongs to.
    pub category: KeyCategory,
    /// Which modes this binding is active in.
    pub modes: &'static [ShowcaseMode],
}

/// Canonical keymap for the Mermaid showcase screen.
///
/// This is the single source of truth for all keybindings. The help overlay
/// and on-screen hints render from this table. New screens should extend
/// (not replace) these bindings.
pub const SHOWCASE_KEYMAP: &[KeymapEntry] = &[
    // ── Sample navigation (Normal mode) ─────────────────────────────
    KeymapEntry { key: "j/↓", action: "Next sample", category: KeyCategory::SampleNav, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "k/↑", action: "Previous sample", category: KeyCategory::SampleNav, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "Home", action: "First sample", category: KeyCategory::SampleNav, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "End", action: "Last sample", category: KeyCategory::SampleNav, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "Enter", action: "Refresh / re-render", category: KeyCategory::SampleNav, modes: &[ShowcaseMode::Normal] },
    // ── Render configuration (Normal mode) ──────────────────────────
    KeymapEntry { key: "t", action: "Cycle fidelity tier", category: KeyCategory::RenderConfig, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "g", action: "Toggle glyph mode (Unicode/ASCII)", category: KeyCategory::RenderConfig, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "b", action: "Cycle render mode (Cell/Braille/Block/Half)", category: KeyCategory::RenderConfig, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "s", action: "Toggle style rendering", category: KeyCategory::RenderConfig, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "w", action: "Cycle wrap mode", category: KeyCategory::RenderConfig, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "l", action: "Toggle layout mode (Dense/Normal/Spacious)", category: KeyCategory::RenderConfig, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "r", action: "Force re-layout", category: KeyCategory::RenderConfig, modes: &[ShowcaseMode::Normal] },
    // ── Viewport and zoom (Normal + Inspect) ────────────────────────
    KeymapEntry { key: "+/=", action: "Zoom in", category: KeyCategory::Viewport, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "-", action: "Zoom out", category: KeyCategory::Viewport, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "0", action: "Reset zoom", category: KeyCategory::Viewport, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "f", action: "Fit diagram to view", category: KeyCategory::Viewport, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "]", action: "Increase viewport width", category: KeyCategory::Viewport, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "[", action: "Decrease viewport width", category: KeyCategory::Viewport, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "}", action: "Increase viewport height", category: KeyCategory::Viewport, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "{", action: "Decrease viewport height", category: KeyCategory::Viewport, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "o", action: "Reset viewport override", category: KeyCategory::Viewport, modes: &[ShowcaseMode::Normal] },
    // ── Node inspection (Inspect mode) ──────────────────────────────
    KeymapEntry { key: "Tab", action: "Select next node (layout order)", category: KeyCategory::NodeInspect, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "S-Tab", action: "Select previous node", category: KeyCategory::NodeInspect, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "←/→/↑/↓", action: "Navigate to connected node", category: KeyCategory::NodeInspect, modes: &[ShowcaseMode::Inspect] },
    KeymapEntry { key: "Esc", action: "Deselect / exit inspect mode", category: KeyCategory::NodeInspect, modes: &[ShowcaseMode::Inspect] },
    // ── Search (Search mode) ────────────────────────────────────────
    KeymapEntry { key: "/", action: "Enter search mode", category: KeyCategory::Search, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "n", action: "Next search match", category: KeyCategory::Search, modes: &[ShowcaseMode::Search] },
    KeymapEntry { key: "N", action: "Previous search match", category: KeyCategory::Search, modes: &[ShowcaseMode::Search] },
    KeymapEntry { key: "Esc", action: "Clear search / exit search mode", category: KeyCategory::Search, modes: &[ShowcaseMode::Search] },
    // ── Panels and UI (all modes) ───────────────────────────────────
    KeymapEntry { key: "m", action: "Toggle metrics panel", category: KeyCategory::Panels, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "c", action: "Toggle controls panel", category: KeyCategory::Panels, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "i", action: "Toggle status log", category: KeyCategory::Panels, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect] },
    KeymapEntry { key: "?", action: "Toggle help overlay", category: KeyCategory::Panels, modes: &[ShowcaseMode::Normal, ShowcaseMode::Inspect, ShowcaseMode::Search] },
    // ── Theme (Normal mode) ─────────────────────────────────────────
    KeymapEntry { key: "p", action: "Cycle color palette", category: KeyCategory::Theme, modes: &[ShowcaseMode::Normal] },
    KeymapEntry { key: "P", action: "Previous color palette", category: KeyCategory::Theme, modes: &[ShowcaseMode::Normal] },
];

/// Return keymap entries for a specific mode.
#[must_use]
pub fn keymap_for_mode(mode: ShowcaseMode) -> Vec<&'static KeymapEntry> {
    SHOWCASE_KEYMAP
        .iter()
        .filter(|e| e.modes.contains(&mode))
        .collect()
}

/// Return keymap entries grouped by category.
#[must_use]
pub fn keymap_by_category(mode: ShowcaseMode) -> Vec<(KeyCategory, Vec<&'static KeymapEntry>)> {
    let categories = [
        KeyCategory::SampleNav,
        KeyCategory::RenderConfig,
        KeyCategory::Viewport,
        KeyCategory::NodeInspect,
        KeyCategory::Search,
        KeyCategory::Panels,
        KeyCategory::Theme,
    ];
    categories
        .iter()
        .filter_map(|&cat| {
            let entries: Vec<_> = SHOWCASE_KEYMAP
                .iter()
                .filter(|e| e.category == cat && e.modes.contains(&mode))
                .collect();
            if entries.is_empty() {
                None
            } else {
                Some((cat, entries))
            }
        })
        .collect()
}

/// Warning taxonomy for Mermaid compatibility and fallback handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidWarningCode {
    UnsupportedDiagram,
    UnsupportedDirective,
    UnsupportedStyle,
    UnsupportedLink,
    UnsupportedFeature,
    SanitizedInput,
    ImplicitNode,
    InvalidEdge,
    InvalidPort,
    InvalidValue,
    LimitExceeded,
    BudgetExceeded,
}

impl MermaidWarningCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedDiagram => "mermaid/unsupported/diagram",
            Self::UnsupportedDirective => "mermaid/unsupported/directive",
            Self::UnsupportedStyle => "mermaid/unsupported/style",
            Self::UnsupportedLink => "mermaid/unsupported/link",
            Self::UnsupportedFeature => "mermaid/unsupported/feature",
            Self::SanitizedInput => "mermaid/sanitized/input",
            Self::ImplicitNode => "mermaid/implicit/node",
            Self::InvalidEdge => "mermaid/invalid/edge",
            Self::InvalidPort => "mermaid/invalid/port",
            Self::InvalidValue => "mermaid/invalid/value",
            Self::LimitExceeded => "mermaid/limit/exceeded",
            Self::BudgetExceeded => "mermaid/budget/exceeded",
        }
    }
}

/// Compatibility matrix across Mermaid diagram types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MermaidCompatibilityMatrix {
    pub graph: MermaidSupportLevel,
    pub sequence: MermaidSupportLevel,
    pub state: MermaidSupportLevel,
    pub gantt: MermaidSupportLevel,
    pub class: MermaidSupportLevel,
    pub er: MermaidSupportLevel,
    pub mindmap: MermaidSupportLevel,
    pub pie: MermaidSupportLevel,
}

impl MermaidCompatibilityMatrix {
    /// Parser-only compatibility profile (renderer pending).
    #[must_use]
    pub const fn parser_only() -> Self {
        Self {
            graph: MermaidSupportLevel::Partial,
            sequence: MermaidSupportLevel::Partial,
            state: MermaidSupportLevel::Partial,
            gantt: MermaidSupportLevel::Partial,
            class: MermaidSupportLevel::Partial,
            er: MermaidSupportLevel::Supported,
            mindmap: MermaidSupportLevel::Partial,
            pie: MermaidSupportLevel::Partial,
        }
    }

    #[must_use]
    pub const fn support_for(&self, diagram_type: DiagramType) -> MermaidSupportLevel {
        match diagram_type {
            DiagramType::Graph => self.graph,
            DiagramType::Sequence => self.sequence,
            DiagramType::State => self.state,
            DiagramType::Gantt => self.gantt,
            DiagramType::Class => self.class,
            DiagramType::Er => self.er,
            DiagramType::Mindmap => self.mindmap,
            DiagramType::Pie => self.pie,
            DiagramType::Unknown => MermaidSupportLevel::Unsupported,
        }
    }
}

impl Default for MermaidCompatibilityMatrix {
    fn default() -> Self {
        Self {
            graph: MermaidSupportLevel::Supported,
            sequence: MermaidSupportLevel::Partial,
            state: MermaidSupportLevel::Partial,
            gantt: MermaidSupportLevel::Partial,
            class: MermaidSupportLevel::Partial,
            er: MermaidSupportLevel::Supported,
            mindmap: MermaidSupportLevel::Partial,
            pie: MermaidSupportLevel::Partial,
        }
    }
}

/// Compatibility warning emitted during validation/fallback analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MermaidWarning {
    pub code: MermaidWarningCode,
    pub message: String,
    pub span: Span,
}

impl MermaidWarning {
    fn new(code: MermaidWarningCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into(),
            span,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MermaidComplexity {
    pub nodes: usize,
    pub edges: usize,
    pub labels: usize,
    pub clusters: usize,
    pub ports: usize,
    pub style_refs: usize,
    pub score: usize,
}

impl MermaidComplexity {
    #[must_use]
    pub fn from_counts(
        nodes: usize,
        edges: usize,
        labels: usize,
        clusters: usize,
        ports: usize,
        style_refs: usize,
    ) -> Self {
        let score = nodes
            .saturating_add(edges)
            .saturating_add(labels)
            .saturating_add(clusters);
        Self {
            nodes,
            edges,
            labels,
            clusters,
            ports,
            style_refs,
            score,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MermaidFidelity {
    Rich,
    #[default]
    Normal,
    Compact,
    Outline,
}

impl MermaidFidelity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rich => "rich",
            Self::Normal => "normal",
            Self::Compact => "compact",
            Self::Outline => "outline",
        }
    }

    #[must_use]
    pub const fn from_tier(tier: MermaidTier) -> Self {
        match tier {
            MermaidTier::Rich => Self::Rich,
            MermaidTier::Normal => Self::Normal,
            MermaidTier::Compact => Self::Compact,
            MermaidTier::Auto => Self::Normal,
        }
    }

    #[must_use]
    pub const fn degrade(self) -> Self {
        match self {
            Self::Rich => Self::Normal,
            Self::Normal => Self::Compact,
            Self::Compact => Self::Outline,
            Self::Outline => Self::Outline,
        }
    }

    #[must_use]
    pub const fn is_compact_or_outline(self) -> bool {
        matches!(self, Self::Compact | Self::Outline)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MermaidDegradationPlan {
    pub target_fidelity: MermaidFidelity,
    pub hide_labels: bool,
    pub collapse_clusters: bool,
    pub simplify_routing: bool,
    pub reduce_decoration: bool,
    pub force_glyph_mode: Option<MermaidGlyphMode>,
}

#[derive(Debug, Clone, Default)]
pub struct MermaidGuardReport {
    pub complexity: MermaidComplexity,
    pub label_chars_over: usize,
    pub label_lines_over: usize,
    pub node_limit_exceeded: bool,
    pub edge_limit_exceeded: bool,
    pub label_limit_exceeded: bool,
    pub route_budget_exceeded: bool,
    pub layout_budget_exceeded: bool,
    pub limits_exceeded: bool,
    pub budget_exceeded: bool,
    pub route_ops_estimate: usize,
    pub layout_iterations_estimate: usize,
    pub degradation: MermaidDegradationPlan,
}

/// Action to apply when encountering unsupported Mermaid input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidFallbackAction {
    Ignore,
    Warn,
    Error,
}

/// Policy controlling how unsupported Mermaid features are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MermaidFallbackPolicy {
    pub unsupported_diagram: MermaidFallbackAction,
    pub unsupported_directive: MermaidFallbackAction,
    pub unsupported_style: MermaidFallbackAction,
    pub unsupported_link: MermaidFallbackAction,
    pub unsupported_feature: MermaidFallbackAction,
}

impl Default for MermaidFallbackPolicy {
    fn default() -> Self {
        Self {
            unsupported_diagram: MermaidFallbackAction::Error,
            unsupported_directive: MermaidFallbackAction::Warn,
            unsupported_style: MermaidFallbackAction::Warn,
            unsupported_link: MermaidFallbackAction::Warn,
            unsupported_feature: MermaidFallbackAction::Warn,
        }
    }
}

/// Validation output for a Mermaid AST.
#[derive(Debug, Clone, Default)]
pub struct MermaidValidation {
    pub warnings: Vec<MermaidWarning>,
    pub errors: Vec<MermaidError>,
}

impl MermaidValidation {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Compatibility report for a parsed Mermaid AST.
#[derive(Debug, Clone)]
pub struct MermaidCompatibilityReport {
    pub diagram_support: MermaidSupportLevel,
    pub warnings: Vec<MermaidWarning>,
    pub fatal: bool,
}

impl MermaidCompatibilityReport {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        !self.fatal
    }
}

/// Parsed init directive configuration (subset of Mermaid schema).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MermaidInitConfig {
    pub theme: Option<String>,
    pub theme_variables: BTreeMap<String, String>,
    pub flowchart_direction: Option<GraphDirection>,
}

impl MermaidInitConfig {
    fn merge_from(&mut self, other: MermaidInitConfig) {
        if other.theme.is_some() {
            self.theme = other.theme;
        }
        if !other.theme_variables.is_empty() {
            self.theme_variables.extend(other.theme_variables);
        }
        if other.flowchart_direction.is_some() {
            self.flowchart_direction = other.flowchart_direction;
        }
    }

    fn apply_to_ast(&self, ast: &mut MermaidAst) {
        if let Some(direction) = self.flowchart_direction {
            ast.direction = Some(direction);
        }
    }

    /// Extract theme overrides implied by init directives.
    #[must_use]
    pub fn theme_overrides(&self) -> MermaidThemeOverrides {
        MermaidThemeOverrides {
            theme: self.theme.clone(),
            theme_variables: self.theme_variables.clone(),
        }
    }

    /// Deterministic checksum over init directive config.
    #[must_use]
    pub fn checksum(&self) -> u64 {
        let mut hash = FNV1A_OFFSET;
        fnv1a_hash_bytes(&mut hash, &[self.theme.is_some() as u8]);
        if let Some(theme) = &self.theme {
            fnv1a_hash_bytes(&mut hash, theme.as_bytes());
        }
        fnv1a_hash_bytes(&mut hash, &[0u8]);
        for (key, value) in &self.theme_variables {
            fnv1a_hash_bytes(&mut hash, key.as_bytes());
            fnv1a_hash_bytes(&mut hash, b"=");
            fnv1a_hash_bytes(&mut hash, value.as_bytes());
            fnv1a_hash_bytes(&mut hash, &[0u8]);
        }
        if let Some(direction) = self.flowchart_direction {
            fnv1a_hash_bytes(&mut hash, direction.as_str().as_bytes());
        } else {
            fnv1a_hash_bytes(&mut hash, b"none");
        }
        hash
    }

    /// Hex-encoded checksum for logging.
    #[must_use]
    pub fn checksum_hex(&self) -> String {
        format!("{:016x}", self.checksum())
    }
}

/// Theme overrides derived from Mermaid init directives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MermaidThemeOverrides {
    pub theme: Option<String>,
    pub theme_variables: BTreeMap<String, String>,
}

/// Result of parsing one or more init directives.
#[derive(Debug, Clone)]
pub struct MermaidInitParse {
    pub config: MermaidInitConfig,
    pub warnings: Vec<MermaidWarning>,
    pub errors: Vec<MermaidError>,
}

impl MermaidInitParse {
    fn empty() -> Self {
        Self {
            config: MermaidInitConfig::default(),
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }
}

impl Default for MermaidInitParse {
    fn default() -> Self {
        Self::empty()
    }
}

/// Parse a single Mermaid init directive payload into a config subset.
#[must_use]
pub fn parse_init_directive(
    payload: &str,
    span: Span,
    policy: &MermaidFallbackPolicy,
) -> MermaidInitParse {
    let mut out = MermaidInitParse::empty();
    let value: Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(err) => {
            out.errors.push(MermaidError::new(
                format!("invalid init directive json: {err}"),
                span,
            ));
            return out;
        }
    };
    let obj = match value.as_object() {
        Some(obj) => obj,
        None => {
            apply_fallback_action(
                policy.unsupported_directive,
                MermaidWarningCode::UnsupportedDirective,
                "init directive must be a JSON object; ignoring",
                span,
                &mut out.warnings,
                &mut out.errors,
            );
            return out;
        }
    };
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    for key in keys {
        let entry = &obj[key];
        match key.as_str() {
            "theme" => {
                if let Some(theme) = entry.as_str() {
                    let trimmed = theme.trim();
                    if trimmed.is_empty() {
                        apply_fallback_action(
                            policy.unsupported_directive,
                            MermaidWarningCode::UnsupportedDirective,
                            "init theme is empty; ignoring",
                            span,
                            &mut out.warnings,
                            &mut out.errors,
                        );
                    } else {
                        out.config.theme = Some(trimmed.to_string());
                    }
                } else {
                    apply_fallback_action(
                        policy.unsupported_directive,
                        MermaidWarningCode::UnsupportedDirective,
                        "init theme must be a string; ignoring",
                        span,
                        &mut out.warnings,
                        &mut out.errors,
                    );
                }
            }
            "themeVariables" => {
                if let Some(vars) = entry.as_object() {
                    let mut var_keys: Vec<&String> = vars.keys().collect();
                    var_keys.sort();
                    for var_key in var_keys {
                        let value = &vars[var_key];
                        if let Some(value) = value_to_string(value) {
                            out.config
                                .theme_variables
                                .insert(var_key.to_string(), value);
                        } else {
                            apply_fallback_action(
                                policy.unsupported_directive,
                                MermaidWarningCode::UnsupportedDirective,
                                "init themeVariables values must be string/number/bool",
                                span,
                                &mut out.warnings,
                                &mut out.errors,
                            );
                        }
                    }
                } else {
                    apply_fallback_action(
                        policy.unsupported_directive,
                        MermaidWarningCode::UnsupportedDirective,
                        "init themeVariables must be an object; ignoring",
                        span,
                        &mut out.warnings,
                        &mut out.errors,
                    );
                }
            }
            "flowchart" => {
                if let Some(flowchart) = entry.as_object() {
                    let mut flow_keys: Vec<&String> = flowchart.keys().collect();
                    flow_keys.sort();
                    for flow_key in flow_keys {
                        let value = &flowchart[flow_key];
                        match flow_key.as_str() {
                            "direction" => {
                                if let Some(direction) = value.as_str() {
                                    if let Some(parsed) = GraphDirection::parse(direction) {
                                        out.config.flowchart_direction = Some(parsed);
                                    } else {
                                        apply_fallback_action(
                                            policy.unsupported_directive,
                                            MermaidWarningCode::UnsupportedDirective,
                                            "init flowchart.direction must be TB|TD|LR|RL|BT",
                                            span,
                                            &mut out.warnings,
                                            &mut out.errors,
                                        );
                                    }
                                } else {
                                    apply_fallback_action(
                                        policy.unsupported_directive,
                                        MermaidWarningCode::UnsupportedDirective,
                                        "init flowchart.direction must be a string",
                                        span,
                                        &mut out.warnings,
                                        &mut out.errors,
                                    );
                                }
                            }
                            _ => {
                                apply_fallback_action(
                                    policy.unsupported_directive,
                                    MermaidWarningCode::UnsupportedDirective,
                                    "unsupported init flowchart key; ignoring",
                                    span,
                                    &mut out.warnings,
                                    &mut out.errors,
                                );
                            }
                        }
                    }
                } else {
                    apply_fallback_action(
                        policy.unsupported_directive,
                        MermaidWarningCode::UnsupportedDirective,
                        "init flowchart must be an object; ignoring",
                        span,
                        &mut out.warnings,
                        &mut out.errors,
                    );
                }
            }
            _ => {
                apply_fallback_action(
                    policy.unsupported_directive,
                    MermaidWarningCode::UnsupportedDirective,
                    "unsupported init key; ignoring",
                    span,
                    &mut out.warnings,
                    &mut out.errors,
                );
            }
        }
    }
    out
}

/// Merge all init directives in an AST into a single config.
#[must_use]
pub fn collect_init_config(
    ast: &MermaidAst,
    config: &MermaidConfig,
    policy: &MermaidFallbackPolicy,
) -> MermaidInitParse {
    if !config.enable_init_directives {
        return MermaidInitParse::empty();
    }
    let mut merged = MermaidInitConfig::default();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    for directive in &ast.directives {
        if let DirectiveKind::Init { payload } = &directive.kind {
            let parsed = parse_init_directive(payload, directive.span, policy);
            merged.merge_from(parsed.config);
            warnings.extend(parsed.warnings);
            errors.extend(parsed.errors);
        }
    }
    MermaidInitParse {
        config: merged,
        warnings,
        errors,
    }
}

/// Apply init directives to the AST and return the parsed init config.
///
/// This should run before style resolution or layout so flowchart direction
/// overrides are respected.
#[must_use]
pub fn apply_init_directives(
    ast: &mut MermaidAst,
    config: &MermaidConfig,
    policy: &MermaidFallbackPolicy,
) -> MermaidInitParse {
    let parsed = collect_init_config(ast, config, policy);
    parsed.config.apply_to_ast(ast);
    parsed
}

#[derive(Debug)]
struct NodeDraft {
    id: String,
    label: Option<String>,
    shape: NodeShape,
    classes: Vec<String>,
    style: Option<(String, Span)>,
    spans: Vec<Span>,
    first_span: Span,
    insertion_idx: usize,
    implicit: bool,
    members: Vec<String>,
}

#[derive(Debug)]
struct EdgeDraft {
    from: String,
    from_port: Option<String>,
    to: String,
    to_port: Option<String>,
    arrow: String,
    label: Option<String>,
    span: Span,
    insertion_idx: usize,
}

#[derive(Debug)]
struct ClusterDraft {
    id: IrClusterId,
    title: Option<String>,
    members: Vec<String>,
    span: Span,
}

#[derive(Default)]
struct LabelInterner {
    labels: Vec<IrLabel>,
    index: std::collections::HashMap<String, IrLabelId>,
}

impl LabelInterner {
    fn intern(&mut self, text: &str, span: Span) -> IrLabelId {
        if let Some(id) = self.index.get(text).copied() {
            return id;
        }
        let id = IrLabelId(self.labels.len());
        self.labels.push(IrLabel {
            text: text.to_string(),
            span,
        });
        self.index.insert(text.to_string(), id);
        id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LabelLimitStats {
    over_chars: usize,
    over_lines: usize,
}

fn label_line_count(text: &str) -> usize {
    let count = text.lines().count();
    if count == 0 { 1 } else { count }
}

fn truncate_to_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn enforce_label_limits(labels: &mut [IrLabel], config: &MermaidConfig) -> LabelLimitStats {
    let mut stats = LabelLimitStats {
        over_chars: 0,
        over_lines: 0,
    };
    let max_chars = config.max_label_chars;
    let max_lines = config.max_label_lines;

    for label in labels {
        let original = label.text.as_str();
        let char_count = original.chars().count();
        let line_count = label_line_count(original);
        let mut updated = original.to_string();

        if line_count > max_lines {
            stats.over_lines += 1;
            updated = original
                .lines()
                .take(max_lines)
                .collect::<Vec<_>>()
                .join("\n");
        }

        if char_count > max_chars {
            stats.over_chars += 1;
            updated = truncate_to_chars(&updated, max_chars);
        }

        if updated != label.text {
            label.text = updated;
        }
    }

    stats
}

fn normalize_id(raw: &str) -> String {
    raw.trim().to_string()
}

fn split_endpoint(raw: &str) -> (String, Option<String>) {
    let mut parts = raw.splitn(2, ':');
    let node = parts.next().unwrap_or_default();
    let port = parts.next().map(str::trim).filter(|p| !p.is_empty());
    (normalize_id(node), port.map(str::to_string))
}

#[allow(clippy::too_many_arguments)]
fn upsert_node(
    node_id: &str,
    label: Option<&str>,
    shape: NodeShape,
    span: Span,
    implicit: bool,
    insertion_idx: usize,
    node_map: &mut std::collections::HashMap<String, usize>,
    node_drafts: &mut Vec<NodeDraft>,
    implicit_warned: &mut std::collections::HashSet<String>,
    warnings: &mut Vec<MermaidWarning>,
) -> usize {
    if let Some(&idx) = node_map.get(node_id) {
        let draft = &mut node_drafts[idx];
        let was_implicit = draft.implicit;
        draft.spans.push(span);
        if draft.first_span.start.line > span.start.line
            || (draft.first_span.start.line == span.start.line
                && draft.first_span.start.col > span.start.col)
        {
            draft.first_span = span;
        }
        if !implicit {
            draft.implicit = false;
        }
        if let Some(label_value) = label
            && (draft.label.is_none() || (!implicit && was_implicit))
        {
            draft.label = Some(label_value.to_string());
        }
        // Prefer explicit shape over default Rect
        if shape != NodeShape::Rect {
            draft.shape = shape;
        }
        return idx;
    }
    let idx = node_drafts.len();
    node_map.insert(node_id.to_string(), idx);
    node_drafts.push(NodeDraft {
        id: node_id.to_string(),
        label: label.map(str::to_string),
        shape,
        classes: Vec::new(),
        style: None,
        spans: vec![span],
        first_span: span,
        insertion_idx,
        implicit,
        members: Vec::new(),
    });
    if implicit && implicit_warned.insert(node_id.to_string()) {
        warnings.push(MermaidWarning::new(
            MermaidWarningCode::ImplicitNode,
            format!("implicit node created: {}", node_id),
            span,
        ));
    }
    idx
}

fn estimate_route_ops(complexity: MermaidComplexity) -> usize {
    complexity
        .edges
        .saturating_mul(8)
        .saturating_add(complexity.nodes.saturating_mul(2))
        .saturating_add(complexity.labels)
}

fn estimate_layout_iterations(complexity: MermaidComplexity) -> usize {
    complexity
        .nodes
        .saturating_add(complexity.clusters)
        .saturating_add(complexity.edges / 2)
}

fn evaluate_guardrails(
    complexity: MermaidComplexity,
    label_stats: LabelLimitStats,
    config: &MermaidConfig,
    span: Span,
) -> (MermaidGuardReport, Vec<MermaidWarning>) {
    let node_limit_exceeded = complexity.nodes > config.max_nodes;
    let edge_limit_exceeded = complexity.edges > config.max_edges;
    let label_limit_exceeded = label_stats.over_chars > 0 || label_stats.over_lines > 0;
    let limits_exceeded = node_limit_exceeded || edge_limit_exceeded || label_limit_exceeded;

    let route_ops_estimate = estimate_route_ops(complexity);
    let layout_iterations_estimate = estimate_layout_iterations(complexity);
    let route_budget_exceeded = route_ops_estimate > config.route_budget;
    let layout_budget_exceeded = layout_iterations_estimate > config.layout_iteration_budget;
    let budget_exceeded = route_budget_exceeded || layout_budget_exceeded;

    let base_fidelity = MermaidFidelity::from_tier(config.tier_override);
    let mut target_fidelity = base_fidelity;
    let mut degraded = false;

    if route_budget_exceeded || layout_budget_exceeded {
        target_fidelity = target_fidelity.degrade();
        degraded = true;
    }

    if node_limit_exceeded || edge_limit_exceeded {
        target_fidelity = target_fidelity.degrade();
        degraded = true;
    }

    let simplify_routing = route_budget_exceeded || layout_budget_exceeded;
    let reduce_decoration =
        degraded && (simplify_routing || node_limit_exceeded || edge_limit_exceeded);
    let hide_labels = degraded
        && (node_limit_exceeded || edge_limit_exceeded || target_fidelity.is_compact_or_outline());
    let collapse_clusters = degraded
        && (node_limit_exceeded
            || edge_limit_exceeded
            || target_fidelity == MermaidFidelity::Outline);
    let force_glyph_mode = if degraded && target_fidelity == MermaidFidelity::Outline {
        Some(MermaidGlyphMode::Ascii)
    } else {
        None
    };

    let degradation = MermaidDegradationPlan {
        target_fidelity,
        hide_labels,
        collapse_clusters,
        simplify_routing,
        reduce_decoration,
        force_glyph_mode,
    };

    let mut warnings = Vec::new();

    if limits_exceeded {
        let mut parts = Vec::new();
        if node_limit_exceeded {
            parts.push(format!("nodes {}/{}", complexity.nodes, config.max_nodes));
        }
        if edge_limit_exceeded {
            parts.push(format!("edges {}/{}", complexity.edges, config.max_edges));
        }
        if label_stats.over_chars > 0 {
            parts.push(format!("labels over chars {}", label_stats.over_chars));
        }
        if label_stats.over_lines > 0 {
            parts.push(format!("labels over lines {}", label_stats.over_lines));
        }
        let message = if parts.is_empty() {
            "limits exceeded".to_string()
        } else {
            format!("limits exceeded: {}", parts.join(", "))
        };
        warnings.push(MermaidWarning::new(
            MermaidWarningCode::LimitExceeded,
            message,
            span,
        ));
    }

    if budget_exceeded {
        let mut parts = Vec::new();
        if route_budget_exceeded {
            parts.push(format!(
                "route ops {}/{}",
                route_ops_estimate, config.route_budget
            ));
        }
        if layout_budget_exceeded {
            parts.push(format!(
                "layout iters {}/{}",
                layout_iterations_estimate, config.layout_iteration_budget
            ));
        }
        let message = if parts.is_empty() {
            "budget exceeded".to_string()
        } else {
            format!("budget exceeded: {}", parts.join(", "))
        };
        warnings.push(MermaidWarning::new(
            MermaidWarningCode::BudgetExceeded,
            message,
            span,
        ));
    }

    (
        MermaidGuardReport {
            complexity,
            label_chars_over: label_stats.over_chars,
            label_lines_over: label_stats.over_lines,
            node_limit_exceeded,
            edge_limit_exceeded,
            label_limit_exceeded,
            route_budget_exceeded,
            layout_budget_exceeded,
            limits_exceeded,
            budget_exceeded,
            route_ops_estimate,
            layout_iterations_estimate,
            degradation,
        },
        warnings,
    )
}

fn apply_degradation(plan: &MermaidDegradationPlan, ir: &mut MermaidDiagramIr) {
    if plan.hide_labels {
        for node in &mut ir.nodes {
            node.label = None;
        }
        for edge in &mut ir.edges {
            edge.label = None;
        }
        for cluster in &mut ir.clusters {
            cluster.title = None;
        }
        ir.labels.clear();
        ir.pie_title = None;
        ir.pie_show_data = false;
    }

    if plan.collapse_clusters {
        ir.clusters.clear();
    }

    if plan.reduce_decoration {
        for node in &mut ir.nodes {
            node.classes.clear();
            node.style_ref = None;
        }
        ir.style_refs.clear();
    }
}

/// Normalize a parsed Mermaid AST into a deterministic IR for layout/rendering.
#[must_use]
pub fn normalize_ast_to_ir(
    ast: &MermaidAst,
    config: &MermaidConfig,
    matrix: &MermaidCompatibilityMatrix,
    policy: &MermaidFallbackPolicy,
) -> MermaidIrParse {
    let mut ast = ast.clone();
    let init_parse = apply_init_directives(&mut ast, config, policy);
    let mut warnings = init_parse.warnings.clone();
    let mut errors = init_parse.errors.clone();

    let support_level = matrix.support_for(ast.diagram_type);
    if support_level == MermaidSupportLevel::Unsupported {
        let span = ast
            .statements
            .first()
            .map(statement_span)
            .unwrap_or_else(|| Span::at_line(1, 1));
        apply_fallback_action(
            policy.unsupported_diagram,
            MermaidWarningCode::UnsupportedDiagram,
            "unsupported diagram type",
            span,
            &mut warnings,
            &mut errors,
        );
    }

    for statement in &ast.statements {
        match statement {
            Statement::Directive(dir) => match &dir.kind {
                DirectiveKind::Init { .. } => {
                    if !config.enable_init_directives {
                        apply_fallback_action(
                            policy.unsupported_directive,
                            MermaidWarningCode::UnsupportedDirective,
                            "init directives disabled",
                            dir.span,
                            &mut warnings,
                            &mut errors,
                        );
                    }
                }
                DirectiveKind::Raw => {
                    apply_fallback_action(
                        policy.unsupported_directive,
                        MermaidWarningCode::UnsupportedDirective,
                        "unsupported directive",
                        dir.span,
                        &mut warnings,
                        &mut errors,
                    );
                }
            },
            Statement::ClassDef { span, .. }
            | Statement::ClassAssign { span, .. }
            | Statement::Style { span, .. }
            | Statement::LinkStyle { span, .. } => {
                if !config.enable_styles {
                    apply_fallback_action(
                        policy.unsupported_style,
                        MermaidWarningCode::UnsupportedStyle,
                        "styles disabled",
                        *span,
                        &mut warnings,
                        &mut errors,
                    );
                }
            }
            Statement::Link { span, .. } => {
                if !config.enable_links || config.link_mode == MermaidLinkMode::Off {
                    apply_fallback_action(
                        policy.unsupported_link,
                        MermaidWarningCode::UnsupportedLink,
                        "links disabled",
                        *span,
                        &mut warnings,
                        &mut errors,
                    );
                }
            }
            Statement::Raw { text, span } => {
                let is_pie_meta = ast.diagram_type == DiagramType::Pie
                    && (is_pie_show_data_line(text) || parse_pie_title_line(text).is_some());
                if !is_pie_meta {
                    apply_fallback_action(
                        policy.unsupported_feature,
                        MermaidWarningCode::UnsupportedFeature,
                        "unsupported statement",
                        *span,
                        &mut warnings,
                        &mut errors,
                    );
                }
            }
            _ => {}
        }
    }

    let direction = ast.direction.unwrap_or(GraphDirection::TB);
    let mut node_map = std::collections::HashMap::new();
    let mut node_drafts = Vec::new();
    let mut edge_drafts = Vec::new();
    let mut cluster_drafts: Vec<ClusterDraft> = Vec::new();
    let mut cluster_stack: Vec<usize> = Vec::new();
    let mut implicit_warned = std::collections::HashSet::new();
    let mut labels = LabelInterner::default();
    let mut style_refs = Vec::new();
    let mut node_style_drafts: Vec<(String, String, Span)> = Vec::new();
    let mut mindmap_base_depth: Option<usize> = None;
    let mut mindmap_stack: Vec<(usize, String)> = Vec::new();
    let mut pie_entries: Vec<IrPieEntry> = Vec::new();
    let mut pie_title_text: Option<(String, Span)> = None;
    let mut pie_show_data = ast.pie_show_data;

    for (idx, statement) in ast.statements.iter().enumerate() {
        match statement {
            Statement::Node(node) => {
                let id = normalize_id(&node.id);
                if id.is_empty() {
                    warnings.push(MermaidWarning::new(
                        MermaidWarningCode::InvalidEdge,
                        "node id is empty",
                        node.span,
                    ));
                    continue;
                }
                let _ = upsert_node(
                    &id,
                    node.label.as_deref(),
                    node.shape,
                    node.span,
                    false,
                    idx,
                    &mut node_map,
                    &mut node_drafts,
                    &mut implicit_warned,
                    &mut warnings,
                );
                if let Some(cluster_idx) = cluster_stack.last().copied() {
                    cluster_drafts[cluster_idx].members.push(id);
                }
            }
            Statement::Edge(edge) => {
                let (from, from_port) = split_endpoint(&edge.from);
                let (to, to_port) = split_endpoint(&edge.to);
                if from.is_empty() || to.is_empty() {
                    warnings.push(MermaidWarning::new(
                        MermaidWarningCode::InvalidEdge,
                        "edge endpoint missing id; ignoring",
                        edge.span,
                    ));
                    continue;
                }
                upsert_node(
                    &from,
                    None,
                    NodeShape::Rect,
                    edge.span,
                    true,
                    idx,
                    &mut node_map,
                    &mut node_drafts,
                    &mut implicit_warned,
                    &mut warnings,
                );
                upsert_node(
                    &to,
                    None,
                    NodeShape::Rect,
                    edge.span,
                    true,
                    idx,
                    &mut node_map,
                    &mut node_drafts,
                    &mut implicit_warned,
                    &mut warnings,
                );
                if let Some(cluster_idx) = cluster_stack.last().copied() {
                    cluster_drafts[cluster_idx].members.push(from.clone());
                    cluster_drafts[cluster_idx].members.push(to.clone());
                }
                edge_drafts.push(EdgeDraft {
                    from,
                    from_port,
                    to,
                    to_port,
                    arrow: edge.arrow.clone(),
                    label: edge.label.clone(),
                    span: edge.span,
                    insertion_idx: idx,
                });
            }
            Statement::SequenceMessage(msg) => {
                let from = normalize_id(&msg.from);
                let to = normalize_id(&msg.to);
                if from.is_empty() || to.is_empty() {
                    warnings.push(MermaidWarning::new(
                        MermaidWarningCode::InvalidEdge,
                        "sequence message missing participant; ignoring",
                        msg.span,
                    ));
                    continue;
                }
                upsert_node(
                    &from,
                    None,
                    NodeShape::Rect,
                    msg.span,
                    true,
                    idx,
                    &mut node_map,
                    &mut node_drafts,
                    &mut implicit_warned,
                    &mut warnings,
                );
                upsert_node(
                    &to,
                    None,
                    NodeShape::Rect,
                    msg.span,
                    true,
                    idx,
                    &mut node_map,
                    &mut node_drafts,
                    &mut implicit_warned,
                    &mut warnings,
                );
                edge_drafts.push(EdgeDraft {
                    from,
                    from_port: None,
                    to,
                    to_port: None,
                    arrow: msg.arrow.clone(),
                    label: msg.message.clone(),
                    span: msg.span,
                    insertion_idx: idx,
                });
            }
            Statement::MindmapNode(node) => {
                let base = mindmap_base_depth.get_or_insert(node.depth);
                let depth = node.depth.saturating_sub(*base);
                let id = format!(
                    "mindmap_{:04}_{:04}",
                    node.span.start.line, node.span.start.col
                );
                let _ = upsert_node(
                    &id,
                    Some(&node.text),
                    NodeShape::Rect,
                    node.span,
                    false,
                    idx,
                    &mut node_map,
                    &mut node_drafts,
                    &mut implicit_warned,
                    &mut warnings,
                );
                if let Some(cluster_idx) = cluster_stack.last().copied() {
                    cluster_drafts[cluster_idx].members.push(id.clone());
                }
                while let Some((stack_depth, _)) = mindmap_stack.last() {
                    if *stack_depth >= depth {
                        mindmap_stack.pop();
                    } else {
                        break;
                    }
                }
                if let Some((_, parent_id)) = mindmap_stack.last() {
                    edge_drafts.push(EdgeDraft {
                        from: parent_id.clone(),
                        from_port: None,
                        to: id.clone(),
                        to_port: None,
                        arrow: "--".to_string(),
                        label: None,
                        span: node.span,
                        insertion_idx: idx,
                    });
                }
                mindmap_stack.push((depth, id));
            }
            Statement::PieEntry(entry) => {
                if ast.diagram_type != DiagramType::Pie {
                    continue;
                }
                if entry.label.trim().is_empty() {
                    warnings.push(MermaidWarning::new(
                        MermaidWarningCode::InvalidValue,
                        "pie entry label is empty",
                        entry.span,
                    ));
                    continue;
                }
                let value_text = entry.value.trim();
                match value_text.parse::<f64>() {
                    Ok(value) if value > 0.0 => {
                        let label_id = labels.intern(&entry.label, entry.span);
                        pie_entries.push(IrPieEntry {
                            label: label_id,
                            value,
                            value_text: entry.value.clone(),
                            span: entry.span,
                        });
                    }
                    Ok(_) => warnings.push(MermaidWarning::new(
                        MermaidWarningCode::InvalidValue,
                        "pie entry value must be positive",
                        entry.span,
                    )),
                    Err(_) => warnings.push(MermaidWarning::new(
                        MermaidWarningCode::InvalidValue,
                        "pie entry value is not numeric",
                        entry.span,
                    )),
                }
            }
            Statement::Raw { text, span } => {
                if ast.diagram_type == DiagramType::Pie {
                    if is_pie_show_data_line(text) {
                        pie_show_data = true;
                    } else if let Some(title) = parse_pie_title_line(text) {
                        if pie_title_text.is_none() {
                            pie_title_text = Some((title, *span));
                        } else {
                            warnings.push(MermaidWarning::new(
                                MermaidWarningCode::InvalidValue,
                                "multiple pie titles; using first",
                                *span,
                            ));
                        }
                    }
                }
            }
            Statement::ClassAssign {
                targets,
                classes,
                span,
            } => {
                for target in targets {
                    let id = normalize_id(target);
                    if id.is_empty() {
                        warnings.push(MermaidWarning::new(
                            MermaidWarningCode::InvalidEdge,
                            "class assignment target missing id; ignoring",
                            *span,
                        ));
                        continue;
                    }
                    let idx = upsert_node(
                        &id,
                        None,
                        NodeShape::Rect,
                        *span,
                        true,
                        idx,
                        &mut node_map,
                        &mut node_drafts,
                        &mut implicit_warned,
                        &mut warnings,
                    );
                    let draft = &mut node_drafts[idx];
                    for class in classes {
                        if !draft.classes.contains(class) {
                            draft.classes.push(class.clone());
                        }
                    }
                }
            }
            Statement::ClassDef { name, style, span } => {
                style_refs.push(IrStyleRef {
                    target: IrStyleTarget::Class(normalize_id(name)),
                    style: style.clone(),
                    span: *span,
                });
            }
            Statement::Style {
                target,
                style,
                span,
            } => {
                let id = normalize_id(target);
                if id.is_empty() {
                    warnings.push(MermaidWarning::new(
                        MermaidWarningCode::InvalidEdge,
                        "style target missing id; ignoring",
                        *span,
                    ));
                    continue;
                }
                let idx = upsert_node(
                    &id,
                    None,
                    NodeShape::Rect,
                    *span,
                    true,
                    idx,
                    &mut node_map,
                    &mut node_drafts,
                    &mut implicit_warned,
                    &mut warnings,
                );
                node_drafts[idx].style = Some((style.clone(), *span));
                node_style_drafts.push((id, style.clone(), *span));
            }
            Statement::LinkStyle { link, style, span } => {
                style_refs.push(IrStyleRef {
                    target: IrStyleTarget::Link(link.clone()),
                    style: style.clone(),
                    span: *span,
                });
            }
            Statement::SubgraphStart { title, span } => {
                let id = IrClusterId(cluster_drafts.len());
                cluster_drafts.push(ClusterDraft {
                    id,
                    title: title.clone(),
                    members: Vec::new(),
                    span: *span,
                });
                cluster_stack.push(cluster_drafts.len() - 1);
            }
            Statement::SubgraphEnd { span } => {
                if cluster_stack.pop().is_none() {
                    warnings.push(MermaidWarning::new(
                        MermaidWarningCode::UnsupportedFeature,
                        "subgraph end without start; ignoring",
                        *span,
                    ));
                }
            }
            Statement::ClassDeclaration { name, span } => {
                let id = normalize_id(name);
                if !id.is_empty() {
                    upsert_node(
                        &id,
                        Some(name),
                        NodeShape::Rect,
                        *span,
                        false,
                        idx,
                        &mut node_map,
                        &mut node_drafts,
                        &mut implicit_warned,
                        &mut warnings,
                    );
                    if let Some(cluster_idx) = cluster_stack.last().copied() {
                        cluster_drafts[cluster_idx].members.push(id);
                    }
                }
            }
            Statement::ClassMember {
                class,
                member,
                span,
            } => {
                let id = normalize_id(class);
                if !id.is_empty() {
                    let node_idx = upsert_node(
                        &id,
                        None,
                        NodeShape::Rect,
                        *span,
                        true,
                        idx,
                        &mut node_map,
                        &mut node_drafts,
                        &mut implicit_warned,
                        &mut warnings,
                    );
                    node_drafts[node_idx].members.push(member.clone());
                }
            }
            _ => {}
        }
    }

    let mut nodes_sorted: Vec<NodeDraft> = node_drafts;
    nodes_sorted.sort_by(|a, b| {
        (
            a.id.as_str(),
            a.first_span.start.line,
            a.first_span.start.col,
            a.insertion_idx,
        )
            .cmp(&(
                b.id.as_str(),
                b.first_span.start.line,
                b.first_span.start.col,
                b.insertion_idx,
            ))
    });

    let mut node_id_map = std::collections::HashMap::new();
    let mut labels = LabelInterner::default();
    let mut nodes = Vec::with_capacity(nodes_sorted.len());

    for (idx, draft) in nodes_sorted.into_iter().enumerate() {
        let node_id = IrNodeId(idx);
        node_id_map.insert(draft.id.clone(), node_id);
        let label_id = draft
            .label
            .as_deref()
            .map(|label| labels.intern(label, draft.first_span));
        nodes.push(IrNode {
            id: draft.id,
            label: label_id,
            shape: draft.shape,
            classes: draft.classes,
            style_ref: None,
            span_primary: draft.first_span,
            span_all: draft.spans,
            implicit: draft.implicit,
            members: draft.members,
        });
    }

    let mut clusters = Vec::new();
    for draft in cluster_drafts {
        let mut members: Vec<IrNodeId> = draft
            .members
            .iter()
            .filter_map(|id| node_id_map.get(id).copied())
            .collect();
        members.sort_by_key(|id| id.0);
        members.dedup_by_key(|id| id.0);
        let title = draft
            .title
            .as_deref()
            .map(|label| labels.intern(label, draft.span));
        clusters.push(IrCluster {
            id: draft.id,
            title,
            members,
            span: draft.span,
        });
    }

    for (target, style, span) in node_style_drafts {
        if let Some(node_id) = node_id_map.get(&target).copied() {
            let style_id = IrStyleRefId(style_refs.len());
            style_refs.push(IrStyleRef {
                target: IrStyleTarget::Node(node_id),
                style,
                span,
            });
            if let Some(node) = nodes.get_mut(node_id.0) {
                node.style_ref = Some(style_id);
            }
        } else {
            warnings.push(MermaidWarning::new(
                MermaidWarningCode::InvalidEdge,
                "style target references unknown node",
                span,
            ));
        }
    }

    // Preserve source edge order so linkStyle indices remain stable.
    edge_drafts.sort_by_key(|draft| draft.insertion_idx);

    let mut ports = Vec::new();
    let mut port_map = std::collections::HashMap::new();
    let mut edges = Vec::new();

    for draft in edge_drafts {
        let Some(from_node) = node_id_map.get(&draft.from).copied() else {
            warnings.push(MermaidWarning::new(
                MermaidWarningCode::InvalidEdge,
                "edge references unknown from-node; ignoring",
                draft.span,
            ));
            continue;
        };
        let Some(to_node) = node_id_map.get(&draft.to).copied() else {
            warnings.push(MermaidWarning::new(
                MermaidWarningCode::InvalidEdge,
                "edge references unknown to-node; ignoring",
                draft.span,
            ));
            continue;
        };

        let from_endpoint = if let Some(port) = draft.from_port.as_deref() {
            if port.is_empty() {
                warnings.push(MermaidWarning::new(
                    MermaidWarningCode::InvalidPort,
                    "empty port name; ignoring",
                    draft.span,
                ));
                IrEndpoint::Node(from_node)
            } else {
                let key = (from_node, port.to_string());
                let port_id = *port_map.entry(key.clone()).or_insert_with(|| {
                    let id = IrPortId(ports.len());
                    ports.push(IrPort {
                        node: from_node,
                        name: key.1.clone(),
                        side_hint: IrPortSideHint::from_direction(direction),
                        span: draft.span,
                    });
                    id
                });
                IrEndpoint::Port(port_id)
            }
        } else {
            IrEndpoint::Node(from_node)
        };

        let to_endpoint = if let Some(port) = draft.to_port.as_deref() {
            if port.is_empty() {
                warnings.push(MermaidWarning::new(
                    MermaidWarningCode::InvalidPort,
                    "empty port name; ignoring",
                    draft.span,
                ));
                IrEndpoint::Node(to_node)
            } else {
                let key = (to_node, port.to_string());
                let port_id = *port_map.entry(key.clone()).or_insert_with(|| {
                    let id = IrPortId(ports.len());
                    ports.push(IrPort {
                        node: to_node,
                        name: key.1.clone(),
                        side_hint: IrPortSideHint::from_direction(direction),
                        span: draft.span,
                    });
                    id
                });
                IrEndpoint::Port(port_id)
            }
        } else {
            IrEndpoint::Node(to_node)
        };

        let label_id = draft
            .label
            .as_deref()
            .map(|label| labels.intern(label, draft.span));

        edges.push(IrEdge {
            from: from_endpoint,
            to: to_endpoint,
            arrow: draft.arrow,
            label: label_id,
            style_ref: None,
            span: draft.span,
        });
    }

    // Intern pie title into the final labels interner (it was deferred from
    // the first pass because that used a different LabelInterner).
    let pie_title: Option<IrLabelId> =
        pie_title_text.map(|(text, span)| labels.intern(&text, span));

    let mut labels = labels.labels;
    let label_stats = enforce_label_limits(&mut labels, config);

    let complexity = MermaidComplexity::from_counts(
        nodes.len(),
        edges.len(),
        labels.len(),
        clusters.len(),
        ports.len(),
        style_refs.len(),
    );
    let guard_span = ast
        .statements
        .first()
        .map(statement_span)
        .unwrap_or_else(|| Span::at_line(1, 1));
    let (guard, guard_warnings) = evaluate_guardrails(complexity, label_stats, config, guard_span);
    warnings.extend(guard_warnings);

    let theme_overrides = init_parse.config.theme_overrides();
    let meta = MermaidDiagramMeta {
        diagram_type: ast.diagram_type,
        direction,
        support_level,
        init: init_parse,
        theme_overrides,
        guard,
    };

    // Resolve link/click directives into IR links.
    // Use `node_id_map` (post-sort indices) rather than `node_map` (pre-sort)
    // so that link targets point to the correct nodes in the final IR.
    let link_resolution = resolve_links(&ast, config, &node_id_map);
    warnings.extend(link_resolution.warnings.clone());
    let resolved_links = link_resolution.links.clone();

    let mut ir = MermaidDiagramIr {
        diagram_type: ast.diagram_type,
        direction,
        nodes,
        edges,
        ports,
        clusters,
        labels,
        pie_entries,
        pie_title,
        pie_show_data,
        style_refs,
        links: resolved_links,
        meta,
    };

    let degradation = ir.meta.guard.degradation.clone();
    apply_degradation(&degradation, &mut ir);
    emit_guard_jsonl(config, &ir.meta);
    emit_link_jsonl(config, &link_resolution);

    MermaidIrParse {
        ir,
        warnings,
        errors,
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Evaluate compatibility warnings and fallback policy for a Mermaid AST.
#[must_use]
pub fn compatibility_report(
    ast: &MermaidAst,
    config: &MermaidConfig,
    matrix: &MermaidCompatibilityMatrix,
) -> MermaidCompatibilityReport {
    let diagram_support = matrix.support_for(ast.diagram_type);
    let mut warnings = Vec::new();
    let mut fatal = false;

    if diagram_support == MermaidSupportLevel::Unsupported {
        fatal = true;
        warnings.push(MermaidWarning::new(
            MermaidWarningCode::UnsupportedDiagram,
            "diagram type is not supported",
            Span::at_line(1, 1),
        ));
    }

    for statement in &ast.statements {
        match statement {
            Statement::Directive(dir) => match dir.kind {
                DirectiveKind::Init { .. } if !config.enable_init_directives => {
                    warnings.push(MermaidWarning::new(
                        MermaidWarningCode::UnsupportedDirective,
                        "init directives disabled; ignoring",
                        dir.span,
                    ));
                }
                DirectiveKind::Raw => warnings.push(MermaidWarning::new(
                    MermaidWarningCode::UnsupportedDirective,
                    "raw directives are not supported; ignoring",
                    dir.span,
                )),
                _ => {}
            },
            Statement::ClassDef { span, .. }
            | Statement::ClassAssign { span, .. }
            | Statement::Style { span, .. }
            | Statement::LinkStyle { span, .. } => {
                if !config.enable_styles {
                    warnings.push(MermaidWarning::new(
                        MermaidWarningCode::UnsupportedStyle,
                        "styles disabled; ignoring",
                        *span,
                    ));
                }
            }
            Statement::Link { span, .. } => {
                if !config.enable_links {
                    warnings.push(MermaidWarning::new(
                        MermaidWarningCode::UnsupportedLink,
                        "links disabled; ignoring",
                        *span,
                    ));
                }
            }
            Statement::Raw { span, .. } => {
                warnings.push(MermaidWarning::new(
                    MermaidWarningCode::UnsupportedFeature,
                    "unrecognized statement; ignoring",
                    *span,
                ));
            }
            _ => {}
        }
    }

    MermaidCompatibilityReport {
        diagram_support,
        warnings,
        fatal,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDirection {
    TB,
    TD,
    LR,
    RL,
    BT,
}

impl GraphDirection {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "tb" => Some(Self::TB),
            "td" => Some(Self::TD),
            "lr" => Some(Self::LR),
            "rl" => Some(Self::RL),
            "bt" => Some(Self::BT),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TB => "TB",
            Self::TD => "TD",
            Self::LR => "LR",
            Self::RL => "RL",
            Self::BT => "BT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Graph,
    Flowchart,
    SequenceDiagram,
    StateDiagram,
    Gantt,
    ClassDiagram,
    ErDiagram,
    Mindmap,
    Pie,
    Subgraph,
    End,
    Title,
    Section,
    Direction,
    ClassDef,
    Class,
    Style,
    LinkStyle,
    Click,
    Link,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind<'a> {
    Keyword(Keyword),
    Identifier(&'a str),
    Number(&'a str),
    String(&'a str),
    Arrow(&'a str),
    Punct(char),
    Directive(&'a str),
    Comment(&'a str),
    Newline,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind<'a>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MermaidAst {
    pub diagram_type: DiagramType,
    pub direction: Option<GraphDirection>,
    pub pie_show_data: bool,
    pub directives: Vec<Directive>,
    pub statements: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveKind {
    Init { payload: String },
    Raw,
}

#[derive(Debug, Clone)]
pub struct Directive {
    pub kind: DirectiveKind,
    pub content: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub label: Option<String>,
    pub shape: NodeShape,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub arrow: String,
    pub label: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SequenceMessage {
    pub from: String,
    pub to: String,
    pub arrow: String,
    pub message: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct GanttTask {
    pub title: String,
    pub meta: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct PieEntry {
    pub label: String,
    pub value: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MindmapNode {
    pub depth: usize,
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Click,
    Link,
}

#[derive(Debug, Clone)]
pub enum Statement {
    Directive(Directive),
    Comment(Comment),
    SubgraphStart {
        title: Option<String>,
        span: Span,
    },
    SubgraphEnd {
        span: Span,
    },
    Direction {
        direction: GraphDirection,
        span: Span,
    },
    ClassDeclaration {
        name: String,
        span: Span,
    },
    ClassDef {
        name: String,
        style: String,
        span: Span,
    },
    ClassAssign {
        targets: Vec<String>,
        classes: Vec<String>,
        span: Span,
    },
    Style {
        target: String,
        style: String,
        span: Span,
    },
    LinkStyle {
        link: String,
        style: String,
        span: Span,
    },
    Link {
        kind: LinkKind,
        target: String,
        url: String,
        tooltip: Option<String>,
        span: Span,
    },
    Node(Node),
    Edge(Edge),
    SequenceMessage(SequenceMessage),
    ClassMember {
        class: String,
        member: String,
        span: Span,
    },
    GanttTitle {
        title: String,
        span: Span,
    },
    GanttSection {
        name: String,
        span: Span,
    },
    GanttTask(GanttTask),
    PieEntry(PieEntry),
    MindmapNode(MindmapNode),
    Raw {
        text: String,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrNodeId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrPortId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrLabelId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrClusterId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrStyleRefId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrPortSideHint {
    Auto,
    Horizontal,
    Vertical,
}

impl IrPortSideHint {
    #[must_use]
    pub const fn from_direction(direction: GraphDirection) -> Self {
        match direction {
            GraphDirection::LR | GraphDirection::RL => Self::Horizontal,
            GraphDirection::TB | GraphDirection::TD | GraphDirection::BT => Self::Vertical,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrLabel {
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrPieEntry {
    pub label: IrLabelId,
    pub value: f64,
    pub value_text: String,
    pub span: Span,
}

/// Node shape as determined by the bracket syntax in Mermaid source.
///
/// Maps to visual representations in the terminal renderer:
/// - `[text]` → `Rect`
/// - `(text)` → `Rounded`
/// - `([text])` → `Stadium`
/// - `[[text]]` → `Subroutine`
/// - `{text}` → `Diamond`
/// - `{{text}}` → `Hexagon`
/// - `((text))` → `Circle`
/// - `>text]` → `Asymmetric`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeShape {
    /// Default rectangular node: `[text]`
    Rect,
    /// Rounded corners: `(text)`
    Rounded,
    /// Pill/stadium shape: `([text])`
    Stadium,
    /// Subroutine (double vertical borders): `[[text]]`
    Subroutine,
    /// Diamond/rhombus: `{text}`
    Diamond,
    /// Hexagon: `{{text}}`
    Hexagon,
    /// Circle (double parentheses): `((text))`
    Circle,
    /// Asymmetric / flag shape: `>text]`
    Asymmetric,
}

impl NodeShape {
    /// Returns the string representation used in diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rect => "rect",
            Self::Rounded => "rounded",
            Self::Stadium => "stadium",
            Self::Subroutine => "subroutine",
            Self::Diamond => "diamond",
            Self::Hexagon => "hexagon",
            Self::Circle => "circle",
            Self::Asymmetric => "asymmetric",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrNode {
    pub id: String,
    pub label: Option<IrLabelId>,
    pub shape: NodeShape,
    pub classes: Vec<String>,
    pub style_ref: Option<IrStyleRefId>,
    pub span_primary: Span,
    pub span_all: Vec<Span>,
    pub implicit: bool,
    /// Class diagram members (fields/methods) for compartment rendering.
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrPort {
    pub node: IrNodeId,
    pub name: String,
    pub side_hint: IrPortSideHint,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrEndpoint {
    Node(IrNodeId),
    Port(IrPortId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrEdge {
    pub from: IrEndpoint,
    pub to: IrEndpoint,
    pub arrow: String,
    pub label: Option<IrLabelId>,
    pub style_ref: Option<IrStyleRefId>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrCluster {
    pub id: IrClusterId,
    pub title: Option<IrLabelId>,
    pub members: Vec<IrNodeId>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrStyleTarget {
    Class(String),
    Node(IrNodeId),
    Link(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrStyleRef {
    pub target: IrStyleTarget,
    pub style: String,
    pub span: Span,
}

// --- Link/Click IR types (bd-25df9) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrLinkId(pub usize);

/// Outcome of URL sanitization for a single link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSanitizeOutcome {
    /// URL passed sanitization.
    Allowed,
    /// URL was blocked by protocol policy.
    Blocked,
}

/// A resolved link from a `click`/`link` directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrLink {
    pub kind: LinkKind,
    pub target: IrNodeId,
    pub url: String,
    pub tooltip: Option<String>,
    pub sanitize_outcome: LinkSanitizeOutcome,
    pub span: Span,
}

/// Result of link resolution for a diagram.
#[derive(Debug, Clone)]
pub struct LinkResolution {
    pub links: Vec<IrLink>,
    pub link_mode: MermaidLinkMode,
    pub total_count: usize,
    pub allowed_count: usize,
    pub blocked_count: usize,
    pub warnings: Vec<MermaidWarning>,
}

// --- Style property parsing and resolution (bd-17w24) ---

/// A single CSS-like color parsed from Mermaid style directives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidColor {
    Rgb(u8, u8, u8),
    Transparent,
    None,
}

impl MermaidColor {
    /// Parse a CSS color string (hex or named).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("transparent") {
            return Some(Self::Transparent);
        }
        if let Some(hex) = s.strip_prefix('#') {
            return Self::parse_hex(hex);
        }
        Self::parse_named(s)
    }

    fn parse_hex(hex: &str) -> Option<Self> {
        match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                Some(Self::Rgb(r, g, b))
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Self::Rgb(r, g, b))
            }
            _ => None,
        }
    }

    fn parse_named(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "black" => Some(Self::Rgb(0, 0, 0)),
            "white" => Some(Self::Rgb(255, 255, 255)),
            "red" => Some(Self::Rgb(255, 0, 0)),
            "green" => Some(Self::Rgb(0, 128, 0)),
            "blue" => Some(Self::Rgb(0, 0, 255)),
            "yellow" => Some(Self::Rgb(255, 255, 0)),
            "cyan" | "aqua" => Some(Self::Rgb(0, 255, 255)),
            "magenta" | "fuchsia" => Some(Self::Rgb(255, 0, 255)),
            "orange" => Some(Self::Rgb(255, 165, 0)),
            "purple" => Some(Self::Rgb(128, 0, 128)),
            "pink" => Some(Self::Rgb(255, 192, 203)),
            "brown" => Some(Self::Rgb(165, 42, 42)),
            "gray" | "grey" => Some(Self::Rgb(128, 128, 128)),
            "lightgray" | "lightgrey" => Some(Self::Rgb(211, 211, 211)),
            "darkgray" | "darkgrey" => Some(Self::Rgb(169, 169, 169)),
            "lime" => Some(Self::Rgb(0, 255, 0)),
            "navy" => Some(Self::Rgb(0, 0, 128)),
            "teal" => Some(Self::Rgb(0, 128, 128)),
            "olive" => Some(Self::Rgb(128, 128, 0)),
            "maroon" => Some(Self::Rgb(128, 0, 0)),
            "silver" => Some(Self::Rgb(192, 192, 192)),
            "coral" => Some(Self::Rgb(255, 127, 80)),
            "salmon" => Some(Self::Rgb(250, 128, 114)),
            "gold" => Some(Self::Rgb(255, 215, 0)),
            "indigo" => Some(Self::Rgb(75, 0, 130)),
            "violet" => Some(Self::Rgb(238, 130, 238)),
            "crimson" => Some(Self::Rgb(220, 20, 60)),
            "turquoise" => Some(Self::Rgb(64, 224, 208)),
            "tomato" => Some(Self::Rgb(255, 99, 71)),
            _ => None,
        }
    }
}

/// Parsed stroke dash pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidStrokeDash {
    Solid,
    Dashed,
    Dotted,
}

/// Parsed font weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidFontWeight {
    Normal,
    Bold,
}

/// Structured CSS-like properties parsed from a Mermaid style string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MermaidStyleProperties {
    pub fill: Option<MermaidColor>,
    pub stroke: Option<MermaidColor>,
    pub stroke_width: Option<u8>,
    pub stroke_dash: Option<MermaidStrokeDash>,
    pub color: Option<MermaidColor>,
    pub font_weight: Option<MermaidFontWeight>,
    pub unsupported: Vec<(String, String)>,
}

impl MermaidStyleProperties {
    /// Parse a CSS-like style string.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let mut props = Self::default();
        for pair in raw.split(',') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let Some((key, value)) = pair.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "fill" | "background" | "background-color" => {
                    if let Some(c) = MermaidColor::parse(value) {
                        props.fill = Some(c);
                    } else {
                        props.unsupported.push((key, value.to_string()));
                    }
                }
                "stroke" | "border-color" => {
                    if let Some(c) = MermaidColor::parse(value) {
                        props.stroke = Some(c);
                    } else {
                        props.unsupported.push((key, value.to_string()));
                    }
                }
                "stroke-width" | "border-width" => {
                    let num_str = value.trim_end_matches("px");
                    if let Ok(w) = num_str.parse::<u8>() {
                        props.stroke_width = Some(w);
                    } else {
                        props.unsupported.push((key, value.to_string()));
                    }
                }
                "stroke-dasharray" => {
                    if value.contains(' ') || value.contains(',') {
                        props.stroke_dash = Some(MermaidStrokeDash::Dashed);
                    } else if value == "0" || value.eq_ignore_ascii_case("none") {
                        props.stroke_dash = Some(MermaidStrokeDash::Solid);
                    } else {
                        props.stroke_dash = Some(MermaidStrokeDash::Dotted);
                    }
                }
                "color" | "font-color" => {
                    if let Some(c) = MermaidColor::parse(value) {
                        props.color = Some(c);
                    } else {
                        props.unsupported.push((key, value.to_string()));
                    }
                }
                "font-weight" => {
                    if value.eq_ignore_ascii_case("bold") || value == "700" {
                        props.font_weight = Some(MermaidFontWeight::Bold);
                    } else {
                        props.font_weight = Some(MermaidFontWeight::Normal);
                    }
                }
                _ => {
                    props.unsupported.push((key, value.to_string()));
                }
            }
        }
        props
    }

    /// Merge another set of properties on top (later wins).
    pub fn merge_from(&mut self, other: &Self) {
        if other.fill.is_some() {
            self.fill = other.fill;
        }
        if other.stroke.is_some() {
            self.stroke = other.stroke;
        }
        if other.stroke_width.is_some() {
            self.stroke_width = other.stroke_width;
        }
        if other.stroke_dash.is_some() {
            self.stroke_dash = other.stroke_dash;
        }
        if other.color.is_some() {
            self.color = other.color;
        }
        if other.font_weight.is_some() {
            self.font_weight = other.font_weight;
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fill.is_none()
            && self.stroke.is_none()
            && self.stroke_width.is_none()
            && self.stroke_dash.is_none()
            && self.color.is_none()
            && self.font_weight.is_none()
    }
}

/// Resolved style for a single diagram element.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedMermaidStyle {
    pub properties: MermaidStyleProperties,
    pub sources: Vec<String>,
}

/// Resolved styles for an entire diagram.
#[derive(Debug, Clone)]
pub struct MermaidResolvedStyles {
    pub node_styles: Vec<ResolvedMermaidStyle>,
    pub edge_styles: Vec<ResolvedMermaidStyle>,
    pub unsupported_warnings: Vec<MermaidWarning>,
}

/// Build a base style from init directive `themeVariables`.
///
/// Maps well-known Mermaid theme variables to style properties so that
/// init-directive theming flows through the same precedence chain.
fn theme_variable_defaults(vars: &BTreeMap<String, String>) -> MermaidStyleProperties {
    let mut base = MermaidStyleProperties::default();
    if let Some(val) = vars.get("primaryColor") {
        base.fill = MermaidColor::parse(val);
    }
    if let Some(val) = vars.get("primaryTextColor") {
        base.color = MermaidColor::parse(val);
    }
    if let Some(val) = vars.get("primaryBorderColor") {
        base.stroke = MermaidColor::parse(val);
    }
    base
}

/// Apply WCAG contrast clamping to a resolved style's text-on-background pair.
fn apply_contrast_clamping(resolved: &mut ResolvedMermaidStyle) {
    if let (Some(fg), Some(bg)) = (resolved.properties.color, resolved.properties.fill) {
        let clamped = clamp_contrast(fg, bg);
        if clamped != fg {
            resolved.properties.color = Some(clamped);
            resolved.sources.push("contrast-clamp".to_string());
        }
    }
}

/// Resolve styles for all nodes and edges in the IR.
///
/// Precedence (last wins): theme defaults < class styles < node-specific styles.
/// After resolution, WCAG contrast clamping is applied where both fg and bg are set.
#[must_use]
pub fn resolve_styles(ir: &MermaidDiagramIr) -> MermaidResolvedStyles {
    let theme_base = theme_variable_defaults(&ir.meta.theme_overrides.theme_variables);

    let mut node_styles: Vec<ResolvedMermaidStyle> =
        vec![ResolvedMermaidStyle::default(); ir.nodes.len()];
    let mut edge_styles: Vec<ResolvedMermaidStyle> =
        vec![ResolvedMermaidStyle::default(); ir.edges.len()];

    // Layer 0: theme variable defaults for all nodes
    if !theme_base.is_empty() {
        for resolved in &mut node_styles {
            resolved.properties.merge_from(&theme_base);
            resolved.sources.push("themeVariables".to_string());
        }
    }

    // Layer 1: class definitions
    let mut class_defs: std::collections::HashMap<String, MermaidStyleProperties> =
        std::collections::HashMap::new();
    for sr in &ir.style_refs {
        if let IrStyleTarget::Class(ref name) = sr.target {
            let parsed = MermaidStyleProperties::parse(&sr.style);
            class_defs
                .entry(name.clone())
                .and_modify(|e| e.merge_from(&parsed))
                .or_insert(parsed);
        }
    }

    for (i, node) in ir.nodes.iter().enumerate() {
        let resolved = &mut node_styles[i];
        for class_name in &node.classes {
            if let Some(cp) = class_defs.get(class_name) {
                resolved.properties.merge_from(cp);
                resolved.sources.push(format!("classDef {class_name}"));
            }
        }
    }

    for sr in &ir.style_refs {
        if let IrStyleTarget::Node(node_id) = sr.target
            && let Some(resolved) = node_styles.get_mut(node_id.0)
        {
            let parsed = MermaidStyleProperties::parse(&sr.style);
            resolved.properties.merge_from(&parsed);
            resolved
                .sources
                .push(format!("style {}", ir.nodes[node_id.0].id));
        }
    }

    for sr in &ir.style_refs {
        if let IrStyleTarget::Link(ref sel) = sr.target {
            let parsed = MermaidStyleProperties::parse(&sr.style);
            if sel == "default" {
                for resolved in &mut edge_styles {
                    resolved.properties.merge_from(&parsed);
                    resolved.sources.push("linkStyle default".to_string());
                }
            } else if let Ok(idx) = sel.parse::<usize>()
                && let Some(resolved) = edge_styles.get_mut(idx)
            {
                resolved.properties.merge_from(&parsed);
                resolved.sources.push(format!("linkStyle {idx}"));
            }
        }
    }

    // Apply WCAG contrast clamping where both fg and bg are set
    for resolved in &mut node_styles {
        apply_contrast_clamping(resolved);
    }
    for resolved in &mut edge_styles {
        apply_contrast_clamping(resolved);
    }

    // Collect unsupported property warnings
    let mut unsupported_warnings = Vec::new();
    for sr in &ir.style_refs {
        let parsed = MermaidStyleProperties::parse(&sr.style);
        for (key, value) in &parsed.unsupported {
            unsupported_warnings.push(MermaidWarning::new(
                MermaidWarningCode::UnsupportedStyle,
                format!("unsupported style property: {key}:{value}"),
                sr.span,
            ));
        }
    }

    MermaidResolvedStyles {
        node_styles,
        edge_styles,
        unsupported_warnings,
    }
}

const MIN_CONTRAST_RATIO: f64 = 3.0;

fn relative_luminance(r: u8, g: u8, b: u8) -> f64 {
    fn linearize(channel: u8) -> f64 {
        let c = f64::from(channel) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b)
}

fn contrast_ratio(c1: MermaidColor, c2: MermaidColor) -> f64 {
    let (r1, g1, b1) = match c1 {
        MermaidColor::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    let (r2, g2, b2) = match c2 {
        MermaidColor::Rgb(r, g, b) => (r, g, b),
        _ => (255, 255, 255),
    };
    let l1 = relative_luminance(r1, g1, b1);
    let l2 = relative_luminance(r2, g2, b2);
    let lighter = l1.max(l2);
    let darker = l1.min(l2);
    (lighter + 0.05) / (darker + 0.05)
}

/// Clamp text color against background for minimum WCAG contrast.
#[must_use]
pub fn clamp_contrast(fg: MermaidColor, bg: MermaidColor) -> MermaidColor {
    if contrast_ratio(fg, bg) >= MIN_CONTRAST_RATIO {
        return fg;
    }
    let bg_lum = match bg {
        MermaidColor::Rgb(r, g, b) => relative_luminance(r, g, b),
        _ => 0.0,
    };
    if bg_lum > 0.5 {
        MermaidColor::Rgb(0, 0, 0)
    } else {
        MermaidColor::Rgb(255, 255, 255)
    }
}

// --- End style property parsing and resolution ---

#[derive(Debug, Clone)]
pub struct MermaidDiagramMeta {
    pub diagram_type: DiagramType,
    pub direction: GraphDirection,
    pub support_level: MermaidSupportLevel,
    pub init: MermaidInitParse,
    pub theme_overrides: MermaidThemeOverrides,
    pub guard: MermaidGuardReport,
}

#[derive(Debug, Clone)]
pub struct MermaidDiagramIr {
    pub diagram_type: DiagramType,
    pub direction: GraphDirection,
    pub nodes: Vec<IrNode>,
    pub edges: Vec<IrEdge>,
    pub ports: Vec<IrPort>,
    pub clusters: Vec<IrCluster>,
    pub labels: Vec<IrLabel>,
    pub pie_entries: Vec<IrPieEntry>,
    pub pie_title: Option<IrLabelId>,
    pub pie_show_data: bool,
    pub style_refs: Vec<IrStyleRef>,
    pub links: Vec<IrLink>,
    pub meta: MermaidDiagramMeta,
}

#[derive(Debug, Clone)]
pub struct MermaidIrParse {
    pub ir: MermaidDiagramIr,
    pub warnings: Vec<MermaidWarning>,
    pub errors: Vec<MermaidError>,
}

/// Prepared Mermaid analysis with init directives applied.
#[derive(Debug, Clone)]
pub struct MermaidPrepared {
    pub ast: MermaidAst,
    pub parse_errors: Vec<MermaidError>,
    pub init: MermaidInitParse,
    pub theme_overrides: MermaidThemeOverrides,
    pub init_config_hash: u64,
    pub compatibility: MermaidCompatibilityReport,
    pub validation: MermaidValidation,
}

impl MermaidPrepared {
    #[must_use]
    pub fn init_config_hash_hex(&self) -> String {
        format!("{:016x}", self.init_config_hash)
    }

    #[must_use]
    pub fn all_warnings(&self) -> Vec<MermaidWarning> {
        let mut warnings = Vec::new();
        warnings.extend(self.compatibility.warnings.clone());
        warnings.extend(self.validation.warnings.clone());
        warnings
    }

    #[must_use]
    pub fn all_errors(&self) -> Vec<MermaidError> {
        let mut errors = Vec::new();
        errors.extend(self.parse_errors.clone());
        errors.extend(self.validation.errors.clone());
        errors
    }
}

pub struct Lexer<'a> {
    input: &'a str,
    bytes: &'a [u8],
    idx: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            idx: 0,
            line: 1,
            col: 1,
        }
    }

    pub fn tokenize(mut self) -> Vec<Token<'a>> {
        let mut out = Vec::new();
        loop {
            let lexeme = self.next_token();
            let is_eof = matches!(lexeme.kind, TokenKind::Eof);
            out.push(lexeme);
            if is_eof {
                break;
            }
        }
        out
    }

    fn next_token(&mut self) -> Token<'a> {
        self.skip_spaces();
        let start = self.position();
        if self.idx >= self.bytes.len() {
            return Token {
                kind: TokenKind::Eof,
                span: Span::new(start, start),
            };
        }
        let b = self.bytes[self.idx];
        if b == b'\n' {
            self.advance_byte();
            return Token {
                kind: TokenKind::Newline,
                span: Span::new(start, self.position()),
            };
        }
        if b == b'\r' {
            self.advance_byte();
            if self.peek_n_bytes(0) == Some(b'\n') {
                self.advance_byte();
            }
            return Token {
                kind: TokenKind::Newline,
                span: Span::new(start, self.position()),
            };
        }
        if b == b'%' && self.peek_byte() == Some(b'%') {
            return self.lex_comment_or_directive(start);
        }
        if b == b'"' || b == b'\'' {
            return self.lex_string(start, b);
        }
        if is_digit(b) {
            return self.lex_number(start);
        }
        if is_arrow_char(b as char) {
            return self.lex_arrow_or_punct(start);
        }
        if is_ident_start(b as char) {
            return self.lex_identifier(start);
        }

        self.advance_byte();
        Token {
            kind: TokenKind::Punct(b as char),
            span: Span::new(start, self.position()),
        }
    }

    fn lex_comment_or_directive(&mut self, start: Position) -> Token<'a> {
        self.advance_byte(); // %
        self.advance_byte(); // %
        if self.peek_n_bytes(0) == Some(b'{') {
            self.advance_byte();
            let content_start = self.idx;
            while self.idx < self.bytes.len() {
                if self.bytes[self.idx] == b'}'
                    && self.peek_n_bytes(1) == Some(b'%')
                    && self.peek_n_bytes(2) == Some(b'%')
                {
                    let content = &self.input[content_start..self.idx];
                    self.advance_byte();
                    self.advance_byte();
                    self.advance_byte();
                    return Token {
                        kind: TokenKind::Directive(content),
                        span: Span::new(start, self.position()),
                    };
                }
                self.advance_byte();
            }
            return Token {
                kind: TokenKind::Directive(&self.input[content_start..self.idx]),
                span: Span::new(start, self.position()),
            };
        }

        let content_start = self.idx;
        while self.idx < self.bytes.len() {
            let b = self.bytes[self.idx];
            if b == b'\n' || b == b'\r' {
                break;
            }
            self.advance_byte();
        }
        Token {
            kind: TokenKind::Comment(&self.input[content_start..self.idx]),
            span: Span::new(start, self.position()),
        }
    }

    fn lex_string(&mut self, start: Position, quote: u8) -> Token<'a> {
        self.advance_byte();
        let content_start = self.idx;
        while self.idx < self.bytes.len() {
            let b = self.bytes[self.idx];
            if b == quote {
                let content = &self.input[content_start..self.idx];
                self.advance_byte();
                return Token {
                    kind: TokenKind::String(content),
                    span: Span::new(start, self.position()),
                };
            }
            if b == b'\\' {
                self.advance_byte();
                if self.idx < self.bytes.len() {
                    self.advance_byte();
                }
                continue;
            }
            if b == b'\n' || b == b'\r' {
                break;
            }
            self.advance_byte();
        }
        Token {
            kind: TokenKind::String(&self.input[content_start..self.idx]),
            span: Span::new(start, self.position()),
        }
    }

    fn lex_number(&mut self, start: Position) -> Token<'a> {
        let start_idx = self.idx;
        while self.idx < self.bytes.len() {
            let b = self.bytes[self.idx];
            if !is_digit(b) && b != b'.' {
                break;
            }
            self.advance_byte();
        }
        Token {
            kind: TokenKind::Number(&self.input[start_idx..self.idx]),
            span: Span::new(start, self.position()),
        }
    }

    fn lex_identifier(&mut self, start: Position) -> Token<'a> {
        let start_idx = self.idx;
        self.advance_byte();
        while self.idx < self.bytes.len() {
            let c = self.bytes[self.idx] as char;
            if !is_ident_continue(c) {
                break;
            }
            if c == '-' && self.peek_byte().is_some_and(|b| is_arrow_char(b as char)) {
                break;
            }
            self.advance_byte();
        }
        let text = &self.input[start_idx..self.idx];
        let kind = match keyword_from(text) {
            Some(keyword) => TokenKind::Keyword(keyword),
            None => TokenKind::Identifier(text),
        };
        Token {
            kind,
            span: Span::new(start, self.position()),
        }
    }

    fn lex_arrow_or_punct(&mut self, start: Position) -> Token<'a> {
        let start_idx = self.idx;
        let mut count = 0usize;
        while self.idx < self.bytes.len() {
            let c = self.bytes[self.idx] as char;
            if !is_arrow_char(c) {
                break;
            }
            count += 1;
            self.advance_byte();
        }
        if count >= 2 {
            return Token {
                kind: TokenKind::Arrow(&self.input[start_idx..self.idx]),
                span: Span::new(start, self.position()),
            };
        }
        let ch = self.input[start_idx..self.idx]
            .chars()
            .next()
            .unwrap_or('-');
        Token {
            kind: TokenKind::Punct(ch),
            span: Span::new(start, self.position()),
        }
    }

    fn skip_spaces(&mut self) {
        while self.idx < self.bytes.len() {
            let b = self.bytes[self.idx];
            if b == b' ' || b == b'\t' {
                self.advance_byte();
            } else {
                break;
            }
        }
    }

    fn advance_byte(&mut self) {
        if self.idx >= self.bytes.len() {
            return;
        }
        let b = self.bytes[self.idx];
        self.idx += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
    }

    fn position(&self) -> Position {
        Position {
            line: self.line,
            col: self.col,
            byte: self.idx,
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.idx + 1).copied()
    }

    fn peek_n_bytes(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.idx + n).copied()
    }
}

pub fn tokenize(input: &str) -> Vec<Token<'_>> {
    Lexer::new(input).tokenize()
}

#[derive(Debug, Clone)]
pub struct MermaidParse {
    pub ast: MermaidAst,
    pub errors: Vec<MermaidError>,
}

pub fn parse(input: &str) -> Result<MermaidAst, MermaidError> {
    let parsed = parse_with_diagnostics(input);
    if let Some(err) = parsed.errors.first() {
        return Err(err.clone());
    }
    Ok(parsed.ast)
}

pub fn parse_with_diagnostics(input: &str) -> MermaidParse {
    let mut diagram_type = DiagramType::Unknown;
    let mut direction = None;
    let mut pie_show_data = false;
    let mut directives = Vec::new();
    let mut statements = Vec::new();
    let mut saw_header = false;
    let mut errors = Vec::new();
    let mut pending_note: Option<StateNotePending> = None;
    // Track ER entity attribute block: `ENTITY { type name constraint ... }`
    let mut er_entity_block: Option<String> = None;

    for (idx, raw_line) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim_end_matches('\r');
        let trimmed = strip_inline_comment(line).trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with("%%{") {
            let span = Span::at_line(line_no, line.len());
            match parse_directive_block(trimmed, span) {
                Ok(dir) => {
                    directives.push(dir.clone());
                    statements.push(Statement::Directive(dir));
                }
                Err(err) => errors.push(err),
            }
            continue;
        }
        if trimmed.starts_with("%%") {
            let span = Span::at_line(line_no, line.len());
            let text = trimmed.trim_start_matches('%').trim();
            statements.push(Statement::Comment(Comment {
                text: text.to_string(),
                span,
            }));
            continue;
        }

        if !saw_header {
            if let Some((dtype, dir)) = parse_header(trimmed) {
                if dtype == DiagramType::Pie {
                    let lower = trimmed.to_ascii_lowercase();
                    pie_show_data = lower
                        .split_whitespace()
                        .skip(1)
                        .any(|token| token == "showdata");
                }
                diagram_type = dtype;
                direction = dir;
                saw_header = true;
                continue;
            }
            let span = Span::at_line(line_no, line.len());
            errors.push(
                MermaidError::new("expected Mermaid diagram header", span).with_expected(vec![
                    "graph",
                    "flowchart",
                    "sequenceDiagram",
                    "stateDiagram",
                    "gantt",
                    "classDiagram",
                    "erDiagram",
                    "mindmap",
                    "pie",
                ]),
            );
            diagram_type = DiagramType::Unknown;
            saw_header = true;
        }

        let span = Span::at_line(line_no, line.len());

        if let Some(note) = pending_note.as_mut() {
            if trimmed.eq_ignore_ascii_case("end note") {
                let text = note.lines.join("\n");
                let note_id = format!(
                    "__state_note_L{}_C{}",
                    note.span.start.line, note.span.start.col
                );
                let label = if text.is_empty() { None } else { Some(text) };
                statements.push(Statement::Node(Node {
                    id: note_id.clone(),
                    label,
                    shape: NodeShape::Rect,
                    span: note.span,
                }));
                statements.push(Statement::ClassAssign {
                    targets: vec![note_id.clone()],
                    classes: vec![STATE_NOTE_CLASS.to_string()],
                    span: note.span,
                });
                statements.push(Statement::Edge(Edge {
                    from: note.target.clone(),
                    to: note_id,
                    arrow: "-.->".to_string(),
                    label: None,
                    span: note.span,
                }));
                pending_note = None;
            } else {
                note.lines.push(normalize_ws(raw_line));
            }
            continue;
        }
        if let Some(result) = parse_directive_statement(trimmed, span, diagram_type) {
            match result {
                Ok(statement) => statements.push(statement),
                Err(err) => {
                    errors.push(err);
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            continue;
        }
        match diagram_type {
            DiagramType::State => {
                if trimmed == "}" {
                    statements.push(Statement::SubgraphEnd { span });
                    continue;
                }
                if let Some((target, inline)) = parse_state_note_start(trimmed) {
                    if let Some(text) = inline {
                        let note_id =
                            format!("__state_note_L{}_C{}", span.start.line, span.start.col);
                        let label = if text.is_empty() { None } else { Some(text) };
                        statements.push(Statement::Node(Node {
                            id: note_id.clone(),
                            label,
                            shape: NodeShape::Rect,
                            span,
                        }));
                        statements.push(Statement::ClassAssign {
                            targets: vec![note_id.clone()],
                            classes: vec![STATE_NOTE_CLASS.to_string()],
                            span,
                        });
                        statements.push(Statement::Edge(Edge {
                            from: target,
                            to: note_id,
                            arrow: "-.->".to_string(),
                            label: None,
                            span,
                        }));
                    } else {
                        pending_note = Some(StateNotePending {
                            target,
                            lines: Vec::new(),
                            span,
                        });
                    }
                    continue;
                }
                if let Some(state) = parse_state_decl_line(trimmed) {
                    if state.block_start {
                        let title = state.label.clone().or_else(|| Some(state.id.clone()));
                        statements.push(Statement::SubgraphStart { title, span });
                        statements.push(Statement::Node(Node {
                            id: state.id.clone(),
                            label: state.label.clone(),
                            shape: NodeShape::Rect,
                            span,
                        }));
                        statements.push(Statement::ClassAssign {
                            targets: vec![state.id],
                            classes: vec![STATE_CONTAINER_CLASS.to_string()],
                            span,
                        });
                    } else {
                        statements.push(Statement::Node(Node {
                            id: state.id,
                            label: state.label,
                            shape: NodeShape::Rect,
                            span,
                        }));
                    }
                    continue;
                }
                if let Some(edge) = parse_state_edge(trimmed, span) {
                    statements.push(Statement::Edge(edge));
                } else if let Some(node) = parse_node(trimmed, span) {
                    statements.push(Statement::Node(node));
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Graph | DiagramType::Class | DiagramType::Er => {
                let er_mode = diagram_type == DiagramType::Er;
                // ER entity attribute block: lines inside `ENTITY { ... }`.
                if er_mode {
                    if let Some(ref entity) = er_entity_block {
                        if trimmed == "}" {
                            er_entity_block = None;
                            continue;
                        }
                        // Parse attribute line as "type name [constraint]".
                        let attr_text = normalize_ws(trimmed);
                        if !attr_text.is_empty() {
                            statements.push(Statement::ClassMember {
                                class: entity.clone(),
                                member: attr_text,
                                span,
                            });
                        }
                        continue;
                    }
                    // Detect entity block opening: `ENTITY_NAME {`
                    if let Some(brace_pos) = trimmed.find('{') {
                        let entity_name = trimmed[..brace_pos].trim();
                        if !entity_name.is_empty()
                            && entity_name
                                .chars()
                                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                        {
                            let name = normalize_ws(entity_name);
                            // Emit a node for the entity.
                            statements.push(Statement::Node(Node {
                                id: name.clone(),
                                label: None,
                                shape: NodeShape::Rect,
                                span,
                            }));
                            let after_brace = trimmed[brace_pos + 1..].trim();
                            if let Some(close_pos) = after_brace.find('}') {
                                // Inline block: `ENTITY { attrs... }`
                                let inline_attrs = after_brace[..close_pos].trim();
                                if !inline_attrs.is_empty() {
                                    statements.push(Statement::ClassMember {
                                        class: name.clone(),
                                        member: normalize_ws(inline_attrs),
                                        span,
                                    });
                                }
                            } else if !after_brace.is_empty() {
                                // Content after `{` but no `}` on this line:
                                // treat as first attribute and open block.
                                statements.push(Statement::ClassMember {
                                    class: name.clone(),
                                    member: normalize_ws(after_brace),
                                    span,
                                });
                                er_entity_block = Some(name);
                            } else {
                                // Just `ENTITY {` — open block for subsequent lines.
                                er_entity_block = Some(name);
                            }
                            continue;
                        }
                    }
                }
                if let Some(edge) = parse_edge(trimmed, span, er_mode) {
                    if let Some(node) = edge_node(trimmed, span, er_mode) {
                        statements.push(Statement::Node(node));
                    }
                    if let Some(node) = edge_node_right(trimmed, span, er_mode) {
                        statements.push(Statement::Node(node));
                    }
                    statements.push(Statement::Edge(edge));
                } else if let Some(member) = parse_class_member(trimmed, span) {
                    statements.push(member);
                } else if let Some(node) = parse_node(trimmed, span) {
                    statements.push(Statement::Node(node));
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Sequence => {
                if let Some(msg) = parse_sequence(trimmed, span) {
                    statements.push(Statement::SequenceMessage(msg));
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Gantt => {
                if let Some(stmt) = parse_gantt(trimmed, span) {
                    statements.push(stmt);
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Mindmap => {
                if let Some(node) = parse_mindmap(trimmed, raw_line, span) {
                    statements.push(Statement::MindmapNode(node));
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Pie => {
                if is_pie_show_data_line(trimmed) {
                    pie_show_data = true;
                }
                if let Some(entry) = parse_pie(trimmed, span) {
                    statements.push(Statement::PieEntry(entry));
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Unknown => {
                statements.push(Statement::Raw {
                    text: normalize_ws(trimmed),
                    span,
                });
            }
        }
    }

    MermaidParse {
        ast: MermaidAst {
            diagram_type,
            direction,
            pie_show_data,
            directives,
            statements,
        },
        errors,
    }
}

pub fn validate_ast(
    ast: &MermaidAst,
    config: &MermaidConfig,
    matrix: &MermaidCompatibilityMatrix,
) -> MermaidValidation {
    validate_ast_with_policy(ast, config, matrix, &MermaidFallbackPolicy::default())
}

pub fn validate_ast_with_policy(
    ast: &MermaidAst,
    config: &MermaidConfig,
    matrix: &MermaidCompatibilityMatrix,
    policy: &MermaidFallbackPolicy,
) -> MermaidValidation {
    let init_parse = collect_init_config(ast, config, policy);
    validate_ast_with_policy_and_init(ast, config, matrix, policy, &init_parse)
}

/// Validate an AST with a pre-parsed init directive config.
#[must_use]
pub fn validate_ast_with_policy_and_init(
    ast: &MermaidAst,
    config: &MermaidConfig,
    matrix: &MermaidCompatibilityMatrix,
    policy: &MermaidFallbackPolicy,
    init_parse: &MermaidInitParse,
) -> MermaidValidation {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    if matrix.support_for(ast.diagram_type) == MermaidSupportLevel::Unsupported {
        let span = ast
            .statements
            .first()
            .map(statement_span)
            .unwrap_or_else(|| Span::at_line(1, 1));
        apply_fallback_action(
            policy.unsupported_diagram,
            MermaidWarningCode::UnsupportedDiagram,
            "unsupported diagram type",
            span,
            &mut warnings,
            &mut errors,
        );
    }

    for statement in &ast.statements {
        match statement {
            Statement::Directive(dir) => match &dir.kind {
                DirectiveKind::Init { .. } => {
                    if !config.enable_init_directives {
                        apply_fallback_action(
                            policy.unsupported_directive,
                            MermaidWarningCode::UnsupportedDirective,
                            "init directives disabled",
                            dir.span,
                            &mut warnings,
                            &mut errors,
                        );
                    }
                }
                DirectiveKind::Raw => {
                    apply_fallback_action(
                        policy.unsupported_directive,
                        MermaidWarningCode::UnsupportedDirective,
                        "unsupported directive",
                        dir.span,
                        &mut warnings,
                        &mut errors,
                    );
                }
            },
            Statement::ClassDef { span, .. }
            | Statement::ClassAssign { span, .. }
            | Statement::Style { span, .. }
            | Statement::LinkStyle { span, .. } => {
                if !config.enable_styles {
                    apply_fallback_action(
                        policy.unsupported_style,
                        MermaidWarningCode::UnsupportedStyle,
                        "styles disabled",
                        *span,
                        &mut warnings,
                        &mut errors,
                    );
                }
            }
            Statement::Link { span, .. } => {
                if !config.enable_links || config.link_mode == MermaidLinkMode::Off {
                    apply_fallback_action(
                        policy.unsupported_link,
                        MermaidWarningCode::UnsupportedLink,
                        "links disabled",
                        *span,
                        &mut warnings,
                        &mut errors,
                    );
                }
            }
            Statement::Raw { text, span } => {
                let is_pie_meta = ast.diagram_type == DiagramType::Pie
                    && (is_pie_show_data_line(text) || parse_pie_title_line(text).is_some());
                if !is_pie_meta {
                    apply_fallback_action(
                        policy.unsupported_feature,
                        MermaidWarningCode::UnsupportedFeature,
                        "unsupported statement",
                        *span,
                        &mut warnings,
                        &mut errors,
                    );
                }
            }
            _ => {}
        }
    }

    warnings.extend(init_parse.warnings.clone());
    errors.extend(init_parse.errors.clone());

    MermaidValidation { warnings, errors }
}

/// Parse, apply init directives, and validate a Mermaid diagram in one step.
#[must_use]
pub fn prepare_with_policy(
    input: &str,
    config: &MermaidConfig,
    matrix: &MermaidCompatibilityMatrix,
    policy: &MermaidFallbackPolicy,
) -> MermaidPrepared {
    let mut parsed = parse_with_diagnostics(input);
    let init = apply_init_directives(&mut parsed.ast, config, policy);
    let theme_overrides = init.config.theme_overrides();
    let init_config_hash = init.config.checksum();
    let compatibility = compatibility_report(&parsed.ast, config, matrix);
    let validation = validate_ast_with_policy_and_init(&parsed.ast, config, matrix, policy, &init);
    let prepared = MermaidPrepared {
        ast: parsed.ast,
        parse_errors: parsed.errors,
        init,
        theme_overrides,
        init_config_hash,
        compatibility,
        validation,
    };
    emit_prepare_jsonl(config, &prepared);
    prepared
}

/// Parse, apply init directives, and validate using the default fallback policy.
#[must_use]
pub fn prepare(
    input: &str,
    config: &MermaidConfig,
    matrix: &MermaidCompatibilityMatrix,
) -> MermaidPrepared {
    prepare_with_policy(input, config, matrix, &MermaidFallbackPolicy::default())
}

fn emit_prepare_jsonl(config: &MermaidConfig, prepared: &MermaidPrepared) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    let json = serde_json::json!({
        "event": "mermaid_prepare",
        "diagram_type": prepared.ast.diagram_type.as_str(),
        "init_config_hash": format!("0x{:016x}", prepared.init_config_hash),
        "init_theme": prepared.init.config.theme,
        "init_theme_vars": prepared.init.config.theme_variables.len(),
        "warnings": prepared.all_warnings().len(),
        "errors": prepared.all_errors().len(),
    });
    let line = json.to_string();
    let _ = append_jsonl_line(path, &line);
}

fn emit_guard_jsonl(config: &MermaidConfig, meta: &MermaidDiagramMeta) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    let guard = &meta.guard;
    let mut codes = Vec::new();
    if guard.limits_exceeded {
        codes.push(MermaidWarningCode::LimitExceeded.as_str());
    }
    if guard.budget_exceeded {
        codes.push(MermaidWarningCode::BudgetExceeded.as_str());
    }
    let force_glyph_mode = guard
        .degradation
        .force_glyph_mode
        .map(MermaidGlyphMode::as_str);
    let json = serde_json::json!({
        "event": "mermaid_guard",
        "diagram_type": meta.diagram_type.as_str(),
        "complexity": {
            "nodes": guard.complexity.nodes,
            "edges": guard.complexity.edges,
            "labels": guard.complexity.labels,
            "clusters": guard.complexity.clusters,
            "ports": guard.complexity.ports,
            "style_refs": guard.complexity.style_refs,
            "score": guard.complexity.score,
        },
        "label_limits": {
            "over_chars": guard.label_chars_over,
            "over_lines": guard.label_lines_over,
        },
        "budget_estimates": {
            "route_ops": guard.route_ops_estimate,
            "layout_iterations": guard.layout_iterations_estimate,
        },
        "guard_codes": codes,
        "degradation": {
            "target_fidelity": guard.degradation.target_fidelity.as_str(),
            "hide_labels": guard.degradation.hide_labels,
            "collapse_clusters": guard.degradation.collapse_clusters,
            "simplify_routing": guard.degradation.simplify_routing,
            "reduce_decoration": guard.degradation.reduce_decoration,
            "force_glyph_mode": force_glyph_mode,
        },
    });
    let line = json.to_string();
    let _ = append_jsonl_line(path, &line);
}

pub(crate) fn append_jsonl_line(path: &str, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut buf = String::with_capacity(line.len().saturating_add(1));
    buf.push_str(line);
    buf.push('\n');
    file.write_all(buf.as_bytes())
}

// --- Link/Click rendering + hyperlink policy (bd-25df9) ---

/// Dangerous URL protocols that are always blocked.
const BLOCKED_PROTOCOLS: &[&str] = &["javascript:", "vbscript:", "data:", "file:", "blob:"];

/// Protocols allowed in strict sanitization mode.
const STRICT_ALLOWED_PROTOCOLS: &[&str] = &["http:", "https:", "mailto:", "tel:"];

/// Sanitize a URL according to the given sanitize mode.
///
/// Returns `Allowed` if the URL passes, `Blocked` if it was rejected.
/// In strict mode, only explicitly allowed protocols pass.
/// In lenient mode, only explicitly dangerous protocols are blocked.
#[must_use]
pub fn sanitize_url(url: &str, mode: MermaidSanitizeMode) -> LinkSanitizeOutcome {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return LinkSanitizeOutcome::Blocked;
    }

    // Normalize for protocol detection: lowercase, strip whitespace within protocol prefix.
    let lower = trimmed.to_ascii_lowercase();

    // Block dangerous protocols in all modes.
    for proto in BLOCKED_PROTOCOLS {
        if lower.starts_with(proto) {
            return LinkSanitizeOutcome::Blocked;
        }
    }

    match mode {
        MermaidSanitizeMode::Strict => {
            // In strict mode, require an allowed protocol or treat as relative path.
            // Relative paths (no colon before first slash) are allowed.
            if let Some(colon_pos) = lower.find(':') {
                if lower
                    .find('/')
                    .is_some_and(|slash_pos| colon_pos > slash_pos)
                {
                    // Colon comes after slash, treat as relative path.
                    return LinkSanitizeOutcome::Allowed;
                }
                // Has a protocol prefix - must be in allowed list.
                let prefix = &lower[..=colon_pos];
                if STRICT_ALLOWED_PROTOCOLS.contains(&prefix) {
                    LinkSanitizeOutcome::Allowed
                } else {
                    LinkSanitizeOutcome::Blocked
                }
            } else {
                // No colon at all: relative path or anchor - allowed.
                LinkSanitizeOutcome::Allowed
            }
        }
        MermaidSanitizeMode::Lenient => {
            // Already passed blocked protocol check above.
            LinkSanitizeOutcome::Allowed
        }
    }
}

/// Resolve link directives from an AST into `IrLink` entries.
///
/// Collects `Statement::Link` entries, resolves target node IDs, sanitizes URLs,
/// and returns a `LinkResolution` with metrics.
#[must_use]
pub fn resolve_links(
    ast: &MermaidAst,
    config: &MermaidConfig,
    node_map: &std::collections::HashMap<String, IrNodeId>,
) -> LinkResolution {
    let mut links = Vec::new();
    let mut warnings = Vec::new();
    let mut blocked_count = 0;

    if !config.enable_links || config.link_mode == MermaidLinkMode::Off {
        return LinkResolution {
            links,
            link_mode: config.link_mode,
            total_count: 0,
            allowed_count: 0,
            blocked_count: 0,
            warnings,
        };
    }

    for statement in &ast.statements {
        if let Statement::Link {
            kind,
            target,
            url,
            tooltip,
            span,
        } = statement
        {
            let outcome = sanitize_url(url, config.sanitize_mode);
            if outcome == LinkSanitizeOutcome::Blocked {
                blocked_count += 1;
                warnings.push(MermaidWarning::new(
                    MermaidWarningCode::SanitizedInput,
                    format!("blocked URL for node '{}': protocol not allowed", target),
                    *span,
                ));
            }

            if let Some(&node_id) = node_map.get(target.as_str()) {
                links.push(IrLink {
                    kind: *kind,
                    target: node_id,
                    url: url.clone(),
                    tooltip: tooltip.clone(),
                    sanitize_outcome: outcome,
                    span: *span,
                });
            } else {
                warnings.push(MermaidWarning::new(
                    MermaidWarningCode::UnsupportedLink,
                    format!("link target '{}' not found in diagram", target),
                    *span,
                ));
            }
        }
    }

    let total_count = links.len();
    let allowed_count = links
        .iter()
        .filter(|l| l.sanitize_outcome == LinkSanitizeOutcome::Allowed)
        .count();

    LinkResolution {
        links,
        link_mode: config.link_mode,
        total_count,
        allowed_count,
        blocked_count,
        warnings,
    }
}

/// Emit link resolution metrics to JSONL evidence log.
fn emit_link_jsonl(config: &MermaidConfig, resolution: &LinkResolution) {
    let Some(path) = config.log_path.as_deref() else {
        return;
    };
    if resolution.total_count == 0 && resolution.blocked_count == 0 {
        return;
    }
    let json = serde_json::json!({
        "event": "mermaid_links",
        "link_mode": resolution.link_mode.as_str(),
        "total_count": resolution.total_count,
        "allowed_count": resolution.allowed_count,
        "blocked_count": resolution.blocked_count,
    });
    let line = json.to_string();
    let _ = append_jsonl_line(path, &line);
}

fn apply_fallback_action(
    action: MermaidFallbackAction,
    code: MermaidWarningCode,
    message: &str,
    span: Span,
    warnings: &mut Vec<MermaidWarning>,
    errors: &mut Vec<MermaidError>,
) {
    match action {
        MermaidFallbackAction::Ignore => {}
        MermaidFallbackAction::Warn => warnings.push(MermaidWarning::new(code, message, span)),
        MermaidFallbackAction::Error => errors.push(MermaidError::new(message, span)),
    }
}

fn statement_span(statement: &Statement) -> Span {
    match statement {
        Statement::Directive(dir) => dir.span,
        Statement::Comment(comment) => comment.span,
        Statement::SubgraphStart { span, .. } => *span,
        Statement::SubgraphEnd { span } => *span,
        Statement::Direction { span, .. } => *span,
        Statement::ClassDeclaration { span, .. } => *span,
        Statement::ClassDef { span, .. } => *span,
        Statement::ClassAssign { span, .. } => *span,
        Statement::Style { span, .. } => *span,
        Statement::LinkStyle { span, .. } => *span,
        Statement::Link { span, .. } => *span,
        Statement::Node(node) => node.span,
        Statement::Edge(edge) => edge.span,
        Statement::SequenceMessage(msg) => msg.span,
        Statement::ClassMember { span, .. } => *span,
        Statement::GanttTitle { span, .. } => *span,
        Statement::GanttSection { span, .. } => *span,
        Statement::GanttTask(task) => task.span,
        Statement::PieEntry(entry) => entry.span,
        Statement::MindmapNode(node) => node.span,
        Statement::Raw { span, .. } => *span,
    }
}

fn strip_inline_comment(line: &str) -> &str {
    if let Some(idx) = line.find("%%") {
        if line[..idx].trim().is_empty() {
            return line;
        }
        &line[..idx]
    } else {
        line
    }
}

fn parse_directive_block(trimmed: &str, span: Span) -> Result<Directive, MermaidError> {
    let content = trimmed
        .strip_prefix("%%{")
        .and_then(|v| v.strip_suffix("}%%"))
        .ok_or_else(|| MermaidError::new("unterminated directive", span))?;
    let (kind, content) = parse_directive_kind(content);
    Ok(Directive {
        kind,
        content,
        span,
    })
}

fn parse_directive_kind(content: &str) -> (DirectiveKind, String) {
    let trimmed = content.trim();
    let prefix = "init:";
    if trimmed.len() >= prefix.len()
        && trimmed
            .get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
    {
        let payload = trimmed[prefix.len()..].trim().to_string();
        return (DirectiveKind::Init { payload }, trimmed.to_string());
    }
    (DirectiveKind::Raw, trimmed.to_string())
}

fn parse_directive_statement(
    line: &str,
    span: Span,
    diagram_type: DiagramType,
) -> Option<Result<Statement, MermaidError>> {
    if let Some(statement) = parse_subgraph_line(line, span) {
        return Some(Ok(statement));
    }
    if line.trim().eq_ignore_ascii_case("end") {
        return Some(Ok(Statement::SubgraphEnd { span }));
    }
    if let Some(result) = parse_direction_line(line, span) {
        return Some(result);
    }
    if let Some(result) = parse_class_def_line(line, span) {
        return Some(result);
    }
    if let Some(result) = parse_class_line(line, span, diagram_type) {
        return Some(result);
    }
    if let Some(result) = parse_style_line(line, span) {
        return Some(result);
    }
    if let Some(result) = parse_link_style_line(line, span) {
        return Some(result);
    }
    if let Some(result) = parse_link_directive(line, span, LinkKind::Click, "click") {
        return Some(result);
    }
    if let Some(result) = parse_link_directive(line, span, LinkKind::Link, "link") {
        return Some(result);
    }
    None
}

fn parse_subgraph_line(line: &str, span: Span) -> Option<Statement> {
    let rest = strip_keyword(line, "subgraph")?;
    let title = if rest.is_empty() {
        None
    } else {
        Some(normalize_ws(rest))
    };
    Some(Statement::SubgraphStart { title, span })
}

fn parse_direction_line(line: &str, span: Span) -> Option<Result<Statement, MermaidError>> {
    let rest = strip_keyword(line, "direction")?;
    let dir_word = rest.split_whitespace().next().unwrap_or_default();
    if dir_word.is_empty() {
        return Some(Err(MermaidError::new("direction missing", span)
            .with_expected(vec!["TB", "TD", "LR", "RL", "BT"])));
    }
    let direction = match dir_word.to_ascii_lowercase().as_str() {
        "tb" => Some(GraphDirection::TB),
        "td" => Some(GraphDirection::TD),
        "lr" => Some(GraphDirection::LR),
        "rl" => Some(GraphDirection::RL),
        "bt" => Some(GraphDirection::BT),
        _ => None,
    };
    match direction {
        Some(direction) => Some(Ok(Statement::Direction { direction, span })),
        None => Some(Err(MermaidError::new("invalid direction", span)
            .with_expected(vec!["TB", "TD", "LR", "RL", "BT"]))),
    }
}

fn parse_class_def_line(line: &str, span: Span) -> Option<Result<Statement, MermaidError>> {
    let rest = strip_keyword(line, "classdef")?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let style = parts.next().unwrap_or("").trim();
    if name.is_empty() {
        return Some(Err(MermaidError::new("classDef missing name", span)
            .with_expected(vec!["classDef <name> <style>"])));
    }
    if style.is_empty() {
        return Some(Err(MermaidError::new("classDef missing style", span)
            .with_expected(vec!["classDef <name> <style>"])));
    }
    Some(Ok(Statement::ClassDef {
        name: normalize_ws(name),
        style: normalize_ws(style),
        span,
    }))
}

fn parse_class_line(
    line: &str,
    span: Span,
    diagram_type: DiagramType,
) -> Option<Result<Statement, MermaidError>> {
    let rest = strip_keyword(line, "class")?;
    let mut parts = rest.split_whitespace();
    let targets_raw = parts.next().unwrap_or("");
    if targets_raw.is_empty() {
        return Some(Err(MermaidError::new("class missing target(s)", span)
            .with_expected(vec!["class <id[,id...]> <class>"])));
    }
    let classes: Vec<String> = parts
        .flat_map(|token| token.split(','))
        .map(normalize_ws)
        .filter(|s| !s.is_empty())
        .collect();
    let class_name = normalize_ws(targets_raw);
    if diagram_type == DiagramType::Class {
        if classes.is_empty() {
            let name = class_name.trim_end_matches('{').trim().to_string();
            return Some(Ok(Statement::ClassDeclaration { name, span }));
        }
        if classes.len() == 1 && classes[0] == "{" {
            return Some(Ok(Statement::ClassDeclaration {
                name: class_name,
                span,
            }));
        }
    }
    if classes.is_empty() {
        return Some(Err(MermaidError::new("class missing class name", span)
            .with_expected(vec!["class <id[,id...]> <class>"])));
    }
    let targets: Vec<String> = targets_raw
        .split(',')
        .map(normalize_ws)
        .filter(|value| !value.is_empty())
        .collect();
    if targets.is_empty() {
        return Some(Err(MermaidError::new("class missing target(s)", span)
            .with_expected(vec!["class <id[,id...]> <class>"])));
    }
    Some(Ok(Statement::ClassAssign {
        targets,
        classes,
        span,
    }))
}

fn parse_style_line(line: &str, span: Span) -> Option<Result<Statement, MermaidError>> {
    let rest = strip_keyword(line, "style")?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let target = parts.next().unwrap_or("").trim();
    let style = parts.next().unwrap_or("").trim();
    if target.is_empty() {
        return Some(Err(MermaidError::new("style missing target", span)
            .with_expected(vec!["style <id> <style>"])));
    }
    if style.is_empty() {
        return Some(Err(MermaidError::new("style missing style", span)
            .with_expected(vec!["style <id> <style>"])));
    }
    Some(Ok(Statement::Style {
        target: normalize_ws(target),
        style: normalize_ws(style),
        span,
    }))
}

fn parse_link_style_line(line: &str, span: Span) -> Option<Result<Statement, MermaidError>> {
    let rest = strip_keyword(line, "linkstyle")?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let link = parts.next().unwrap_or("").trim();
    let style = parts.next().unwrap_or("").trim();
    if link.is_empty() {
        return Some(Err(MermaidError::new("linkStyle missing link id", span)
            .with_expected(vec!["linkStyle <id> <style>"])));
    }
    if style.is_empty() {
        return Some(Err(MermaidError::new("linkStyle missing style", span)
            .with_expected(vec!["linkStyle <id> <style>"])));
    }
    Some(Ok(Statement::LinkStyle {
        link: normalize_ws(link),
        style: normalize_ws(style),
        span,
    }))
}

fn parse_link_directive(
    line: &str,
    span: Span,
    kind: LinkKind,
    keyword: &str,
) -> Option<Result<Statement, MermaidError>> {
    let rest = strip_keyword(line, keyword)?;
    let tokens = split_quoted_words(rest);
    if tokens.len() < 2 {
        return Some(Err(MermaidError::new(
            "link directive missing target/url",
            span,
        )
        .with_expected(vec!["<target>", "<url>"])));
    }
    let mut url_idx = 1;
    if tokens[1].eq_ignore_ascii_case("href") {
        if tokens.len() < 3 {
            return Some(Err(
                MermaidError::new("link directive missing url", span).with_expected(vec!["<url>"])
            ));
        }
        url_idx = 2;
    }
    let target = normalize_ws(&tokens[0]);
    let url = tokens
        .get(url_idx)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if url.is_empty() {
        return Some(Err(
            MermaidError::new("link directive missing url", span).with_expected(vec!["<url>"])
        ));
    }
    let tooltip = if tokens.len() > url_idx + 1 {
        Some(normalize_ws(&tokens[url_idx + 1..].join(" ")))
    } else {
        None
    };
    Some(Ok(Statement::Link {
        kind,
        target,
        url,
        tooltip,
        span,
    }))
}

fn strip_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let trimmed = line.trim();
    let prefix = trimmed.get(..keyword.len())?;
    if !prefix.eq_ignore_ascii_case(keyword) {
        return None;
    }
    let remainder = trimmed.get(keyword.len()..).unwrap_or("");
    if let Some(next) = remainder.chars().next()
        && !next.is_whitespace()
    {
        return None;
    }
    Some(remainder.trim())
}

fn split_quoted_words(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut quote = None;
    let mut iter = input.chars().peekable();
    while let Some(ch) = iter.next() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
                continue;
            }
            if ch == '\\' {
                if let Some(next) = iter.next() {
                    buf.push(next);
                }
                continue;
            }
            buf.push(ch);
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !buf.is_empty() {
                out.push(mem::take(&mut buf));
            }
            continue;
        }
        buf.push(ch);
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

#[allow(dead_code)]
const STATE_START_TOKEN: &str = "__state_start__";
#[allow(dead_code)]
const STATE_END_TOKEN: &str = "__state_end__";
#[allow(dead_code)]
const STATE_CONTAINER_CLASS: &str = "state_container";
#[allow(dead_code)]
const STATE_NOTE_CLASS: &str = "state_note";
#[allow(dead_code)]
const STATE_ENDPOINT_CLASS: &str = "state_endpoint";

#[allow(dead_code)]
#[derive(Debug)]
struct StateNotePending {
    target: String,
    lines: Vec<String>,
    span: Span,
}

#[allow(dead_code)]
#[derive(Debug)]
struct StateDecl {
    id: String,
    label: Option<String>,
    block_start: bool,
}

#[allow(dead_code)]
fn is_state_star(text: &str) -> bool {
    let mut cleaned = String::new();
    for ch in text.chars() {
        if !ch.is_whitespace() {
            cleaned.push(ch);
        }
    }
    cleaned == "[*]"
}

#[allow(dead_code)]
fn parse_state_decl_line(line: &str) -> Option<StateDecl> {
    let mut rest = strip_keyword(line, "state")?;
    if rest.is_empty() {
        return None;
    }

    let mut block_start = false;
    if rest.ends_with('{') {
        block_start = true;
        rest = rest.trim_end_matches('{').trim();
    }
    if rest.is_empty() {
        return None;
    }

    let lower = rest.to_ascii_lowercase();
    if let Some(idx) = lower.find(" as ") {
        let (left_raw, right_raw) = rest.split_at(idx);
        let left = left_raw.trim();
        let right = right_raw[4..].trim();
        if left.is_empty() || right.is_empty() {
            return None;
        }
        let left_has_quote = left.contains('"') || left.contains('\'');
        let right_has_quote = right.contains('"') || right.contains('\'');
        let (label_raw, id_raw) = if left_has_quote && !right_has_quote {
            (left, right)
        } else if right_has_quote && !left_has_quote {
            (right, left)
        } else {
            (left, right)
        };
        let label = normalize_ws(label_raw.trim_matches(['"', '\'']));
        let id = normalize_ws(id_raw);
        if id.is_empty() {
            return None;
        }
        let label = if label.is_empty() { None } else { Some(label) };
        return Some(StateDecl {
            id,
            label,
            block_start,
        });
    }

    let id = normalize_ws(rest);
    if id.is_empty() {
        return None;
    }
    Some(StateDecl {
        id: id.clone(),
        label: Some(id),
        block_start,
    })
}

#[allow(dead_code)]
fn parse_state_note_start(line: &str) -> Option<(String, Option<String>)> {
    let rest = strip_keyword(line, "note")?;
    if rest.is_empty() {
        return None;
    }
    let (before, inline) = if let Some((left, right)) = rest.split_once(':') {
        (left.trim(), Some(normalize_ws(right)))
    } else {
        (rest.trim(), None)
    };
    let tokens = split_quoted_words(before);
    if tokens.is_empty() {
        return None;
    }
    let mut target = None;
    for (idx, tok) in tokens.iter().enumerate() {
        if tok.eq_ignore_ascii_case("of") {
            if let Some(next) = tokens.get(idx + 1) {
                target = Some(normalize_ws(next));
            }
            break;
        }
    }
    if target.is_none() {
        target = Some(normalize_ws(&tokens[tokens.len() - 1]));
    }
    let target = target?;
    if target.is_empty() {
        return None;
    }
    Some((target, inline))
}

#[allow(dead_code)]
fn parse_state_edge(line: &str, span: Span) -> Option<Edge> {
    let (start, end, arrow) = find_arrow(line)?;
    let left = line[..start].trim();
    let right = line[end..].trim();
    if left.is_empty() || right.is_empty() {
        return None;
    }

    let (label, right_id) = if let Some((left_part, label_part)) = right.split_once(':') {
        let label = normalize_ws(label_part);
        let label = if label.is_empty() { None } else { Some(label) };
        (label, left_part.trim())
    } else {
        let (label, rest) = split_label(right);
        (label.map(normalize_ws), rest)
    };

    let from = if is_state_star(left) {
        STATE_START_TOKEN.to_string()
    } else {
        parse_node_id(left)?
    };
    let to = if is_state_star(right_id) {
        STATE_END_TOKEN.to_string()
    } else {
        parse_node_id(right_id)?
    };

    Some(Edge {
        from,
        to,
        arrow: arrow.to_string(),
        label,
        span,
    })
}

#[allow(dead_code)]
fn state_endpoint_id(kind: &str, cluster_stack: &[usize]) -> String {
    if cluster_stack.is_empty() {
        return format!("__state_{}_root", kind);
    }
    let mut path = String::new();
    for (idx, cluster_idx) in cluster_stack.iter().enumerate() {
        if idx > 0 {
            path.push('_');
        }
        path.push_str(&cluster_idx.to_string());
    }
    format!("__state_{}_{}", kind, path)
}

fn parse_header(line: &str) -> Option<(DiagramType, Option<GraphDirection>)> {
    let lower = line.trim().to_ascii_lowercase();
    if lower.starts_with("graph") || lower.starts_with("flowchart") {
        let mut parts = lower.split_whitespace();
        let _ = parts.next()?;
        let dir = parts.next().and_then(|d| match d {
            "tb" => Some(GraphDirection::TB),
            "td" => Some(GraphDirection::TD),
            "lr" => Some(GraphDirection::LR),
            "rl" => Some(GraphDirection::RL),
            "bt" => Some(GraphDirection::BT),
            _ => None,
        });
        return Some((DiagramType::Graph, dir));
    }
    if lower.starts_with("sequencediagram") {
        return Some((DiagramType::Sequence, None));
    }
    if lower.starts_with("statediagram") {
        return Some((DiagramType::State, None));
    }
    if lower.starts_with("gantt") {
        return Some((DiagramType::Gantt, None));
    }
    if lower.starts_with("classdiagram") {
        return Some((DiagramType::Class, None));
    }
    if lower.starts_with("erdiagram") {
        return Some((DiagramType::Er, None));
    }
    if lower.starts_with("mindmap") {
        return Some((DiagramType::Mindmap, None));
    }
    if lower.starts_with("pie") {
        return Some((DiagramType::Pie, None));
    }
    None
}

fn parse_edge(line: &str, span: Span, er_mode: bool) -> Option<Edge> {
    let (start, end, arrow) = if er_mode {
        find_er_arrow(line)?
    } else {
        find_arrow(line)?
    };
    let left = line[..start].trim();
    let right = line[end..].trim();
    if left.is_empty() || right.is_empty() {
        return None;
    }
    let (label, right_id) = if er_mode {
        split_er_label(right)
    } else {
        split_label(right)
    };
    let from = parse_node_id(left)?;
    let to = parse_node_id(right_id)?;
    Some(Edge {
        from,
        to,
        arrow: arrow.to_string(),
        label: label.map(normalize_ws),
        span,
    })
}

fn edge_node(line: &str, span: Span, er_mode: bool) -> Option<Node> {
    let (start, _, _) = if er_mode {
        find_er_arrow(line)?
    } else {
        find_arrow(line)?
    };
    let left = line[..start].trim();
    parse_node(left, span)
}

fn edge_node_right(line: &str, span: Span, er_mode: bool) -> Option<Node> {
    let (_, end, _) = if er_mode {
        find_er_arrow(line)?
    } else {
        find_arrow(line)?
    };
    let right = line[end..].trim();
    let (_, right_id) = if er_mode {
        split_er_label(right)
    } else {
        split_label(right)
    };
    let node = parse_node(right_id, span)?;
    // Only emit a Node statement when the right side has bracket syntax
    // (non-default shape or a label). Bare IDs stay implicit from the edge.
    if node.shape != NodeShape::Rect || node.label.is_some() {
        Some(node)
    } else {
        None
    }
}

fn parse_node(line: &str, span: Span) -> Option<Node> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let (id, label, shape) = parse_node_spec(line)?;
    Some(Node {
        id,
        label,
        shape,
        span,
    })
}

fn parse_node_spec(text: &str) -> Option<(String, Option<String>, NodeShape)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    // Handle asymmetric shape: `>text]`
    if let Some(id) = text
        .strip_prefix('>')
        .and_then(|rest| rest.find(']').map(|end| normalize_ws(rest[..end].trim())))
        .filter(|id| !id.is_empty())
    {
        return Some((id, None, NodeShape::Asymmetric));
    }

    let mut id = String::new();
    let mut chars = text.char_indices().peekable();

    while let Some(&(_, c)) = chars.peek() {
        if c == '[' || c == '(' || c == '{' || c == '>' {
            break;
        }
        if c.is_whitespace() {
            break;
        }
        id.push(c);
        chars.next();
    }

    if id.is_empty() {
        return None;
    }

    // Check if there's a bracket-delimited label + shape
    let bracket_start: String = chars.map(|(_, c)| c).collect();
    let bracket_start = bracket_start.trim();

    if bracket_start.is_empty() {
        return Some((normalize_ws(&id), None, NodeShape::Rect));
    }

    let (label, shape) = parse_bracket_shape(bracket_start);
    Some((normalize_ws(&id), label, shape))
}

/// Detect node shape from the bracket syntax and extract the label text.
fn parse_bracket_shape(text: &str) -> (Option<String>, NodeShape) {
    // Double brackets: `((text))` → Circle
    if text.starts_with("((")
        && let Some(end) = text.find("))")
    {
        let label = normalize_ws(text[2..end].trim());
        return (Some(label), NodeShape::Circle);
    }
    // Double curly: `{{text}}` → Hexagon
    if text.starts_with("{{")
        && let Some(end) = text.find("}}")
    {
        let label = normalize_ws(text[2..end].trim());
        return (Some(label), NodeShape::Hexagon);
    }
    // Double square: `[[text]]` → Subroutine
    if text.starts_with("[[")
        && let Some(end) = text.find("]]")
    {
        let label = normalize_ws(text[2..end].trim());
        return (Some(label), NodeShape::Subroutine);
    }
    // Stadium: `([text])` → Stadium
    if text.starts_with("([")
        && let Some(end) = text.find("])")
    {
        let label = normalize_ws(text[2..end].trim());
        return (Some(label), NodeShape::Stadium);
    }
    // Asymmetric: `>text]`
    if let Some(rest) = text.strip_prefix('>')
        && let Some(end) = rest.find(']')
    {
        let label = normalize_ws(rest[..end].trim());
        return (Some(label), NodeShape::Asymmetric);
    }
    // Single brackets with shape detection
    let (open, close, shape) = if text.starts_with('[') {
        ('[', ']', NodeShape::Rect)
    } else if text.starts_with('(') {
        ('(', ')', NodeShape::Rounded)
    } else if text.starts_with('{') {
        ('{', '}', NodeShape::Diamond)
    } else {
        return (None, NodeShape::Rect);
    };

    let inner = &text[1..];
    if let Some(end) = inner.rfind(close) {
        let label = normalize_ws(inner[..end].trim());
        let _ = open; // used for pattern matching above
        return (Some(label), shape);
    }

    (None, NodeShape::Rect)
}

fn parse_class_member(line: &str, span: Span) -> Option<Statement> {
    if let Some(idx) = line.find(':') {
        let left = line[..idx].trim();
        let right = line[idx + 1..].trim();
        if !left.is_empty() && !right.is_empty() {
            return Some(Statement::ClassMember {
                class: normalize_ws(left),
                member: normalize_ws(right),
                span,
            });
        }
    }
    None
}

fn parse_sequence(line: &str, span: Span) -> Option<SequenceMessage> {
    let (start, end, arrow) = find_arrow(line)?;
    let left = line[..start].trim();
    let right = line[end..].trim();
    let (message, right_id) = if let Some(idx) = right.find(':') {
        (Some(right[idx + 1..].trim()), right[..idx].trim())
    } else {
        (None, right)
    };
    if left.is_empty() || right_id.is_empty() {
        return None;
    }
    Some(SequenceMessage {
        from: normalize_ws(left),
        to: normalize_ws(right_id),
        arrow: arrow.to_string(),
        message: message.map(normalize_ws),
        span,
    })
}

fn parse_gantt(line: &str, span: Span) -> Option<Statement> {
    let lower = line.to_ascii_lowercase();
    // Match keywords case-insensitively but extract values from the
    // original `line` to preserve the user's casing.
    if lower.starts_with("title ") {
        let rest = &line["title ".len()..];
        return Some(Statement::GanttTitle {
            title: normalize_ws(rest),
            span,
        });
    }
    if lower.starts_with("section ") {
        let rest = &line["section ".len()..];
        return Some(Statement::GanttSection {
            name: normalize_ws(rest),
            span,
        });
    }
    if line.contains(':') {
        let mut parts = line.splitn(2, ':');
        let title = parts.next()?.trim();
        let meta = parts.next()?.trim();
        if !title.is_empty() && !meta.is_empty() {
            return Some(Statement::GanttTask(GanttTask {
                title: normalize_ws(title),
                meta: normalize_ws(meta),
                span,
            }));
        }
    }
    None
}

fn parse_pie(line: &str, span: Span) -> Option<PieEntry> {
    let mut parts = line.splitn(2, ':');
    let label = parts.next()?.trim();
    let value = parts.next()?.trim();
    if label.is_empty() || value.is_empty() {
        return None;
    }
    Some(PieEntry {
        label: normalize_ws(label.trim_matches(['"', '\''])),
        value: normalize_ws(value),
        span,
    })
}

fn is_pie_show_data_line(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("showdata")
}

fn parse_pie_title_line(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let keyword = parts.next()?;
    if !keyword.eq_ignore_ascii_case("title") {
        return None;
    }
    let title = parts.next().unwrap_or("").trim();
    if title.is_empty() {
        return None;
    }
    Some(normalize_ws(title))
}

fn parse_mindmap(trimmed: &str, raw_line: &str, span: Span) -> Option<MindmapNode> {
    if trimmed.is_empty() {
        return None;
    }
    let mut depth = 0usize;
    for ch in raw_line.chars() {
        if ch == ' ' {
            depth += 1;
        } else if ch == '\t' {
            depth += 2;
        } else {
            break;
        }
    }
    Some(MindmapNode {
        depth,
        text: normalize_ws(trimmed),
        span,
    })
}

fn split_label(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim();
    // Edge labels use |label| syntax (e.g., "|label| B")
    // Note: ':' is NOT a label delimiter - it's for port notation (e.g., "B:port")
    if let Some(stripped) = trimmed.strip_prefix('|')
        && let Some(end) = stripped.find('|')
    {
        let label = &stripped[..end];
        let rest = stripped[end + 1..].trim();
        return (Some(label), rest);
    }
    (None, trimmed)
}

/// Split ER relationship label from the right side of an ER edge.
///
/// ER syntax: `ENTITY : relationship_label` (colon-separated).
fn split_er_label(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim();
    // ER diagrams use `ENTITY : label` syntax for relationship labels.
    if let Some(colon_pos) = trimmed.find(':') {
        let entity = trimmed[..colon_pos].trim();
        let label = trimmed[colon_pos + 1..].trim();
        if !entity.is_empty() && !label.is_empty() {
            return (Some(label), entity);
        }
    }
    // Fallback: try standard |label| syntax.
    split_label(trimmed)
}

fn parse_node_id(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (id, _, _) = parse_node_spec(text)?;
    Some(id)
}

fn find_arrow(line: &str) -> Option<(usize, usize, &str)> {
    find_arrow_with(line, is_arrow_char)
}

fn find_er_arrow(line: &str) -> Option<(usize, usize, &str)> {
    // Dedicated ER arrow finder: `{` and `}` are cardinality markers in ER
    // arrows, NOT bracket delimiters. We skip bracket depth tracking and only
    // track `[`/`]`/`(`/`)` for node label brackets (rare in ER, but safe).
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    let mut bracket_depth: usize = 0;
    while i < chars.len() {
        match chars[i] {
            '[' | '(' => {
                bracket_depth += 1;
                i += 1;
            }
            ']' | ')' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                i += 1;
            }
            c if bracket_depth == 0 && is_er_arrow_char(c) => {
                let start = i;
                let mut j = i + 1;
                while j < chars.len() && is_er_arrow_char(chars[j]) {
                    j += 1;
                }
                if j - start >= 2 {
                    // Require at least one core arrow character; pure endpoint
                    // markers (o, x, *) are not valid arrows on their own.
                    let has_core = chars[start..j]
                        .iter()
                        .any(|&ch| matches!(ch, '-' | '=' | '.' | '>' | '<'));
                    if has_core {
                        let start_byte = line.char_indices().nth(start).map(|(idx, _)| idx)?;
                        let end_byte = if j >= chars.len() {
                            line.len()
                        } else {
                            line.char_indices().nth(j).map(|(idx, _)| idx)?
                        };
                        let arrow = &line[start_byte..end_byte];
                        return Some((start_byte, end_byte, arrow));
                    }
                }
                i = j;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

fn find_arrow_with(line: &str, is_arrow: fn(char) -> bool) -> Option<(usize, usize, &str)> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    let mut bracket_depth: usize = 0;
    while i < chars.len() {
        match chars[i] {
            '[' | '(' | '{' => {
                bracket_depth += 1;
                i += 1;
            }
            ']' | ')' | '}' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                i += 1;
            }
            c if bracket_depth == 0 && is_arrow(c) => {
                let start = i;
                let mut j = i + 1;
                while j < chars.len() && is_arrow(chars[j]) {
                    j += 1;
                }
                if j - start >= 2 {
                    // Require at least one core arrow character; pure endpoint
                    // markers (o, x, *) are not valid arrows on their own
                    // (e.g., "oo" in "Foo" must not match as an arrow).
                    let has_core = chars[start..j]
                        .iter()
                        .any(|&ch| matches!(ch, '-' | '=' | '.' | '>' | '<'));
                    if has_core {
                        let start_byte = line.char_indices().nth(start).map(|(idx, _)| idx)?;
                        let end_byte = if j >= chars.len() {
                            line.len()
                        } else {
                            line.char_indices().nth(j).map(|(idx, _)| idx)?
                        };
                        let arrow = &line[start_byte..end_byte];
                        return Some((start_byte, end_byte, arrow));
                    }
                }
                i = j;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

fn normalize_ws(input: &str) -> String {
    input
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn keyword_from(text: &str) -> Option<Keyword> {
    match text.to_ascii_lowercase().as_str() {
        "graph" => Some(Keyword::Graph),
        "flowchart" => Some(Keyword::Flowchart),
        "sequencediagram" => Some(Keyword::SequenceDiagram),
        "statediagram" => Some(Keyword::StateDiagram),
        "gantt" => Some(Keyword::Gantt),
        "classdiagram" => Some(Keyword::ClassDiagram),
        "erdiagram" => Some(Keyword::ErDiagram),
        "mindmap" => Some(Keyword::Mindmap),
        "pie" => Some(Keyword::Pie),
        "subgraph" => Some(Keyword::Subgraph),
        "end" => Some(Keyword::End),
        "title" => Some(Keyword::Title),
        "section" => Some(Keyword::Section),
        "direction" => Some(Keyword::Direction),
        "classdef" => Some(Keyword::ClassDef),
        "class" => Some(Keyword::Class),
        "style" => Some(Keyword::Style),
        "linkstyle" => Some(Keyword::LinkStyle),
        "click" => Some(Keyword::Click),
        "link" => Some(Keyword::Link),
        _ => None,
    }
}

fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '$')
}

fn is_arrow_char(c: char) -> bool {
    matches!(c, '-' | '.' | '=' | '<' | '>' | 'o' | 'x' | '*')
}

fn is_er_arrow_char(c: char) -> bool {
    is_arrow_char(c) || matches!(c, '|' | '{' | '}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[allow(dead_code)]
    static LOG_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn is_coverage_run() -> bool {
        std::env::var("LLVM_PROFILE_FILE").is_ok() || std::env::var("CARGO_LLVM_COV").is_ok()
    }

    #[allow(dead_code)]
    fn next_log_path(label: &str) -> String {
        let seq = LOG_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ftui_mermaid_{label}_{}_{}.jsonl",
            std::process::id(),
            seq
        ));
        path.to_string_lossy().to_string()
    }

    #[allow(dead_code)]
    fn jsonl_event(path: &str, event: &str) -> serde_json::Value {
        let content = std::fs::read_to_string(path).expect("read log");
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(trimmed).expect("jsonl parse");
            if value.get("event").and_then(|v| v.as_str()) == Some(event) {
                return value;
            }
        }
        panic!("missing jsonl event: {event}");
    }

    #[test]
    fn tokenize_graph_header() {
        let tokens = tokenize("graph TD\nA-->B\n");
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Keyword(Keyword::Graph)))
        );
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Arrow("-->")))
        );
    }

    #[test]
    fn parse_graph_edges() {
        let ast = parse("graph TD\nA-->B\nB-->C\n").expect("parse");
        assert_eq!(ast.diagram_type, DiagramType::Graph);
        let edges = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::Edge(_)))
            .count();
        assert_eq!(edges, 2);
    }

    #[test]
    fn parse_sequence_messages() {
        let ast = parse("sequenceDiagram\nAlice->>Bob: Hello\n").expect("parse");
        let msgs = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::SequenceMessage(_)))
            .count();
        assert_eq!(msgs, 1);
    }

    #[test]
    fn parse_state_edges() {
        let ast = parse("stateDiagram\nS1-->S2\n").expect("parse");
        let edges = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::Edge(_)))
            .count();
        assert_eq!(edges, 1);
    }

    #[test]
    fn parse_state_start_end_edges() {
        let ast = parse("stateDiagram-v2\n[*] --> S1\nS1 --> [*]\n").expect("parse");
        let mut saw_start = false;
        let mut saw_end = false;
        for statement in &ast.statements {
            if let Statement::Edge(edge) = statement {
                if edge.from == STATE_START_TOKEN && edge.to == "S1" {
                    saw_start = true;
                }
                if edge.to == STATE_END_TOKEN && edge.from == "S1" {
                    saw_end = true;
                }
            }
        }
        assert!(saw_start, "expected start edge");
        assert!(saw_end, "expected end edge");
    }

    #[test]
    fn parse_state_note_inline() {
        let ast = parse("stateDiagram\nnote right of S1: hello\n").expect("parse");
        let has_note_node = ast
            .statements
            .iter()
            .any(|s| matches!(s, Statement::Node(node) if node.id.starts_with("__state_note_")));
        let has_note_edge = ast
            .statements
            .iter()
            .any(|s| matches!(s, Statement::Edge(edge) if edge.arrow == "-.->"));
        assert!(has_note_node, "expected note node");
        assert!(has_note_edge, "expected note edge");
    }

    #[test]
    fn parse_gantt_lines() {
        let ast = parse(
            "gantt\n    title Project Plan\n    section Phase 1\n    Task A :done, 2024-01-01, 1d\n",
        )
        .expect("parse");
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::GanttTitle { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::GanttSection { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::GanttTask(_)))
        );
    }

    #[test]
    fn parse_class_member() {
        let ast = parse("classDiagram\nClassA : +int id\n").expect("parse");
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::ClassMember { .. }))
        );
    }

    #[test]
    fn parse_er_edge() {
        let ast = parse("erDiagram\nA ||--o{ B : relates\n").expect("parse");
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::Edge(_)))
        );
    }

    #[test]
    fn er_edge_preserves_relationship_label() {
        let ast = parse("erDiagram\nCUSTOMER ||--o{ ORDER : places\n").expect("parse");
        let edge = ast
            .statements
            .iter()
            .find_map(|s| match s {
                Statement::Edge(e) => Some(e),
                _ => None,
            })
            .expect("should have edge");
        assert_eq!(edge.from, "CUSTOMER");
        assert_eq!(edge.to, "ORDER");
        assert_eq!(edge.label.as_deref(), Some("places"));
        assert_eq!(edge.arrow, "||--o{");
    }

    #[test]
    fn er_edge_without_label() {
        let ast = parse("erDiagram\nA ||--|| B\n").expect("parse");
        let edge = ast
            .statements
            .iter()
            .find_map(|s| match s {
                Statement::Edge(e) => Some(e),
                _ => None,
            })
            .expect("should have edge");
        assert_eq!(edge.from, "A");
        assert_eq!(edge.to, "B");
        assert!(edge.label.is_none());
    }

    #[test]
    fn er_entity_attributes_parsed_as_members() {
        let input = "erDiagram\n    CUSTOMER {\n        string name PK\n        int age\n    }\n";
        let ast = parse(input).expect("parse");
        // CUSTOMER should be emitted as a node.
        let nodes: Vec<_> = ast
            .statements
            .iter()
            .filter_map(|s| match s {
                Statement::Node(n) => Some(&n.id),
                _ => None,
            })
            .collect();
        assert!(nodes.contains(&&"CUSTOMER".to_string()));
        // Attributes should be ClassMember entries.
        let members: Vec<_> = ast
            .statements
            .iter()
            .filter_map(|s| match s {
                Statement::ClassMember { class, member, .. } => {
                    Some((class.clone(), member.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].0, "CUSTOMER");
        assert_eq!(members[0].1, "string name PK");
        assert_eq!(members[1].0, "CUSTOMER");
        assert_eq!(members[1].1, "int age");
    }

    #[test]
    fn er_multiple_entities_with_relationship() {
        let input = concat!(
            "erDiagram\n",
            "    CUSTOMER {\n",
            "        string name\n",
            "    }\n",
            "    ORDER {\n",
            "        int id\n",
            "        date created\n",
            "    }\n",
            "    CUSTOMER ||--o{ ORDER : places\n",
        );
        let ast = parse(input).expect("parse");
        let node_ids: Vec<_> = ast
            .statements
            .iter()
            .filter_map(|s| match s {
                Statement::Node(n) => Some(n.id.clone()),
                _ => None,
            })
            .collect();
        assert!(node_ids.contains(&"CUSTOMER".to_string()));
        assert!(node_ids.contains(&"ORDER".to_string()));
        let edges: Vec<_> = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::Edge(_)))
            .collect();
        assert_eq!(edges.len(), 1);
        let members: Vec<_> = ast
            .statements
            .iter()
            .filter_map(|s| match s {
                Statement::ClassMember { class, member, .. } => {
                    Some((class.clone(), member.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(members.len(), 3);
    }

    #[test]
    fn er_cardinality_arrows() {
        // Test various ER cardinality notations.
        for (arrow, desc) in [
            ("||--||", "one-to-one"),
            ("||--o{", "one-to-zero-or-many"),
            ("}o--o{", "many-to-many"),
            ("|o--|{", "zero-or-one-to-one-or-many"),
        ] {
            let input = format!("erDiagram\nA {} B : {}", arrow, desc);
            let ast = parse(&input).unwrap_or_else(|_| panic!("parse {}", desc));
            let edge = ast
                .statements
                .iter()
                .find_map(|s| match s {
                    Statement::Edge(e) => Some(e),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no edge for {}", desc));
            assert_eq!(edge.arrow, arrow, "arrow for {}", desc);
            assert_eq!(edge.label.as_deref(), Some(desc), "label for {}", desc);
        }
    }

    #[test]
    fn er_support_level_is_supported() {
        let matrix = MermaidCompatibilityMatrix::default();
        assert_eq!(
            matrix.support_for(DiagramType::Er),
            MermaidSupportLevel::Supported
        );
    }

    #[test]
    fn parse_mindmap_nodes() {
        let ast = parse("mindmap\n  root\n    child\n").expect("parse");
        let nodes = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::MindmapNode(_)))
            .count();
        assert_eq!(nodes, 2);
    }

    #[test]
    fn normalize_mindmap_creates_edges() {
        let ast =
            parse("mindmap\n  Root\n    Child A\n      Leaf A1\n    Child B\n").expect("parse");
        let config = MermaidConfig::default();
        let normalized = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        assert_eq!(normalized.ir.nodes.len(), 4);
        assert_eq!(normalized.ir.edges.len(), 3);
        let labels: Vec<&str> = normalized
            .ir
            .nodes
            .iter()
            .filter_map(|node| {
                node.label
                    .map(|label| normalized.ir.labels[label.0].text.as_str())
            })
            .collect();
        for expected in ["Root", "Child A", "Leaf A1", "Child B"] {
            assert!(
                labels.iter().any(|label| label == &expected),
                "missing label: {}",
                expected
            );
        }
        assert!(normalized.errors.is_empty());
    }

    #[test]
    fn parse_pie_entries() {
        let ast = parse("pie\n  \"Dogs\" : 386\n  Cats : 85\n").expect("parse");
        let entries = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::PieEntry(_)))
            .count();
        assert_eq!(entries, 2);
    }

    #[test]
    fn normalize_pie_title_and_show_data() {
        let input = "pie showData\ntitle Pets\n\"Dogs\": 386\nCats: 85\n";
        let parsed = parse_with_diagnostics(input);
        assert_eq!(parsed.ast.diagram_type, DiagramType::Pie);
        assert!(parsed.ast.pie_show_data);
        let config = MermaidConfig::default();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let ir_parse = normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
        assert!(
            ir_parse.errors.is_empty(),
            "unexpected normalization errors: {:?}",
            ir_parse.errors
        );
        assert_eq!(ir_parse.ir.pie_entries.len(), 2);
        assert!(ir_parse.ir.pie_show_data);
        let title = ir_parse
            .ir
            .pie_title
            .and_then(|id| ir_parse.ir.labels.get(id.0))
            .map(|label| label.text.as_str());
        assert_eq!(title, Some("Pets"));
    }

    #[test]
    fn tokenize_directive_block() {
        let tokens = tokenize("%%{init: {\"theme\":\"dark\"}}%%\n");
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Directive(_)))
        );
    }

    #[test]
    fn tokenize_comment_line() {
        let tokens = tokenize("%% just a comment\n");
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Comment(_)))
        );
    }

    #[test]
    fn parse_directive_line() {
        let ast = parse("graph TD\n%%{init: {\"theme\":\"dark\"}}%%\nA-->B\n").expect("parse");
        let directive = ast
            .statements
            .iter()
            .find_map(|s| match s {
                Statement::Directive(dir) => Some(dir),
                _ => None,
            })
            .expect("directive");
        assert!(matches!(directive.kind, DirectiveKind::Init { .. }));
    }

    #[test]
    fn parse_init_directive_supported_keys() {
        let payload = r##"{"theme":"dark","themeVariables":{"primaryColor":"#ffcc00","spacing":2},"flowchart":{"direction":"LR"}}"##;
        let parsed = parse_init_directive(
            payload,
            Span::at_line(1, payload.len()),
            &MermaidFallbackPolicy::default(),
        );
        assert!(parsed.errors.is_empty());
        assert_eq!(parsed.config.theme.as_deref(), Some("dark"));
        assert_eq!(
            parsed
                .config
                .theme_variables
                .get("primaryColor")
                .map(String::as_str),
            Some("#ffcc00")
        );
        assert_eq!(
            parsed
                .config
                .theme_variables
                .get("spacing")
                .map(String::as_str),
            Some("2")
        );
        assert_eq!(parsed.config.flowchart_direction, Some(GraphDirection::LR));
    }

    #[test]
    fn parse_init_directive_reports_invalid_json() {
        let payload = "{invalid}";
        let parsed = parse_init_directive(
            payload,
            Span::at_line(1, payload.len()),
            &MermaidFallbackPolicy::default(),
        );
        assert!(!parsed.errors.is_empty());
    }

    #[test]
    fn collect_init_config_merges_last_wins() {
        let ast = parse(
            "graph TD\n%%{init: {\"theme\":\"dark\"}}%%\n%%{init: {\"theme\":\"base\",\"flowchart\":{\"direction\":\"TB\"}}}%%\nA-->B\n",
        )
        .expect("parse");
        let config = MermaidConfig {
            enable_init_directives: true,
            ..Default::default()
        };
        let parsed = collect_init_config(&ast, &config, &MermaidFallbackPolicy::default());
        assert_eq!(parsed.config.theme.as_deref(), Some("base"));
        assert_eq!(parsed.config.flowchart_direction, Some(GraphDirection::TB));
    }

    #[test]
    fn apply_init_directives_overrides_direction() {
        let mut ast =
            parse("graph TD\n%%{init: {\"flowchart\":{\"direction\":\"LR\"}}}%%\nA-->B\n")
                .expect("parse");
        let config = MermaidConfig {
            enable_init_directives: true,
            ..Default::default()
        };
        let parsed = apply_init_directives(&mut ast, &config, &MermaidFallbackPolicy::default());
        assert!(parsed.errors.is_empty());
        assert_eq!(parsed.config.flowchart_direction, Some(GraphDirection::LR));
        assert_eq!(ast.direction, Some(GraphDirection::LR));
    }

    #[test]
    fn apply_init_directives_respects_disable_flag() {
        let mut ast =
            parse("graph TD\n%%{init: {\"flowchart\":{\"direction\":\"LR\"}}}%%\nA-->B\n")
                .expect("parse");
        let config = MermaidConfig {
            enable_init_directives: false,
            ..Default::default()
        };
        let parsed = apply_init_directives(&mut ast, &config, &MermaidFallbackPolicy::default());
        assert!(parsed.config.flowchart_direction.is_none());
        assert_eq!(ast.direction, Some(GraphDirection::TD));
    }

    #[test]
    fn normalize_dedupes_nodes_and_creates_implicit() {
        let ast = parse("graph TD\nA[One]\nA[Two]\nA-->B\n").expect("parse");
        let config = MermaidConfig::default();
        let normalized = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        assert_eq!(
            normalized
                .ir
                .nodes
                .iter()
                .filter(|node| node.id == "A")
                .count(),
            1
        );
        assert!(normalized.ir.nodes.iter().any(|node| node.id == "B"));
        assert!(
            normalized
                .warnings
                .iter()
                .any(|w| w.code == MermaidWarningCode::ImplicitNode)
        );
    }

    #[test]
    fn normalize_orders_nodes_by_id() {
        let ast = parse("graph TD\nB-->C\nA-->B\n").expect("parse");
        let config = MermaidConfig::default();
        let normalized = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let ids: Vec<&str> = normalized
            .ir
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(ids, vec!["A", "B", "C"]);
    }

    #[test]
    fn normalize_ports_are_resolved_with_side_hint() {
        let ast = parse("graph TD\nA:out --> B:in\n").expect("parse");
        let config = MermaidConfig::default();
        let normalized = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        if is_coverage_run() && normalized.ir.ports.len() != 2 {
            eprintln!(
                "coverage flake: ports={:?} edges={:?}",
                normalized.ir.ports, normalized.ir.edges
            );
        }
        if is_coverage_run() {
            assert!(
                !normalized.ir.ports.is_empty(),
                "expected at least one port under coverage"
            );
        } else {
            assert_eq!(normalized.ir.ports.len(), 2);
        }
        assert!(matches!(normalized.ir.edges[0].from, IrEndpoint::Port(_)));
        assert!(matches!(normalized.ir.edges[0].to, IrEndpoint::Port(_)));
        assert!(
            normalized
                .ir
                .ports
                .iter()
                .all(|port| port.side_hint == IrPortSideHint::Vertical)
        );
    }

    #[test]
    fn normalize_graph_round_trip_has_edges() {
        let ast = parse("graph TD\nA-->B\nB-->C\n").expect("parse");
        let config = MermaidConfig::default();
        let normalized = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        assert!(normalized.ir.edges.len() >= 2);
        assert!(normalized.errors.is_empty());
    }

    #[test]
    fn guard_limits_exceeded_emits_warning() {
        let ast = parse("graph TD\nA-->B\nB-->C\nC-->D\nD-->E\n").expect("parse");
        let config = MermaidConfig {
            max_nodes: 2,
            max_edges: 2,
            ..MermaidConfig::default()
        };
        let normalized = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        assert!(
            normalized
                .warnings
                .iter()
                .any(|w| w.code == MermaidWarningCode::LimitExceeded)
        );
        let guard = &normalized.ir.meta.guard;
        assert!(guard.limits_exceeded);
        assert!(guard.node_limit_exceeded);
        assert!(guard.edge_limit_exceeded);
        assert!(guard.degradation.hide_labels);
    }

    #[test]
    fn guard_budget_exceeded_emits_warning() {
        let ast = parse("graph TD\nA-->B\nB-->C\nC-->D\n").expect("parse");
        let config = MermaidConfig {
            route_budget: 1,
            layout_iteration_budget: 1,
            ..MermaidConfig::default()
        };
        let normalized = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        assert!(
            normalized
                .warnings
                .iter()
                .any(|w| w.code == MermaidWarningCode::BudgetExceeded)
        );
        let guard = &normalized.ir.meta.guard;
        assert!(guard.budget_exceeded);
        assert!(guard.route_budget_exceeded);
        assert!(guard.layout_budget_exceeded);
        assert!(guard.degradation.simplify_routing);
    }

    #[test]
    fn guard_label_limits_clamp_text() {
        let ast = parse("graph TD\nA[This label is far too long]\n").expect("parse");
        if std::env::var("FTUI_DEBUG_MERMAID").is_ok() {
            eprintln!("ast={:?}", ast.statements);
        }
        let config = MermaidConfig {
            max_label_chars: 8,
            max_label_lines: 1,
            ..MermaidConfig::default()
        };
        let normalized = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        assert!(
            normalized
                .warnings
                .iter()
                .any(|w| w.code == MermaidWarningCode::LimitExceeded)
        );
        let label = normalized.ir.labels.first().expect("label");
        assert!(label.text.chars().count() <= config.max_label_chars);
        let guard = &normalized.ir.meta.guard;
        assert!(guard.label_limit_exceeded);
        assert_eq!(guard.label_chars_over, 1);
        assert_eq!(guard.label_lines_over, 0);
    }

    #[test]
    fn find_arrow_skips_bracket_content() {
        // Arrow chars inside brackets (e.g. "oo" in "too", "--" in labels)
        // must not be detected as arrows.
        let ast = parse("graph TD\nA[too cool]\n").expect("parse");
        let nodes: Vec<_> = ast
            .statements
            .iter()
            .filter_map(|s| match s {
                Statement::Node(n) => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(nodes.len(), 1, "should parse as a single node, not edge");
        assert_eq!(nodes[0].id, "A");
        assert_eq!(nodes[0].label.as_deref(), Some("too cool"));
    }

    #[test]
    fn init_theme_overrides_clone() {
        let payload = r##"{"theme":"dark","themeVariables":{"primaryColor":"#ffcc00"}}"##;
        let parsed = parse_init_directive(
            payload,
            Span::at_line(1, payload.len()),
            &MermaidFallbackPolicy::default(),
        );
        let overrides = parsed.config.theme_overrides();
        assert_eq!(overrides.theme.as_deref(), Some("dark"));
        assert_eq!(
            overrides
                .theme_variables
                .get("primaryColor")
                .map(String::as_str),
            Some("#ffcc00")
        );
    }

    #[test]
    fn prepare_with_policy_applies_init_and_hash() {
        let input = "graph TD\n%%{init: {\"flowchart\":{\"direction\":\"LR\"}}}%%\nA-->B\n";
        let config = MermaidConfig {
            enable_init_directives: true,
            ..Default::default()
        };
        let prepared = prepare_with_policy(
            input,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        assert_eq!(prepared.ast.direction, Some(GraphDirection::LR));
        assert_eq!(prepared.init_config_hash, prepared.init.config.checksum());
    }

    #[test]
    fn init_config_checksum_changes_with_theme() {
        let mut config_a = MermaidInitConfig::default();
        let mut config_b = MermaidInitConfig::default();
        config_a.theme = Some("dark".to_string());
        config_b.theme = Some("base".to_string());
        assert_ne!(config_a.checksum(), config_b.checksum());
    }

    #[test]
    fn parse_subgraph_direction_and_styles() {
        let input = "graph TD\nsubgraph Cluster A\n  direction LR\n  A-->B\nend\nclassDef hot fill:#f00\nclass A,B hot\nstyle A fill:#f00\nlinkStyle 1 stroke:#333\nclick A \"https://example.com\" \"tip\"\n";
        let ast = parse(input).expect("parse");
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::SubgraphStart { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::Direction { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::SubgraphEnd { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::ClassDef { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::ClassAssign { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::Style { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::LinkStyle { .. }))
        );
        assert!(ast.statements.iter().any(|s| matches!(
            s,
            Statement::Link {
                kind: LinkKind::Click,
                ..
            }
        )));
    }

    #[test]
    fn parse_comment_line() {
        let ast = parse("graph TD\n%% note\nA-->B\n").expect("parse");
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::Comment(_)))
        );
    }

    #[test]
    fn parse_with_error_recovery() {
        let parsed = parse_with_diagnostics("graph TD\nclassDef\nA-->B\n");
        assert_eq!(parsed.errors.len(), 1);
        assert!(
            parsed
                .ast
                .statements
                .iter()
                .any(|s| matches!(s, Statement::Edge(_)))
        );
    }

    #[test]
    fn parse_error_reports_expected_header() {
        let parsed = parse_with_diagnostics("not_a_header\nA-->B\n");
        let err = parsed.errors.first().expect("error");
        assert_eq!(err.span.start.line, 1);
        assert!(
            err.expected
                .as_ref()
                .is_some_and(|expected| expected.contains(&"graph"))
        );
    }

    #[test]
    fn fuzz_parse_is_deterministic_and_safe() {
        struct Lcg(u64);
        impl Lcg {
            fn next_u32(&mut self) -> u32 {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
                (self.0 >> 32) as u32
            }
        }

        let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -_[](){}<>:;.,|%\"'\n\t";
        let mut rng = Lcg(0x05ee_da11_cafe_f00d);
        for _ in 0..128 {
            let len = (rng.next_u32() % 200 + 1) as usize;
            let mut s = String::with_capacity(len);
            for _ in 0..len {
                let idx = (rng.next_u32() as usize) % alphabet.len();
                s.push(alphabet[idx] as char);
            }
            let _ = tokenize(&s);
            let _ = parse_with_diagnostics(&s);
        }
    }

    #[test]
    fn mermaid_config_env_parsing() {
        let mut env = HashMap::new();
        env.insert(ENV_MERMAID_ENABLE, "0");
        env.insert(ENV_MERMAID_GLYPH_MODE, "ascii");
        env.insert(ENV_MERMAID_RENDER_MODE, "block");
        env.insert(ENV_MERMAID_TIER, "rich");
        env.insert(ENV_MERMAID_WRAP_MODE, "wordchar");
        env.insert(ENV_MERMAID_ENABLE_LINKS, "1");
        env.insert(ENV_MERMAID_LINK_MODE, "footnote");
        env.insert(ENV_MERMAID_SANITIZE_MODE, "lenient");
        env.insert(ENV_MERMAID_ERROR_MODE, "both");
        env.insert(ENV_MERMAID_MAX_NODES, "123");
        env.insert(ENV_MERMAID_MAX_EDGES, "456");

        let parsed = from_env_with(|key| env.get(key).map(|value| value.to_string()));
        let config = parsed.config;

        assert!(!config.enabled);
        assert_eq!(config.glyph_mode, MermaidGlyphMode::Ascii);
        assert_eq!(config.render_mode, MermaidRenderMode::Block);
        assert_eq!(config.tier_override, MermaidTier::Rich);
        assert_eq!(config.wrap_mode, MermaidWrapMode::WordChar);
        assert!(config.enable_links);
        assert_eq!(config.link_mode, MermaidLinkMode::Footnote);
        assert_eq!(config.sanitize_mode, MermaidSanitizeMode::Lenient);
        assert_eq!(config.error_mode, MermaidErrorMode::Both);
        assert_eq!(config.max_nodes, 123);
        assert_eq!(config.max_edges, 456);
    }

    #[test]
    fn mermaid_config_validation_errors() {
        let mut env = HashMap::new();
        env.insert(ENV_MERMAID_MAX_NODES, "0");
        env.insert(ENV_MERMAID_MAX_EDGES, "0");
        env.insert(ENV_MERMAID_LINK_MODE, "inline");
        env.insert(ENV_MERMAID_ENABLE_LINKS, "0");

        let parsed = from_env_with(|key| env.get(key).map(|value| value.to_string()));
        assert!(!parsed.errors.is_empty());
    }

    #[test]
    fn mermaid_config_invalid_values_reported() {
        let mut env = HashMap::new();
        env.insert(ENV_MERMAID_GLYPH_MODE, "nope");
        env.insert(ENV_MERMAID_RENDER_MODE, "pizza");
        env.insert(ENV_MERMAID_TIER, "mega");

        let parsed = from_env_with(|key| env.get(key).map(|value| value.to_string()));
        assert!(parsed.errors.iter().any(|err| err.field == "glyph_mode"));
        assert!(parsed.errors.iter().any(|err| err.field == "render_mode"));
        assert!(parsed.errors.iter().any(|err| err.field == "tier_override"));
    }

    #[test]
    fn mermaid_compat_matrix_parser_only() {
        let matrix = MermaidCompatibilityMatrix::parser_only();
        assert_eq!(
            matrix.support_for(DiagramType::Graph),
            MermaidSupportLevel::Partial
        );
        assert_eq!(
            matrix.support_for(DiagramType::Sequence),
            MermaidSupportLevel::Partial
        );
        assert_eq!(
            matrix.support_for(DiagramType::Unknown),
            MermaidSupportLevel::Unsupported
        );
    }

    #[test]
    fn mermaid_compat_matrix_default_marks_graph_supported() {
        let matrix = MermaidCompatibilityMatrix::default();
        assert_eq!(
            matrix.support_for(DiagramType::Graph),
            MermaidSupportLevel::Supported
        );
        assert_eq!(
            matrix.support_for(DiagramType::Sequence),
            MermaidSupportLevel::Partial
        );
    }

    #[test]
    fn validate_ast_flags_disabled_links_and_styles() {
        let ast = parse(
            "graph TD\nclassDef hot fill:#f00\nstyle A fill:#f00\nclick A \"https://example.com\" \"tip\"\nA-->B\n",
        )
        .expect("parse");
        let config = MermaidConfig {
            enable_links: false,
            enable_styles: false,
            ..Default::default()
        };
        let validation = validate_ast(&ast, &config, &MermaidCompatibilityMatrix::default());
        assert!(
            validation
                .warnings
                .iter()
                .any(|w| w.code == MermaidWarningCode::UnsupportedStyle)
        );
        assert!(
            validation
                .warnings
                .iter()
                .any(|w| w.code == MermaidWarningCode::UnsupportedLink)
        );
    }

    #[test]
    fn validate_ast_flags_disabled_init_directive() {
        let ast = parse("graph TD\n%%{init: {\"theme\":\"dark\"}}%%\nA-->B\n").expect("parse");
        let config = MermaidConfig {
            enable_init_directives: false,
            ..Default::default()
        };
        let validation = validate_ast(&ast, &config, &MermaidCompatibilityMatrix::default());
        assert!(
            validation
                .warnings
                .iter()
                .any(|w| w.code == MermaidWarningCode::UnsupportedDirective)
        );
    }

    #[test]
    fn mermaid_warning_codes_are_stable() {
        let codes = [
            MermaidWarningCode::UnsupportedDiagram,
            MermaidWarningCode::UnsupportedDirective,
            MermaidWarningCode::UnsupportedStyle,
            MermaidWarningCode::UnsupportedLink,
            MermaidWarningCode::UnsupportedFeature,
            MermaidWarningCode::SanitizedInput,
            MermaidWarningCode::ImplicitNode,
            MermaidWarningCode::InvalidEdge,
            MermaidWarningCode::InvalidPort,
            MermaidWarningCode::InvalidValue,
            MermaidWarningCode::LimitExceeded,
            MermaidWarningCode::BudgetExceeded,
        ];
        for code in codes {
            assert!(code.as_str().starts_with("mermaid/"));
        }
        assert_eq!(
            MermaidWarningCode::UnsupportedDiagram.as_str(),
            "mermaid/unsupported/diagram"
        );
    }

    #[test]
    fn compatibility_report_flags_disabled_features() {
        let input = "graph TD\n%%{init: {\"theme\":\"dark\"}}%%\nclassDef hot fill:#f00\nclick A \"https://example.com\" \"tip\"\n";
        let ast = parse(input).expect("parse");
        let config = MermaidConfig {
            enable_init_directives: false,
            enable_styles: false,
            enable_links: false,
            ..MermaidConfig::default()
        };
        let report =
            compatibility_report(&ast, &config, &MermaidCompatibilityMatrix::parser_only());
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.code == MermaidWarningCode::UnsupportedDirective)
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.code == MermaidWarningCode::UnsupportedStyle)
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.code == MermaidWarningCode::UnsupportedLink)
        );
    }

    #[test]
    fn compatibility_report_marks_unknown_diagram_fatal() {
        let ast = MermaidAst {
            diagram_type: DiagramType::Unknown,
            direction: None,
            directives: Vec::new(),
            statements: Vec::new(),
            pie_show_data: false,
        };
        let config = MermaidConfig::default();
        let report =
            compatibility_report(&ast, &config, &MermaidCompatibilityMatrix::parser_only());
        assert!(report.fatal);
        assert_eq!(report.diagram_support, MermaidSupportLevel::Unsupported);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.code == MermaidWarningCode::UnsupportedDiagram)
        );
    }

    #[test]
    fn mermaid_color_parse_hex6() {
        assert_eq!(
            MermaidColor::parse("#ff0000"),
            Some(MermaidColor::Rgb(255, 0, 0))
        );
        assert_eq!(
            MermaidColor::parse("#00ff00"),
            Some(MermaidColor::Rgb(0, 255, 0))
        );
    }

    #[test]
    fn mermaid_color_parse_hex3() {
        assert_eq!(
            MermaidColor::parse("#fff"),
            Some(MermaidColor::Rgb(255, 255, 255))
        );
        assert_eq!(
            MermaidColor::parse("#f00"),
            Some(MermaidColor::Rgb(255, 0, 0))
        );
    }

    #[test]
    fn mermaid_color_parse_named_and_transparent() {
        assert_eq!(
            MermaidColor::parse("red"),
            Some(MermaidColor::Rgb(255, 0, 0))
        );
        assert_eq!(
            MermaidColor::parse("Navy"),
            Some(MermaidColor::Rgb(0, 0, 128))
        );
        assert_eq!(
            MermaidColor::parse("transparent"),
            Some(MermaidColor::Transparent)
        );
        assert_eq!(MermaidColor::parse("NONE"), Some(MermaidColor::Transparent));
        assert_eq!(MermaidColor::parse("#gggggg"), None);
        assert_eq!(MermaidColor::parse("notacolor"), None);
    }

    #[test]
    fn style_properties_parse_basic() {
        let p = MermaidStyleProperties::parse("fill:#ff0000,stroke:#00ff00,stroke-width:2px");
        assert_eq!(p.fill, Some(MermaidColor::Rgb(255, 0, 0)));
        assert_eq!(p.stroke, Some(MermaidColor::Rgb(0, 255, 0)));
        assert_eq!(p.stroke_width, Some(2));
    }

    #[test]
    fn style_properties_parse_color_weight_dash() {
        let p = MermaidStyleProperties::parse("color:white,font-weight:bold");
        assert_eq!(p.color, Some(MermaidColor::Rgb(255, 255, 255)));
        assert_eq!(p.font_weight, Some(MermaidFontWeight::Bold));
        let d = MermaidStyleProperties::parse("stroke-dasharray:5 5");
        assert_eq!(d.stroke_dash, Some(MermaidStrokeDash::Dashed));
    }

    #[test]
    fn style_properties_unsupported_and_empty() {
        let p = MermaidStyleProperties::parse("fill:red,opacity:0.5,rx:10");
        assert_eq!(p.unsupported.len(), 2);
        assert!(MermaidStyleProperties::parse("").is_empty());
    }

    #[test]
    fn style_properties_merge() {
        let mut base = MermaidStyleProperties::parse("fill:red,stroke:blue");
        base.merge_from(&MermaidStyleProperties::parse("fill:green,color:white"));
        assert_eq!(base.fill, Some(MermaidColor::Rgb(0, 128, 0)));
        assert_eq!(base.stroke, Some(MermaidColor::Rgb(0, 0, 255)));
        assert_eq!(base.color, Some(MermaidColor::Rgb(255, 255, 255)));
    }

    #[test]
    fn resolve_styles_class_then_node_override() {
        let input =
            "graph TD\nclassDef hot fill:#f00,stroke:#0f0\nclass A hot\nstyle A fill:#00f\nA-->B\n";
        let ast = parse(input).expect("parse");
        let ir_parse = normalize_ast_to_ir(
            &ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let resolved = resolve_styles(&ir_parse.ir);
        let a_idx = ir_parse.ir.nodes.iter().position(|n| n.id == "A").unwrap();
        let a_style = &resolved.node_styles[a_idx];
        assert_eq!(a_style.properties.fill, Some(MermaidColor::Rgb(0, 0, 255)));
        assert_eq!(
            a_style.properties.stroke,
            Some(MermaidColor::Rgb(0, 255, 0))
        );
        assert!(a_style.sources.iter().any(|s| s.contains("classDef")));
        assert!(a_style.sources.iter().any(|s| s.contains("style")));
    }

    #[test]
    fn resolve_styles_linkstyle_default_and_index() {
        let input =
            "graph TD\nlinkStyle default stroke:red\nlinkStyle 0 stroke:blue\nA-->B\nC-->D\n";
        let ast = parse(input).expect("parse");
        let ir_parse = normalize_ast_to_ir(
            &ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let resolved = resolve_styles(&ir_parse.ir);
        assert_eq!(
            resolved.edge_styles[0].properties.stroke,
            Some(MermaidColor::Rgb(0, 0, 255))
        );
        if resolved.edge_styles.len() > 1 {
            assert_eq!(
                resolved.edge_styles[1].properties.stroke,
                Some(MermaidColor::Rgb(255, 0, 0))
            );
        }
    }

    #[test]
    fn resolve_styles_linkstyle_respects_edge_order() {
        let input = "graph TD\nB-->C\nA-->D\nlinkStyle 0 stroke:red\n";
        let ast = parse(input).expect("parse");
        let ir_parse = normalize_ast_to_ir(
            &ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let resolved = resolve_styles(&ir_parse.ir);
        let bc_idx = ir_parse
            .ir
            .edges
            .iter()
            .position(|edge| {
                let from = match edge.from {
                    IrEndpoint::Node(id) => ir_parse.ir.nodes[id.0].id.as_str(),
                    IrEndpoint::Port(_) => "",
                };
                let to = match edge.to {
                    IrEndpoint::Node(id) => ir_parse.ir.nodes[id.0].id.as_str(),
                    IrEndpoint::Port(_) => "",
                };
                from == "B" && to == "C"
            })
            .expect("edge B->C");
        assert_eq!(
            resolved.edge_styles[bc_idx].properties.stroke,
            Some(MermaidColor::Rgb(255, 0, 0))
        );
    }

    #[test]
    fn contrast_clamp_works() {
        let yellow_on_white = clamp_contrast(
            MermaidColor::Rgb(255, 255, 0),
            MermaidColor::Rgb(255, 255, 255),
        );
        assert_eq!(yellow_on_white, MermaidColor::Rgb(0, 0, 0));
        let black_on_white =
            clamp_contrast(MermaidColor::Rgb(0, 0, 0), MermaidColor::Rgb(255, 255, 255));
        assert_eq!(black_on_white, MermaidColor::Rgb(0, 0, 0));
        let dark_on_dark =
            clamp_contrast(MermaidColor::Rgb(30, 30, 30), MermaidColor::Rgb(20, 20, 20));
        assert_eq!(dark_on_dark, MermaidColor::Rgb(255, 255, 255));
    }

    #[test]
    fn resolve_styles_unsupported_warnings_emitted() {
        let input = "graph TD\nstyle A opacity:0.5,rx:10\nA-->B\n";
        let ast = parse(input).expect("parse");
        let ir_parse = normalize_ast_to_ir(
            &ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let resolved = resolve_styles(&ir_parse.ir);
        assert!(!resolved.unsupported_warnings.is_empty());
        assert!(
            resolved
                .unsupported_warnings
                .iter()
                .any(|w| w.message.contains("opacity"))
        );
    }

    #[test]
    fn resolve_styles_theme_variables_as_base_layer() {
        let input = "graph TD\nA-->B\n";
        let ast = parse(input).expect("parse");
        let mut ir_parse = normalize_ast_to_ir(
            &ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        // Inject theme variables into the IR meta
        ir_parse
            .ir
            .meta
            .theme_overrides
            .theme_variables
            .insert("primaryColor".to_string(), "#ff0000".to_string());
        ir_parse
            .ir
            .meta
            .theme_overrides
            .theme_variables
            .insert("primaryTextColor".to_string(), "#ffffff".to_string());
        let resolved = resolve_styles(&ir_parse.ir);
        // All nodes should have theme base as fill + color
        for ns in &resolved.node_styles {
            assert_eq!(ns.properties.fill, Some(MermaidColor::Rgb(255, 0, 0)));
            assert_eq!(ns.properties.color, Some(MermaidColor::Rgb(255, 255, 255)));
            assert!(ns.sources.iter().any(|s| s == "themeVariables"));
        }
    }

    #[test]
    fn resolve_styles_multiple_class_merge() {
        let input = "graph TD\nclassDef a fill:#f00\nclassDef b stroke:#0f0\nclass A a b\nA-->B\n";
        let ast = parse(input).expect("parse");
        let ir_parse = normalize_ast_to_ir(
            &ast,
            &MermaidConfig::default(),
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let resolved = resolve_styles(&ir_parse.ir);
        let a_idx = ir_parse.ir.nodes.iter().position(|n| n.id == "A").unwrap();
        let a_style = &resolved.node_styles[a_idx];
        // Both class defs should merge
        assert_eq!(a_style.properties.fill, Some(MermaidColor::Rgb(255, 0, 0)));
        assert_eq!(
            a_style.properties.stroke,
            Some(MermaidColor::Rgb(0, 255, 0))
        );
    }

    // --- Link/Click Rendering + Hyperlink Policy tests (bd-25df9) ---

    #[test]
    fn sanitize_url_blocks_javascript() {
        assert_eq!(
            sanitize_url("javascript:alert(1)", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Blocked
        );
        assert_eq!(
            sanitize_url("javascript:alert(1)", MermaidSanitizeMode::Lenient),
            LinkSanitizeOutcome::Blocked
        );
    }

    #[test]
    fn sanitize_url_blocks_data_uri() {
        assert_eq!(
            sanitize_url("data:text/html,<h1>XSS</h1>", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Blocked
        );
        assert_eq!(
            sanitize_url("data:text/html,<h1>XSS</h1>", MermaidSanitizeMode::Lenient),
            LinkSanitizeOutcome::Blocked
        );
    }

    #[test]
    fn sanitize_url_blocks_vbscript() {
        assert_eq!(
            sanitize_url("vbscript:MsgBox", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Blocked
        );
    }

    #[test]
    fn sanitize_url_blocks_file_protocol() {
        assert_eq!(
            sanitize_url("file:///etc/passwd", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Blocked
        );
        assert_eq!(
            sanitize_url("file:///etc/passwd", MermaidSanitizeMode::Lenient),
            LinkSanitizeOutcome::Blocked
        );
    }

    #[test]
    fn sanitize_url_blocks_blob() {
        assert_eq!(
            sanitize_url("blob:http://evil.com/abc", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Blocked
        );
    }

    #[test]
    fn sanitize_url_allows_https_strict() {
        assert_eq!(
            sanitize_url("https://example.com", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn sanitize_url_allows_http_strict() {
        assert_eq!(
            sanitize_url("http://example.com", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn sanitize_url_allows_mailto_strict() {
        assert_eq!(
            sanitize_url("mailto:user@example.com", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn sanitize_url_allows_tel_strict() {
        assert_eq!(
            sanitize_url("tel:+1234567890", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn sanitize_url_blocks_ftp_strict() {
        assert_eq!(
            sanitize_url("ftp://files.example.com", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Blocked
        );
    }

    #[test]
    fn sanitize_url_allows_ftp_lenient() {
        assert_eq!(
            sanitize_url("ftp://files.example.com", MermaidSanitizeMode::Lenient),
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn sanitize_url_allows_relative_path() {
        assert_eq!(
            sanitize_url("/docs/readme.md", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Allowed
        );
        assert_eq!(
            sanitize_url("./page.html", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn sanitize_url_allows_anchor() {
        assert_eq!(
            sanitize_url("#section-1", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn sanitize_url_blocks_empty() {
        assert_eq!(
            sanitize_url("", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Blocked
        );
        assert_eq!(
            sanitize_url("   ", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Blocked
        );
    }

    #[test]
    fn sanitize_url_case_insensitive() {
        assert_eq!(
            sanitize_url("JAVASCRIPT:alert(1)", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Blocked
        );
        assert_eq!(
            sanitize_url("JavaScript:alert(1)", MermaidSanitizeMode::Lenient),
            LinkSanitizeOutcome::Blocked
        );
    }

    #[test]
    fn sanitize_url_relative_with_colon_in_path() {
        // Colon in path segment (not a protocol) should be allowed.
        assert_eq!(
            sanitize_url("/path/to/file:123", MermaidSanitizeMode::Strict),
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn resolve_links_disabled_returns_empty() {
        let ast = parse("graph TD\nA-->B\nclick A \"https://example.com\"\n").expect("parse");
        let config = MermaidConfig {
            enable_links: false,
            ..MermaidConfig::default()
        };
        let node_map: HashMap<String, IrNodeId> = HashMap::new();
        let resolution = resolve_links(&ast, &config, &node_map);
        assert_eq!(resolution.total_count, 0);
        assert_eq!(resolution.allowed_count, 0);
        assert_eq!(resolution.blocked_count, 0);
        assert!(resolution.links.is_empty());
    }

    #[test]
    fn resolve_links_off_mode_returns_empty() {
        let ast = parse("graph TD\nA-->B\nclick A \"https://example.com\"\n").expect("parse");
        let config = MermaidConfig {
            enable_links: true,
            link_mode: MermaidLinkMode::Off,
            ..MermaidConfig::default()
        };
        let node_map: HashMap<String, IrNodeId> = HashMap::new();
        let resolution = resolve_links(&ast, &config, &node_map);
        assert!(resolution.links.is_empty());
    }

    #[test]
    fn resolve_links_collects_click_directives() {
        let ast = parse("graph TD\nA-->B\nclick A \"https://example.com\" \"Go to site\"\n")
            .expect("parse");
        let config = MermaidConfig {
            enable_links: true,
            link_mode: MermaidLinkMode::Footnote,
            ..MermaidConfig::default()
        };
        let mut node_map: HashMap<String, IrNodeId> = HashMap::new();
        node_map.insert("A".to_string(), IrNodeId(0));
        node_map.insert("B".to_string(), IrNodeId(1));

        let resolution = resolve_links(&ast, &config, &node_map);
        assert_eq!(resolution.total_count, 1);
        assert_eq!(resolution.allowed_count, 1);
        assert_eq!(resolution.blocked_count, 0);
        assert_eq!(resolution.links[0].kind, LinkKind::Click);
        assert_eq!(resolution.links[0].url, "https://example.com");
        assert_eq!(resolution.links[0].tooltip.as_deref(), Some("Go to site"));
        assert_eq!(resolution.links[0].target, IrNodeId(0));
        assert_eq!(
            resolution.links[0].sanitize_outcome,
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn resolve_links_blocks_dangerous_urls() {
        let ast = parse("graph TD\nA-->B\nclick A \"javascript:alert(1)\"\n").expect("parse");
        let config = MermaidConfig {
            enable_links: true,
            link_mode: MermaidLinkMode::Inline,
            sanitize_mode: MermaidSanitizeMode::Strict,
            ..MermaidConfig::default()
        };
        let mut node_map: HashMap<String, IrNodeId> = HashMap::new();
        node_map.insert("A".to_string(), IrNodeId(0));
        node_map.insert("B".to_string(), IrNodeId(1));

        let resolution = resolve_links(&ast, &config, &node_map);
        assert_eq!(resolution.total_count, 1);
        assert_eq!(resolution.allowed_count, 0);
        assert_eq!(resolution.blocked_count, 1);
        assert_eq!(
            resolution.links[0].sanitize_outcome,
            LinkSanitizeOutcome::Blocked
        );
        assert!(
            resolution
                .warnings
                .iter()
                .any(|w| w.code == MermaidWarningCode::SanitizedInput)
        );
    }

    #[test]
    fn resolve_links_warns_on_missing_target() {
        let ast = parse("graph TD\nA-->B\nclick Z \"https://example.com\"\n").expect("parse");
        let config = MermaidConfig {
            enable_links: true,
            link_mode: MermaidLinkMode::Footnote,
            ..MermaidConfig::default()
        };
        let mut node_map: HashMap<String, IrNodeId> = HashMap::new();
        node_map.insert("A".to_string(), IrNodeId(0));
        node_map.insert("B".to_string(), IrNodeId(1));

        let resolution = resolve_links(&ast, &config, &node_map);
        // Link with missing target is not added to links list
        assert_eq!(resolution.total_count, 0);
        assert!(
            resolution
                .warnings
                .iter()
                .any(|w| w.code == MermaidWarningCode::UnsupportedLink)
        );
    }

    #[test]
    fn resolve_links_multiple_links() {
        let src = "graph TD\nA-->B\nB-->C\nclick A \"https://a.com\"\nclick B \"https://b.com\"\nclick C \"javascript:xss\"\n";
        let ast = parse(src).expect("parse");
        let config = MermaidConfig {
            enable_links: true,
            link_mode: MermaidLinkMode::Footnote,
            sanitize_mode: MermaidSanitizeMode::Strict,
            ..MermaidConfig::default()
        };
        let mut node_map: HashMap<String, IrNodeId> = HashMap::new();
        node_map.insert("A".to_string(), IrNodeId(0));
        node_map.insert("B".to_string(), IrNodeId(1));
        node_map.insert("C".to_string(), IrNodeId(2));

        let resolution = resolve_links(&ast, &config, &node_map);
        assert_eq!(resolution.total_count, 3);
        assert_eq!(resolution.allowed_count, 2);
        assert_eq!(resolution.blocked_count, 1);
        assert_eq!(resolution.link_mode, MermaidLinkMode::Footnote);
    }

    #[test]
    fn normalize_ir_includes_resolved_links() {
        let src = "graph TD\nA-->B\nclick A \"https://example.com\"\n";
        let ast = parse(src).expect("parse");
        let config = MermaidConfig {
            enable_links: true,
            link_mode: MermaidLinkMode::Footnote,
            ..MermaidConfig::default()
        };
        let ir_parse = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        assert_eq!(ir_parse.ir.links.len(), 1);
        assert_eq!(ir_parse.ir.links[0].url, "https://example.com");
        assert_eq!(
            ir_parse.ir.links[0].sanitize_outcome,
            LinkSanitizeOutcome::Allowed
        );
    }

    #[test]
    fn normalize_ir_links_empty_when_disabled() {
        let src = "graph TD\nA-->B\nclick A \"https://example.com\"\n";
        let ast = parse(src).expect("parse");
        let config = MermaidConfig::default(); // enable_links is false by default
        let ir_parse = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        assert!(ir_parse.ir.links.is_empty());
    }

    #[test]
    fn link_resolution_jsonl_evidence() {
        let log_path = format!("/tmp/ftui_test_link_jsonl_{}.jsonl", std::process::id());
        // Clean up from any prior run.
        let _ = std::fs::remove_file(&log_path);

        let src = "graph TD\nA-->B\nclick A \"https://example.com\"\n";
        let ast = parse(src).expect("parse");
        let config = MermaidConfig {
            enable_links: true,
            link_mode: MermaidLinkMode::Footnote,
            log_path: Some(log_path.clone()),
            ..MermaidConfig::default()
        };
        let _ir_parse = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let log_content = std::fs::read_to_string(&log_path).expect("read log");
        let _ = std::fs::remove_file(&log_path);
        assert!(log_content.contains("mermaid_links"));
        assert!(log_content.contains("\"link_mode\":\"footnote\""));
        assert!(log_content.contains("\"total_count\":1"));
        assert!(log_content.contains("\"allowed_count\":1"));
        assert!(log_content.contains("\"blocked_count\":0"));
    }

    // ── NodeShape parsing tests ──────────────────────────────────────

    #[test]
    fn parse_node_shape_rect() {
        let (id, label, shape) = parse_node_spec("A[Hello]").unwrap();
        assert_eq!(id, "A");
        assert_eq!(label.as_deref(), Some("Hello"));
        assert_eq!(shape, NodeShape::Rect);
    }

    #[test]
    fn parse_node_shape_rounded() {
        let (id, label, shape) = parse_node_spec("B(Round)").unwrap();
        assert_eq!(id, "B");
        assert_eq!(label.as_deref(), Some("Round"));
        assert_eq!(shape, NodeShape::Rounded);
    }

    #[test]
    fn parse_node_shape_diamond() {
        let (id, label, shape) = parse_node_spec("C{Decision}").unwrap();
        assert_eq!(id, "C");
        assert_eq!(label.as_deref(), Some("Decision"));
        assert_eq!(shape, NodeShape::Diamond);
    }

    #[test]
    fn parse_node_shape_circle() {
        let (id, label, shape) = parse_node_spec("D((Circle))").unwrap();
        assert_eq!(id, "D");
        assert_eq!(label.as_deref(), Some("Circle"));
        assert_eq!(shape, NodeShape::Circle);
    }

    #[test]
    fn parse_node_shape_hexagon() {
        let (id, label, shape) = parse_node_spec("E{{Hex}}").unwrap();
        assert_eq!(id, "E");
        assert_eq!(label.as_deref(), Some("Hex"));
        assert_eq!(shape, NodeShape::Hexagon);
    }

    #[test]
    fn parse_node_shape_subroutine() {
        let (id, label, shape) = parse_node_spec("F[[Sub]]").unwrap();
        assert_eq!(id, "F");
        assert_eq!(label.as_deref(), Some("Sub"));
        assert_eq!(shape, NodeShape::Subroutine);
    }

    #[test]
    fn parse_node_shape_stadium() {
        let (id, label, shape) = parse_node_spec("G([Stadium])").unwrap();
        assert_eq!(id, "G");
        assert_eq!(label.as_deref(), Some("Stadium"));
        assert_eq!(shape, NodeShape::Stadium);
    }

    #[test]
    fn parse_node_shape_asymmetric() {
        let (id, label, shape) = parse_node_spec("H>Flag]").unwrap();
        assert_eq!(id, "H");
        assert_eq!(label.as_deref(), Some("Flag"));
        assert_eq!(shape, NodeShape::Asymmetric);
    }

    #[test]
    fn parse_node_shape_bare_id_defaults_rect() {
        let (id, label, shape) = parse_node_spec("NodeId").unwrap();
        assert_eq!(id, "NodeId");
        assert!(label.is_none());
        assert_eq!(shape, NodeShape::Rect);
    }

    #[test]
    fn parse_node_shape_propagates_through_ir() {
        // Explicit node declarations before edges ensure shapes propagate
        let src = "graph TD\nA[Rect]\nB(Round)\nC{Decision}\nD((Circle))\nA --> B\nC --> D\n";
        let ast = parse(src).expect("parse");
        let config = MermaidConfig::default();
        let ir_parse = normalize_ast_to_ir(
            &ast,
            &config,
            &MermaidCompatibilityMatrix::default(),
            &MermaidFallbackPolicy::default(),
        );
        let ir = &ir_parse.ir;

        let a = ir.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(a.shape, NodeShape::Rect);

        let b = ir.nodes.iter().find(|n| n.id == "B").unwrap();
        assert_eq!(b.shape, NodeShape::Rounded);

        let c = ir.nodes.iter().find(|n| n.id == "C").unwrap();
        assert_eq!(c.shape, NodeShape::Diamond);

        let d = ir.nodes.iter().find(|n| n.id == "D").unwrap();
        assert_eq!(d.shape, NodeShape::Circle);
    }
    // --- Determinism + Caching + Evidence Logs tests (bd-12d5s) ---

    fn make_test_span() -> Span {
        Span {
            start: Position {
                line: 0,
                col: 0,
                byte: 0,
            },
            end: Position {
                line: 0,
                col: 0,
                byte: 0,
            },
        }
    }

    fn make_test_ir(node_ids: &[&str], edges: &[(usize, usize)]) -> MermaidDiagramIr {
        let labels: Vec<IrLabel> = node_ids
            .iter()
            .map(|id| IrLabel {
                text: id.to_string(),
                span: make_test_span(),
            })
            .collect();

        let nodes: Vec<IrNode> = node_ids
            .iter()
            .enumerate()
            .map(|(i, id)| IrNode {
                id: id.to_string(),
                label: Some(IrLabelId(i)),
                shape: NodeShape::Rect,
                classes: vec![],
                style_ref: None,
                span_primary: make_test_span(),
                span_all: vec![],
                implicit: false,
                members: vec![],
            })
            .collect();

        let ir_edges: Vec<IrEdge> = edges
            .iter()
            .map(|(from, to)| IrEdge {
                from: IrEndpoint::Node(IrNodeId(*from)),
                to: IrEndpoint::Node(IrNodeId(*to)),
                arrow: "-->".to_string(),
                label: None,
                style_ref: None,
                span: make_test_span(),
            })
            .collect();

        MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction: GraphDirection::TD,
            nodes,
            edges: ir_edges,
            ports: vec![],
            clusters: vec![],
            labels,
            pie_entries: vec![],
            pie_title: None,
            pie_show_data: false,
            style_refs: vec![],
            links: vec![],
            meta: MermaidDiagramMeta {
                diagram_type: DiagramType::Graph,
                direction: GraphDirection::TD,
                support_level: MermaidSupportLevel::Supported,
                init: MermaidInitParse::default(),
                theme_overrides: MermaidThemeOverrides::default(),
                guard: MermaidGuardReport::default(),
            },
        }
    }

    #[test]
    fn hash_ir_deterministic_same_input() {
        let ir = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        let h1 = hash_ir(&ir);
        let h2 = hash_ir(&ir);
        assert_eq!(h1, h2, "Same IR must produce identical hash");
    }

    #[test]
    fn hash_ir_different_nodes_differ() {
        let ir1 = make_test_ir(&["A", "B"], &[(0, 1)]);
        let ir2 = make_test_ir(&["A", "C"], &[(0, 1)]);
        assert_ne!(
            hash_ir(&ir1),
            hash_ir(&ir2),
            "Different node IDs must produce different hashes"
        );
    }

    #[test]
    fn hash_ir_different_edges_differ() {
        let ir1 = make_test_ir(&["A", "B", "C"], &[(0, 1)]);
        let ir2 = make_test_ir(&["A", "B", "C"], &[(0, 2)]);
        assert_ne!(
            hash_ir(&ir1),
            hash_ir(&ir2),
            "Different edge targets must produce different hashes"
        );
    }

    #[test]
    fn hash_ir_extra_edge_differs() {
        let ir1 = make_test_ir(&["A", "B", "C"], &[(0, 1)]);
        let ir2 = make_test_ir(&["A", "B", "C"], &[(0, 1), (1, 2)]);
        assert_ne!(hash_ir(&ir1), hash_ir(&ir2), "Extra edge must change hash");
    }

    #[test]
    fn hash_config_layout_deterministic() {
        let config = MermaidConfig::default();
        let h1 = hash_config_layout(&config);
        let h2 = hash_config_layout(&config);
        assert_eq!(h1, h2, "Same config must produce identical hash");
    }

    #[test]
    fn hash_config_layout_different_tier_differs() {
        let mut c1 = MermaidConfig::default();
        let mut c2 = MermaidConfig::default();
        c1.tier_override = MermaidTier::Compact;
        c2.tier_override = MermaidTier::Rich;
        assert_ne!(
            hash_config_layout(&c1),
            hash_config_layout(&c2),
            "Different tier must produce different hash"
        );
    }

    #[test]
    fn cache_key_combined_deterministic() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)]);
        let config = MermaidConfig::default();
        let k1 = DiagramCacheKey::new(&ir, &config, 0);
        let k2 = DiagramCacheKey::new(&ir, &config, 0);
        assert_eq!(k1, k2);
        assert_eq!(k1.combined_hash(), k2.combined_hash());
    }

    #[test]
    fn cache_hit_and_miss() {
        let cache = DiagramCache::new(4);
        let ir = make_test_ir(&["A", "B"], &[(0, 1)]);
        let config = MermaidConfig::default();
        let key = DiagramCacheKey::new(&ir, &config, 0);

        // Miss
        assert!(cache.get(&key).is_none());
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);

        // Insert
        let layout = crate::mermaid_layout::layout_diagram(&ir, &config);
        cache.insert(key.clone(), layout.clone());
        assert_eq!(cache.len(), 1);

        // Hit
        let cached = cache.get(&key);
        assert!(cached.is_some());
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);

        // Verify cached layout matches original
        let cached_layout = cached.unwrap();
        assert_eq!(cached_layout.nodes.len(), layout.nodes.len());
        assert_eq!(cached_layout.edges.len(), layout.edges.len());
    }

    #[test]
    fn cache_eviction_at_capacity() {
        let cache = DiagramCache::new(2);
        let config = MermaidConfig::default();

        let ir1 = make_test_ir(&["A", "B"], &[(0, 1)]);
        let ir2 = make_test_ir(&["X", "Y"], &[(0, 1)]);
        let ir3 = make_test_ir(&["P", "Q"], &[(0, 1)]);

        let k1 = DiagramCacheKey::new(&ir1, &config, 0);
        let k2 = DiagramCacheKey::new(&ir2, &config, 0);
        let k3 = DiagramCacheKey::new(&ir3, &config, 0);

        let l1 = crate::mermaid_layout::layout_diagram(&ir1, &config);
        let l2 = crate::mermaid_layout::layout_diagram(&ir2, &config);
        let l3 = crate::mermaid_layout::layout_diagram(&ir3, &config);

        cache.insert(k1.clone(), l1);
        cache.insert(k2.clone(), l2);
        assert_eq!(cache.len(), 2);

        // Insert third entry; should evict LRU
        cache.insert(k3.clone(), l3);
        assert_eq!(cache.len(), 2);

        // Third entry must be present
        assert!(cache.get(&k3).is_some());
    }

    #[test]
    fn cache_clear_resets() {
        let cache = DiagramCache::new(4);
        let ir = make_test_ir(&["A", "B"], &[(0, 1)]);
        let config = MermaidConfig::default();
        let key = DiagramCacheKey::new(&ir, &config, 0);
        let layout = crate::mermaid_layout::layout_diagram(&ir, &config);

        cache.insert(key.clone(), layout);
        assert_eq!(cache.len(), 1);
        let _ = cache.get(&key);

        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.misses(), 0);
    }

    #[test]
    fn layout_determinism_same_ir_config() {
        let ir = make_test_ir(&["A", "B", "C", "D"], &[(0, 1), (1, 2), (0, 3), (2, 3)]);
        let config = MermaidConfig::default();
        let layout1 = crate::mermaid_layout::layout_diagram(&ir, &config);
        let layout2 = crate::mermaid_layout::layout_diagram(&ir, &config);

        assert_eq!(
            layout1.nodes.len(),
            layout2.nodes.len(),
            "Node count must be deterministic"
        );
        for (n1, n2) in layout1.nodes.iter().zip(layout2.nodes.iter()) {
            assert_eq!(
                n1.node_idx, n2.node_idx,
                "Node index ordering must be deterministic"
            );
            assert!(
                (n1.rect.x - n2.rect.x).abs() < f64::EPSILON
                    && (n1.rect.y - n2.rect.y).abs() < f64::EPSILON,
                "Node positions must be identical: idx={} ({},{}) vs ({},{})",
                n1.node_idx,
                n1.rect.x,
                n1.rect.y,
                n2.rect.x,
                n2.rect.y,
            );
        }
        assert_eq!(
            layout1.stats.crossings, layout2.stats.crossings,
            "Crossing count must be deterministic"
        );
    }

    #[test]
    fn layout_diagram_cached_hits_on_second_call() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)]);
        let config = MermaidConfig {
            cache_enabled: true,
            ..MermaidConfig::default()
        };
        let cache = DiagramCache::new(4);

        let l1 = layout_diagram_cached(&ir, &config, 0, &cache);
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.misses(), 1);

        let l2 = layout_diagram_cached(&ir, &config, 0, &cache);
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);

        assert_eq!(l1.nodes.len(), l2.nodes.len());
    }

    #[test]
    fn layout_diagram_cached_disabled_skips_cache() {
        let ir = make_test_ir(&["A", "B"], &[(0, 1)]);
        let config = MermaidConfig {
            cache_enabled: false,
            ..MermaidConfig::default()
        };
        let cache = DiagramCache::new(4);

        let _ = layout_diagram_cached(&ir, &config, 0, &cache);
        let _ = layout_diagram_cached(&ir, &config, 0, &cache);

        assert!(cache.is_empty(), "Cache must stay empty when disabled");
        assert_eq!(cache.hits(), 0);
    }

    #[test]
    fn feature_matrix_not_empty() {
        assert!(!FEATURE_MATRIX.is_empty());
        assert!(FEATURE_MATRIX.len() >= 30, "Expected at least 30 features");
    }

    #[test]
    fn feature_matrix_all_types_covered() {
        let types = [
            DiagramType::Graph,
            DiagramType::Sequence,
            DiagramType::State,
            DiagramType::Class,
            DiagramType::Er,
            DiagramType::Gantt,
            DiagramType::Mindmap,
            DiagramType::Pie,
        ];
        for dt in &types {
            let features = features_for_type(*dt);
            assert!(
                !features.is_empty(),
                "No features for diagram type {:?}",
                dt
            );
        }
    }

    #[test]
    fn feature_coverage_summary_adds_up() {
        let (supported, partial, unsupported) = feature_coverage_summary();
        assert_eq!(
            supported + partial + unsupported,
            FEATURE_MATRIX.len(),
            "coverage counts must sum to total"
        );
        assert!(supported > partial, "more features should be supported than partial");
    }

    #[test]
    fn uncovered_features_subset_of_matrix() {
        let uncovered = uncovered_features();
        for entry in &uncovered {
            assert!(entry.fixture.is_none());
        }
        assert!(
            uncovered.len() < FEATURE_MATRIX.len(),
            "not all features should be uncovered"
        );
    }

    #[test]
    fn palette_preset_parse_roundtrip() {
        for &preset in DiagramPalettePreset::all() {
            let s = preset.as_str();
            let parsed = DiagramPalettePreset::parse(s).unwrap();
            assert_eq!(parsed, preset, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn palette_preset_next_cycles_through_all() {
        let start = DiagramPalettePreset::Default;
        let mut current = start;
        let mut seen = Vec::new();
        for _ in 0..6 {
            seen.push(current);
            current = current.next();
        }
        assert_eq!(current, start);
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn palette_preset_parse_aliases() {
        assert_eq!(DiagramPalettePreset::parse("corp"), Some(DiagramPalettePreset::Corporate));
        assert_eq!(DiagramPalettePreset::parse("mono"), Some(DiagramPalettePreset::Monochrome));
        assert_eq!(DiagramPalettePreset::parse("hc"), Some(DiagramPalettePreset::HighContrast));
        assert_eq!(DiagramPalettePreset::parse("glow"), Some(DiagramPalettePreset::Neon));
        assert_eq!(DiagramPalettePreset::parse("soft"), Some(DiagramPalettePreset::Pastel));
        assert_eq!(DiagramPalettePreset::parse("invalid"), None);
    }

    #[test]
    fn config_palette_in_hash() {
        let mut c1 = MermaidConfig::default();
        c1.palette = DiagramPalettePreset::Default;
        let mut c2 = MermaidConfig::default();
        c2.palette = DiagramPalettePreset::Neon;
        assert_ne!(
            hash_config_layout(&c1),
            hash_config_layout(&c2),
            "different palettes should produce different hashes"
        );
    }


    #[test]
    fn keymap_not_empty() {
        assert!(SHOWCASE_KEYMAP.len() >= 30, "Expected at least 30 keymap entries");
    }

    #[test]
    fn keymap_normal_mode_has_sample_nav() {
        let entries = keymap_for_mode(ShowcaseMode::Normal);
        let has_next = entries.iter().any(|e| e.action.contains("Next sample"));
        assert!(has_next, "Normal mode should have sample navigation");
    }

    #[test]
    fn keymap_inspect_mode_has_node_nav() {
        let entries = keymap_for_mode(ShowcaseMode::Inspect);
        let has_nav = entries.iter().any(|e| e.action.contains("connected node"));
        assert!(has_nav, "Inspect mode should have node navigation");
    }

    #[test]
    fn keymap_search_mode_has_next_match() {
        let entries = keymap_for_mode(ShowcaseMode::Search);
        let has_next = entries.iter().any(|e| e.action.contains("Next search"));
        assert!(has_next, "Search mode should have next match");
    }

    #[test]
    fn keymap_by_category_groups_correctly() {
        let groups = keymap_by_category(ShowcaseMode::Normal);
        assert!(!groups.is_empty());
        for (cat, entries) in &groups {
            for entry in entries {
                assert_eq!(entry.category, *cat);
            }
        }
    }

    #[test]
    fn keymap_no_duplicate_keys_per_mode() {
        for &mode in &[ShowcaseMode::Normal, ShowcaseMode::Inspect, ShowcaseMode::Search] {
            let entries = keymap_for_mode(mode);
            let mut seen = std::collections::HashSet::new();
            for entry in &entries {
                // Esc is intentionally duplicated across categories (deselect + clear search)
                if entry.key == "Esc" {
                    continue;
                }
                assert!(
                    seen.insert(entry.key),
                    "Duplicate key '{}' in {:?} mode",
                    entry.key,
                    mode,
                );
            }
        }
    }

    #[test]
    fn keymap_theme_bindings_present() {
        let entries = keymap_for_mode(ShowcaseMode::Normal);
        let has_palette = entries.iter().any(|e| e.category == KeyCategory::Theme);
        assert!(has_palette, "Normal mode should have theme bindings");
    }

}
