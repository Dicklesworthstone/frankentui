#![forbid(unsafe_code)]

//! Theme system with built-in palettes and dynamic theme selection.
//!
//! This module provides a small set of coherent, high-contrast themes and
//! color tokens that resolve against the current theme at runtime.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::sync::{Mutex, MutexGuard};

// Thread-local flag to track if current thread holds THEME_TEST_LOCK.
// Used for reentrant-style locking in set_theme() when called from within ScopedThemeLock.
thread_local! {
    static THEME_LOCK_HELD: Cell<bool> = const { Cell::new(false) };
}

#[cfg(feature = "syntax")]
use crate::syntax::HighlightTheme;
use ftui_render::cell::PackedRgba;
use ftui_style::Style;

/// Built-in theme identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeId {
    /// Cyberpunk Aurora / Doodlestein Punk (default).
    CyberpunkAurora,
    /// JetBrains Darcula-inspired dark theme.
    Darcula,
    /// Sleek, modern light theme.
    LumenLight,
    /// Nordic-inspired low-contrast dark theme.
    NordicFrost,
    /// Doom (1993) industrial/hell theme.
    Doom,
    /// Quake (1996) gothic/medieval theme.
    Quake,
    /// Classic Monokai.
    Monokai,
    /// Solarized dark.
    SolarizedDark,
    /// Solarized light.
    SolarizedLight,
    /// Gruvbox dark.
    GruvboxDark,
    /// Gruvbox light.
    GruvboxLight,
    /// Atom One Dark.
    OneDark,
    /// Tokyo Night.
    TokyoNight,
    /// Catppuccin Mocha.
    CatppuccinMocha,
    /// Rosé Pine.
    RosePine,
    /// Night Owl.
    NightOwl,
    /// Dracula.
    Dracula,
    /// Material Ocean.
    MaterialOcean,
    /// Ayu Dark.
    AyuDark,
    /// Ayu Light.
    AyuLight,
    /// Kanagawa Wave.
    KanagawaWave,
    /// Everforest Dark.
    EverforestDark,
    /// Everforest Light.
    EverforestLight,
    /// GitHub Dark.
    GitHubDark,
    /// GitHub Light.
    GitHubLight,
    /// Synthwave '84.
    Synthwave84,
    /// Palenight.
    Palenight,
    /// Horizon Dark.
    HorizonDark,
    /// Nord.
    Nord,
    /// One Light.
    OneLight,
    /// Catppuccin Latte.
    CatppuccinLatte,
    /// Catppuccin Frappe.
    CatppuccinFrappe,
    /// Catppuccin Macchiato.
    CatppuccinMacchiato,
    /// Kanagawa Lotus.
    KanagawaLotus,
    /// Nightfox.
    Nightfox,
    /// Dayfox.
    Dayfox,
    /// Oceanic Next.
    OceanicNext,
    /// Cobalt2.
    Cobalt2,
    /// PaperColor Dark.
    PaperColorDark,
    /// PaperColor Light.
    PaperColorLight,
    /// High contrast accessibility theme.
    HighContrast,
}

impl ThemeId {
    pub const ALL: [ThemeId; 41] = [
        ThemeId::CyberpunkAurora,
        ThemeId::Darcula,
        ThemeId::LumenLight,
        ThemeId::NordicFrost,
        ThemeId::Doom,
        ThemeId::Quake,
        ThemeId::Monokai,
        ThemeId::SolarizedDark,
        ThemeId::SolarizedLight,
        ThemeId::GruvboxDark,
        ThemeId::GruvboxLight,
        ThemeId::OneDark,
        ThemeId::TokyoNight,
        ThemeId::CatppuccinMocha,
        ThemeId::RosePine,
        ThemeId::NightOwl,
        ThemeId::Dracula,
        ThemeId::MaterialOcean,
        ThemeId::AyuDark,
        ThemeId::AyuLight,
        ThemeId::KanagawaWave,
        ThemeId::EverforestDark,
        ThemeId::EverforestLight,
        ThemeId::GitHubDark,
        ThemeId::GitHubLight,
        ThemeId::Synthwave84,
        ThemeId::Palenight,
        ThemeId::HorizonDark,
        ThemeId::Nord,
        ThemeId::OneLight,
        ThemeId::CatppuccinLatte,
        ThemeId::CatppuccinFrappe,
        ThemeId::CatppuccinMacchiato,
        ThemeId::KanagawaLotus,
        ThemeId::Nightfox,
        ThemeId::Dayfox,
        ThemeId::OceanicNext,
        ThemeId::Cobalt2,
        ThemeId::PaperColorDark,
        ThemeId::PaperColorLight,
        ThemeId::HighContrast,
    ];

    /// Themes suitable for normal use (excludes accessibility-only themes).
    pub const STANDARD: [ThemeId; 40] = [
        ThemeId::CyberpunkAurora,
        ThemeId::Darcula,
        ThemeId::LumenLight,
        ThemeId::NordicFrost,
        ThemeId::Doom,
        ThemeId::Quake,
        ThemeId::Monokai,
        ThemeId::SolarizedDark,
        ThemeId::SolarizedLight,
        ThemeId::GruvboxDark,
        ThemeId::GruvboxLight,
        ThemeId::OneDark,
        ThemeId::TokyoNight,
        ThemeId::CatppuccinMocha,
        ThemeId::RosePine,
        ThemeId::NightOwl,
        ThemeId::Dracula,
        ThemeId::MaterialOcean,
        ThemeId::AyuDark,
        ThemeId::AyuLight,
        ThemeId::KanagawaWave,
        ThemeId::EverforestDark,
        ThemeId::EverforestLight,
        ThemeId::GitHubDark,
        ThemeId::GitHubLight,
        ThemeId::Synthwave84,
        ThemeId::Palenight,
        ThemeId::HorizonDark,
        ThemeId::Nord,
        ThemeId::OneLight,
        ThemeId::CatppuccinLatte,
        ThemeId::CatppuccinFrappe,
        ThemeId::CatppuccinMacchiato,
        ThemeId::KanagawaLotus,
        ThemeId::Nightfox,
        ThemeId::Dayfox,
        ThemeId::OceanicNext,
        ThemeId::Cobalt2,
        ThemeId::PaperColorDark,
        ThemeId::PaperColorLight,
    ];

    pub const fn index(self) -> usize {
        match self {
            ThemeId::CyberpunkAurora => 0,
            ThemeId::Darcula => 1,
            ThemeId::LumenLight => 2,
            ThemeId::NordicFrost => 3,
            ThemeId::Doom => 4,
            ThemeId::Quake => 5,
            ThemeId::Monokai => 6,
            ThemeId::SolarizedDark => 7,
            ThemeId::SolarizedLight => 8,
            ThemeId::GruvboxDark => 9,
            ThemeId::GruvboxLight => 10,
            ThemeId::OneDark => 11,
            ThemeId::TokyoNight => 12,
            ThemeId::CatppuccinMocha => 13,
            ThemeId::RosePine => 14,
            ThemeId::NightOwl => 15,
            ThemeId::Dracula => 16,
            ThemeId::MaterialOcean => 17,
            ThemeId::AyuDark => 18,
            ThemeId::AyuLight => 19,
            ThemeId::KanagawaWave => 20,
            ThemeId::EverforestDark => 21,
            ThemeId::EverforestLight => 22,
            ThemeId::GitHubDark => 23,
            ThemeId::GitHubLight => 24,
            ThemeId::Synthwave84 => 25,
            ThemeId::Palenight => 26,
            ThemeId::HorizonDark => 27,
            ThemeId::Nord => 28,
            ThemeId::OneLight => 29,
            ThemeId::CatppuccinLatte => 30,
            ThemeId::CatppuccinFrappe => 31,
            ThemeId::CatppuccinMacchiato => 32,
            ThemeId::KanagawaLotus => 33,
            ThemeId::Nightfox => 34,
            ThemeId::Dayfox => 35,
            ThemeId::OceanicNext => 36,
            ThemeId::Cobalt2 => 37,
            ThemeId::PaperColorDark => 38,
            ThemeId::PaperColorLight => 39,
            ThemeId::HighContrast => 40,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            ThemeId::CyberpunkAurora => "Cyberpunk Aurora",
            ThemeId::Darcula => "Darcula",
            ThemeId::LumenLight => "Lumen Light",
            ThemeId::NordicFrost => "Nordic Frost",
            ThemeId::Doom => "Doom",
            ThemeId::Quake => "Quake",
            ThemeId::Monokai => "Monokai",
            ThemeId::SolarizedDark => "Solarized Dark",
            ThemeId::SolarizedLight => "Solarized Light",
            ThemeId::GruvboxDark => "Gruvbox Dark",
            ThemeId::GruvboxLight => "Gruvbox Light",
            ThemeId::OneDark => "One Dark",
            ThemeId::TokyoNight => "Tokyo Night",
            ThemeId::CatppuccinMocha => "Catppuccin Mocha",
            ThemeId::RosePine => "Rose Pine",
            ThemeId::NightOwl => "Night Owl",
            ThemeId::Dracula => "Dracula",
            ThemeId::MaterialOcean => "Material Ocean",
            ThemeId::AyuDark => "Ayu Dark",
            ThemeId::AyuLight => "Ayu Light",
            ThemeId::KanagawaWave => "Kanagawa Wave",
            ThemeId::EverforestDark => "Everforest Dark",
            ThemeId::EverforestLight => "Everforest Light",
            ThemeId::GitHubDark => "GitHub Dark",
            ThemeId::GitHubLight => "GitHub Light",
            ThemeId::Synthwave84 => "Synthwave '84",
            ThemeId::Palenight => "Palenight",
            ThemeId::HorizonDark => "Horizon Dark",
            ThemeId::Nord => "Nord",
            ThemeId::OneLight => "One Light",
            ThemeId::CatppuccinLatte => "Catppuccin Latte",
            ThemeId::CatppuccinFrappe => "Catppuccin Frappe",
            ThemeId::CatppuccinMacchiato => "Catppuccin Macchiato",
            ThemeId::KanagawaLotus => "Kanagawa Lotus",
            ThemeId::Nightfox => "Nightfox",
            ThemeId::Dayfox => "Dayfox",
            ThemeId::OceanicNext => "Oceanic Next",
            ThemeId::Cobalt2 => "Cobalt2",
            ThemeId::PaperColorDark => "PaperColor Dark",
            ThemeId::PaperColorLight => "PaperColor Light",
            ThemeId::HighContrast => "High Contrast",
        }
    }

    pub const fn next(self) -> Self {
        let idx = (self.index() + 1) % Self::ALL.len();
        Self::ALL[idx]
    }

    /// Get the next theme in the standard rotation (skipping accessibility-only themes).
    pub const fn next_non_accessibility(self) -> Self {
        let current_standard_idx = match self {
            ThemeId::HighContrast => 0,
            _ => self.index(),
        };
        let next_idx = (current_standard_idx + 1) % Self::STANDARD.len();
        Self::STANDARD[next_idx]
    }

    pub const fn from_index(idx: usize) -> Self {
        Self::ALL[idx % Self::ALL.len()]
    }
}

/// Theme palette with semantic slots used throughout the UI.
#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub bg_deep: PackedRgba,
    pub bg_base: PackedRgba,
    pub bg_surface: PackedRgba,
    pub bg_overlay: PackedRgba,
    pub bg_highlight: PackedRgba,
    pub fg_primary: PackedRgba,
    pub fg_secondary: PackedRgba,
    pub fg_muted: PackedRgba,
    pub fg_disabled: PackedRgba,
    pub accent_primary: PackedRgba,
    pub accent_secondary: PackedRgba,
    pub accent_success: PackedRgba,
    pub accent_warning: PackedRgba,
    pub accent_error: PackedRgba,
    pub accent_info: PackedRgba,
    pub accent_link: PackedRgba,
    pub accent_slots: [PackedRgba; 12],
    pub syntax_keyword: PackedRgba,
    pub syntax_string: PackedRgba,
    pub syntax_number: PackedRgba,
    pub syntax_comment: PackedRgba,
    pub syntax_function: PackedRgba,
    pub syntax_type: PackedRgba,
    pub syntax_operator: PackedRgba,
    pub syntax_punctuation: PackedRgba,
}

const THEMES: [ThemePalette; 41] = [
    ThemePalette {
        bg_deep: PackedRgba::rgb(10, 14, 20),
        bg_base: PackedRgba::rgb(26, 31, 41),
        bg_surface: PackedRgba::rgb(30, 36, 48),
        bg_overlay: PackedRgba::rgb(45, 55, 70),
        bg_highlight: PackedRgba::rgb(61, 79, 95),
        fg_primary: PackedRgba::rgb(179, 244, 255),
        fg_secondary: PackedRgba::rgb(199, 213, 224),
        fg_muted: PackedRgba::rgb(140, 160, 180),
        fg_disabled: PackedRgba::rgb(61, 79, 95),
        accent_primary: PackedRgba::rgb(0, 170, 255),
        accent_secondary: PackedRgba::rgb(255, 0, 255),
        accent_success: PackedRgba::rgb(57, 255, 180),
        accent_warning: PackedRgba::rgb(255, 229, 102),
        accent_error: PackedRgba::rgb(255, 51, 102),
        accent_info: PackedRgba::rgb(0, 255, 255),
        accent_link: PackedRgba::rgb(102, 204, 255),
        accent_slots: [
            PackedRgba::rgb(0, 170, 255),
            PackedRgba::rgb(255, 0, 255),
            PackedRgba::rgb(57, 255, 180),
            PackedRgba::rgb(255, 229, 102),
            PackedRgba::rgb(255, 51, 102),
            PackedRgba::rgb(0, 255, 255),
            PackedRgba::rgb(102, 204, 255),
            PackedRgba::rgb(255, 107, 157),
            PackedRgba::rgb(107, 255, 205),
            PackedRgba::rgb(255, 239, 153),
            PackedRgba::rgb(102, 255, 255),
            PackedRgba::rgb(255, 102, 255),
        ],
        syntax_keyword: PackedRgba::rgb(255, 102, 255),
        syntax_string: PackedRgba::rgb(57, 255, 180),
        syntax_number: PackedRgba::rgb(255, 229, 102),
        syntax_comment: PackedRgba::rgb(61, 79, 95),
        syntax_function: PackedRgba::rgb(0, 170, 255),
        syntax_type: PackedRgba::rgb(102, 255, 255),
        syntax_operator: PackedRgba::rgb(199, 213, 224),
        syntax_punctuation: PackedRgba::rgb(140, 160, 180),
    },
    ThemePalette {
        bg_deep: PackedRgba::rgb(43, 43, 43),
        bg_base: PackedRgba::rgb(50, 50, 50),
        bg_surface: PackedRgba::rgb(60, 63, 65),
        bg_overlay: PackedRgba::rgb(75, 80, 82),
        bg_highlight: PackedRgba::rgb(90, 96, 98),
        fg_primary: PackedRgba::rgb(169, 183, 198),
        fg_secondary: PackedRgba::rgb(146, 161, 177),
        fg_muted: PackedRgba::rgb(118, 132, 147),
        fg_disabled: PackedRgba::rgb(85, 90, 92),
        accent_primary: PackedRgba::rgb(104, 151, 187),
        accent_secondary: PackedRgba::rgb(152, 118, 170),
        accent_success: PackedRgba::rgb(130, 180, 110),
        accent_warning: PackedRgba::rgb(255, 198, 109),
        accent_error: PackedRgba::rgb(255, 115, 115),
        accent_info: PackedRgba::rgb(179, 212, 252),
        accent_link: PackedRgba::rgb(74, 136, 199),
        accent_slots: [
            PackedRgba::rgb(104, 151, 187),
            PackedRgba::rgb(152, 118, 170),
            PackedRgba::rgb(130, 180, 110),
            PackedRgba::rgb(255, 198, 109),
            PackedRgba::rgb(204, 120, 50),
            PackedRgba::rgb(191, 97, 106),
            PackedRgba::rgb(187, 181, 41),
            PackedRgba::rgb(100, 150, 180),
            PackedRgba::rgb(149, 102, 71),
            PackedRgba::rgb(134, 138, 147),
            PackedRgba::rgb(161, 99, 158),
            PackedRgba::rgb(127, 140, 141),
        ],
        syntax_keyword: PackedRgba::rgb(204, 120, 50),
        syntax_string: PackedRgba::rgb(106, 135, 89),
        syntax_number: PackedRgba::rgb(104, 151, 187),
        syntax_comment: PackedRgba::rgb(128, 128, 128),
        syntax_function: PackedRgba::rgb(255, 198, 109),
        syntax_type: PackedRgba::rgb(152, 118, 170),
        syntax_operator: PackedRgba::rgb(169, 183, 198),
        syntax_punctuation: PackedRgba::rgb(134, 138, 147),
    },
    ThemePalette {
        bg_deep: PackedRgba::rgb(248, 249, 251),
        bg_base: PackedRgba::rgb(238, 241, 245),
        bg_surface: PackedRgba::rgb(230, 235, 241),
        bg_overlay: PackedRgba::rgb(220, 227, 236),
        bg_highlight: PackedRgba::rgb(208, 217, 228),
        fg_primary: PackedRgba::rgb(31, 41, 51),
        fg_secondary: PackedRgba::rgb(62, 76, 89),
        fg_muted: PackedRgba::rgb(123, 135, 148),
        fg_disabled: PackedRgba::rgb(160, 172, 181),
        accent_primary: PackedRgba::rgb(37, 99, 235),
        accent_secondary: PackedRgba::rgb(124, 58, 237),
        accent_success: PackedRgba::rgb(15, 120, 55),
        accent_warning: PackedRgba::rgb(180, 83, 9),
        accent_error: PackedRgba::rgb(185, 28, 28),
        accent_info: PackedRgba::rgb(2, 132, 199),
        accent_link: PackedRgba::rgb(37, 99, 235),
        accent_slots: [
            PackedRgba::rgb(37, 99, 235),
            PackedRgba::rgb(124, 58, 237),
            PackedRgba::rgb(15, 120, 55),
            PackedRgba::rgb(180, 83, 9),
            PackedRgba::rgb(185, 28, 28),
            PackedRgba::rgb(2, 132, 199),
            PackedRgba::rgb(13, 148, 136),
            PackedRgba::rgb(190, 24, 93),
            PackedRgba::rgb(79, 70, 229),
            PackedRgba::rgb(194, 65, 12),
            PackedRgba::rgb(13, 148, 136),
            PackedRgba::rgb(126, 34, 206),
        ],
        syntax_keyword: PackedRgba::rgb(124, 58, 237),
        syntax_string: PackedRgba::rgb(15, 120, 55),
        syntax_number: PackedRgba::rgb(217, 119, 6),
        syntax_comment: PackedRgba::rgb(154, 165, 177),
        syntax_function: PackedRgba::rgb(37, 99, 235),
        syntax_type: PackedRgba::rgb(14, 165, 233),
        syntax_operator: PackedRgba::rgb(71, 85, 105),
        syntax_punctuation: PackedRgba::rgb(100, 116, 139),
    },
    ThemePalette {
        bg_deep: PackedRgba::rgb(46, 52, 64),
        bg_base: PackedRgba::rgb(59, 66, 82),
        bg_surface: PackedRgba::rgb(67, 76, 94),
        bg_overlay: PackedRgba::rgb(76, 86, 106),
        bg_highlight: PackedRgba::rgb(94, 129, 172),
        fg_primary: PackedRgba::rgb(236, 239, 244),
        fg_secondary: PackedRgba::rgb(216, 222, 233),
        fg_muted: PackedRgba::rgb(163, 179, 194),
        fg_disabled: PackedRgba::rgb(123, 135, 148),
        accent_primary: PackedRgba::rgb(136, 192, 208),
        accent_secondary: PackedRgba::rgb(129, 161, 193),
        accent_success: PackedRgba::rgb(163, 190, 140),
        accent_warning: PackedRgba::rgb(235, 203, 139),
        accent_error: PackedRgba::rgb(240, 150, 160),
        accent_info: PackedRgba::rgb(143, 188, 187),
        accent_link: PackedRgba::rgb(136, 192, 208),
        accent_slots: [
            PackedRgba::rgb(136, 192, 208),
            PackedRgba::rgb(129, 161, 193),
            PackedRgba::rgb(163, 190, 140),
            PackedRgba::rgb(235, 203, 139),
            PackedRgba::rgb(240, 150, 160),
            PackedRgba::rgb(143, 188, 187),
            PackedRgba::rgb(180, 142, 173),
            PackedRgba::rgb(94, 129, 172),
            PackedRgba::rgb(208, 135, 112),
            PackedRgba::rgb(229, 233, 240),
            PackedRgba::rgb(216, 222, 233),
            PackedRgba::rgb(143, 188, 187),
        ],
        syntax_keyword: PackedRgba::rgb(129, 161, 193),
        syntax_string: PackedRgba::rgb(163, 190, 140),
        syntax_number: PackedRgba::rgb(180, 142, 173),
        syntax_comment: PackedRgba::rgb(97, 110, 136),
        syntax_function: PackedRgba::rgb(136, 192, 208),
        syntax_type: PackedRgba::rgb(143, 188, 187),
        syntax_operator: PackedRgba::rgb(216, 222, 233),
        syntax_punctuation: PackedRgba::rgb(229, 233, 240),
    },
    // Doom (1993)
    ThemePalette {
        bg_deep: PackedRgba::rgb(26, 26, 26),
        bg_base: PackedRgba::rgb(38, 38, 38),
        bg_surface: PackedRgba::rgb(47, 47, 47),
        bg_overlay: PackedRgba::rgb(64, 64, 64),
        bg_highlight: PackedRgba::rgb(79, 79, 79),
        fg_primary: PackedRgba::rgb(211, 211, 211),
        fg_secondary: PackedRgba::rgb(169, 169, 169),
        fg_muted: PackedRgba::rgb(128, 128, 128),
        fg_disabled: PackedRgba::rgb(80, 80, 80),
        accent_primary: PackedRgba::rgb(178, 34, 34),
        accent_secondary: PackedRgba::rgb(50, 205, 50),
        accent_success: PackedRgba::rgb(50, 205, 50),
        accent_warning: PackedRgba::rgb(255, 215, 0),
        accent_error: PackedRgba::rgb(139, 0, 0),
        accent_info: PackedRgba::rgb(65, 105, 225),
        accent_link: PackedRgba::rgb(30, 144, 255),
        accent_slots: [
            PackedRgba::rgb(178, 34, 34),
            PackedRgba::rgb(50, 205, 50),
            PackedRgba::rgb(255, 255, 0),
            PackedRgba::rgb(255, 215, 0),
            PackedRgba::rgb(139, 0, 0),
            PackedRgba::rgb(65, 105, 225),
            PackedRgba::rgb(255, 69, 0),
            PackedRgba::rgb(173, 255, 47),
            PackedRgba::rgb(255, 105, 180),
            PackedRgba::rgb(0, 206, 209),
            PackedRgba::rgb(148, 0, 211),
            PackedRgba::rgb(124, 252, 0),
        ],
        syntax_keyword: PackedRgba::rgb(255, 215, 0),
        syntax_string: PackedRgba::rgb(50, 205, 50),
        syntax_number: PackedRgba::rgb(255, 69, 0),
        syntax_comment: PackedRgba::rgb(128, 128, 128),
        syntax_function: PackedRgba::rgb(65, 105, 225),
        syntax_type: PackedRgba::rgb(178, 34, 34),
        syntax_operator: PackedRgba::rgb(211, 211, 211),
        syntax_punctuation: PackedRgba::rgb(169, 169, 169),
    },
    // Quake (1996)
    ThemePalette {
        bg_deep: PackedRgba::rgb(28, 28, 28),
        bg_base: PackedRgba::rgb(40, 40, 40),
        bg_surface: PackedRgba::rgb(46, 39, 34),
        bg_overlay: PackedRgba::rgb(62, 54, 48),
        bg_highlight: PackedRgba::rgb(74, 64, 58),
        fg_primary: PackedRgba::rgb(210, 180, 140),
        fg_secondary: PackedRgba::rgb(195, 176, 145),
        fg_muted: PackedRgba::rgb(139, 115, 85),
        fg_disabled: PackedRgba::rgb(92, 64, 51),
        accent_primary: PackedRgba::rgb(139, 69, 19),
        accent_secondary: PackedRgba::rgb(85, 107, 47),
        accent_success: PackedRgba::rgb(85, 107, 47),
        accent_warning: PackedRgba::rgb(210, 105, 30),
        accent_error: PackedRgba::rgb(128, 0, 0),
        accent_info: PackedRgba::rgb(70, 130, 180),
        accent_link: PackedRgba::rgb(135, 206, 235),
        accent_slots: [
            PackedRgba::rgb(139, 69, 19),
            PackedRgba::rgb(85, 107, 47),
            PackedRgba::rgb(205, 133, 63),
            PackedRgba::rgb(210, 105, 30),
            PackedRgba::rgb(128, 0, 0),
            PackedRgba::rgb(70, 130, 180),
            PackedRgba::rgb(160, 82, 45),
            PackedRgba::rgb(107, 142, 35),
            PackedRgba::rgb(222, 184, 135),
            PackedRgba::rgb(95, 158, 160),
            PackedRgba::rgb(139, 0, 139),
            PackedRgba::rgb(189, 183, 107),
        ],
        syntax_keyword: PackedRgba::rgb(210, 105, 30),
        syntax_string: PackedRgba::rgb(85, 107, 47),
        syntax_number: PackedRgba::rgb(205, 133, 63),
        syntax_comment: PackedRgba::rgb(139, 115, 85),
        syntax_function: PackedRgba::rgb(139, 69, 19),
        syntax_type: PackedRgba::rgb(160, 82, 45),
        syntax_operator: PackedRgba::rgb(210, 180, 140),
        syntax_punctuation: PackedRgba::rgb(195, 176, 145),
    },
    // Monokai
    ThemePalette {
        bg_deep: PackedRgba::rgb(30, 31, 28),
        bg_base: PackedRgba::rgb(39, 40, 34),
        bg_surface: PackedRgba::rgb(45, 46, 39),
        bg_overlay: PackedRgba::rgb(58, 59, 52),
        bg_highlight: PackedRgba::rgb(73, 74, 67),
        fg_primary: PackedRgba::rgb(248, 248, 242),
        fg_secondary: PackedRgba::rgb(214, 214, 204),
        fg_muted: PackedRgba::rgb(117, 113, 94),
        fg_disabled: PackedRgba::rgb(90, 87, 72),
        accent_primary: PackedRgba::rgb(102, 217, 239),
        accent_secondary: PackedRgba::rgb(174, 129, 255),
        accent_success: PackedRgba::rgb(166, 226, 46),
        accent_warning: PackedRgba::rgb(230, 219, 116),
        accent_error: PackedRgba::rgb(249, 38, 114),
        accent_info: PackedRgba::rgb(253, 151, 31),
        accent_link: PackedRgba::rgb(102, 217, 239),
        accent_slots: [
            PackedRgba::rgb(102, 217, 239),
            PackedRgba::rgb(174, 129, 255),
            PackedRgba::rgb(166, 226, 46),
            PackedRgba::rgb(230, 219, 116),
            PackedRgba::rgb(249, 38, 114),
            PackedRgba::rgb(253, 151, 31),
            PackedRgba::rgb(120, 220, 232),
            PackedRgba::rgb(255, 102, 153),
            PackedRgba::rgb(191, 255, 96),
            PackedRgba::rgb(255, 216, 102),
            PackedRgba::rgb(187, 154, 247),
            PackedRgba::rgb(255, 176, 84),
        ],
        syntax_keyword: PackedRgba::rgb(249, 38, 114),
        syntax_string: PackedRgba::rgb(230, 219, 116),
        syntax_number: PackedRgba::rgb(174, 129, 255),
        syntax_comment: PackedRgba::rgb(117, 113, 94),
        syntax_function: PackedRgba::rgb(166, 226, 46),
        syntax_type: PackedRgba::rgb(102, 217, 239),
        syntax_operator: PackedRgba::rgb(248, 248, 242),
        syntax_punctuation: PackedRgba::rgb(117, 113, 94),
    },
    // Solarized Dark
    ThemePalette {
        bg_deep: PackedRgba::rgb(0, 43, 54),
        bg_base: PackedRgba::rgb(7, 54, 66),
        bg_surface: PackedRgba::rgb(11, 59, 73),
        bg_overlay: PackedRgba::rgb(20, 75, 91),
        bg_highlight: PackedRgba::rgb(31, 95, 117),
        fg_primary: PackedRgba::rgb(147, 161, 161),
        fg_secondary: PackedRgba::rgb(203, 214, 208),
        fg_muted: PackedRgba::rgb(101, 123, 131),
        fg_disabled: PackedRgba::rgb(88, 110, 117),
        accent_primary: PackedRgba::rgb(38, 139, 210),
        accent_secondary: PackedRgba::rgb(108, 113, 196),
        accent_success: PackedRgba::rgb(133, 153, 0),
        accent_warning: PackedRgba::rgb(181, 137, 0),
        accent_error: PackedRgba::rgb(220, 50, 47),
        accent_info: PackedRgba::rgb(42, 161, 152),
        accent_link: PackedRgba::rgb(38, 139, 210),
        accent_slots: [
            PackedRgba::rgb(38, 139, 210),
            PackedRgba::rgb(108, 113, 196),
            PackedRgba::rgb(133, 153, 0),
            PackedRgba::rgb(181, 137, 0),
            PackedRgba::rgb(220, 50, 47),
            PackedRgba::rgb(42, 161, 152),
            PackedRgba::rgb(203, 75, 22),
            PackedRgba::rgb(211, 54, 130),
            PackedRgba::rgb(147, 161, 161),
            PackedRgba::rgb(238, 232, 213),
            PackedRgba::rgb(147, 161, 161),
            PackedRgba::rgb(42, 161, 152),
        ],
        syntax_keyword: PackedRgba::rgb(108, 113, 196),
        syntax_string: PackedRgba::rgb(42, 161, 152),
        syntax_number: PackedRgba::rgb(211, 54, 130),
        syntax_comment: PackedRgba::rgb(88, 110, 117),
        syntax_function: PackedRgba::rgb(38, 139, 210),
        syntax_type: PackedRgba::rgb(181, 137, 0),
        syntax_operator: PackedRgba::rgb(147, 161, 161),
        syntax_punctuation: PackedRgba::rgb(101, 123, 131),
    },
    // Solarized Light
    ThemePalette {
        bg_deep: PackedRgba::rgb(253, 246, 227),
        bg_base: PackedRgba::rgb(245, 239, 219),
        bg_surface: PackedRgba::rgb(238, 232, 213),
        bg_overlay: PackedRgba::rgb(227, 220, 201),
        bg_highlight: PackedRgba::rgb(216, 207, 184),
        fg_primary: PackedRgba::rgb(88, 110, 117),
        fg_secondary: PackedRgba::rgb(101, 123, 131),
        fg_muted: PackedRgba::rgb(147, 161, 161),
        fg_disabled: PackedRgba::rgb(168, 180, 181),
        accent_primary: PackedRgba::rgb(38, 139, 210),
        accent_secondary: PackedRgba::rgb(108, 113, 196),
        accent_success: PackedRgba::rgb(133, 153, 0),
        accent_warning: PackedRgba::rgb(181, 137, 0),
        accent_error: PackedRgba::rgb(220, 50, 47),
        accent_info: PackedRgba::rgb(42, 161, 152),
        accent_link: PackedRgba::rgb(38, 139, 210),
        accent_slots: [
            PackedRgba::rgb(38, 139, 210),
            PackedRgba::rgb(108, 113, 196),
            PackedRgba::rgb(133, 153, 0),
            PackedRgba::rgb(181, 137, 0),
            PackedRgba::rgb(220, 50, 47),
            PackedRgba::rgb(42, 161, 152),
            PackedRgba::rgb(203, 75, 22),
            PackedRgba::rgb(211, 54, 130),
            PackedRgba::rgb(101, 123, 131),
            PackedRgba::rgb(147, 161, 161),
            PackedRgba::rgb(88, 110, 117),
            PackedRgba::rgb(42, 161, 152),
        ],
        syntax_keyword: PackedRgba::rgb(108, 113, 196),
        syntax_string: PackedRgba::rgb(42, 161, 152),
        syntax_number: PackedRgba::rgb(211, 54, 130),
        syntax_comment: PackedRgba::rgb(147, 161, 161),
        syntax_function: PackedRgba::rgb(38, 139, 210),
        syntax_type: PackedRgba::rgb(181, 137, 0),
        syntax_operator: PackedRgba::rgb(88, 110, 117),
        syntax_punctuation: PackedRgba::rgb(101, 123, 131),
    },
    // Gruvbox Dark
    ThemePalette {
        bg_deep: PackedRgba::rgb(29, 32, 33),
        bg_base: PackedRgba::rgb(40, 40, 40),
        bg_surface: PackedRgba::rgb(50, 48, 47),
        bg_overlay: PackedRgba::rgb(60, 56, 54),
        bg_highlight: PackedRgba::rgb(80, 73, 69),
        fg_primary: PackedRgba::rgb(235, 219, 178),
        fg_secondary: PackedRgba::rgb(213, 196, 161),
        fg_muted: PackedRgba::rgb(168, 153, 132),
        fg_disabled: PackedRgba::rgb(124, 111, 100),
        accent_primary: PackedRgba::rgb(131, 165, 152),
        accent_secondary: PackedRgba::rgb(211, 134, 155),
        accent_success: PackedRgba::rgb(184, 187, 38),
        accent_warning: PackedRgba::rgb(250, 189, 47),
        accent_error: PackedRgba::rgb(251, 73, 52),
        accent_info: PackedRgba::rgb(142, 192, 124),
        accent_link: PackedRgba::rgb(69, 133, 136),
        accent_slots: [
            PackedRgba::rgb(131, 165, 152),
            PackedRgba::rgb(211, 134, 155),
            PackedRgba::rgb(184, 187, 38),
            PackedRgba::rgb(250, 189, 47),
            PackedRgba::rgb(251, 73, 52),
            PackedRgba::rgb(142, 192, 124),
            PackedRgba::rgb(254, 128, 25),
            PackedRgba::rgb(177, 98, 134),
            PackedRgba::rgb(104, 157, 106),
            PackedRgba::rgb(215, 153, 33),
            PackedRgba::rgb(69, 133, 136),
            PackedRgba::rgb(146, 131, 116),
        ],
        syntax_keyword: PackedRgba::rgb(251, 73, 52),
        syntax_string: PackedRgba::rgb(184, 187, 38),
        syntax_number: PackedRgba::rgb(211, 134, 155),
        syntax_comment: PackedRgba::rgb(146, 131, 116),
        syntax_function: PackedRgba::rgb(250, 189, 47),
        syntax_type: PackedRgba::rgb(142, 192, 124),
        syntax_operator: PackedRgba::rgb(235, 219, 178),
        syntax_punctuation: PackedRgba::rgb(168, 153, 132),
    },
    // Gruvbox Light
    ThemePalette {
        bg_deep: PackedRgba::rgb(249, 245, 215),
        bg_base: PackedRgba::rgb(251, 241, 199),
        bg_surface: PackedRgba::rgb(242, 229, 188),
        bg_overlay: PackedRgba::rgb(235, 219, 178),
        bg_highlight: PackedRgba::rgb(213, 196, 161),
        fg_primary: PackedRgba::rgb(60, 56, 54),
        fg_secondary: PackedRgba::rgb(80, 73, 69),
        fg_muted: PackedRgba::rgb(124, 111, 100),
        fg_disabled: PackedRgba::rgb(168, 153, 132),
        accent_primary: PackedRgba::rgb(7, 102, 120),
        accent_secondary: PackedRgba::rgb(143, 63, 113),
        accent_success: PackedRgba::rgb(121, 116, 14),
        accent_warning: PackedRgba::rgb(181, 118, 20),
        accent_error: PackedRgba::rgb(204, 36, 29),
        accent_info: PackedRgba::rgb(66, 123, 88),
        accent_link: PackedRgba::rgb(7, 102, 120),
        accent_slots: [
            PackedRgba::rgb(7, 102, 120),
            PackedRgba::rgb(143, 63, 113),
            PackedRgba::rgb(121, 116, 14),
            PackedRgba::rgb(181, 118, 20),
            PackedRgba::rgb(204, 36, 29),
            PackedRgba::rgb(66, 123, 88),
            PackedRgba::rgb(175, 58, 3),
            PackedRgba::rgb(121, 36, 86),
            PackedRgba::rgb(152, 151, 26),
            PackedRgba::rgb(214, 93, 14),
            PackedRgba::rgb(62, 104, 98),
            PackedRgba::rgb(104, 157, 106),
        ],
        syntax_keyword: PackedRgba::rgb(204, 36, 29),
        syntax_string: PackedRgba::rgb(121, 116, 14),
        syntax_number: PackedRgba::rgb(143, 63, 113),
        syntax_comment: PackedRgba::rgb(146, 131, 116),
        syntax_function: PackedRgba::rgb(7, 102, 120),
        syntax_type: PackedRgba::rgb(181, 118, 20),
        syntax_operator: PackedRgba::rgb(60, 56, 54),
        syntax_punctuation: PackedRgba::rgb(124, 111, 100),
    },
    // One Dark
    ThemePalette {
        bg_deep: PackedRgba::rgb(30, 33, 39),
        bg_base: PackedRgba::rgb(40, 44, 52),
        bg_surface: PackedRgba::rgb(47, 52, 63),
        bg_overlay: PackedRgba::rgb(58, 64, 75),
        bg_highlight: PackedRgba::rgb(75, 82, 99),
        fg_primary: PackedRgba::rgb(171, 178, 191),
        fg_secondary: PackedRgba::rgb(200, 204, 212),
        fg_muted: PackedRgba::rgb(127, 132, 142),
        fg_disabled: PackedRgba::rgb(92, 99, 112),
        accent_primary: PackedRgba::rgb(97, 175, 239),
        accent_secondary: PackedRgba::rgb(198, 120, 221),
        accent_success: PackedRgba::rgb(152, 195, 121),
        accent_warning: PackedRgba::rgb(229, 192, 123),
        accent_error: PackedRgba::rgb(224, 108, 117),
        accent_info: PackedRgba::rgb(86, 182, 194),
        accent_link: PackedRgba::rgb(97, 175, 239),
        accent_slots: [
            PackedRgba::rgb(97, 175, 239),
            PackedRgba::rgb(198, 120, 221),
            PackedRgba::rgb(152, 195, 121),
            PackedRgba::rgb(229, 192, 123),
            PackedRgba::rgb(224, 108, 117),
            PackedRgba::rgb(86, 182, 194),
            PackedRgba::rgb(209, 154, 102),
            PackedRgba::rgb(190, 140, 255),
            PackedRgba::rgb(130, 170, 255),
            PackedRgba::rgb(114, 156, 31),
            PackedRgba::rgb(56, 169, 255),
            PackedRgba::rgb(255, 164, 96),
        ],
        syntax_keyword: PackedRgba::rgb(198, 120, 221),
        syntax_string: PackedRgba::rgb(152, 195, 121),
        syntax_number: PackedRgba::rgb(209, 154, 102),
        syntax_comment: PackedRgba::rgb(92, 99, 112),
        syntax_function: PackedRgba::rgb(97, 175, 239),
        syntax_type: PackedRgba::rgb(229, 192, 123),
        syntax_operator: PackedRgba::rgb(171, 178, 191),
        syntax_punctuation: PackedRgba::rgb(127, 132, 142),
    },
    // Tokyo Night
    ThemePalette {
        bg_deep: PackedRgba::rgb(22, 22, 30),
        bg_base: PackedRgba::rgb(26, 27, 38),
        bg_surface: PackedRgba::rgb(36, 40, 59),
        bg_overlay: PackedRgba::rgb(47, 53, 73),
        bg_highlight: PackedRgba::rgb(59, 66, 97),
        fg_primary: PackedRgba::rgb(192, 202, 245),
        fg_secondary: PackedRgba::rgb(169, 177, 214),
        fg_muted: PackedRgba::rgb(115, 122, 162),
        fg_disabled: PackedRgba::rgb(86, 95, 137),
        accent_primary: PackedRgba::rgb(122, 162, 247),
        accent_secondary: PackedRgba::rgb(187, 154, 247),
        accent_success: PackedRgba::rgb(158, 206, 106),
        accent_warning: PackedRgba::rgb(224, 175, 104),
        accent_error: PackedRgba::rgb(247, 118, 142),
        accent_info: PackedRgba::rgb(125, 207, 255),
        accent_link: PackedRgba::rgb(122, 162, 247),
        accent_slots: [
            PackedRgba::rgb(122, 162, 247),
            PackedRgba::rgb(187, 154, 247),
            PackedRgba::rgb(158, 206, 106),
            PackedRgba::rgb(224, 175, 104),
            PackedRgba::rgb(247, 118, 142),
            PackedRgba::rgb(125, 207, 255),
            PackedRgba::rgb(255, 158, 100),
            PackedRgba::rgb(42, 195, 222),
            PackedRgba::rgb(187, 196, 255),
            PackedRgba::rgb(192, 132, 252),
            PackedRgba::rgb(137, 220, 235),
            PackedRgba::rgb(158, 206, 106),
        ],
        syntax_keyword: PackedRgba::rgb(187, 154, 247),
        syntax_string: PackedRgba::rgb(158, 206, 106),
        syntax_number: PackedRgba::rgb(255, 158, 100),
        syntax_comment: PackedRgba::rgb(86, 95, 137),
        syntax_function: PackedRgba::rgb(122, 162, 247),
        syntax_type: PackedRgba::rgb(42, 195, 222),
        syntax_operator: PackedRgba::rgb(192, 202, 245),
        syntax_punctuation: PackedRgba::rgb(115, 122, 162),
    },
    // Catppuccin Mocha
    ThemePalette {
        bg_deep: PackedRgba::rgb(17, 17, 27),
        bg_base: PackedRgba::rgb(30, 30, 46),
        bg_surface: PackedRgba::rgb(49, 50, 68),
        bg_overlay: PackedRgba::rgb(69, 71, 90),
        bg_highlight: PackedRgba::rgb(88, 91, 112),
        fg_primary: PackedRgba::rgb(205, 214, 244),
        fg_secondary: PackedRgba::rgb(186, 194, 222),
        fg_muted: PackedRgba::rgb(127, 132, 156),
        fg_disabled: PackedRgba::rgb(108, 112, 134),
        accent_primary: PackedRgba::rgb(137, 180, 250),
        accent_secondary: PackedRgba::rgb(203, 166, 247),
        accent_success: PackedRgba::rgb(166, 227, 161),
        accent_warning: PackedRgba::rgb(249, 226, 175),
        accent_error: PackedRgba::rgb(243, 139, 168),
        accent_info: PackedRgba::rgb(137, 220, 235),
        accent_link: PackedRgba::rgb(116, 199, 236),
        accent_slots: [
            PackedRgba::rgb(137, 180, 250),
            PackedRgba::rgb(203, 166, 247),
            PackedRgba::rgb(166, 227, 161),
            PackedRgba::rgb(249, 226, 175),
            PackedRgba::rgb(243, 139, 168),
            PackedRgba::rgb(137, 220, 235),
            PackedRgba::rgb(250, 179, 135),
            PackedRgba::rgb(148, 226, 213),
            PackedRgba::rgb(116, 199, 236),
            PackedRgba::rgb(245, 194, 231),
            PackedRgba::rgb(205, 214, 244),
            PackedRgba::rgb(181, 232, 224),
        ],
        syntax_keyword: PackedRgba::rgb(203, 166, 247),
        syntax_string: PackedRgba::rgb(166, 227, 161),
        syntax_number: PackedRgba::rgb(250, 179, 135),
        syntax_comment: PackedRgba::rgb(108, 112, 134),
        syntax_function: PackedRgba::rgb(137, 180, 250),
        syntax_type: PackedRgba::rgb(148, 226, 213),
        syntax_operator: PackedRgba::rgb(205, 214, 244),
        syntax_punctuation: PackedRgba::rgb(127, 132, 156),
    },
    // Rose Pine
    ThemePalette {
        bg_deep: PackedRgba::rgb(25, 23, 36),
        bg_base: PackedRgba::rgb(31, 29, 46),
        bg_surface: PackedRgba::rgb(38, 35, 58),
        bg_overlay: PackedRgba::rgb(49, 47, 68),
        bg_highlight: PackedRgba::rgb(64, 61, 82),
        fg_primary: PackedRgba::rgb(224, 222, 244),
        fg_secondary: PackedRgba::rgb(144, 140, 170),
        fg_muted: PackedRgba::rgb(110, 106, 134),
        fg_disabled: PackedRgba::rgb(82, 79, 103),
        accent_primary: PackedRgba::rgb(156, 207, 216),
        accent_secondary: PackedRgba::rgb(196, 167, 231),
        accent_success: PackedRgba::rgb(49, 116, 143),
        accent_warning: PackedRgba::rgb(246, 193, 119),
        accent_error: PackedRgba::rgb(235, 111, 146),
        accent_info: PackedRgba::rgb(156, 207, 216),
        accent_link: PackedRgba::rgb(196, 167, 231),
        accent_slots: [
            PackedRgba::rgb(156, 207, 216),
            PackedRgba::rgb(196, 167, 231),
            PackedRgba::rgb(49, 116, 143),
            PackedRgba::rgb(246, 193, 119),
            PackedRgba::rgb(235, 111, 146),
            PackedRgba::rgb(156, 207, 216),
            PackedRgba::rgb(235, 188, 186),
            PackedRgba::rgb(86, 82, 110),
            PackedRgba::rgb(224, 222, 244),
            PackedRgba::rgb(49, 116, 143),
            PackedRgba::rgb(196, 167, 231),
            PackedRgba::rgb(246, 193, 119),
        ],
        syntax_keyword: PackedRgba::rgb(196, 167, 231),
        syntax_string: PackedRgba::rgb(156, 207, 216),
        syntax_number: PackedRgba::rgb(246, 193, 119),
        syntax_comment: PackedRgba::rgb(82, 79, 103),
        syntax_function: PackedRgba::rgb(235, 188, 186),
        syntax_type: PackedRgba::rgb(49, 116, 143),
        syntax_operator: PackedRgba::rgb(224, 222, 244),
        syntax_punctuation: PackedRgba::rgb(110, 106, 134),
    },
    // Night Owl
    ThemePalette {
        bg_deep: PackedRgba::rgb(1, 22, 39),
        bg_base: PackedRgba::rgb(1, 22, 39),
        bg_surface: PackedRgba::rgb(11, 34, 57),
        bg_overlay: PackedRgba::rgb(18, 48, 79),
        bg_highlight: PackedRgba::rgb(29, 59, 83),
        fg_primary: PackedRgba::rgb(214, 222, 235),
        fg_secondary: PackedRgba::rgb(167, 173, 186),
        fg_muted: PackedRgba::rgb(99, 119, 119),
        fg_disabled: PackedRgba::rgb(75, 100, 121),
        accent_primary: PackedRgba::rgb(130, 170, 255),
        accent_secondary: PackedRgba::rgb(199, 146, 234),
        accent_success: PackedRgba::rgb(173, 219, 103),
        accent_warning: PackedRgba::rgb(236, 196, 141),
        accent_error: PackedRgba::rgb(255, 88, 116),
        accent_info: PackedRgba::rgb(127, 219, 202),
        accent_link: PackedRgba::rgb(130, 170, 255),
        accent_slots: [
            PackedRgba::rgb(130, 170, 255),
            PackedRgba::rgb(199, 146, 234),
            PackedRgba::rgb(173, 219, 103),
            PackedRgba::rgb(236, 196, 141),
            PackedRgba::rgb(255, 88, 116),
            PackedRgba::rgb(127, 219, 202),
            PackedRgba::rgb(247, 140, 108),
            PackedRgba::rgb(130, 170, 255),
            PackedRgba::rgb(193, 132, 252),
            PackedRgba::rgb(99, 187, 227),
            PackedRgba::rgb(173, 219, 103),
            PackedRgba::rgb(255, 203, 107),
        ],
        syntax_keyword: PackedRgba::rgb(199, 146, 234),
        syntax_string: PackedRgba::rgb(236, 196, 141),
        syntax_number: PackedRgba::rgb(247, 140, 108),
        syntax_comment: PackedRgba::rgb(99, 119, 119),
        syntax_function: PackedRgba::rgb(130, 170, 255),
        syntax_type: PackedRgba::rgb(127, 219, 202),
        syntax_operator: PackedRgba::rgb(214, 222, 235),
        syntax_punctuation: PackedRgba::rgb(99, 119, 119),
    },
    // Dracula
    ThemePalette {
        bg_deep: PackedRgba::rgb(18, 18, 26),
        bg_base: PackedRgba::rgb(40, 42, 54),
        bg_surface: PackedRgba::rgb(48, 51, 67),
        bg_overlay: PackedRgba::rgb(68, 71, 90),
        bg_highlight: PackedRgba::rgb(98, 114, 164),
        fg_primary: PackedRgba::rgb(248, 248, 242),
        fg_secondary: PackedRgba::rgb(224, 224, 222),
        fg_muted: PackedRgba::rgb(160, 164, 188),
        fg_disabled: PackedRgba::rgb(98, 114, 164),
        accent_primary: PackedRgba::rgb(139, 233, 253),
        accent_secondary: PackedRgba::rgb(189, 147, 249),
        accent_success: PackedRgba::rgb(80, 250, 123),
        accent_warning: PackedRgba::rgb(241, 250, 140),
        accent_error: PackedRgba::rgb(255, 85, 85),
        accent_info: PackedRgba::rgb(255, 184, 108),
        accent_link: PackedRgba::rgb(139, 233, 253),
        accent_slots: [
            PackedRgba::rgb(139, 233, 253),
            PackedRgba::rgb(189, 147, 249),
            PackedRgba::rgb(80, 250, 123),
            PackedRgba::rgb(241, 250, 140),
            PackedRgba::rgb(255, 85, 85),
            PackedRgba::rgb(255, 184, 108),
            PackedRgba::rgb(255, 121, 198),
            PackedRgba::rgb(98, 114, 164),
            PackedRgba::rgb(189, 147, 249),
            PackedRgba::rgb(139, 233, 253),
            PackedRgba::rgb(80, 250, 123),
            PackedRgba::rgb(255, 121, 198),
        ],
        syntax_keyword: PackedRgba::rgb(255, 121, 198),
        syntax_string: PackedRgba::rgb(241, 250, 140),
        syntax_number: PackedRgba::rgb(189, 147, 249),
        syntax_comment: PackedRgba::rgb(98, 114, 164),
        syntax_function: PackedRgba::rgb(80, 250, 123),
        syntax_type: PackedRgba::rgb(139, 233, 253),
        syntax_operator: PackedRgba::rgb(248, 248, 242),
        syntax_punctuation: PackedRgba::rgb(160, 164, 188),
    },
    // Material Ocean
    ThemePalette {
        bg_deep: PackedRgba::rgb(13, 20, 33),
        bg_base: PackedRgba::rgb(26, 32, 44),
        bg_surface: PackedRgba::rgb(37, 44, 58),
        bg_overlay: PackedRgba::rgb(53, 62, 79),
        bg_highlight: PackedRgba::rgb(84, 109, 128),
        fg_primary: PackedRgba::rgb(238, 255, 255),
        fg_secondary: PackedRgba::rgb(181, 204, 219),
        fg_muted: PackedRgba::rgb(137, 151, 170),
        fg_disabled: PackedRgba::rgb(84, 109, 128),
        accent_primary: PackedRgba::rgb(130, 170, 255),
        accent_secondary: PackedRgba::rgb(199, 146, 234),
        accent_success: PackedRgba::rgb(195, 232, 141),
        accent_warning: PackedRgba::rgb(255, 203, 107),
        accent_error: PackedRgba::rgb(240, 113, 120),
        accent_info: PackedRgba::rgb(137, 221, 255),
        accent_link: PackedRgba::rgb(130, 170, 255),
        accent_slots: [
            PackedRgba::rgb(130, 170, 255),
            PackedRgba::rgb(199, 146, 234),
            PackedRgba::rgb(195, 232, 141),
            PackedRgba::rgb(255, 203, 107),
            PackedRgba::rgb(240, 113, 120),
            PackedRgba::rgb(137, 221, 255),
            PackedRgba::rgb(247, 140, 108),
            PackedRgba::rgb(149, 117, 205),
            PackedRgba::rgb(176, 190, 197),
            PackedRgba::rgb(128, 222, 234),
            PackedRgba::rgb(255, 171, 145),
            PackedRgba::rgb(174, 213, 129),
        ],
        syntax_keyword: PackedRgba::rgb(199, 146, 234),
        syntax_string: PackedRgba::rgb(195, 232, 141),
        syntax_number: PackedRgba::rgb(247, 140, 108),
        syntax_comment: PackedRgba::rgb(84, 109, 128),
        syntax_function: PackedRgba::rgb(130, 170, 255),
        syntax_type: PackedRgba::rgb(137, 221, 255),
        syntax_operator: PackedRgba::rgb(238, 255, 255),
        syntax_punctuation: PackedRgba::rgb(137, 151, 170),
    },
    // Ayu Dark
    ThemePalette {
        bg_deep: PackedRgba::rgb(12, 15, 20),
        bg_base: PackedRgba::rgb(15, 18, 25),
        bg_surface: PackedRgba::rgb(21, 26, 36),
        bg_overlay: PackedRgba::rgb(31, 39, 54),
        bg_highlight: PackedRgba::rgb(54, 66, 90),
        fg_primary: PackedRgba::rgb(230, 225, 221),
        fg_secondary: PackedRgba::rgb(201, 198, 195),
        fg_muted: PackedRgba::rgb(130, 144, 173),
        fg_disabled: PackedRgba::rgb(92, 103, 129),
        accent_primary: PackedRgba::rgb(57, 184, 255),
        accent_secondary: PackedRgba::rgb(255, 141, 102),
        accent_success: PackedRgba::rgb(186, 228, 127),
        accent_warning: PackedRgba::rgb(255, 204, 102),
        accent_error: PackedRgba::rgb(255, 102, 102),
        accent_info: PackedRgba::rgb(149, 230, 203),
        accent_link: PackedRgba::rgb(57, 184, 255),
        accent_slots: [
            PackedRgba::rgb(57, 184, 255),
            PackedRgba::rgb(255, 141, 102),
            PackedRgba::rgb(186, 228, 127),
            PackedRgba::rgb(255, 204, 102),
            PackedRgba::rgb(255, 102, 102),
            PackedRgba::rgb(149, 230, 203),
            PackedRgba::rgb(207, 160, 255),
            PackedRgba::rgb(255, 173, 122),
            PackedRgba::rgb(120, 200, 255),
            PackedRgba::rgb(166, 241, 194),
            PackedRgba::rgb(255, 220, 120),
            PackedRgba::rgb(255, 130, 130),
        ],
        syntax_keyword: PackedRgba::rgb(255, 204, 102),
        syntax_string: PackedRgba::rgb(186, 228, 127),
        syntax_number: PackedRgba::rgb(255, 141, 102),
        syntax_comment: PackedRgba::rgb(92, 103, 129),
        syntax_function: PackedRgba::rgb(57, 184, 255),
        syntax_type: PackedRgba::rgb(149, 230, 203),
        syntax_operator: PackedRgba::rgb(230, 225, 221),
        syntax_punctuation: PackedRgba::rgb(130, 144, 173),
    },
    // Ayu Light
    ThemePalette {
        bg_deep: PackedRgba::rgb(250, 248, 245),
        bg_base: PackedRgba::rgb(245, 243, 240),
        bg_surface: PackedRgba::rgb(238, 235, 229),
        bg_overlay: PackedRgba::rgb(227, 223, 214),
        bg_highlight: PackedRgba::rgb(212, 206, 194),
        fg_primary: PackedRgba::rgb(92, 88, 82),
        fg_secondary: PackedRgba::rgb(110, 105, 98),
        fg_muted: PackedRgba::rgb(143, 136, 127),
        fg_disabled: PackedRgba::rgb(176, 169, 159),
        accent_primary: PackedRgba::rgb(0, 102, 204),
        accent_secondary: PackedRgba::rgb(175, 82, 222),
        accent_success: PackedRgba::rgb(64, 153, 68),
        accent_warning: PackedRgba::rgb(182, 128, 0),
        accent_error: PackedRgba::rgb(198, 40, 40),
        accent_info: PackedRgba::rgb(0, 139, 139),
        accent_link: PackedRgba::rgb(0, 102, 204),
        accent_slots: [
            PackedRgba::rgb(0, 102, 204),
            PackedRgba::rgb(175, 82, 222),
            PackedRgba::rgb(64, 153, 68),
            PackedRgba::rgb(182, 128, 0),
            PackedRgba::rgb(198, 40, 40),
            PackedRgba::rgb(0, 139, 139),
            PackedRgba::rgb(110, 74, 221),
            PackedRgba::rgb(0, 120, 212),
            PackedRgba::rgb(34, 139, 34),
            PackedRgba::rgb(205, 133, 0),
            PackedRgba::rgb(220, 20, 60),
            PackedRgba::rgb(32, 178, 170),
        ],
        syntax_keyword: PackedRgba::rgb(175, 82, 222),
        syntax_string: PackedRgba::rgb(64, 153, 68),
        syntax_number: PackedRgba::rgb(182, 128, 0),
        syntax_comment: PackedRgba::rgb(176, 169, 159),
        syntax_function: PackedRgba::rgb(0, 102, 204),
        syntax_type: PackedRgba::rgb(0, 139, 139),
        syntax_operator: PackedRgba::rgb(92, 88, 82),
        syntax_punctuation: PackedRgba::rgb(143, 136, 127),
    },
    // Kanagawa Wave
    ThemePalette {
        bg_deep: PackedRgba::rgb(22, 27, 34),
        bg_base: PackedRgba::rgb(31, 36, 48),
        bg_surface: PackedRgba::rgb(42, 50, 66),
        bg_overlay: PackedRgba::rgb(57, 68, 88),
        bg_highlight: PackedRgba::rgb(84, 97, 122),
        fg_primary: PackedRgba::rgb(220, 215, 186),
        fg_secondary: PackedRgba::rgb(199, 192, 147),
        fg_muted: PackedRgba::rgb(140, 138, 120),
        fg_disabled: PackedRgba::rgb(99, 105, 113),
        accent_primary: PackedRgba::rgb(125, 172, 216),
        accent_secondary: PackedRgba::rgb(149, 127, 184),
        accent_success: PackedRgba::rgb(118, 145, 84),
        accent_warning: PackedRgba::rgb(223, 188, 110),
        accent_error: PackedRgba::rgb(196, 99, 102),
        accent_info: PackedRgba::rgb(106, 147, 181),
        accent_link: PackedRgba::rgb(125, 172, 216),
        accent_slots: [
            PackedRgba::rgb(125, 172, 216),
            PackedRgba::rgb(149, 127, 184),
            PackedRgba::rgb(118, 145, 84),
            PackedRgba::rgb(223, 188, 110),
            PackedRgba::rgb(196, 99, 102),
            PackedRgba::rgb(106, 147, 181),
            PackedRgba::rgb(180, 142, 173),
            PackedRgba::rgb(194, 166, 120),
            PackedRgba::rgb(147, 183, 196),
            PackedRgba::rgb(121, 135, 128),
            PackedRgba::rgb(255, 160, 102),
            PackedRgba::rgb(165, 123, 214),
        ],
        syntax_keyword: PackedRgba::rgb(149, 127, 184),
        syntax_string: PackedRgba::rgb(118, 145, 84),
        syntax_number: PackedRgba::rgb(255, 160, 102),
        syntax_comment: PackedRgba::rgb(99, 105, 113),
        syntax_function: PackedRgba::rgb(125, 172, 216),
        syntax_type: PackedRgba::rgb(106, 147, 181),
        syntax_operator: PackedRgba::rgb(220, 215, 186),
        syntax_punctuation: PackedRgba::rgb(140, 138, 120),
    },
    // Everforest Dark
    ThemePalette {
        bg_deep: PackedRgba::rgb(35, 44, 40),
        bg_base: PackedRgba::rgb(45, 53, 47),
        bg_surface: PackedRgba::rgb(56, 64, 56),
        bg_overlay: PackedRgba::rgb(72, 80, 70),
        bg_highlight: PackedRgba::rgb(95, 104, 90),
        fg_primary: PackedRgba::rgb(211, 198, 170),
        fg_secondary: PackedRgba::rgb(188, 176, 150),
        fg_muted: PackedRgba::rgb(150, 138, 114),
        fg_disabled: PackedRgba::rgb(112, 102, 86),
        accent_primary: PackedRgba::rgb(125, 170, 117),
        accent_secondary: PackedRgba::rgb(166, 123, 91),
        accent_success: PackedRgba::rgb(167, 192, 128),
        accent_warning: PackedRgba::rgb(219, 188, 127),
        accent_error: PackedRgba::rgb(230, 126, 128),
        accent_info: PackedRgba::rgb(127, 187, 179),
        accent_link: PackedRgba::rgb(122, 166, 218),
        accent_slots: [
            PackedRgba::rgb(125, 170, 117),
            PackedRgba::rgb(166, 123, 91),
            PackedRgba::rgb(167, 192, 128),
            PackedRgba::rgb(219, 188, 127),
            PackedRgba::rgb(230, 126, 128),
            PackedRgba::rgb(127, 187, 179),
            PackedRgba::rgb(122, 166, 218),
            PackedRgba::rgb(214, 153, 82),
            PackedRgba::rgb(159, 201, 124),
            PackedRgba::rgb(143, 181, 167),
            PackedRgba::rgb(198, 141, 122),
            PackedRgba::rgb(120, 132, 108),
        ],
        syntax_keyword: PackedRgba::rgb(166, 123, 91),
        syntax_string: PackedRgba::rgb(167, 192, 128),
        syntax_number: PackedRgba::rgb(214, 153, 82),
        syntax_comment: PackedRgba::rgb(112, 102, 86),
        syntax_function: PackedRgba::rgb(122, 166, 218),
        syntax_type: PackedRgba::rgb(127, 187, 179),
        syntax_operator: PackedRgba::rgb(211, 198, 170),
        syntax_punctuation: PackedRgba::rgb(150, 138, 114),
    },
    // Everforest Light
    ThemePalette {
        bg_deep: PackedRgba::rgb(255, 248, 232),
        bg_base: PackedRgba::rgb(248, 240, 221),
        bg_surface: PackedRgba::rgb(242, 232, 204),
        bg_overlay: PackedRgba::rgb(230, 220, 189),
        bg_highlight: PackedRgba::rgb(214, 203, 167),
        fg_primary: PackedRgba::rgb(92, 106, 81),
        fg_secondary: PackedRgba::rgb(103, 117, 92),
        fg_muted: PackedRgba::rgb(138, 146, 122),
        fg_disabled: PackedRgba::rgb(171, 176, 151),
        accent_primary: PackedRgba::rgb(63, 118, 80),
        accent_secondary: PackedRgba::rgb(141, 115, 99),
        accent_success: PackedRgba::rgb(95, 141, 78),
        accent_warning: PackedRgba::rgb(163, 126, 61),
        accent_error: PackedRgba::rgb(189, 80, 78),
        accent_info: PackedRgba::rgb(53, 140, 164),
        accent_link: PackedRgba::rgb(58, 122, 153),
        accent_slots: [
            PackedRgba::rgb(63, 118, 80),
            PackedRgba::rgb(141, 115, 99),
            PackedRgba::rgb(95, 141, 78),
            PackedRgba::rgb(163, 126, 61),
            PackedRgba::rgb(189, 80, 78),
            PackedRgba::rgb(53, 140, 164),
            PackedRgba::rgb(58, 122, 153),
            PackedRgba::rgb(120, 138, 94),
            PackedRgba::rgb(135, 120, 84),
            PackedRgba::rgb(176, 98, 82),
            PackedRgba::rgb(80, 156, 175),
            PackedRgba::rgb(146, 152, 119),
        ],
        syntax_keyword: PackedRgba::rgb(141, 115, 99),
        syntax_string: PackedRgba::rgb(95, 141, 78),
        syntax_number: PackedRgba::rgb(163, 126, 61),
        syntax_comment: PackedRgba::rgb(171, 176, 151),
        syntax_function: PackedRgba::rgb(58, 122, 153),
        syntax_type: PackedRgba::rgb(53, 140, 164),
        syntax_operator: PackedRgba::rgb(92, 106, 81),
        syntax_punctuation: PackedRgba::rgb(138, 146, 122),
    },
    // GitHub Dark
    ThemePalette {
        bg_deep: PackedRgba::rgb(13, 17, 23),
        bg_base: PackedRgba::rgb(22, 27, 34),
        bg_surface: PackedRgba::rgb(33, 38, 45),
        bg_overlay: PackedRgba::rgb(48, 54, 61),
        bg_highlight: PackedRgba::rgb(75, 83, 95),
        fg_primary: PackedRgba::rgb(230, 237, 243),
        fg_secondary: PackedRgba::rgb(198, 208, 220),
        fg_muted: PackedRgba::rgb(139, 148, 158),
        fg_disabled: PackedRgba::rgb(95, 103, 112),
        accent_primary: PackedRgba::rgb(47, 129, 247),
        accent_secondary: PackedRgba::rgb(163, 113, 247),
        accent_success: PackedRgba::rgb(63, 185, 80),
        accent_warning: PackedRgba::rgb(210, 153, 34),
        accent_error: PackedRgba::rgb(248, 81, 73),
        accent_info: PackedRgba::rgb(121, 192, 255),
        accent_link: PackedRgba::rgb(47, 129, 247),
        accent_slots: [
            PackedRgba::rgb(47, 129, 247),
            PackedRgba::rgb(163, 113, 247),
            PackedRgba::rgb(63, 185, 80),
            PackedRgba::rgb(210, 153, 34),
            PackedRgba::rgb(248, 81, 73),
            PackedRgba::rgb(121, 192, 255),
            PackedRgba::rgb(255, 123, 114),
            PackedRgba::rgb(121, 192, 255),
            PackedRgba::rgb(140, 149, 159),
            PackedRgba::rgb(86, 211, 100),
            PackedRgba::rgb(213, 173, 79),
            PackedRgba::rgb(188, 140, 255),
        ],
        syntax_keyword: PackedRgba::rgb(210, 153, 34),
        syntax_string: PackedRgba::rgb(121, 192, 255),
        syntax_number: PackedRgba::rgb(121, 192, 255),
        syntax_comment: PackedRgba::rgb(95, 103, 112),
        syntax_function: PackedRgba::rgb(210, 153, 34),
        syntax_type: PackedRgba::rgb(163, 113, 247),
        syntax_operator: PackedRgba::rgb(230, 237, 243),
        syntax_punctuation: PackedRgba::rgb(139, 148, 158),
    },
    // GitHub Light
    ThemePalette {
        bg_deep: PackedRgba::rgb(255, 255, 255),
        bg_base: PackedRgba::rgb(246, 248, 250),
        bg_surface: PackedRgba::rgb(234, 238, 242),
        bg_overlay: PackedRgba::rgb(220, 224, 229),
        bg_highlight: PackedRgba::rgb(204, 210, 217),
        fg_primary: PackedRgba::rgb(31, 35, 40),
        fg_secondary: PackedRgba::rgb(65, 72, 81),
        fg_muted: PackedRgba::rgb(87, 96, 106),
        fg_disabled: PackedRgba::rgb(110, 119, 129),
        accent_primary: PackedRgba::rgb(9, 105, 218),
        accent_secondary: PackedRgba::rgb(130, 80, 223),
        accent_success: PackedRgba::rgb(26, 127, 55),
        accent_warning: PackedRgba::rgb(154, 103, 0),
        accent_error: PackedRgba::rgb(207, 34, 46),
        accent_info: PackedRgba::rgb(9, 105, 218),
        accent_link: PackedRgba::rgb(9, 105, 218),
        accent_slots: [
            PackedRgba::rgb(9, 105, 218),
            PackedRgba::rgb(130, 80, 223),
            PackedRgba::rgb(26, 127, 55),
            PackedRgba::rgb(154, 103, 0),
            PackedRgba::rgb(207, 34, 46),
            PackedRgba::rgb(9, 105, 218),
            PackedRgba::rgb(166, 42, 120),
            PackedRgba::rgb(3, 102, 214),
            PackedRgba::rgb(15, 123, 108),
            PackedRgba::rgb(191, 135, 0),
            PackedRgba::rgb(87, 96, 106),
            PackedRgba::rgb(98, 57, 186),
        ],
        syntax_keyword: PackedRgba::rgb(130, 80, 223),
        syntax_string: PackedRgba::rgb(26, 127, 55),
        syntax_number: PackedRgba::rgb(9, 105, 218),
        syntax_comment: PackedRgba::rgb(110, 119, 129),
        syntax_function: PackedRgba::rgb(154, 103, 0),
        syntax_type: PackedRgba::rgb(9, 105, 218),
        syntax_operator: PackedRgba::rgb(31, 35, 40),
        syntax_punctuation: PackedRgba::rgb(87, 96, 106),
    },
    // Synthwave '84
    ThemePalette {
        bg_deep: PackedRgba::rgb(25, 20, 54),
        bg_base: PackedRgba::rgb(36, 31, 76),
        bg_surface: PackedRgba::rgb(47, 42, 92),
        bg_overlay: PackedRgba::rgb(67, 58, 116),
        bg_highlight: PackedRgba::rgb(96, 84, 152),
        fg_primary: PackedRgba::rgb(241, 245, 255),
        fg_secondary: PackedRgba::rgb(212, 214, 255),
        fg_muted: PackedRgba::rgb(157, 159, 207),
        fg_disabled: PackedRgba::rgb(116, 119, 164),
        accent_primary: PackedRgba::rgb(255, 124, 247),
        accent_secondary: PackedRgba::rgb(110, 255, 253),
        accent_success: PackedRgba::rgb(114, 255, 163),
        accent_warning: PackedRgba::rgb(255, 230, 109),
        accent_error: PackedRgba::rgb(255, 85, 122),
        accent_info: PackedRgba::rgb(123, 194, 255),
        accent_link: PackedRgba::rgb(110, 255, 253),
        accent_slots: [
            PackedRgba::rgb(255, 124, 247),
            PackedRgba::rgb(110, 255, 253),
            PackedRgba::rgb(114, 255, 163),
            PackedRgba::rgb(255, 230, 109),
            PackedRgba::rgb(255, 85, 122),
            PackedRgba::rgb(123, 194, 255),
            PackedRgba::rgb(255, 161, 79),
            PackedRgba::rgb(255, 99, 188),
            PackedRgba::rgb(144, 107, 255),
            PackedRgba::rgb(96, 255, 202),
            PackedRgba::rgb(255, 247, 153),
            PackedRgba::rgb(165, 214, 255),
        ],
        syntax_keyword: PackedRgba::rgb(255, 124, 247),
        syntax_string: PackedRgba::rgb(110, 255, 253),
        syntax_number: PackedRgba::rgb(255, 161, 79),
        syntax_comment: PackedRgba::rgb(116, 119, 164),
        syntax_function: PackedRgba::rgb(123, 194, 255),
        syntax_type: PackedRgba::rgb(114, 255, 163),
        syntax_operator: PackedRgba::rgb(241, 245, 255),
        syntax_punctuation: PackedRgba::rgb(157, 159, 207),
    },
    // Palenight
    ThemePalette {
        bg_deep: PackedRgba::rgb(24, 27, 38),
        bg_base: PackedRgba::rgb(41, 45, 62),
        bg_surface: PackedRgba::rgb(50, 56, 75),
        bg_overlay: PackedRgba::rgb(67, 77, 103),
        bg_highlight: PackedRgba::rgb(93, 107, 145),
        fg_primary: PackedRgba::rgb(166, 172, 205),
        fg_secondary: PackedRgba::rgb(149, 157, 203),
        fg_muted: PackedRgba::rgb(103, 112, 148),
        fg_disabled: PackedRgba::rgb(80, 87, 115),
        accent_primary: PackedRgba::rgb(130, 170, 255),
        accent_secondary: PackedRgba::rgb(199, 146, 234),
        accent_success: PackedRgba::rgb(195, 232, 141),
        accent_warning: PackedRgba::rgb(255, 203, 107),
        accent_error: PackedRgba::rgb(255, 83, 112),
        accent_info: PackedRgba::rgb(137, 221, 255),
        accent_link: PackedRgba::rgb(130, 170, 255),
        accent_slots: [
            PackedRgba::rgb(130, 170, 255),
            PackedRgba::rgb(199, 146, 234),
            PackedRgba::rgb(195, 232, 141),
            PackedRgba::rgb(255, 203, 107),
            PackedRgba::rgb(255, 83, 112),
            PackedRgba::rgb(137, 221, 255),
            PackedRgba::rgb(247, 140, 108),
            PackedRgba::rgb(130, 170, 255),
            PackedRgba::rgb(193, 132, 252),
            PackedRgba::rgb(99, 187, 227),
            PackedRgba::rgb(173, 219, 103),
            PackedRgba::rgb(255, 203, 107),
        ],
        syntax_keyword: PackedRgba::rgb(199, 146, 234),
        syntax_string: PackedRgba::rgb(195, 232, 141),
        syntax_number: PackedRgba::rgb(247, 140, 108),
        syntax_comment: PackedRgba::rgb(80, 87, 115),
        syntax_function: PackedRgba::rgb(130, 170, 255),
        syntax_type: PackedRgba::rgb(137, 221, 255),
        syntax_operator: PackedRgba::rgb(166, 172, 205),
        syntax_punctuation: PackedRgba::rgb(103, 112, 148),
    },
    // Horizon Dark
    ThemePalette {
        bg_deep: PackedRgba::rgb(28, 30, 39),
        bg_base: PackedRgba::rgb(36, 39, 50),
        bg_surface: PackedRgba::rgb(45, 49, 63),
        bg_overlay: PackedRgba::rgb(62, 67, 84),
        bg_highlight: PackedRgba::rgb(84, 90, 111),
        fg_primary: PackedRgba::rgb(220, 223, 228),
        fg_secondary: PackedRgba::rgb(191, 195, 203),
        fg_muted: PackedRgba::rgb(145, 150, 163),
        fg_disabled: PackedRgba::rgb(103, 109, 125),
        accent_primary: PackedRgba::rgb(224, 122, 95),
        accent_secondary: PackedRgba::rgb(178, 148, 187),
        accent_success: PackedRgba::rgb(166, 218, 149),
        accent_warning: PackedRgba::rgb(242, 200, 121),
        accent_error: PackedRgba::rgb(234, 84, 85),
        accent_info: PackedRgba::rgb(89, 154, 218),
        accent_link: PackedRgba::rgb(89, 154, 218),
        accent_slots: [
            PackedRgba::rgb(224, 122, 95),
            PackedRgba::rgb(178, 148, 187),
            PackedRgba::rgb(166, 218, 149),
            PackedRgba::rgb(242, 200, 121),
            PackedRgba::rgb(234, 84, 85),
            PackedRgba::rgb(89, 154, 218),
            PackedRgba::rgb(248, 150, 30),
            PackedRgba::rgb(188, 190, 196),
            PackedRgba::rgb(103, 170, 222),
            PackedRgba::rgb(150, 194, 140),
            PackedRgba::rgb(220, 130, 100),
            PackedRgba::rgb(206, 162, 209),
        ],
        syntax_keyword: PackedRgba::rgb(178, 148, 187),
        syntax_string: PackedRgba::rgb(166, 218, 149),
        syntax_number: PackedRgba::rgb(248, 150, 30),
        syntax_comment: PackedRgba::rgb(103, 109, 125),
        syntax_function: PackedRgba::rgb(89, 154, 218),
        syntax_type: PackedRgba::rgb(224, 122, 95),
        syntax_operator: PackedRgba::rgb(220, 223, 228),
        syntax_punctuation: PackedRgba::rgb(145, 150, 163),
    },
    // Nord
    ThemePalette {
        bg_deep: PackedRgba::rgb(35, 41, 52),
        bg_base: PackedRgba::rgb(46, 52, 64),
        bg_surface: PackedRgba::rgb(59, 66, 82),
        bg_overlay: PackedRgba::rgb(76, 86, 106),
        bg_highlight: PackedRgba::rgb(94, 129, 172),
        fg_primary: PackedRgba::rgb(236, 239, 244),
        fg_secondary: PackedRgba::rgb(229, 233, 240),
        fg_muted: PackedRgba::rgb(179, 188, 203),
        fg_disabled: PackedRgba::rgb(136, 152, 176),
        accent_primary: PackedRgba::rgb(129, 161, 193),
        accent_secondary: PackedRgba::rgb(180, 142, 173),
        accent_success: PackedRgba::rgb(163, 190, 140),
        accent_warning: PackedRgba::rgb(235, 203, 139),
        accent_error: PackedRgba::rgb(191, 97, 106),
        accent_info: PackedRgba::rgb(143, 188, 187),
        accent_link: PackedRgba::rgb(136, 192, 208),
        accent_slots: [
            PackedRgba::rgb(129, 161, 193),
            PackedRgba::rgb(180, 142, 173),
            PackedRgba::rgb(163, 190, 140),
            PackedRgba::rgb(235, 203, 139),
            PackedRgba::rgb(191, 97, 106),
            PackedRgba::rgb(143, 188, 187),
            PackedRgba::rgb(136, 192, 208),
            PackedRgba::rgb(94, 129, 172),
            PackedRgba::rgb(208, 135, 112),
            PackedRgba::rgb(216, 222, 233),
            PackedRgba::rgb(163, 179, 194),
            PackedRgba::rgb(191, 97, 106),
        ],
        syntax_keyword: PackedRgba::rgb(180, 142, 173),
        syntax_string: PackedRgba::rgb(163, 190, 140),
        syntax_number: PackedRgba::rgb(208, 135, 112),
        syntax_comment: PackedRgba::rgb(136, 152, 176),
        syntax_function: PackedRgba::rgb(129, 161, 193),
        syntax_type: PackedRgba::rgb(143, 188, 187),
        syntax_operator: PackedRgba::rgb(236, 239, 244),
        syntax_punctuation: PackedRgba::rgb(179, 188, 203),
    },
    // One Light
    ThemePalette {
        bg_deep: PackedRgba::rgb(250, 250, 250),
        bg_base: PackedRgba::rgb(245, 246, 247),
        bg_surface: PackedRgba::rgb(234, 236, 239),
        bg_overlay: PackedRgba::rgb(222, 226, 232),
        bg_highlight: PackedRgba::rgb(206, 213, 223),
        fg_primary: PackedRgba::rgb(56, 58, 66),
        fg_secondary: PackedRgba::rgb(80, 84, 93),
        fg_muted: PackedRgba::rgb(120, 126, 138),
        fg_disabled: PackedRgba::rgb(155, 162, 174),
        accent_primary: PackedRgba::rgb(64, 120, 242),
        accent_secondary: PackedRgba::rgb(160, 82, 223),
        accent_success: PackedRgba::rgb(80, 161, 79),
        accent_warning: PackedRgba::rgb(196, 135, 15),
        accent_error: PackedRgba::rgb(228, 86, 73),
        accent_info: PackedRgba::rgb(1, 132, 188),
        accent_link: PackedRgba::rgb(64, 120, 242),
        accent_slots: [
            PackedRgba::rgb(64, 120, 242),
            PackedRgba::rgb(160, 82, 223),
            PackedRgba::rgb(80, 161, 79),
            PackedRgba::rgb(196, 135, 15),
            PackedRgba::rgb(228, 86, 73),
            PackedRgba::rgb(1, 132, 188),
            PackedRgba::rgb(225, 111, 61),
            PackedRgba::rgb(191, 64, 191),
            PackedRgba::rgb(3, 102, 214),
            PackedRgba::rgb(28, 126, 214),
            PackedRgba::rgb(138, 106, 38),
            PackedRgba::rgb(20, 139, 148),
        ],
        syntax_keyword: PackedRgba::rgb(160, 82, 223),
        syntax_string: PackedRgba::rgb(80, 161, 79),
        syntax_number: PackedRgba::rgb(225, 111, 61),
        syntax_comment: PackedRgba::rgb(155, 162, 174),
        syntax_function: PackedRgba::rgb(64, 120, 242),
        syntax_type: PackedRgba::rgb(1, 132, 188),
        syntax_operator: PackedRgba::rgb(56, 58, 66),
        syntax_punctuation: PackedRgba::rgb(120, 126, 138),
    },
    // Catppuccin Latte
    ThemePalette {
        bg_deep: PackedRgba::rgb(245, 242, 232),
        bg_base: PackedRgba::rgb(239, 241, 245),
        bg_surface: PackedRgba::rgb(230, 233, 239),
        bg_overlay: PackedRgba::rgb(220, 224, 232),
        bg_highlight: PackedRgba::rgb(204, 208, 218),
        fg_primary: PackedRgba::rgb(76, 79, 105),
        fg_secondary: PackedRgba::rgb(92, 95, 119),
        fg_muted: PackedRgba::rgb(140, 143, 161),
        fg_disabled: PackedRgba::rgb(172, 176, 190),
        accent_primary: PackedRgba::rgb(30, 102, 245),
        accent_secondary: PackedRgba::rgb(136, 57, 239),
        accent_success: PackedRgba::rgb(64, 160, 43),
        accent_warning: PackedRgba::rgb(223, 142, 29),
        accent_error: PackedRgba::rgb(210, 15, 57),
        accent_info: PackedRgba::rgb(4, 165, 229),
        accent_link: PackedRgba::rgb(30, 102, 245),
        accent_slots: [
            PackedRgba::rgb(30, 102, 245),
            PackedRgba::rgb(136, 57, 239),
            PackedRgba::rgb(64, 160, 43),
            PackedRgba::rgb(223, 142, 29),
            PackedRgba::rgb(210, 15, 57),
            PackedRgba::rgb(4, 165, 229),
            PackedRgba::rgb(254, 100, 11),
            PackedRgba::rgb(234, 118, 203),
            PackedRgba::rgb(32, 159, 181),
            PackedRgba::rgb(114, 135, 253),
            PackedRgba::rgb(64, 160, 43),
            PackedRgba::rgb(136, 57, 239),
        ],
        syntax_keyword: PackedRgba::rgb(136, 57, 239),
        syntax_string: PackedRgba::rgb(64, 160, 43),
        syntax_number: PackedRgba::rgb(254, 100, 11),
        syntax_comment: PackedRgba::rgb(172, 176, 190),
        syntax_function: PackedRgba::rgb(30, 102, 245),
        syntax_type: PackedRgba::rgb(32, 159, 181),
        syntax_operator: PackedRgba::rgb(76, 79, 105),
        syntax_punctuation: PackedRgba::rgb(140, 143, 161),
    },
    // Catppuccin Frappe
    ThemePalette {
        bg_deep: PackedRgba::rgb(35, 38, 52),
        bg_base: PackedRgba::rgb(48, 52, 70),
        bg_surface: PackedRgba::rgb(65, 69, 89),
        bg_overlay: PackedRgba::rgb(81, 87, 109),
        bg_highlight: PackedRgba::rgb(98, 104, 128),
        fg_primary: PackedRgba::rgb(198, 208, 245),
        fg_secondary: PackedRgba::rgb(181, 191, 226),
        fg_muted: PackedRgba::rgb(148, 156, 187),
        fg_disabled: PackedRgba::rgb(115, 121, 148),
        accent_primary: PackedRgba::rgb(140, 170, 238),
        accent_secondary: PackedRgba::rgb(202, 158, 230),
        accent_success: PackedRgba::rgb(166, 209, 137),
        accent_warning: PackedRgba::rgb(229, 200, 144),
        accent_error: PackedRgba::rgb(231, 130, 132),
        accent_info: PackedRgba::rgb(153, 209, 219),
        accent_link: PackedRgba::rgb(140, 170, 238),
        accent_slots: [
            PackedRgba::rgb(140, 170, 238),
            PackedRgba::rgb(202, 158, 230),
            PackedRgba::rgb(166, 209, 137),
            PackedRgba::rgb(229, 200, 144),
            PackedRgba::rgb(231, 130, 132),
            PackedRgba::rgb(153, 209, 219),
            PackedRgba::rgb(239, 159, 118),
            PackedRgba::rgb(244, 184, 228),
            PackedRgba::rgb(129, 200, 190),
            PackedRgba::rgb(186, 187, 241),
            PackedRgba::rgb(166, 209, 137),
            PackedRgba::rgb(202, 158, 230),
        ],
        syntax_keyword: PackedRgba::rgb(202, 158, 230),
        syntax_string: PackedRgba::rgb(166, 209, 137),
        syntax_number: PackedRgba::rgb(239, 159, 118),
        syntax_comment: PackedRgba::rgb(115, 121, 148),
        syntax_function: PackedRgba::rgb(140, 170, 238),
        syntax_type: PackedRgba::rgb(153, 209, 219),
        syntax_operator: PackedRgba::rgb(198, 208, 245),
        syntax_punctuation: PackedRgba::rgb(148, 156, 187),
    },
    // Catppuccin Macchiato
    ThemePalette {
        bg_deep: PackedRgba::rgb(24, 26, 38),
        bg_base: PackedRgba::rgb(36, 39, 58),
        bg_surface: PackedRgba::rgb(54, 58, 79),
        bg_overlay: PackedRgba::rgb(73, 77, 100),
        bg_highlight: PackedRgba::rgb(91, 96, 120),
        fg_primary: PackedRgba::rgb(202, 211, 245),
        fg_secondary: PackedRgba::rgb(184, 192, 224),
        fg_muted: PackedRgba::rgb(147, 154, 183),
        fg_disabled: PackedRgba::rgb(110, 115, 141),
        accent_primary: PackedRgba::rgb(138, 173, 244),
        accent_secondary: PackedRgba::rgb(198, 160, 246),
        accent_success: PackedRgba::rgb(166, 218, 149),
        accent_warning: PackedRgba::rgb(238, 212, 159),
        accent_error: PackedRgba::rgb(237, 135, 150),
        accent_info: PackedRgba::rgb(145, 215, 227),
        accent_link: PackedRgba::rgb(138, 173, 244),
        accent_slots: [
            PackedRgba::rgb(138, 173, 244),
            PackedRgba::rgb(198, 160, 246),
            PackedRgba::rgb(166, 218, 149),
            PackedRgba::rgb(238, 212, 159),
            PackedRgba::rgb(237, 135, 150),
            PackedRgba::rgb(145, 215, 227),
            PackedRgba::rgb(245, 169, 127),
            PackedRgba::rgb(245, 189, 230),
            PackedRgba::rgb(125, 196, 228),
            PackedRgba::rgb(183, 189, 248),
            PackedRgba::rgb(166, 218, 149),
            PackedRgba::rgb(198, 160, 246),
        ],
        syntax_keyword: PackedRgba::rgb(198, 160, 246),
        syntax_string: PackedRgba::rgb(166, 218, 149),
        syntax_number: PackedRgba::rgb(245, 169, 127),
        syntax_comment: PackedRgba::rgb(110, 115, 141),
        syntax_function: PackedRgba::rgb(138, 173, 244),
        syntax_type: PackedRgba::rgb(145, 215, 227),
        syntax_operator: PackedRgba::rgb(202, 211, 245),
        syntax_punctuation: PackedRgba::rgb(147, 154, 183),
    },
    // Kanagawa Lotus
    ThemePalette {
        bg_deep: PackedRgba::rgb(248, 242, 229),
        bg_base: PackedRgba::rgb(241, 234, 215),
        bg_surface: PackedRgba::rgb(232, 224, 201),
        bg_overlay: PackedRgba::rgb(221, 211, 184),
        bg_highlight: PackedRgba::rgb(202, 191, 162),
        fg_primary: PackedRgba::rgb(84, 73, 58),
        fg_secondary: PackedRgba::rgb(110, 99, 83),
        fg_muted: PackedRgba::rgb(138, 125, 107),
        fg_disabled: PackedRgba::rgb(168, 155, 136),
        accent_primary: PackedRgba::rgb(77, 127, 168),
        accent_secondary: PackedRgba::rgb(126, 94, 167),
        accent_success: PackedRgba::rgb(93, 139, 77),
        accent_warning: PackedRgba::rgb(174, 129, 61),
        accent_error: PackedRgba::rgb(173, 77, 91),
        accent_info: PackedRgba::rgb(84, 140, 128),
        accent_link: PackedRgba::rgb(77, 127, 168),
        accent_slots: [
            PackedRgba::rgb(77, 127, 168),
            PackedRgba::rgb(126, 94, 167),
            PackedRgba::rgb(93, 139, 77),
            PackedRgba::rgb(174, 129, 61),
            PackedRgba::rgb(173, 77, 91),
            PackedRgba::rgb(84, 140, 128),
            PackedRgba::rgb(191, 109, 60),
            PackedRgba::rgb(129, 116, 94),
            PackedRgba::rgb(103, 127, 90),
            PackedRgba::rgb(160, 118, 71),
            PackedRgba::rgb(63, 126, 150),
            PackedRgba::rgb(126, 94, 167),
        ],
        syntax_keyword: PackedRgba::rgb(126, 94, 167),
        syntax_string: PackedRgba::rgb(93, 139, 77),
        syntax_number: PackedRgba::rgb(191, 109, 60),
        syntax_comment: PackedRgba::rgb(168, 155, 136),
        syntax_function: PackedRgba::rgb(77, 127, 168),
        syntax_type: PackedRgba::rgb(84, 140, 128),
        syntax_operator: PackedRgba::rgb(84, 73, 58),
        syntax_punctuation: PackedRgba::rgb(138, 125, 107),
    },
    // Nightfox
    ThemePalette {
        bg_deep: PackedRgba::rgb(16, 20, 29),
        bg_base: PackedRgba::rgb(25, 32, 45),
        bg_surface: PackedRgba::rgb(33, 42, 58),
        bg_overlay: PackedRgba::rgb(47, 58, 78),
        bg_highlight: PackedRgba::rgb(72, 89, 119),
        fg_primary: PackedRgba::rgb(205, 214, 244),
        fg_secondary: PackedRgba::rgb(176, 184, 212),
        fg_muted: PackedRgba::rgb(131, 139, 167),
        fg_disabled: PackedRgba::rgb(95, 104, 129),
        accent_primary: PackedRgba::rgb(130, 170, 255),
        accent_secondary: PackedRgba::rgb(192, 132, 252),
        accent_success: PackedRgba::rgb(162, 217, 175),
        accent_warning: PackedRgba::rgb(245, 169, 127),
        accent_error: PackedRgba::rgb(240, 143, 104),
        accent_info: PackedRgba::rgb(134, 216, 255),
        accent_link: PackedRgba::rgb(130, 170, 255),
        accent_slots: [
            PackedRgba::rgb(130, 170, 255),
            PackedRgba::rgb(192, 132, 252),
            PackedRgba::rgb(162, 217, 175),
            PackedRgba::rgb(245, 169, 127),
            PackedRgba::rgb(240, 143, 104),
            PackedRgba::rgb(134, 216, 255),
            PackedRgba::rgb(225, 146, 94),
            PackedRgba::rgb(122, 162, 247),
            PackedRgba::rgb(150, 205, 251),
            PackedRgba::rgb(197, 160, 246),
            PackedRgba::rgb(160, 216, 239),
            PackedRgba::rgb(245, 169, 127),
        ],
        syntax_keyword: PackedRgba::rgb(192, 132, 252),
        syntax_string: PackedRgba::rgb(162, 217, 175),
        syntax_number: PackedRgba::rgb(225, 146, 94),
        syntax_comment: PackedRgba::rgb(95, 104, 129),
        syntax_function: PackedRgba::rgb(130, 170, 255),
        syntax_type: PackedRgba::rgb(134, 216, 255),
        syntax_operator: PackedRgba::rgb(205, 214, 244),
        syntax_punctuation: PackedRgba::rgb(131, 139, 167),
    },
    // Dayfox
    ThemePalette {
        bg_deep: PackedRgba::rgb(252, 252, 252),
        bg_base: PackedRgba::rgb(248, 249, 251),
        bg_surface: PackedRgba::rgb(237, 239, 244),
        bg_overlay: PackedRgba::rgb(225, 228, 236),
        bg_highlight: PackedRgba::rgb(208, 214, 225),
        fg_primary: PackedRgba::rgb(56, 67, 90),
        fg_secondary: PackedRgba::rgb(78, 92, 118),
        fg_muted: PackedRgba::rgb(111, 122, 147),
        fg_disabled: PackedRgba::rgb(146, 154, 173),
        accent_primary: PackedRgba::rgb(40, 108, 184),
        accent_secondary: PackedRgba::rgb(151, 94, 242),
        accent_success: PackedRgba::rgb(57, 145, 119),
        accent_warning: PackedRgba::rgb(200, 118, 42),
        accent_error: PackedRgba::rgb(188, 72, 75),
        accent_info: PackedRgba::rgb(49, 134, 190),
        accent_link: PackedRgba::rgb(40, 108, 184),
        accent_slots: [
            PackedRgba::rgb(40, 108, 184),
            PackedRgba::rgb(151, 94, 242),
            PackedRgba::rgb(57, 145, 119),
            PackedRgba::rgb(200, 118, 42),
            PackedRgba::rgb(188, 72, 75),
            PackedRgba::rgb(49, 134, 190),
            PackedRgba::rgb(160, 105, 39),
            PackedRgba::rgb(110, 112, 230),
            PackedRgba::rgb(34, 146, 187),
            PackedRgba::rgb(177, 86, 145),
            PackedRgba::rgb(80, 116, 163),
            PackedRgba::rgb(57, 145, 119),
        ],
        syntax_keyword: PackedRgba::rgb(151, 94, 242),
        syntax_string: PackedRgba::rgb(57, 145, 119),
        syntax_number: PackedRgba::rgb(160, 105, 39),
        syntax_comment: PackedRgba::rgb(146, 154, 173),
        syntax_function: PackedRgba::rgb(40, 108, 184),
        syntax_type: PackedRgba::rgb(49, 134, 190),
        syntax_operator: PackedRgba::rgb(56, 67, 90),
        syntax_punctuation: PackedRgba::rgb(111, 122, 147),
    },
    // Oceanic Next
    ThemePalette {
        bg_deep: PackedRgba::rgb(25, 38, 49),
        bg_base: PackedRgba::rgb(32, 46, 58),
        bg_surface: PackedRgba::rgb(48, 67, 82),
        bg_overlay: PackedRgba::rgb(63, 86, 103),
        bg_highlight: PackedRgba::rgb(79, 104, 121),
        fg_primary: PackedRgba::rgb(204, 222, 235),
        fg_secondary: PackedRgba::rgb(165, 183, 199),
        fg_muted: PackedRgba::rgb(126, 145, 161),
        fg_disabled: PackedRgba::rgb(91, 110, 126),
        accent_primary: PackedRgba::rgb(102, 217, 239),
        accent_secondary: PackedRgba::rgb(197, 149, 197),
        accent_success: PackedRgba::rgb(153, 199, 148),
        accent_warning: PackedRgba::rgb(250, 200, 99),
        accent_error: PackedRgba::rgb(236, 95, 102),
        accent_info: PackedRgba::rgb(91, 196, 191),
        accent_link: PackedRgba::rgb(102, 217, 239),
        accent_slots: [
            PackedRgba::rgb(102, 217, 239),
            PackedRgba::rgb(197, 149, 197),
            PackedRgba::rgb(153, 199, 148),
            PackedRgba::rgb(250, 200, 99),
            PackedRgba::rgb(236, 95, 102),
            PackedRgba::rgb(91, 196, 191),
            PackedRgba::rgb(249, 145, 87),
            PackedRgba::rgb(102, 178, 255),
            PackedRgba::rgb(173, 219, 103),
            PackedRgba::rgb(255, 203, 107),
            PackedRgba::rgb(199, 146, 234),
            PackedRgba::rgb(127, 219, 202),
        ],
        syntax_keyword: PackedRgba::rgb(197, 149, 197),
        syntax_string: PackedRgba::rgb(153, 199, 148),
        syntax_number: PackedRgba::rgb(249, 145, 87),
        syntax_comment: PackedRgba::rgb(91, 110, 126),
        syntax_function: PackedRgba::rgb(102, 217, 239),
        syntax_type: PackedRgba::rgb(91, 196, 191),
        syntax_operator: PackedRgba::rgb(204, 222, 235),
        syntax_punctuation: PackedRgba::rgb(126, 145, 161),
    },
    // Cobalt2
    ThemePalette {
        bg_deep: PackedRgba::rgb(0, 29, 69),
        bg_base: PackedRgba::rgb(16, 42, 86),
        bg_surface: PackedRgba::rgb(24, 55, 109),
        bg_overlay: PackedRgba::rgb(40, 73, 132),
        bg_highlight: PackedRgba::rgb(58, 95, 160),
        fg_primary: PackedRgba::rgb(255, 255, 255),
        fg_secondary: PackedRgba::rgb(220, 234, 255),
        fg_muted: PackedRgba::rgb(169, 198, 235),
        fg_disabled: PackedRgba::rgb(117, 152, 201),
        accent_primary: PackedRgba::rgb(255, 157, 0),
        accent_secondary: PackedRgba::rgb(255, 98, 140),
        accent_success: PackedRgba::rgb(61, 219, 134),
        accent_warning: PackedRgba::rgb(255, 214, 102),
        accent_error: PackedRgba::rgb(255, 98, 140),
        accent_info: PackedRgba::rgb(64, 204, 255),
        accent_link: PackedRgba::rgb(64, 204, 255),
        accent_slots: [
            PackedRgba::rgb(255, 157, 0),
            PackedRgba::rgb(255, 98, 140),
            PackedRgba::rgb(61, 219, 134),
            PackedRgba::rgb(255, 214, 102),
            PackedRgba::rgb(255, 98, 140),
            PackedRgba::rgb(64, 204, 255),
            PackedRgba::rgb(192, 132, 252),
            PackedRgba::rgb(123, 194, 255),
            PackedRgba::rgb(255, 176, 84),
            PackedRgba::rgb(114, 255, 163),
            PackedRgba::rgb(110, 255, 253),
            PackedRgba::rgb(255, 124, 247),
        ],
        syntax_keyword: PackedRgba::rgb(255, 157, 0),
        syntax_string: PackedRgba::rgb(61, 219, 134),
        syntax_number: PackedRgba::rgb(255, 214, 102),
        syntax_comment: PackedRgba::rgb(117, 152, 201),
        syntax_function: PackedRgba::rgb(64, 204, 255),
        syntax_type: PackedRgba::rgb(255, 98, 140),
        syntax_operator: PackedRgba::rgb(255, 255, 255),
        syntax_punctuation: PackedRgba::rgb(169, 198, 235),
    },
    // PaperColor Dark
    ThemePalette {
        bg_deep: PackedRgba::rgb(25, 25, 25),
        bg_base: PackedRgba::rgb(30, 30, 30),
        bg_surface: PackedRgba::rgb(38, 38, 38),
        bg_overlay: PackedRgba::rgb(51, 51, 51),
        bg_highlight: PackedRgba::rgb(68, 68, 68),
        fg_primary: PackedRgba::rgb(208, 208, 208),
        fg_secondary: PackedRgba::rgb(188, 188, 188),
        fg_muted: PackedRgba::rgb(148, 148, 148),
        fg_disabled: PackedRgba::rgb(108, 108, 108),
        accent_primary: PackedRgba::rgb(95, 175, 215),
        accent_secondary: PackedRgba::rgb(175, 95, 175),
        accent_success: PackedRgba::rgb(95, 175, 95),
        accent_warning: PackedRgba::rgb(215, 175, 95),
        accent_error: PackedRgba::rgb(215, 95, 95),
        accent_info: PackedRgba::rgb(95, 175, 215),
        accent_link: PackedRgba::rgb(95, 175, 215),
        accent_slots: [
            PackedRgba::rgb(95, 175, 215),
            PackedRgba::rgb(175, 95, 175),
            PackedRgba::rgb(95, 175, 95),
            PackedRgba::rgb(215, 175, 95),
            PackedRgba::rgb(215, 95, 95),
            PackedRgba::rgb(95, 175, 215),
            PackedRgba::rgb(95, 135, 175),
            PackedRgba::rgb(175, 135, 95),
            PackedRgba::rgb(135, 175, 95),
            PackedRgba::rgb(175, 95, 135),
            PackedRgba::rgb(95, 175, 175),
            PackedRgba::rgb(215, 135, 95),
        ],
        syntax_keyword: PackedRgba::rgb(175, 95, 175),
        syntax_string: PackedRgba::rgb(95, 175, 95),
        syntax_number: PackedRgba::rgb(215, 175, 95),
        syntax_comment: PackedRgba::rgb(108, 108, 108),
        syntax_function: PackedRgba::rgb(95, 175, 215),
        syntax_type: PackedRgba::rgb(95, 175, 175),
        syntax_operator: PackedRgba::rgb(208, 208, 208),
        syntax_punctuation: PackedRgba::rgb(148, 148, 148),
    },
    // PaperColor Light
    ThemePalette {
        bg_deep: PackedRgba::rgb(255, 255, 255),
        bg_base: PackedRgba::rgb(248, 248, 248),
        bg_surface: PackedRgba::rgb(238, 238, 238),
        bg_overlay: PackedRgba::rgb(226, 226, 226),
        bg_highlight: PackedRgba::rgb(210, 210, 210),
        fg_primary: PackedRgba::rgb(68, 68, 68),
        fg_secondary: PackedRgba::rgb(88, 88, 88),
        fg_muted: PackedRgba::rgb(118, 118, 118),
        fg_disabled: PackedRgba::rgb(148, 148, 148),
        accent_primary: PackedRgba::rgb(0, 95, 175),
        accent_secondary: PackedRgba::rgb(135, 0, 135),
        accent_success: PackedRgba::rgb(0, 135, 0),
        accent_warning: PackedRgba::rgb(175, 95, 0),
        accent_error: PackedRgba::rgb(175, 0, 0),
        accent_info: PackedRgba::rgb(0, 135, 175),
        accent_link: PackedRgba::rgb(0, 95, 175),
        accent_slots: [
            PackedRgba::rgb(0, 95, 175),
            PackedRgba::rgb(135, 0, 135),
            PackedRgba::rgb(0, 135, 0),
            PackedRgba::rgb(175, 95, 0),
            PackedRgba::rgb(175, 0, 0),
            PackedRgba::rgb(0, 135, 175),
            PackedRgba::rgb(95, 95, 175),
            PackedRgba::rgb(175, 95, 95),
            PackedRgba::rgb(95, 135, 0),
            PackedRgba::rgb(135, 95, 0),
            PackedRgba::rgb(0, 135, 135),
            PackedRgba::rgb(95, 0, 135),
        ],
        syntax_keyword: PackedRgba::rgb(135, 0, 135),
        syntax_string: PackedRgba::rgb(0, 135, 0),
        syntax_number: PackedRgba::rgb(175, 95, 0),
        syntax_comment: PackedRgba::rgb(148, 148, 148),
        syntax_function: PackedRgba::rgb(0, 95, 175),
        syntax_type: PackedRgba::rgb(0, 135, 175),
        syntax_operator: PackedRgba::rgb(68, 68, 68),
        syntax_punctuation: PackedRgba::rgb(118, 118, 118),
    },
    // High Contrast accessibility theme (WCAG AAA compliant)
    ThemePalette {
        bg_deep: PackedRgba::rgb(0, 0, 0),
        bg_base: PackedRgba::rgb(0, 0, 0),
        bg_surface: PackedRgba::rgb(20, 20, 20),
        bg_overlay: PackedRgba::rgb(40, 40, 40),
        bg_highlight: PackedRgba::rgb(80, 80, 80),
        fg_primary: PackedRgba::rgb(255, 255, 255),
        fg_secondary: PackedRgba::rgb(230, 230, 230),
        fg_muted: PackedRgba::rgb(180, 180, 180),
        fg_disabled: PackedRgba::rgb(120, 120, 120),
        accent_primary: PackedRgba::rgb(0, 255, 255),
        accent_secondary: PackedRgba::rgb(255, 255, 0),
        accent_success: PackedRgba::rgb(0, 255, 0),
        accent_warning: PackedRgba::rgb(255, 255, 0),
        accent_error: PackedRgba::rgb(255, 100, 100),
        accent_info: PackedRgba::rgb(100, 200, 255),
        accent_link: PackedRgba::rgb(100, 200, 255),
        accent_slots: [
            PackedRgba::rgb(0, 255, 255),
            PackedRgba::rgb(255, 255, 0),
            PackedRgba::rgb(0, 255, 0),
            PackedRgba::rgb(255, 165, 0),
            PackedRgba::rgb(255, 100, 100),
            PackedRgba::rgb(100, 200, 255),
            PackedRgba::rgb(255, 0, 255),
            PackedRgba::rgb(0, 255, 128),
            PackedRgba::rgb(255, 128, 0),
            PackedRgba::rgb(128, 255, 255),
            PackedRgba::rgb(255, 128, 255),
            PackedRgba::rgb(128, 255, 0),
        ],
        syntax_keyword: PackedRgba::rgb(255, 255, 0),
        syntax_string: PackedRgba::rgb(0, 255, 0),
        syntax_number: PackedRgba::rgb(255, 165, 0),
        syntax_comment: PackedRgba::rgb(128, 128, 128),
        syntax_function: PackedRgba::rgb(0, 255, 255),
        syntax_type: PackedRgba::rgb(255, 0, 255),
        syntax_operator: PackedRgba::rgb(255, 255, 255),
        syntax_punctuation: PackedRgba::rgb(200, 200, 200),
    },
];

static CURRENT_THEME: AtomicUsize = AtomicUsize::new(0);
static HARMONIZED_THEMES: OnceLock<[ThemePalette; ThemeId::ALL.len()]> = OnceLock::new();

/// Internal: set theme without acquiring the lock.
/// Used by `ScopedThemeLock::new()` which already holds the lock.
fn set_theme_internal(theme: ThemeId) {
    CURRENT_THEME.store(theme.index(), Ordering::Relaxed);
}

/// Set the active theme.
///
/// Acquires `THEME_TEST_LOCK` to serialize with parallel tests. If the current
/// thread already holds the lock (via `ScopedThemeLock`), sets the theme directly.
pub fn set_theme(theme: ThemeId) {
    // Check if current thread already holds the lock (reentrant case from ScopedThemeLock)
    let held = THEME_LOCK_HELD.with(|h| h.get());
    if held {
        // Current thread holds lock, set directly without re-acquiring
        set_theme_internal(theme);
    } else {
        // Acquire lock to serialize with other threads
        let _guard = THEME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_theme_internal(theme);
    }
}

/// Get the active theme.
pub fn current_theme() -> ThemeId {
    ThemeId::from_index(CURRENT_THEME.load(Ordering::Relaxed))
}

/// Get the active theme name.
pub fn current_theme_name() -> &'static str {
    current_theme().name()
}

/// Cycle to the next theme.
pub fn cycle_theme() -> ThemeId {
    let next = current_theme().next();
    set_theme(next);
    next
}

/// Return the palette for a theme.
pub fn palette(theme: ThemeId) -> &'static ThemePalette {
    &harmonized_palettes()[theme.index()]
}

/// Return the current palette.
pub fn current_palette() -> &'static ThemePalette {
    palette(current_theme())
}

/// Return the total number of themes.
pub const fn theme_count() -> usize {
    ThemeId::ALL.len()
}

fn harmonized_palettes() -> &'static [ThemePalette; ThemeId::ALL.len()] {
    HARMONIZED_THEMES.get_or_init(|| {
        ThemeId::ALL.map(|theme| harmonize_theme_palette(theme, THEMES[theme.index()]))
    })
}

/// Mutex for serializing theme access in tests.
///
/// Used by `ScopedThemeLock` to prevent race conditions when multiple
/// tests set different themes concurrently.
static THEME_TEST_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard for exclusive theme access during tests.
///
/// Acquires `THEME_TEST_LOCK`, sets the specified theme, and releases
/// the lock when dropped. This prevents race conditions in parallel tests
/// that read from the global theme state.
///
/// # Example
///
/// ```ignore
/// let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
/// // Theme is now CyberpunkAurora and other tests cannot change it
/// let checksum = render_to_checksum(&frame);
/// // Lock released when _guard is dropped
/// ```
pub struct ScopedThemeLock<'a> {
    _guard: MutexGuard<'a, ()>,
}

impl<'a> ScopedThemeLock<'a> {
    /// Create a new scoped theme lock, setting the specified theme.
    ///
    /// Blocks until the lock can be acquired. While held, other threads'
    /// calls to `set_theme()` or `ScopedThemeLock::new()` will block.
    #[must_use]
    pub fn new(theme: ThemeId) -> Self {
        let guard = THEME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Mark that current thread holds the lock (for reentrant set_theme calls)
        THEME_LOCK_HELD.with(|h| h.set(true));
        set_theme_internal(theme);
        Self { _guard: guard }
    }
}

impl Drop for ScopedThemeLock<'_> {
    fn drop(&mut self) {
        // Clear the thread-local flag when releasing the lock
        THEME_LOCK_HELD.with(|h| h.set(false));
    }
}

/// Token that resolves to a theme color at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorToken {
    BgDeep,
    BgBase,
    BgSurface,
    BgOverlay,
    BgHighlight,
    FgPrimary,
    FgSecondary,
    FgMuted,
    FgDisabled,
    AccentPrimary,
    AccentSecondary,
    AccentSuccess,
    AccentWarning,
    AccentError,
    AccentInfo,
    AccentLink,
    AccentSlot(usize),
    SyntaxKeyword,
    SyntaxString,
    SyntaxNumber,
    SyntaxComment,
    SyntaxFunction,
    SyntaxType,
    SyntaxOperator,
    SyntaxPunctuation,
    // Semantic colors (status / priority / issue type)
    StatusOpen,
    StatusInProgress,
    StatusBlocked,
    StatusClosed,
    PriorityP0,
    PriorityP1,
    PriorityP2,
    PriorityP3,
    PriorityP4,
    IssueBug,
    IssueFeature,
    IssueTask,
    IssueEpic,
}

impl ColorToken {
    pub fn resolve_in(self, palette: &ThemePalette) -> PackedRgba {
        match self {
            ColorToken::BgDeep => palette.bg_deep,
            ColorToken::BgBase => palette.bg_base,
            ColorToken::BgSurface => palette.bg_surface,
            ColorToken::BgOverlay => palette.bg_overlay,
            ColorToken::BgHighlight => palette.bg_highlight,
            ColorToken::FgPrimary => palette.fg_primary,
            ColorToken::FgSecondary => palette.fg_secondary,
            ColorToken::FgMuted => palette.fg_muted,
            ColorToken::FgDisabled => palette.fg_disabled,
            ColorToken::AccentPrimary => palette.accent_primary,
            ColorToken::AccentSecondary => palette.accent_secondary,
            ColorToken::AccentSuccess => palette.accent_success,
            ColorToken::AccentWarning => palette.accent_warning,
            ColorToken::AccentError => palette.accent_error,
            ColorToken::AccentInfo => palette.accent_info,
            ColorToken::AccentLink => palette.accent_link,
            ColorToken::AccentSlot(idx) => palette.accent_slots[idx % palette.accent_slots.len()],
            ColorToken::SyntaxKeyword => palette.syntax_keyword,
            ColorToken::SyntaxString => palette.syntax_string,
            ColorToken::SyntaxNumber => palette.syntax_number,
            ColorToken::SyntaxComment => palette.syntax_comment,
            ColorToken::SyntaxFunction => palette.syntax_function,
            ColorToken::SyntaxType => palette.syntax_type,
            ColorToken::SyntaxOperator => palette.syntax_operator,
            ColorToken::SyntaxPunctuation => palette.syntax_punctuation,
            ColorToken::StatusOpen => ensure_contrast(
                palette.accent_success,
                palette.bg_base,
                palette.fg_primary,
                palette.bg_deep,
            ),
            ColorToken::StatusInProgress => ensure_contrast(
                palette.accent_info,
                palette.bg_base,
                palette.fg_primary,
                palette.bg_deep,
            ),
            ColorToken::StatusBlocked => ensure_contrast(
                palette.accent_error,
                palette.bg_base,
                palette.fg_primary,
                palette.bg_deep,
            ),
            ColorToken::StatusClosed => ensure_contrast(
                palette.fg_muted,
                palette.bg_base,
                palette.fg_primary,
                palette.bg_deep,
            ),
            ColorToken::PriorityP0 => palette.accent_error,
            ColorToken::PriorityP1 => {
                blend_colors(palette.accent_warning, palette.accent_error, 0.6)
            }
            ColorToken::PriorityP2 => palette.accent_warning,
            ColorToken::PriorityP3 => palette.accent_info,
            ColorToken::PriorityP4 => palette.fg_muted,
            ColorToken::IssueBug => palette.accent_error,
            ColorToken::IssueFeature => palette.accent_secondary,
            ColorToken::IssueTask => palette.accent_primary,
            ColorToken::IssueEpic => palette.accent_warning,
        }
    }

    pub fn resolve(self) -> PackedRgba {
        self.resolve_in(current_palette())
    }
}

impl From<ColorToken> for PackedRgba {
    fn from(token: ColorToken) -> Self {
        token.resolve()
    }
}

/// A theme color with explicit alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlphaColor {
    base: ColorToken,
    alpha: u8,
}

impl AlphaColor {
    pub const fn new(base: ColorToken, alpha: u8) -> Self {
        Self { base, alpha }
    }

    pub fn resolve(self) -> PackedRgba {
        let base = self.base.resolve();
        PackedRgba::rgba(base.r(), base.g(), base.b(), self.alpha)
    }
}

impl From<AlphaColor> for PackedRgba {
    fn from(token: AlphaColor) -> Self {
        token.resolve()
    }
}

/// Apply an explicit alpha to a theme token.
pub fn with_alpha(token: ColorToken, alpha: u8) -> PackedRgba {
    AlphaColor::new(token, alpha).resolve()
}

/// Apply a floating opacity to a theme token.
pub fn with_opacity(token: ColorToken, opacity: f32) -> PackedRgba {
    token.resolve().with_opacity(opacity)
}

/// Blend a themed overlay over a base color using source-over.
pub fn blend_over(overlay: ColorToken, base: ColorToken, opacity: f32) -> PackedRgba {
    overlay.resolve().with_opacity(opacity).over(base.resolve())
}

/// Blend raw colors using source-over.
pub fn blend_colors(overlay: PackedRgba, base: PackedRgba, opacity: f32) -> PackedRgba {
    overlay.with_opacity(opacity).over(base)
}

/// Sample a smooth, repeating gradient over the current theme's accent slots.
///
/// `t` is periodic with period 1.0 (i.e., `t=0.0` and `t=1.0` return the same color).
///
/// This is intended for visual polish (sparklines, animated accents, demo effects) while
/// staying coherent with the active theme.
pub fn accent_gradient(t: f64) -> PackedRgba {
    let slots = &current_palette().accent_slots;
    let t = t.rem_euclid(1.0);
    let t = t.clamp(0.0, 1.0);
    if slots.is_empty() {
        return accent::PRIMARY.resolve();
    }

    if slots.len() == 1 {
        return slots[0];
    }

    let max_idx = slots.len() - 1;
    let pos = t * max_idx as f64;
    let idx = (pos.floor() as usize).min(max_idx);
    let frac = pos - idx as f64;

    let a = slots[idx];
    let b = slots[(idx + 1).min(max_idx)];

    let r = (a.r() as f64 + (b.r() as f64 - a.r() as f64) * frac).round() as u8;
    let g = (a.g() as f64 + (b.g() as f64 - a.g() as f64) * frac).round() as u8;
    let b_val = (a.b() as f64 + (b.b() as f64 - a.b() as f64) * frac).round() as u8;
    PackedRgba::rgb(r, g, b_val)
}

fn color_distance(a: PackedRgba, b: PackedRgba) -> u16 {
    let dr = i16::from(a.r()) - i16::from(b.r());
    let dg = i16::from(a.g()) - i16::from(b.g());
    let db = i16::from(a.b()) - i16::from(b.b());
    dr.unsigned_abs() + dg.unsigned_abs() + db.unsigned_abs()
}

fn ensure_min_contrast(
    fg: PackedRgba,
    bg: PackedRgba,
    light: PackedRgba,
    dark: PackedRgba,
    min_ratio: f64,
) -> PackedRgba {
    let current = contrast::contrast_ratio(fg, bg);
    if current >= min_ratio {
        return fg;
    }

    let target = if contrast::relative_luminance(bg) < 0.5 {
        light
    } else {
        dark
    };

    let mut best = fg;
    let mut best_ratio = current;
    for step in 1..=16 {
        let t = step as f32 / 16.0;
        let candidate = blend_colors(target, fg, t);
        let ratio = contrast::contrast_ratio(candidate, bg);
        if ratio > best_ratio {
            best = candidate;
            best_ratio = ratio;
        }
        if ratio >= min_ratio {
            return candidate;
        }
    }
    best
}

fn harmonize_theme_palette(theme: ThemeId, base: ThemePalette) -> ThemePalette {
    if theme == ThemeId::HighContrast {
        return base;
    }

    let mut p = base;
    let is_light = contrast::relative_luminance(p.bg_base) >= 0.45;
    let dark_fallback = PackedRgba::rgb(10, 12, 18);
    let light_fallback = PackedRgba::rgb(244, 247, 252);

    // Keep a coherent 5-step background ladder around bg_base.
    p.bg_deep = if is_light {
        blend_colors(PackedRgba::WHITE, p.bg_base, 0.22)
    } else {
        blend_colors(PackedRgba::BLACK, p.bg_base, 0.24)
    };
    p.bg_surface = if is_light {
        blend_colors(PackedRgba::BLACK, p.bg_base, 0.06)
    } else {
        blend_colors(PackedRgba::WHITE, p.bg_base, 0.08)
    };
    p.bg_overlay = if is_light {
        blend_colors(PackedRgba::BLACK, p.bg_base, 0.12)
    } else {
        blend_colors(PackedRgba::WHITE, p.bg_base, 0.16)
    };
    p.bg_highlight = if is_light {
        blend_colors(PackedRgba::BLACK, p.bg_base, 0.22)
    } else {
        blend_colors(PackedRgba::WHITE, p.bg_base, 0.28)
    };

    // Harmonized text hierarchy.
    p.fg_primary = ensure_min_contrast(p.fg_primary, p.bg_base, light_fallback, dark_fallback, 5.6);
    p.fg_secondary = ensure_min_contrast(
        blend_colors(p.bg_base, p.fg_primary, 0.22),
        p.bg_base,
        light_fallback,
        dark_fallback,
        4.6,
    );
    p.fg_muted = ensure_min_contrast(
        blend_colors(p.bg_base, p.fg_primary, 0.42),
        p.bg_base,
        light_fallback,
        dark_fallback,
        3.1,
    );
    p.fg_disabled = ensure_min_contrast(
        blend_colors(p.bg_base, p.fg_primary, 0.58),
        p.bg_base,
        light_fallback,
        dark_fallback,
        2.2,
    );

    // Harmonize semantic accents while preserving each theme's identity.
    p.accent_primary = ensure_min_contrast(
        p.accent_primary,
        p.bg_base,
        light_fallback,
        dark_fallback,
        3.2,
    );
    p.accent_secondary = ensure_min_contrast(
        p.accent_secondary,
        p.bg_base,
        light_fallback,
        dark_fallback,
        3.2,
    );
    p.accent_success = ensure_min_contrast(
        p.accent_success,
        p.bg_base,
        light_fallback,
        dark_fallback,
        3.2,
    );
    p.accent_warning = ensure_min_contrast(
        p.accent_warning,
        p.bg_base,
        light_fallback,
        dark_fallback,
        3.2,
    );
    p.accent_error = ensure_min_contrast(
        p.accent_error,
        p.bg_base,
        light_fallback,
        dark_fallback,
        3.2,
    );
    p.accent_info =
        ensure_min_contrast(p.accent_info, p.bg_base, light_fallback, dark_fallback, 3.2);
    p.accent_link =
        ensure_min_contrast(p.accent_link, p.bg_base, light_fallback, dark_fallback, 3.2);

    if color_distance(p.accent_primary, p.accent_secondary) < 30 {
        p.accent_secondary = ensure_min_contrast(
            blend_colors(p.accent_error, p.accent_secondary, 0.35),
            p.bg_base,
            light_fallback,
            dark_fallback,
            3.2,
        );
    }
    if color_distance(p.accent_success, p.accent_warning) < 30 {
        p.accent_warning = ensure_min_contrast(
            blend_colors(p.accent_error, p.accent_warning, 0.40),
            p.bg_base,
            light_fallback,
            dark_fallback,
            3.2,
        );
    }
    if color_distance(p.accent_link, p.accent_primary) < 24 {
        p.accent_link = ensure_min_contrast(
            blend_colors(p.accent_info, p.accent_primary, 0.55),
            p.bg_base,
            light_fallback,
            dark_fallback,
            3.2,
        );
    }

    // Shared, semantically ordered accent slots across all themes.
    p.accent_slots = [
        p.accent_primary,
        p.accent_secondary,
        p.accent_success,
        p.accent_warning,
        p.accent_error,
        p.accent_info,
        p.accent_link,
        ensure_min_contrast(
            blend_colors(p.accent_secondary, p.accent_primary, 0.50),
            p.bg_base,
            light_fallback,
            dark_fallback,
            3.0,
        ),
        ensure_min_contrast(
            blend_colors(p.accent_success, p.accent_info, 0.50),
            p.bg_base,
            light_fallback,
            dark_fallback,
            3.0,
        ),
        ensure_min_contrast(
            blend_colors(p.accent_warning, p.accent_error, 0.45),
            p.bg_base,
            light_fallback,
            dark_fallback,
            3.0,
        ),
        ensure_min_contrast(
            blend_colors(p.accent_primary, p.accent_warning, 0.45),
            p.bg_base,
            light_fallback,
            dark_fallback,
            3.0,
        ),
        ensure_min_contrast(
            blend_colors(p.accent_secondary, p.accent_success, 0.45),
            p.bg_base,
            light_fallback,
            dark_fallback,
            3.0,
        ),
    ];

    // Unified syntax semantics for consistency across themes.
    p.syntax_keyword = p.accent_secondary;
    p.syntax_string = p.accent_success;
    p.syntax_number = p.accent_warning;
    p.syntax_comment = p.fg_disabled;
    p.syntax_function = p.accent_primary;
    p.syntax_type = p.accent_info;
    p.syntax_operator = p.fg_secondary;
    p.syntax_punctuation = p.fg_muted;

    p
}

fn ensure_contrast(
    fg: PackedRgba,
    bg: PackedRgba,
    light: PackedRgba,
    dark: PackedRgba,
) -> PackedRgba {
    let (light, dark) = if contrast::relative_luminance(light) >= contrast::relative_luminance(dark)
    {
        (light, dark)
    } else {
        (dark, light)
    };

    if contrast::meets_wcag_aa(fg, bg) {
        return fg;
    }

    let target = if contrast::relative_luminance(bg) < 0.5 {
        light
    } else {
        dark
    };

    let mut best = fg;
    let mut best_ratio = contrast::contrast_ratio(fg, bg);
    for step in 1..=10 {
        let t = step as f32 / 10.0;
        let candidate = blend_colors(target, fg, t);
        let ratio = contrast::contrast_ratio(candidate, bg);
        if ratio > best_ratio {
            best = candidate;
            best_ratio = ratio;
        }
        if ratio >= 4.5 {
            return candidate;
        }
    }

    best
}

const SEMANTIC_TINT_OPACITY: f32 = 0.18;

fn semantic_tint(token: ColorToken) -> PackedRgba {
    with_opacity(token, SEMANTIC_TINT_OPACITY)
}

fn semantic_text(token: ColorToken) -> PackedRgba {
    let base_bg = bg::BASE.resolve();
    let tint = semantic_tint(token);
    let composed = tint.over(base_bg);
    let candidates = [
        fg::PRIMARY.resolve(),
        fg::SECONDARY.resolve(),
        bg::DEEP.resolve(),
        PackedRgba::WHITE,
        PackedRgba::BLACK,
    ];
    let best = contrast::best_text_color(composed, &candidates);
    // Ensure WCAG AA compliance - fall back to black or white if needed
    if contrast::meets_wcag_aa(best, composed) {
        best
    } else if contrast::contrast_ratio(PackedRgba::BLACK, composed)
        >= contrast::contrast_ratio(PackedRgba::WHITE, composed)
    {
        PackedRgba::BLACK
    } else {
        PackedRgba::WHITE
    }
}

/// Background colors.
pub mod bg {
    use super::ColorToken;

    pub const DEEP: ColorToken = ColorToken::BgDeep;
    pub const BASE: ColorToken = ColorToken::BgBase;
    pub const SURFACE: ColorToken = ColorToken::BgSurface;
    pub const OVERLAY: ColorToken = ColorToken::BgOverlay;
    pub const HIGHLIGHT: ColorToken = ColorToken::BgHighlight;
}

/// Foreground / text colors.
pub mod fg {
    use super::ColorToken;

    pub const PRIMARY: ColorToken = ColorToken::FgPrimary;
    pub const SECONDARY: ColorToken = ColorToken::FgSecondary;
    pub const MUTED: ColorToken = ColorToken::FgMuted;
    pub const DISABLED: ColorToken = ColorToken::FgDisabled;
}

/// Accent / semantic colors.
pub mod accent {
    use super::ColorToken;

    pub const PRIMARY: ColorToken = ColorToken::AccentPrimary;
    pub const SECONDARY: ColorToken = ColorToken::AccentSecondary;
    pub const SUCCESS: ColorToken = ColorToken::AccentSuccess;
    pub const WARNING: ColorToken = ColorToken::AccentWarning;
    pub const ERROR: ColorToken = ColorToken::AccentError;
    pub const INFO: ColorToken = ColorToken::AccentInfo;
    pub const LINK: ColorToken = ColorToken::AccentLink;

    pub const ACCENT_1: ColorToken = ColorToken::AccentSlot(0);
    pub const ACCENT_2: ColorToken = ColorToken::AccentSlot(1);
    pub const ACCENT_3: ColorToken = ColorToken::AccentSlot(2);
    pub const ACCENT_4: ColorToken = ColorToken::AccentSlot(3);
    pub const ACCENT_5: ColorToken = ColorToken::AccentSlot(4);
    pub const ACCENT_6: ColorToken = ColorToken::AccentSlot(5);
    pub const ACCENT_7: ColorToken = ColorToken::AccentSlot(6);
    pub const ACCENT_8: ColorToken = ColorToken::AccentSlot(7);
    pub const ACCENT_9: ColorToken = ColorToken::AccentSlot(8);
    pub const ACCENT_10: ColorToken = ColorToken::AccentSlot(9);
    pub const ACCENT_11: ColorToken = ColorToken::AccentSlot(10);
    pub const ACCENT_12: ColorToken = ColorToken::AccentSlot(11);
}

/// Status colors (open / in-progress / blocked / closed).
pub mod status {
    use super::{semantic_text, semantic_tint, ColorToken, PackedRgba};

    pub const OPEN: ColorToken = ColorToken::StatusOpen;
    pub const IN_PROGRESS: ColorToken = ColorToken::StatusInProgress;
    pub const BLOCKED: ColorToken = ColorToken::StatusBlocked;
    pub const CLOSED: ColorToken = ColorToken::StatusClosed;

    pub fn open_bg() -> PackedRgba {
        semantic_tint(OPEN)
    }

    pub fn in_progress_bg() -> PackedRgba {
        semantic_tint(IN_PROGRESS)
    }

    pub fn blocked_bg() -> PackedRgba {
        semantic_tint(BLOCKED)
    }

    pub fn closed_bg() -> PackedRgba {
        semantic_tint(CLOSED)
    }

    pub fn open_text() -> PackedRgba {
        semantic_text(OPEN)
    }

    pub fn in_progress_text() -> PackedRgba {
        semantic_text(IN_PROGRESS)
    }

    pub fn blocked_text() -> PackedRgba {
        semantic_text(BLOCKED)
    }

    pub fn closed_text() -> PackedRgba {
        semantic_text(CLOSED)
    }
}

/// Priority colors (P0-P4).
pub mod priority {
    use super::{semantic_text, semantic_tint, ColorToken, PackedRgba};

    pub const P0: ColorToken = ColorToken::PriorityP0;
    pub const P1: ColorToken = ColorToken::PriorityP1;
    pub const P2: ColorToken = ColorToken::PriorityP2;
    pub const P3: ColorToken = ColorToken::PriorityP3;
    pub const P4: ColorToken = ColorToken::PriorityP4;

    pub fn p0_bg() -> PackedRgba {
        semantic_tint(P0)
    }

    pub fn p1_bg() -> PackedRgba {
        semantic_tint(P1)
    }

    pub fn p2_bg() -> PackedRgba {
        semantic_tint(P2)
    }

    pub fn p3_bg() -> PackedRgba {
        semantic_tint(P3)
    }

    pub fn p4_bg() -> PackedRgba {
        semantic_tint(P4)
    }

    pub fn p0_text() -> PackedRgba {
        semantic_text(P0)
    }

    pub fn p1_text() -> PackedRgba {
        semantic_text(P1)
    }

    pub fn p2_text() -> PackedRgba {
        semantic_text(P2)
    }

    pub fn p3_text() -> PackedRgba {
        semantic_text(P3)
    }

    pub fn p4_text() -> PackedRgba {
        semantic_text(P4)
    }
}

/// Issue type colors (bug / feature / task / epic).
pub mod issue_type {
    use super::{semantic_text, semantic_tint, ColorToken, PackedRgba};

    pub const BUG: ColorToken = ColorToken::IssueBug;
    pub const FEATURE: ColorToken = ColorToken::IssueFeature;
    pub const TASK: ColorToken = ColorToken::IssueTask;
    pub const EPIC: ColorToken = ColorToken::IssueEpic;

    pub fn bug_bg() -> PackedRgba {
        semantic_tint(BUG)
    }

    pub fn feature_bg() -> PackedRgba {
        semantic_tint(FEATURE)
    }

    pub fn task_bg() -> PackedRgba {
        semantic_tint(TASK)
    }

    pub fn epic_bg() -> PackedRgba {
        semantic_tint(EPIC)
    }

    pub fn bug_text() -> PackedRgba {
        semantic_text(BUG)
    }

    pub fn feature_text() -> PackedRgba {
        semantic_text(FEATURE)
    }

    pub fn task_text() -> PackedRgba {
        semantic_text(TASK)
    }

    pub fn epic_text() -> PackedRgba {
        semantic_text(EPIC)
    }
}

/// Intent colors (success / warning / info / error).
pub mod intent {
    use super::{accent, semantic_text, semantic_tint, ColorToken, PackedRgba};

    pub const SUCCESS: ColorToken = accent::SUCCESS;
    pub const WARNING: ColorToken = accent::WARNING;
    pub const INFO: ColorToken = accent::INFO;
    pub const ERROR: ColorToken = accent::ERROR;

    pub fn success_bg() -> PackedRgba {
        semantic_tint(SUCCESS)
    }

    pub fn warning_bg() -> PackedRgba {
        semantic_tint(WARNING)
    }

    pub fn info_bg() -> PackedRgba {
        semantic_tint(INFO)
    }

    pub fn error_bg() -> PackedRgba {
        semantic_tint(ERROR)
    }

    pub fn success_text() -> PackedRgba {
        semantic_text(SUCCESS)
    }

    pub fn warning_text() -> PackedRgba {
        semantic_text(WARNING)
    }

    pub fn info_text() -> PackedRgba {
        semantic_text(INFO)
    }

    pub fn error_text() -> PackedRgba {
        semantic_text(ERROR)
    }
}

/// Alpha-aware overlay colors.
pub mod alpha {
    use super::{accent, bg, AlphaColor};

    pub const SURFACE: AlphaColor = AlphaColor::new(bg::SURFACE, 220);
    pub const OVERLAY: AlphaColor = AlphaColor::new(bg::OVERLAY, 210);
    pub const HIGHLIGHT: AlphaColor = AlphaColor::new(bg::HIGHLIGHT, 200);

    pub const ACCENT_PRIMARY: AlphaColor = AlphaColor::new(accent::PRIMARY, 210);
    pub const ACCENT_SECONDARY: AlphaColor = AlphaColor::new(accent::SECONDARY, 200);
}

/// Syntax highlighting colors.
pub mod syntax {
    use super::ColorToken;

    pub const KEYWORD: ColorToken = ColorToken::SyntaxKeyword;
    pub const STRING: ColorToken = ColorToken::SyntaxString;
    pub const NUMBER: ColorToken = ColorToken::SyntaxNumber;
    pub const COMMENT: ColorToken = ColorToken::SyntaxComment;
    pub const FUNCTION: ColorToken = ColorToken::SyntaxFunction;
    pub const TYPE: ColorToken = ColorToken::SyntaxType;
    pub const OPERATOR: ColorToken = ColorToken::SyntaxOperator;
    pub const PUNCTUATION: ColorToken = ColorToken::SyntaxPunctuation;
}

/// Contrast utilities (WCAG AA).
pub mod contrast {
    use super::PackedRgba;

    const WCAG_AA_CONTRAST: f64 = 4.5;

    pub fn srgb_to_linear(c: f64) -> f64 {
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    pub fn relative_luminance(color: PackedRgba) -> f64 {
        let r = srgb_to_linear(color.r() as f64 / 255.0);
        let g = srgb_to_linear(color.g() as f64 / 255.0);
        let b = srgb_to_linear(color.b() as f64 / 255.0);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    pub fn contrast_ratio(fg: PackedRgba, bg: PackedRgba) -> f64 {
        let lum_fg = relative_luminance(fg);
        let lum_bg = relative_luminance(bg);
        let lighter = lum_fg.max(lum_bg);
        let darker = lum_fg.min(lum_bg);
        (lighter + 0.05) / (darker + 0.05)
    }

    pub fn meets_wcag_aa(fg: PackedRgba, bg: PackedRgba) -> bool {
        contrast_ratio(fg, bg) >= WCAG_AA_CONTRAST
    }

    pub fn best_text_color(bg: PackedRgba, candidates: &[PackedRgba]) -> PackedRgba {
        let mut best = candidates[0];
        let mut best_ratio = contrast_ratio(best, bg);
        for &candidate in candidates.iter().skip(1) {
            let ratio = contrast_ratio(candidate, bg);
            if ratio > best_ratio {
                best = candidate;
                best_ratio = ratio;
            }
        }
        best
    }
}

/// A semantic swatch with pre-computed styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemanticSwatch {
    pub fg: PackedRgba,
    pub bg: PackedRgba,
    pub text: PackedRgba,
    pub fg_style: Style,
    pub badge_style: Style,
}

impl SemanticSwatch {
    fn new(fg: PackedRgba, bg: PackedRgba, text: PackedRgba) -> Self {
        Self {
            fg,
            bg,
            text,
            fg_style: Style::new().fg(fg),
            badge_style: Style::new().fg(text).bg(bg).bold(),
        }
    }

    fn from_token_in(
        token: ColorToken,
        palette: &ThemePalette,
        base_bg: PackedRgba,
        opacity: f32,
    ) -> Self {
        let fg = token.resolve_in(palette);
        let bg = fg.with_opacity(opacity);
        let composed = bg.over(base_bg);
        let candidates = [
            palette.fg_primary,
            palette.fg_secondary,
            palette.bg_deep,
            PackedRgba::WHITE,
            PackedRgba::BLACK,
        ];
        let text = contrast::best_text_color(composed, &candidates);
        Self::new(fg, bg, text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusStyles {
    pub open: SemanticSwatch,
    pub in_progress: SemanticSwatch,
    pub blocked: SemanticSwatch,
    pub closed: SemanticSwatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PriorityStyles {
    pub p0: SemanticSwatch,
    pub p1: SemanticSwatch,
    pub p2: SemanticSwatch,
    pub p3: SemanticSwatch,
    pub p4: SemanticSwatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IssueTypeStyles {
    pub bug: SemanticSwatch,
    pub feature: SemanticSwatch,
    pub task: SemanticSwatch,
    pub epic: SemanticSwatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntentStyles {
    pub success: SemanticSwatch,
    pub warning: SemanticSwatch,
    pub info: SemanticSwatch,
    pub error: SemanticSwatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemanticStyles {
    pub status: StatusStyles,
    pub priority: PriorityStyles,
    pub issue_type: IssueTypeStyles,
    pub intent: IntentStyles,
}

/// Semantic status badge variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusBadge {
    Open,
    InProgress,
    Blocked,
    Closed,
}

/// Semantic priority badge variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PriorityBadge {
    P0,
    P1,
    P2,
    P3,
    P4,
}

/// Label + style for a semantic badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BadgeSpec {
    pub label: &'static str,
    pub style: Style,
}

static SEMANTIC_STYLES_ALL: OnceLock<[SemanticStyles; ThemeId::ALL.len()]> = OnceLock::new();

fn semantic_styles_for(theme: ThemeId) -> SemanticStyles {
    let palette = palette(theme);
    let base_bg = palette.bg_base;
    let opacity = SEMANTIC_TINT_OPACITY;
    SemanticStyles {
        status: StatusStyles {
            open: SemanticSwatch::from_token_in(status::OPEN, palette, base_bg, opacity),
            in_progress: SemanticSwatch::from_token_in(
                status::IN_PROGRESS,
                palette,
                base_bg,
                opacity,
            ),
            blocked: SemanticSwatch::from_token_in(status::BLOCKED, palette, base_bg, opacity),
            closed: SemanticSwatch::from_token_in(status::CLOSED, palette, base_bg, opacity),
        },
        priority: PriorityStyles {
            p0: SemanticSwatch::from_token_in(priority::P0, palette, base_bg, opacity),
            p1: SemanticSwatch::from_token_in(priority::P1, palette, base_bg, opacity),
            p2: SemanticSwatch::from_token_in(priority::P2, palette, base_bg, opacity),
            p3: SemanticSwatch::from_token_in(priority::P3, palette, base_bg, opacity),
            p4: SemanticSwatch::from_token_in(priority::P4, palette, base_bg, opacity),
        },
        issue_type: IssueTypeStyles {
            bug: SemanticSwatch::from_token_in(issue_type::BUG, palette, base_bg, opacity),
            feature: SemanticSwatch::from_token_in(issue_type::FEATURE, palette, base_bg, opacity),
            task: SemanticSwatch::from_token_in(issue_type::TASK, palette, base_bg, opacity),
            epic: SemanticSwatch::from_token_in(issue_type::EPIC, palette, base_bg, opacity),
        },
        intent: IntentStyles {
            success: SemanticSwatch::from_token_in(intent::SUCCESS, palette, base_bg, opacity),
            warning: SemanticSwatch::from_token_in(intent::WARNING, palette, base_bg, opacity),
            info: SemanticSwatch::from_token_in(intent::INFO, palette, base_bg, opacity),
            error: SemanticSwatch::from_token_in(intent::ERROR, palette, base_bg, opacity),
        },
    }
}

/// Pre-compute semantic styles for the current theme.
pub fn semantic_styles() -> SemanticStyles {
    *semantic_styles_cached()
}

/// Build a semantic status badge (label + style) for the current theme.
#[must_use]
pub fn status_badge(status: StatusBadge) -> BadgeSpec {
    let styles = semantic_styles();
    match status {
        StatusBadge::Open => BadgeSpec {
            label: "OPEN",
            style: styles.status.open.badge_style,
        },
        StatusBadge::InProgress => BadgeSpec {
            label: "PROG",
            style: styles.status.in_progress.badge_style,
        },
        StatusBadge::Blocked => BadgeSpec {
            label: "BLKD",
            style: styles.status.blocked.badge_style,
        },
        StatusBadge::Closed => BadgeSpec {
            label: "DONE",
            style: styles.status.closed.badge_style,
        },
    }
}

/// Build a semantic priority badge (label + style) for the current theme.
#[must_use]
pub fn priority_badge(priority: PriorityBadge) -> BadgeSpec {
    let styles = semantic_styles();
    match priority {
        PriorityBadge::P0 => BadgeSpec {
            label: "P0",
            style: styles.priority.p0.badge_style,
        },
        PriorityBadge::P1 => BadgeSpec {
            label: "P1",
            style: styles.priority.p1.badge_style,
        },
        PriorityBadge::P2 => BadgeSpec {
            label: "P2",
            style: styles.priority.p2.badge_style,
        },
        PriorityBadge::P3 => BadgeSpec {
            label: "P3",
            style: styles.priority.p3.badge_style,
        },
        PriorityBadge::P4 => BadgeSpec {
            label: "P4",
            style: styles.priority.p4.badge_style,
        },
    }
}

/// Borrow pre-computed semantic styles for the current theme (cached per built-in theme).
pub fn semantic_styles_cached() -> &'static SemanticStyles {
    let all = SEMANTIC_STYLES_ALL.get_or_init(|| ThemeId::ALL.map(semantic_styles_for));
    &all[current_theme().index()]
}

/// Build a syntax highlight theme from the active palette.
#[cfg(feature = "syntax")]
pub fn syntax_theme() -> HighlightTheme {
    HighlightTheme {
        keyword: Style::new().fg(syntax::KEYWORD).bold(),
        keyword_control: Style::new().fg(syntax::KEYWORD),
        keyword_type: Style::new().fg(syntax::TYPE),
        keyword_modifier: Style::new().fg(syntax::KEYWORD),
        string: Style::new().fg(syntax::STRING),
        string_escape: Style::new().fg(accent::WARNING),
        number: Style::new().fg(syntax::NUMBER),
        boolean: Style::new().fg(syntax::NUMBER),
        identifier: Style::new().fg(fg::PRIMARY),
        type_name: Style::new().fg(syntax::TYPE),
        constant: Style::new().fg(syntax::NUMBER),
        function: Style::new().fg(syntax::FUNCTION),
        macro_name: Style::new().fg(accent::SECONDARY),
        comment: Style::new().fg(syntax::COMMENT).italic(),
        comment_block: Style::new().fg(syntax::COMMENT).italic(),
        comment_doc: Style::new().fg(syntax::COMMENT).italic(),
        operator: Style::new().fg(syntax::OPERATOR),
        punctuation: Style::new().fg(syntax::PUNCTUATION),
        delimiter: Style::new().fg(syntax::PUNCTUATION),
        attribute: Style::new().fg(accent::INFO),
        lifetime: Style::new().fg(accent::WARNING),
        label: Style::new().fg(accent::WARNING),
        heading: Style::new().fg(accent::PRIMARY).bold(),
        link: Style::new().fg(accent::LINK).underline(),
        emphasis: Style::new().italic(),
        whitespace: Style::new(),
        error: Style::new().fg(accent::ERROR).bold(),
        text: Style::new().fg(fg::PRIMARY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_rotation_wraps() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        // CyberpunkAurora -> Darcula
        set_theme(ThemeId::CyberpunkAurora);
        assert_eq!(cycle_theme(), ThemeId::Darcula);
        // NordicFrost -> Doom
        set_theme(ThemeId::NordicFrost);
        assert_eq!(cycle_theme(), ThemeId::Doom);
        // PaperColorLight -> HighContrast
        set_theme(ThemeId::PaperColorLight);
        assert_eq!(cycle_theme(), ThemeId::HighContrast);
        // HighContrast -> CyberpunkAurora (wraps)
        assert_eq!(cycle_theme(), ThemeId::CyberpunkAurora);
    }

    #[test]
    fn token_resolves_from_palette() {
        let _guard = ScopedThemeLock::new(ThemeId::Darcula);
        let color: PackedRgba = fg::PRIMARY.into();
        assert_eq!(color, palette(ThemeId::Darcula).fg_primary);
    }

    #[test]
    fn alpha_color_preserves_channel_and_alpha() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let color = AlphaColor::new(bg::BASE, 123).resolve();
        let base = current_palette().bg_base;
        assert_eq!(color.r(), base.r());
        assert_eq!(color.g(), base.g());
        assert_eq!(color.b(), base.b());
        assert_eq!(color.a(), 123);
    }

    #[test]
    fn blend_over_matches_packed_rgba() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let blended = blend_over(accent::PRIMARY, bg::BASE, 0.5);
        let expected = accent::PRIMARY
            .resolve()
            .with_opacity(0.5)
            .over(bg::BASE.resolve());
        assert_eq!(blended, expected);
    }

    #[test]
    fn accent_gradient_wraps() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            assert_eq!(accent_gradient(0.0), accent_gradient(1.0));
            assert_eq!(accent_gradient(-1.0), accent_gradient(0.0));
        }
    }

    #[test]
    fn status_colors_have_valid_contrast() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let base = bg::BASE.resolve();
            let open = status::OPEN.resolve();
            let progress = status::IN_PROGRESS.resolve();
            let blocked = status::BLOCKED.resolve();
            let closed = status::CLOSED.resolve();
            assert!(
                contrast::contrast_ratio(open, base) >= 4.5,
                "OPEN contrast too low for {theme:?}"
            );
            assert!(
                contrast::contrast_ratio(progress, base) >= 4.5,
                "IN_PROGRESS contrast too low for {theme:?}"
            );
            assert!(
                contrast::contrast_ratio(blocked, base) >= 4.5,
                "BLOCKED contrast too low for {theme:?}"
            );
            assert!(
                contrast::contrast_ratio(closed, base) >= 4.5,
                "CLOSED contrast too low for {theme:?}"
            );
        }
    }

    #[test]
    fn priority_colors_distinct() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let colors = [
                priority::P0.resolve(),
                priority::P1.resolve(),
                priority::P2.resolve(),
                priority::P3.resolve(),
                priority::P4.resolve(),
            ];
            for i in 0..colors.len() {
                for j in (i + 1)..colors.len() {
                    assert_ne!(
                        colors[i], colors[j],
                        "Priority colors should be distinct for {theme:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn status_bg_variants_have_low_opacity() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            assert!(status::open_bg().a() < 128);
            assert!(status::in_progress_bg().a() < 128);
            assert!(status::blocked_bg().a() < 128);
            assert!(status::closed_bg().a() < 128);
        }
    }

    #[test]
    fn status_badge_text_meets_contrast() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let base = bg::BASE.resolve();
            let bg_open = status::open_bg().over(base);
            let text_open = status::open_text();
            assert!(
                contrast::meets_wcag_aa(text_open, bg_open),
                "OPEN badge contrast too low for {theme:?}"
            );
        }
    }

    #[test]
    fn semantic_styles_build_valid_badge_styles() {
        for theme in ThemeId::ALL {
            let styles = semantic_styles_for(theme);
            let base_bg = palette(theme).bg_base;

            let swatches = [
                styles.status.open,
                styles.status.in_progress,
                styles.status.blocked,
                styles.status.closed,
                styles.priority.p0,
                styles.priority.p1,
                styles.priority.p2,
                styles.priority.p3,
                styles.priority.p4,
                styles.issue_type.bug,
                styles.issue_type.feature,
                styles.issue_type.task,
                styles.issue_type.epic,
                styles.intent.success,
                styles.intent.warning,
                styles.intent.info,
                styles.intent.error,
            ];

            for swatch in swatches {
                assert!(
                    swatch.fg_style.fg.is_some(),
                    "missing fg_style.fg for {theme:?}"
                );
                assert!(
                    swatch.badge_style.fg.is_some(),
                    "missing badge_style.fg for {theme:?}"
                );
                assert!(
                    swatch.badge_style.bg.is_some(),
                    "missing badge_style.bg for {theme:?}"
                );

                let badge_bg = swatch.bg.over(base_bg);
                assert!(
                    contrast::meets_wcag_aa(swatch.text, badge_bg),
                    "badge text contrast too low for {theme:?}"
                );

                assert_ne!(
                    swatch.badge_style.fg, swatch.badge_style.bg,
                    "badge fg/bg should differ for {theme:?}"
                );
            }
        }
    }

    #[test]
    fn status_badge_labels_are_distinct() {
        let labels = [
            status_badge(StatusBadge::Open).label,
            status_badge(StatusBadge::InProgress).label,
            status_badge(StatusBadge::Blocked).label,
            status_badge(StatusBadge::Closed).label,
        ];
        for i in 0..labels.len() {
            for j in (i + 1)..labels.len() {
                assert_ne!(
                    labels[i], labels[j],
                    "status badge labels should be distinct"
                );
            }
        }
    }

    #[test]
    fn priority_badge_labels_and_colors_are_distinct() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let badges = [
                priority_badge(PriorityBadge::P0),
                priority_badge(PriorityBadge::P1),
                priority_badge(PriorityBadge::P2),
                priority_badge(PriorityBadge::P3),
                priority_badge(PriorityBadge::P4),
            ];

            let labels: Vec<_> = badges.iter().map(|b| b.label).collect();
            for i in 0..labels.len() {
                for j in (i + 1)..labels.len() {
                    assert_ne!(
                        labels[i], labels[j],
                        "priority badge labels should be distinct"
                    );
                }
            }

            let bgs: Vec<_> = badges.iter().map(|b| b.style.bg).collect();
            for i in 0..bgs.len() {
                for j in (i + 1)..bgs.len() {
                    assert_ne!(bgs[i], bgs[j], "priority badge backgrounds should differ");
                }
            }
        }
    }

    #[test]
    fn theme_id_index_round_trips() {
        for theme in ThemeId::ALL {
            assert_eq!(ThemeId::from_index(theme.index()), theme);
        }
    }

    #[test]
    fn theme_id_from_index_wraps() {
        let n = ThemeId::ALL.len();
        assert_eq!(ThemeId::from_index(n), ThemeId::CyberpunkAurora);
        assert_eq!(ThemeId::from_index(n + 2), ThemeId::LumenLight);
    }

    #[test]
    fn theme_id_names_are_non_empty_and_distinct() {
        let names: Vec<&str> = ThemeId::ALL.iter().map(|t| t.name()).collect();
        for name in &names {
            assert!(!name.is_empty());
        }
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                assert_ne!(names[i], names[j]);
            }
        }
    }

    #[test]
    fn theme_id_next_visits_all_then_wraps() {
        let mut t = ThemeId::CyberpunkAurora;
        let mut visited = vec![t];
        for _ in 0..ThemeId::ALL.len() {
            t = t.next();
            visited.push(t);
        }
        // After a full cycle, should wrap back to start.
        assert_eq!(*visited.last().unwrap(), ThemeId::CyberpunkAurora);
        // Should have visited all unique themes exactly once before wrapping.
        let unique: std::collections::HashSet<_> = visited[..ThemeId::ALL.len()].iter().collect();
        assert_eq!(unique.len(), ThemeId::ALL.len());
    }

    #[test]
    fn theme_id_next_non_accessibility_skips_high_contrast() {
        // High contrast hops into the standard rotation.
        assert_eq!(
            ThemeId::HighContrast.next_non_accessibility(),
            ThemeId::Darcula
        );
        // Standard themes cycle through STANDARD array.
        assert_eq!(
            ThemeId::CyberpunkAurora.next_non_accessibility(),
            ThemeId::Darcula
        );
        assert_eq!(ThemeId::NordicFrost.next_non_accessibility(), ThemeId::Doom);
        assert_eq!(ThemeId::NightOwl.next_non_accessibility(), ThemeId::Dracula);
        assert_eq!(ThemeId::HorizonDark.next_non_accessibility(), ThemeId::Nord);
        assert_eq!(
            ThemeId::PaperColorLight.next_non_accessibility(),
            ThemeId::CyberpunkAurora
        );
    }

    #[test]
    fn theme_count_matches_all() {
        assert_eq!(theme_count(), ThemeId::ALL.len());
        assert!(theme_count() >= 10);
    }

    #[test]
    fn accent_slot_wraps_index() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let pal = current_palette();
        // AccentSlot(12) should wrap to AccentSlot(0)
        let slot0 = ColorToken::AccentSlot(0).resolve_in(pal);
        let slot12 = ColorToken::AccentSlot(12).resolve_in(pal);
        assert_eq!(slot0, slot12);
        // AccentSlot(13) should wrap to AccentSlot(1)
        let slot1 = ColorToken::AccentSlot(1).resolve_in(pal);
        let slot13 = ColorToken::AccentSlot(13).resolve_in(pal);
        assert_eq!(slot1, slot13);
    }

    #[test]
    fn with_alpha_sets_alpha_channel() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let color = with_alpha(accent::PRIMARY, 128);
        assert_eq!(color.a(), 128);
        let base = accent::PRIMARY.resolve();
        assert_eq!(color.r(), base.r());
    }

    #[test]
    fn with_opacity_zero_is_transparent() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let color = with_opacity(accent::PRIMARY, 0.0);
        assert_eq!(color.a(), 0);
    }

    #[test]
    fn contrast_srgb_to_linear_boundaries() {
        // 0.0 maps to 0.0
        assert!((contrast::srgb_to_linear(0.0) - 0.0).abs() < 1e-10);
        // 1.0 maps to 1.0
        assert!((contrast::srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        // Threshold boundary (~0.03928)
        let below = contrast::srgb_to_linear(0.03);
        let above = contrast::srgb_to_linear(0.04);
        assert!(below < above);
    }

    #[test]
    fn contrast_luminance_black_and_white() {
        let black_lum = contrast::relative_luminance(PackedRgba::BLACK);
        let white_lum = contrast::relative_luminance(PackedRgba::WHITE);
        assert!(black_lum < 0.01, "black luminance should be near 0");
        assert!(white_lum > 0.99, "white luminance should be near 1");
    }

    #[test]
    fn contrast_ratio_black_on_white_is_21() {
        let ratio = contrast::contrast_ratio(PackedRgba::BLACK, PackedRgba::WHITE);
        assert!(
            (ratio - 21.0).abs() < 0.1,
            "black-on-white contrast should be ~21:1, got {ratio}"
        );
    }

    #[test]
    fn contrast_best_text_picks_highest_ratio() {
        let bg = PackedRgba::rgb(128, 128, 128);
        let candidates = [PackedRgba::BLACK, PackedRgba::WHITE];
        let best = contrast::best_text_color(bg, &candidates);
        // On medium gray, either black or white should win; white typically has higher contrast
        let ratio_best = contrast::contrast_ratio(best, bg);
        for &c in &candidates {
            assert!(ratio_best >= contrast::contrast_ratio(c, bg) - 0.01);
        }
    }

    #[test]
    fn scoped_theme_lock_allows_reentrant_set_theme() {
        let _guard = ScopedThemeLock::new(ThemeId::Darcula);
        assert_eq!(current_theme(), ThemeId::Darcula);
        // set_theme within the lock scope should succeed (reentrant)
        set_theme(ThemeId::NordicFrost);
        assert_eq!(current_theme(), ThemeId::NordicFrost);
    }

    #[test]
    fn intent_bg_and_text_return_valid_colors() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let base = bg::BASE.resolve();
            // Each intent bg should have low alpha (tint)
            assert!(intent::success_bg().a() < 128);
            assert!(intent::warning_bg().a() < 128);
            assert!(intent::info_bg().a() < 128);
            assert!(intent::error_bg().a() < 128);
            // Text colors should meet contrast over composed bg
            let bg_success = intent::success_bg().over(base);
            let text = intent::success_text();
            assert!(
                contrast::meets_wcag_aa(text, bg_success),
                "intent success text contrast too low for {theme:?}"
            );
        }
    }

    #[test]
    fn issue_type_bg_and_text_return_valid_colors() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let base = bg::BASE.resolve();
            // Each issue type bg should have low alpha (tint)
            assert!(issue_type::bug_bg().a() < 128);
            assert!(issue_type::feature_bg().a() < 128);
            assert!(issue_type::task_bg().a() < 128);
            assert!(issue_type::epic_bg().a() < 128);
            // Text should meet contrast
            let bg_bug = issue_type::bug_bg().over(base);
            let text = issue_type::bug_text();
            assert!(
                contrast::meets_wcag_aa(text, bg_bug),
                "issue bug text contrast too low for {theme:?}"
            );
        }
    }

    #[test]
    fn all_palettes_have_12_accent_slots() {
        for theme in ThemeId::ALL {
            let pal = palette(theme);
            assert_eq!(pal.accent_slots.len(), 12);
        }
    }

    #[test]
    fn harmonized_slots_follow_semantic_order() {
        for theme in ThemeId::STANDARD {
            let pal = palette(theme);
            assert_eq!(pal.accent_slots[0], pal.accent_primary);
            assert_eq!(pal.accent_slots[1], pal.accent_secondary);
            assert_eq!(pal.accent_slots[2], pal.accent_success);
            assert_eq!(pal.accent_slots[3], pal.accent_warning);
            assert_eq!(pal.accent_slots[4], pal.accent_error);
            assert_eq!(pal.accent_slots[5], pal.accent_info);
            assert_eq!(pal.accent_slots[6], pal.accent_link);
        }
    }

    #[test]
    fn harmonized_syntax_semantics_are_consistent() {
        for theme in ThemeId::STANDARD {
            let pal = palette(theme);
            assert_eq!(pal.syntax_keyword, pal.accent_secondary);
            assert_eq!(pal.syntax_string, pal.accent_success);
            assert_eq!(pal.syntax_number, pal.accent_warning);
            assert_eq!(pal.syntax_function, pal.accent_primary);
            assert_eq!(pal.syntax_type, pal.accent_info);
            assert_eq!(pal.syntax_operator, pal.fg_secondary);
            assert_eq!(pal.syntax_punctuation, pal.fg_muted);
        }
    }

    #[test]
    fn harmonized_text_hierarchy_has_ordered_contrast() {
        for theme in ThemeId::ALL {
            if theme == ThemeId::HighContrast {
                continue;
            }
            let pal = palette(theme);
            let c_primary = contrast::contrast_ratio(pal.fg_primary, pal.bg_base);
            let c_secondary = contrast::contrast_ratio(pal.fg_secondary, pal.bg_base);
            let c_muted = contrast::contrast_ratio(pal.fg_muted, pal.bg_base);
            assert!(
                c_primary >= c_secondary,
                "primary < secondary for {theme:?}"
            );
            assert!(c_secondary >= c_muted, "secondary < muted for {theme:?}");
        }
    }

    // ── Edge-case: ThemeId ───────────────────────────────────────────

    #[test]
    fn theme_id_all_has_expected_elements() {
        assert_eq!(ThemeId::ALL.len(), 41);
    }

    #[test]
    fn theme_id_standard_has_expected_elements_no_high_contrast() {
        assert_eq!(ThemeId::STANDARD.len(), 40);
        for &theme in &ThemeId::STANDARD {
            assert_ne!(theme, ThemeId::HighContrast);
        }
    }

    #[test]
    fn theme_id_from_index_large_wraps() {
        let n = ThemeId::ALL.len();
        assert_eq!(
            ThemeId::from_index(usize::MAX),
            ThemeId::ALL[usize::MAX % n]
        );
        assert_eq!(ThemeId::from_index(n), ThemeId::CyberpunkAurora);
        assert_eq!(ThemeId::from_index(n + 1), ThemeId::Darcula);
    }

    #[test]
    fn theme_id_next_non_accessibility_cycles_all_standard() {
        let mut t = ThemeId::CyberpunkAurora;
        let mut visited = vec![t];
        for _ in 0..ThemeId::STANDARD.len() {
            t = t.next_non_accessibility();
            visited.push(t);
        }
        assert_eq!(*visited.last().unwrap(), ThemeId::CyberpunkAurora);
        let unique: std::collections::HashSet<_> =
            visited[..ThemeId::STANDARD.len()].iter().collect();
        assert_eq!(unique.len(), ThemeId::STANDARD.len());
    }

    #[test]
    fn theme_id_copy_eq() {
        let a = ThemeId::Darcula;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn theme_id_debug() {
        let dbg = format!("{:?}", ThemeId::LumenLight);
        assert!(dbg.contains("LumenLight"));
    }

    #[test]
    fn theme_id_hash_consistency() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let hash = |t: ThemeId| {
            let mut h = DefaultHasher::new();
            t.hash(&mut h);
            h.finish()
        };

        assert_eq!(hash(ThemeId::Darcula), hash(ThemeId::Darcula));
        assert_ne!(hash(ThemeId::Darcula), hash(ThemeId::LumenLight));
    }

    #[test]
    fn theme_id_index_values() {
        assert_eq!(ThemeId::CyberpunkAurora.index(), 0);
        assert_eq!(ThemeId::Darcula.index(), 1);
        assert_eq!(ThemeId::LumenLight.index(), 2);
        assert_eq!(ThemeId::NordicFrost.index(), 3);
        assert_eq!(ThemeId::Doom.index(), 4);
        assert_eq!(ThemeId::Quake.index(), 5);
        assert_eq!(ThemeId::NightOwl.index(), 15);
        assert_eq!(ThemeId::Dracula.index(), 16);
        assert_eq!(ThemeId::HorizonDark.index(), 27);
        assert_eq!(ThemeId::Nord.index(), 28);
        assert_eq!(ThemeId::PaperColorLight.index(), 39);
        assert_eq!(ThemeId::HighContrast.index(), ThemeId::ALL.len() - 1);
    }

    // ── Edge-case: ColorToken ────────────────────────────────────────

    #[test]
    fn color_token_copy_eq() {
        let a = ColorToken::AccentPrimary;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn color_token_debug() {
        let dbg = format!("{:?}", ColorToken::FgPrimary);
        assert!(dbg.contains("FgPrimary"));
    }

    #[test]
    fn color_token_hash() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let hash = |t: ColorToken| {
            let mut h = DefaultHasher::new();
            t.hash(&mut h);
            h.finish()
        };

        assert_eq!(hash(ColorToken::BgBase), hash(ColorToken::BgBase));
        assert_ne!(hash(ColorToken::BgBase), hash(ColorToken::FgPrimary));
    }

    #[test]
    fn color_token_accent_slot_different_indices_differ() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let pal = current_palette();
        let s0 = ColorToken::AccentSlot(0).resolve_in(pal);
        let s1 = ColorToken::AccentSlot(1).resolve_in(pal);
        assert_ne!(s0, s1);
    }

    // ── Edge-case: AlphaColor ────────────────────────────────────────

    #[test]
    fn alpha_color_copy_eq() {
        let a = AlphaColor::new(accent::PRIMARY, 200);
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn alpha_color_debug() {
        let ac = AlphaColor::new(bg::BASE, 128);
        let dbg = format!("{:?}", ac);
        assert!(dbg.contains("AlphaColor"));
    }

    #[test]
    fn alpha_color_zero_alpha() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let color = AlphaColor::new(accent::PRIMARY, 0).resolve();
        assert_eq!(color.a(), 0);
    }

    #[test]
    fn alpha_color_max_alpha() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let color = AlphaColor::new(accent::PRIMARY, 255).resolve();
        assert_eq!(color.a(), 255);
        let base = accent::PRIMARY.resolve();
        assert_eq!(color.r(), base.r());
        assert_eq!(color.g(), base.g());
        assert_eq!(color.b(), base.b());
    }

    #[test]
    fn alpha_color_into_packed_rgba() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let ac = AlphaColor::new(fg::PRIMARY, 100);
        let direct: PackedRgba = ac.into();
        let resolved = ac.resolve();
        assert_eq!(direct, resolved);
    }

    // ── Edge-case: accent_gradient ───────────────────────────────────

    #[test]
    fn accent_gradient_at_zero_equals_first_slot() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let at_zero = accent_gradient(0.0);
        let first_slot = current_palette().accent_slots[0];
        assert_eq!(at_zero, first_slot);
    }

    #[test]
    fn accent_gradient_at_one_wraps_to_first_slot() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        // t=1.0 → rem_euclid(1.0) = 0.0 → first slot (wraps)
        let at_one = accent_gradient(1.0);
        let first_slot = current_palette().accent_slots[0];
        assert_eq!(at_one, first_slot);
    }

    #[test]
    fn accent_gradient_negative_wraps() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let neg = accent_gradient(-0.5);
        let pos = accent_gradient(0.5);
        assert_eq!(neg, pos);
    }

    #[test]
    fn accent_gradient_large_values_wrap() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let at_1000 = accent_gradient(1000.0);
        let at_0 = accent_gradient(0.0);
        assert_eq!(at_1000, at_0);
    }

    #[test]
    fn accent_gradient_midpoint_interpolates() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let at_mid = accent_gradient(0.5);
        assert_eq!(at_mid.a(), 255);
    }

    // ── Edge-case: blend_colors ──────────────────────────────────────

    #[test]
    fn blend_colors_opacity_zero_returns_base() {
        let base = PackedRgba::rgb(100, 150, 200);
        let overlay = PackedRgba::rgb(255, 0, 0);
        let result = blend_colors(overlay, base, 0.0);
        assert_eq!(result.r(), base.r());
        assert_eq!(result.g(), base.g());
        assert_eq!(result.b(), base.b());
    }

    #[test]
    fn blend_colors_opacity_one_returns_overlay() {
        let base = PackedRgba::rgb(100, 150, 200);
        let overlay = PackedRgba::rgb(255, 0, 0);
        let result = blend_colors(overlay, base, 1.0);
        assert_eq!(result.r(), overlay.r());
        assert_eq!(result.g(), overlay.g());
        assert_eq!(result.b(), overlay.b());
    }

    // ── Edge-case: with_opacity ──────────────────────────────────────

    #[test]
    fn with_opacity_one_is_opaque() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let color = with_opacity(accent::PRIMARY, 1.0);
        assert_eq!(color.a(), 255);
    }

    // ── Edge-case: contrast utilities ────────────────────────────────

    #[test]
    fn contrast_ratio_same_color_is_one() {
        let color = PackedRgba::rgb(128, 128, 128);
        let ratio = contrast::contrast_ratio(color, color);
        assert!(
            (ratio - 1.0).abs() < 0.01,
            "same-color ratio should be 1.0, got {ratio}"
        );
    }

    #[test]
    fn contrast_ratio_is_symmetric() {
        let a = PackedRgba::rgb(50, 100, 150);
        let b = PackedRgba::rgb(200, 220, 240);
        let r1 = contrast::contrast_ratio(a, b);
        let r2 = contrast::contrast_ratio(b, a);
        assert!(
            (r1 - r2).abs() < 0.001,
            "contrast ratio should be symmetric: {r1} vs {r2}"
        );
    }

    #[test]
    fn contrast_ratio_always_at_least_one() {
        let colors = [
            PackedRgba::BLACK,
            PackedRgba::WHITE,
            PackedRgba::rgb(128, 0, 0),
            PackedRgba::rgb(0, 128, 0),
            PackedRgba::rgb(0, 0, 128),
        ];
        for &a in &colors {
            for &b in &colors {
                let ratio = contrast::contrast_ratio(a, b);
                assert!(ratio >= 1.0, "contrast ratio should be >= 1.0, got {ratio}");
            }
        }
    }

    #[test]
    fn meets_wcag_aa_same_color_fails() {
        let color = PackedRgba::rgb(128, 128, 128);
        assert!(!contrast::meets_wcag_aa(color, color));
    }

    #[test]
    fn meets_wcag_aa_black_on_white_passes() {
        assert!(contrast::meets_wcag_aa(
            PackedRgba::BLACK,
            PackedRgba::WHITE
        ));
    }

    #[test]
    fn srgb_to_linear_at_threshold() {
        let below = contrast::srgb_to_linear(0.03928);
        let above = contrast::srgb_to_linear(0.03929);
        assert!((below - above).abs() < 0.001);
    }

    #[test]
    fn luminance_pure_red_green_blue() {
        let r = contrast::relative_luminance(PackedRgba::rgb(255, 0, 0));
        let g = contrast::relative_luminance(PackedRgba::rgb(0, 255, 0));
        let b = contrast::relative_luminance(PackedRgba::rgb(0, 0, 255));
        assert!(g > r, "green should be brighter than red");
        assert!(g > b, "green should be brighter than blue");
        assert!(r > b, "red should be brighter than blue");
    }

    // ── Edge-case: current_theme_name ────────────────────────────────

    #[test]
    fn current_theme_name_matches_current_theme() {
        let _guard = ScopedThemeLock::new(ThemeId::NordicFrost);
        assert_eq!(current_theme_name(), "Nordic Frost");
    }

    // ── Edge-case: palette retrieval ─────────────────────────────────

    #[test]
    fn palette_returns_different_palettes_per_theme() {
        let a = palette(ThemeId::CyberpunkAurora);
        let b = palette(ThemeId::Darcula);
        assert_ne!(a.bg_base, b.bg_base);
    }

    #[test]
    fn current_palette_matches_explicit_palette() {
        let _guard = ScopedThemeLock::new(ThemeId::LumenLight);
        let cp = current_palette();
        let ep = palette(ThemeId::LumenLight);
        assert_eq!(cp.bg_base, ep.bg_base);
        assert_eq!(cp.fg_primary, ep.fg_primary);
    }

    // ── Edge-case: semantic_styles_cached ─────────────────────────────

    #[test]
    fn semantic_styles_cached_matches_direct() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let cached = *semantic_styles_cached();
            let direct = semantic_styles();
            assert_eq!(cached, direct, "cached != direct for {:?}", theme);
        }
    }

    // ── Edge-case: badge specs ───────────────────────────────────────

    #[test]
    fn status_badge_labels_exact() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        assert_eq!(status_badge(StatusBadge::Open).label, "OPEN");
        assert_eq!(status_badge(StatusBadge::InProgress).label, "PROG");
        assert_eq!(status_badge(StatusBadge::Blocked).label, "BLKD");
        assert_eq!(status_badge(StatusBadge::Closed).label, "DONE");
    }

    #[test]
    fn priority_badge_labels_exact() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        assert_eq!(priority_badge(PriorityBadge::P0).label, "P0");
        assert_eq!(priority_badge(PriorityBadge::P1).label, "P1");
        assert_eq!(priority_badge(PriorityBadge::P2).label, "P2");
        assert_eq!(priority_badge(PriorityBadge::P3).label, "P3");
        assert_eq!(priority_badge(PriorityBadge::P4).label, "P4");
    }

    // ── Edge-case: trait coverage ────────────────────────────────────

    #[test]
    fn badge_spec_clone_debug() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let badge = status_badge(StatusBadge::Open);
        let cloned = badge;
        assert_eq!(badge, cloned);
        let dbg = format!("{:?}", badge);
        assert!(dbg.contains("BadgeSpec"));
    }

    #[test]
    fn semantic_swatch_clone_debug() {
        let styles = semantic_styles_for(ThemeId::CyberpunkAurora);
        let swatch = styles.status.open;
        let cloned = swatch;
        assert_eq!(swatch, cloned);
        let dbg = format!("{:?}", swatch);
        assert!(dbg.contains("SemanticSwatch"));
    }

    #[test]
    fn semantic_styles_debug() {
        let styles = semantic_styles_for(ThemeId::Darcula);
        let dbg = format!("{:?}", styles);
        assert!(dbg.contains("SemanticStyles"));
    }

    #[test]
    fn status_badge_enum_copy_debug() {
        let a = StatusBadge::Open;
        let b = a;
        assert_eq!(a, b);
        let dbg = format!("{:?}", a);
        assert!(dbg.contains("Open"));
    }

    #[test]
    fn priority_badge_enum_copy_debug() {
        let a = PriorityBadge::P0;
        let b = a;
        assert_eq!(a, b);
        let dbg = format!("{:?}", a);
        assert!(dbg.contains("P0"));
    }

    #[test]
    fn status_badge_hash() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let hash = |s: StatusBadge| {
            let mut h = DefaultHasher::new();
            s.hash(&mut h);
            h.finish()
        };

        assert_eq!(hash(StatusBadge::Open), hash(StatusBadge::Open));
        assert_ne!(hash(StatusBadge::Open), hash(StatusBadge::Closed));
    }

    #[test]
    fn theme_palette_debug() {
        let pal = palette(ThemeId::CyberpunkAurora);
        let dbg = format!("{:?}", pal);
        assert!(dbg.contains("ThemePalette"));
    }

    // ── Edge-case: priority bg/text ──────────────────────────────────

    #[test]
    fn priority_bg_has_low_opacity_all_themes() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            assert!(
                priority::p0_bg().a() < 128,
                "P0 bg alpha too high for {:?}",
                theme
            );
            assert!(
                priority::p1_bg().a() < 128,
                "P1 bg alpha too high for {:?}",
                theme
            );
            assert!(
                priority::p2_bg().a() < 128,
                "P2 bg alpha too high for {:?}",
                theme
            );
            assert!(
                priority::p3_bg().a() < 128,
                "P3 bg alpha too high for {:?}",
                theme
            );
            assert!(
                priority::p4_bg().a() < 128,
                "P4 bg alpha too high for {:?}",
                theme
            );
        }
    }

    #[test]
    fn priority_text_meets_contrast_all_themes() {
        for theme in ThemeId::ALL {
            let _guard = ScopedThemeLock::new(theme);
            let base = bg::BASE.resolve();
            let bg_p0 = priority::p0_bg().over(base);
            let text_p0 = priority::p0_text();
            assert!(
                contrast::meets_wcag_aa(text_p0, bg_p0),
                "P0 text contrast too low for {:?}",
                theme
            );
        }
    }

    // ── Edge-case: alpha module constants ─────────────────────────────

    #[test]
    fn alpha_module_constants_have_expected_alphas() {
        assert_eq!(alpha::SURFACE.alpha, 220);
        assert_eq!(alpha::OVERLAY.alpha, 210);
        assert_eq!(alpha::HIGHLIGHT.alpha, 200);
        assert_eq!(alpha::ACCENT_PRIMARY.alpha, 210);
        assert_eq!(alpha::ACCENT_SECONDARY.alpha, 200);
    }

    #[test]
    fn alpha_module_resolves_to_expected_alpha() {
        let _guard = ScopedThemeLock::new(ThemeId::CyberpunkAurora);
        let surface = alpha::SURFACE.resolve();
        assert_eq!(surface.a(), 220);
        let overlay = alpha::OVERLAY.resolve();
        assert_eq!(overlay.a(), 210);
    }

    // ── Edge-case: high contrast theme ───────────────────────────────

    #[test]
    fn high_contrast_theme_has_pure_black_bg() {
        let pal = palette(ThemeId::HighContrast);
        assert_eq!(pal.bg_deep, PackedRgba::rgb(0, 0, 0));
        assert_eq!(pal.bg_base, PackedRgba::rgb(0, 0, 0));
    }

    #[test]
    fn high_contrast_theme_has_pure_white_fg() {
        let pal = palette(ThemeId::HighContrast);
        assert_eq!(pal.fg_primary, PackedRgba::rgb(255, 255, 255));
    }

    #[test]
    fn high_contrast_max_contrast_ratio() {
        let pal = palette(ThemeId::HighContrast);
        let ratio = contrast::contrast_ratio(pal.fg_primary, pal.bg_base);
        assert!(
            (ratio - 21.0).abs() < 0.1,
            "HighContrast fg/bg should be ~21:1, got {ratio}"
        );
    }
}
