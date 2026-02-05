#![forbid(unsafe_code)]

//! Style types for FrankenTUI with CSS-like cascading semantics.
//!
//! # Role in FrankenTUI
//! `ftui-style` is the shared vocabulary for colors and styling. Widgets,
//! render, and extras use these types to stay visually consistent without
//! dragging in rendering or runtime dependencies.
//!
//! # This crate provides
//! - [`Style`] for unified text styling with CSS-like inheritance.
//! - [`StyleSheet`] for named style registration (CSS-like classes).
//! - [`Theme`] for semantic color slots with light/dark mode support.
//! - Color types and downgrade utilities.
//! - Table themes and effects used by widgets and markdown rendering.
//!
//! # How it fits in the system
//! `ftui-render` stores style values in cells, `ftui-widgets` computes styles
//! for UI components, and `ftui-extras` uses themes for richer rendering
//! (markdown, charts, and demo visuals). This crate keeps that style layer
//! deterministic and reusable.

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
    TableThemeDiagnostics, TableThemeSpec,
};
pub use theme::{AdaptiveColor, ResolvedTheme, Theme, ThemeBuilder};

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::{CellAttrs, PackedRgba, StyleFlags as CellFlags};

    #[test]
    fn theme_builder_from_theme_preserves_base_fields() {
        let base = Theme::builder()
            .primary(Color::rgb(10, 20, 30))
            .text(Color::rgb(40, 50, 60))
            .build();

        let updated = ThemeBuilder::from_theme(base.clone())
            .text(Color::rgb(70, 80, 90))
            .build();

        assert_eq!(updated.primary, base.primary);
        assert_eq!(updated.background, base.background);
        assert_eq!(updated.text, AdaptiveColor::from(Color::rgb(70, 80, 90)));
    }

    #[test]
    fn adaptive_color_resolves_by_mode() {
        let adaptive = AdaptiveColor::adaptive(Color::rgb(1, 2, 3), Color::rgb(4, 5, 6));
        assert_eq!(adaptive.resolve(false), Color::rgb(1, 2, 3));
        assert_eq!(adaptive.resolve(true), Color::rgb(4, 5, 6));
    }

    #[test]
    fn packed_rgba_round_trip_channels() {
        let packed = PackedRgba::rgba(12, 34, 56, 78);
        assert_eq!(packed.r(), 12);
        assert_eq!(packed.g(), 34);
        assert_eq!(packed.b(), 56);
        assert_eq!(packed.a(), 78);

        let rgb: Rgb = packed.into();
        assert_eq!(rgb, Rgb::new(12, 34, 56));

        let color: Color = packed.into();
        assert_eq!(color.to_rgb(), Rgb::new(12, 34, 56));
    }

    #[test]
    fn packed_rgba_rgb_defaults_to_opaque() {
        let packed = PackedRgba::rgb(1, 2, 3);
        assert_eq!(packed.a(), 255);
    }

    #[test]
    fn color_profile_defaults_to_ansi16() {
        let profile = ColorProfile::detect_from_env(None, None, None);
        assert_eq!(profile, ColorProfile::Ansi16);
    }

    #[test]
    fn style_flags_round_trip_to_cell_flags() {
        let style_flags = StyleFlags::BOLD
            .union(StyleFlags::ITALIC)
            .union(StyleFlags::UNDERLINE)
            .union(StyleFlags::BLINK);

        let cell_flags: CellFlags = style_flags.into();
        assert!(cell_flags.contains(CellFlags::BOLD));
        assert!(cell_flags.contains(CellFlags::ITALIC));
        assert!(cell_flags.contains(CellFlags::UNDERLINE));
        assert!(cell_flags.contains(CellFlags::BLINK));

        let round_trip = StyleFlags::from(cell_flags);
        assert!(round_trip.contains(StyleFlags::BOLD));
        assert!(round_trip.contains(StyleFlags::ITALIC));
        assert!(round_trip.contains(StyleFlags::UNDERLINE));
        assert!(round_trip.contains(StyleFlags::BLINK));
    }

    #[test]
    fn extended_underlines_map_to_cell_underline() {
        let style_flags = StyleFlags::DOUBLE_UNDERLINE.union(StyleFlags::CURLY_UNDERLINE);
        let cell_flags: CellFlags = style_flags.into();
        assert!(cell_flags.contains(CellFlags::UNDERLINE));
    }

    #[test]
    fn cell_attrs_preserve_link_id_with_flags() {
        let flags = CellFlags::BOLD | CellFlags::ITALIC | CellFlags::UNDERLINE | CellFlags::BLINK;
        let attrs = CellAttrs::new(flags, 4242);
        assert_eq!(attrs.link_id(), 4242);
        assert!(attrs.has_flag(CellFlags::BOLD));
        assert!(attrs.has_flag(CellFlags::ITALIC));
        assert!(attrs.has_flag(CellFlags::UNDERLINE));
        assert!(attrs.has_flag(CellFlags::BLINK));
    }
}
