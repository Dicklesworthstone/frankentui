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

#[derive(Debug, Clone)]
pub struct MermaidError {
    pub message: String,
    pub span: Span,
    pub expected: Option<Vec<&'static str>>,
}

impl MermaidError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            expected: None,
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

/// Mermaid engine configuration (deterministic, env-overridable).
///
/// # Environment Variables
/// - `FTUI_MERMAID_ENABLE` (bool)
/// - `FTUI_MERMAID_GLYPH_MODE` = unicode|ascii
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
#[derive(Debug, Clone)]
pub struct MermaidConfig {
    pub enabled: bool,
    pub glyph_mode: MermaidGlyphMode,
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
}

impl Default for MermaidConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            glyph_mode: MermaidGlyphMode::Unicode,
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
            "Mermaid: {enabled} · {} · {}",
            self.glyph_mode, self.tier_override
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

/// Warning taxonomy for Mermaid compatibility and fallback handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidWarningCode {
    UnsupportedDiagram,
    UnsupportedDirective,
    UnsupportedStyle,
    UnsupportedLink,
    UnsupportedFeature,
    SanitizedInput,
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
            er: MermaidSupportLevel::Partial,
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
            er: MermaidSupportLevel::Partial,
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
            if self.peek_byte() == Some(b'\n') {
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
    let mut directives = Vec::new();
    let mut statements = Vec::new();
    let mut saw_header = false;
    let mut errors = Vec::new();

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
            DiagramType::Graph | DiagramType::State | DiagramType::Class | DiagramType::Er => {
                let er_mode = diagram_type == DiagramType::Er;
                if let Some(edge) = parse_edge(trimmed, span, er_mode) {
                    if let Some(node) = edge_node(trimmed, span, er_mode) {
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
            Statement::Raw { span, .. } => {
                apply_fallback_action(
                    policy.unsupported_feature,
                    MermaidWarningCode::UnsupportedFeature,
                    "unsupported statement",
                    *span,
                    &mut warnings,
                    &mut errors,
                );
            }
            _ => {}
        }
    }

    let init_parse = collect_init_config(ast, config, policy);
    warnings.extend(init_parse.warnings);
    errors.extend(init_parse.errors);

    MermaidValidation { warnings, errors }
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
    let dir_token = rest.split_whitespace().next().unwrap_or("");
    if dir_token.is_empty() {
        return Some(Err(MermaidError::new("direction missing", span)
            .with_expected(vec!["TB", "TD", "LR", "RL", "BT"])));
    }
    let direction = match dir_token.to_ascii_lowercase().as_str() {
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
    let classes: Vec<String> = parts.map(normalize_ws).filter(|s| !s.is_empty()).collect();
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
    let (label, right_id) = split_label(right);
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

fn parse_node(line: &str, span: Span) -> Option<Node> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let (id, label) = parse_node_spec(line)?;
    Some(Node { id, label, span })
}

fn parse_node_spec(text: &str) -> Option<(String, Option<String>)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut id = String::new();
    let mut label = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '[' || c == '(' || c == '{' {
            let closing = match c {
                '[' => ']',
                '(' => ')',
                '{' => '}',
                _ => ']',
            };
            let rest: String = chars.collect();
            if let Some(end) = rest.find(closing) {
                label = Some(normalize_ws(rest[..end].trim()));
            }
            break;
        }
        if c.is_whitespace() {
            break;
        }
        id.push(c);
    }
    if id.is_empty() {
        return None;
    }
    Some((normalize_ws(&id), label))
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
    if let Some(rest) = lower.strip_prefix("title ") {
        return Some(Statement::GanttTitle {
            title: normalize_ws(rest),
            span,
        });
    }
    if let Some(rest) = lower.strip_prefix("section ") {
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
    if let Some(stripped) = trimmed.strip_prefix('|')
        && let Some(end) = stripped.find('|')
    {
        let label = &stripped[..end];
        let rest = stripped[end + 1..].trim();
        return (Some(label), rest);
    }
    if let Some(idx) = trimmed.find(':') {
        let label = trimmed[idx + 1..].trim();
        let rest = trimmed[..idx].trim();
        return (Some(label), rest);
    }
    (None, trimmed)
}

fn parse_node_id(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (id, _) = parse_node_spec(text)?;
    Some(id)
}

fn find_arrow(line: &str) -> Option<(usize, usize, &str)> {
    find_arrow_with(line, is_arrow_char)
}

fn find_er_arrow(line: &str) -> Option<(usize, usize, &str)> {
    find_arrow_with(line, is_er_arrow_char)
}

fn find_arrow_with(line: &str, is_arrow: fn(char) -> bool) -> Option<(usize, usize, &str)> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if is_arrow(chars[i]) {
            let start = i;
            let mut j = i + 1;
            while j < chars.len() && is_arrow(chars[j]) {
                j += 1;
            }
            if j - start >= 2 {
                let start_byte = line.char_indices().nth(start).map(|(idx, _)| idx)?;
                let end_byte = if j >= chars.len() {
                    line.len()
                } else {
                    line.char_indices().nth(j).map(|(idx, _)| idx)?
                };
                let arrow = &line[start_byte..end_byte];
                return Some((start_byte, end_byte, arrow));
            }
            i = j;
        } else {
            i += 1;
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
        env.insert(ENV_MERMAID_TIER, "mega");

        let parsed = from_env_with(|key| env.get(key).map(|value| value.to_string()));
        assert!(parsed.errors.iter().any(|err| err.field == "glyph_mode"));
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
}
