#![forbid(unsafe_code)]

//! Style types for FrankenTUI with CSS-like cascading semantics.
//!
//! This crate provides:
//! - [`Style`] for unified text styling with CSS-like inheritance
//! - [`StyleSheet`] for named style registration (CSS-like classes)
//! - [`Theme`] for semantic color slots with light/dark mode support
//! - Color types and downgrade utilities

/// Color types, profiles, and downgrade utilities.
pub mod color;
/// Style types with CSS-like cascading semantics.
pub mod style;
/// StyleSheet registry for named styles.
pub mod stylesheet;
/// Table theme types and presets.
pub mod table_theme;
/// Theme system with semantic color slots.
pub mod theme;

pub use color::{
    // Color types
    Ansi16,
    Color,
    ColorCache,
    ColorProfile,
    MonoColor,
    Rgb,
    // WCAG constants
    WCAG_AA_LARGE_TEXT,
    WCAG_AA_NORMAL_TEXT,
    WCAG_AAA_LARGE_TEXT,
    WCAG_AAA_NORMAL_TEXT,
    // WCAG contrast utilities
    best_text_color,
    best_text_color_packed,
    contrast_ratio,
    contrast_ratio_packed,
    meets_wcag_aa,
    meets_wcag_aa_large_text,
    meets_wcag_aa_packed,
    meets_wcag_aaa,
    relative_luminance,
    relative_luminance_packed,
};
pub use style::{Style, StyleFlags};
pub use stylesheet::{StyleId, StyleSheet};
pub use table_theme::{
    BlendMode, Gradient, StyleMask, TableEffect, TableEffectResolver, TableEffectRule,
    TableEffectScope, TableEffectTarget, TablePresetId, TableSection, TableTheme,
};
pub use theme::{AdaptiveColor, ResolvedTheme, Theme, ThemeBuilder};
