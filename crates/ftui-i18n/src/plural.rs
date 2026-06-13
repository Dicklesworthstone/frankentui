//! CLDR plural rules for locale-aware pluralization.
//!
//! Implements a subset of the Unicode CLDR plural rules covering the
//! most common language families. Each [`PluralRule`] maps an integer
//! count to a [`PluralCategory`].
//!
//! # Invariants
//!
//! 1. Every `PluralRule` must map any `i64` to exactly one `PluralCategory`.
//! 2. The `Other` category is always the catch-all fallback.
//! 3. Rules are pure functions: same count always yields same category.

use core::fmt;

/// CLDR plural categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl fmt::Display for PluralCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => write!(f, "zero"),
            Self::One => write!(f, "one"),
            Self::Two => write!(f, "two"),
            Self::Few => write!(f, "few"),
            Self::Many => write!(f, "many"),
            Self::Other => write!(f, "other"),
        }
    }
}

/// Plural form strings keyed by category.
#[derive(Debug, Clone, Default)]
pub struct PluralForms {
    pub zero: Option<String>,
    pub one: String,
    pub two: Option<String>,
    pub few: Option<String>,
    pub many: Option<String>,
    pub other: String,
}

impl PluralForms {
    /// Select the appropriate form for the given category, falling back
    /// to `other` if the specific form is absent.
    #[must_use]
    pub fn select(&self, category: PluralCategory) -> &str {
        match category {
            PluralCategory::Zero => self.zero.as_deref().unwrap_or(&self.other),
            PluralCategory::One => &self.one,
            PluralCategory::Two => self.two.as_deref().unwrap_or(&self.other),
            PluralCategory::Few => self.few.as_deref().unwrap_or(&self.other),
            PluralCategory::Many => self.many.as_deref().unwrap_or(&self.other),
            PluralCategory::Other => &self.other,
        }
    }
}

/// A plural rule function that maps a count to a plural category.
///
/// Built-in rules cover the most common CLDR language groups.
/// Custom rules can be provided via the function pointer variant.
#[derive(Clone)]
pub enum PluralRule {
    /// English-like: `one` for 1, `other` for everything else.
    English,
    /// Russian/Slavic: `one` for 1, `few` for 2-4, `many` for 5-20,
    /// then repeats based on last two digits.
    Russian,
    /// Arabic: `zero` for 0, `one` for 1, `two` for 2, `few` for 3-10,
    /// `many` for 11-99, `other` for 100+.
    Arabic,
    /// French-like: `one` for 0-1, `other` for everything else.
    French,
    /// Chinese/Japanese/Korean: always `other` (no plural distinction).
    CJK,
    /// Polish: similar to Russian but with different thresholds.
    Polish,
    /// Custom rule function.
    Custom(fn(i64) -> PluralCategory),
}

impl PluralRule {
    /// Determine the plural category for the given count.
    #[must_use]
    pub fn categorize(&self, count: i64) -> PluralCategory {
        let n = count.unsigned_abs();
        match self {
            Self::English => english_rule(n),
            Self::Russian => russian_rule(n),
            Self::Arabic => arabic_rule(n),
            Self::French => french_rule(n),
            Self::CJK => PluralCategory::Other,
            Self::Polish => polish_rule(n),
            Self::Custom(f) => f(count),
        }
    }

    /// Select the best rule for a locale tag (e.g., `"en"`, `"ru"`, `"ar"`).
    ///
    /// Falls back to English if the language is unknown.
    #[must_use]
    pub fn for_locale(lang: &str) -> Self {
        // Extract the primary language subtag
        let primary = lang.split(['-', '_']).next().unwrap_or(lang);

        match primary.to_ascii_lowercase().as_str() {
            "en" | "de" | "nl" | "sv" | "da" | "no" | "nb" | "nn" | "it" | "es" | "pt" | "el"
            | "hu" | "fi" | "et" | "he" | "tr" | "bg" => Self::English,
            "fr" | "hi" | "bn" => Self::French,
            "ru" | "uk" | "hr" | "sr" | "bs" => Self::Russian,
            "pl" => Self::Polish,
            "ar" => Self::Arabic,
            "zh" | "ja" | "ko" | "th" | "vi" | "id" | "ms" => Self::CJK,
            _ => Self::English,
        }
    }
}

impl fmt::Debug for PluralRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::English => write!(f, "PluralRule::English"),
            Self::Russian => write!(f, "PluralRule::Russian"),
            Self::Arabic => write!(f, "PluralRule::Arabic"),
            Self::French => write!(f, "PluralRule::French"),
            Self::CJK => write!(f, "PluralRule::CJK"),
            Self::Polish => write!(f, "PluralRule::Polish"),
            Self::Custom(_) => write!(f, "PluralRule::Custom(...)"),
        }
    }
}

// ── Rule implementations ────────────────────────────────────────────

fn english_rule(n: u64) -> PluralCategory {
    if n == 1 {
        PluralCategory::One
    } else {
        PluralCategory::Other
    }
}

fn french_rule(n: u64) -> PluralCategory {
    if n <= 1 {
        PluralCategory::One
    } else {
        PluralCategory::Other
    }
}

fn russian_rule(n: u64) -> PluralCategory {
    let mod10 = n % 10;
    let mod100 = n % 100;

    if mod10 == 1 && mod100 != 11 {
        PluralCategory::One
    } else if (2..=4).contains(&mod10) && !(12..=14).contains(&mod100) {
        PluralCategory::Few
    } else if mod10 == 0 || (5..=9).contains(&mod10) || (11..=14).contains(&mod100) {
        PluralCategory::Many
    } else {
        PluralCategory::Other
    }
}

fn polish_rule(n: u64) -> PluralCategory {
    let mod10 = n % 10;
    let mod100 = n % 100;

    if n == 1 {
        PluralCategory::One
    } else if (2..=4).contains(&mod10) && !(12..=14).contains(&mod100) {
        PluralCategory::Few
    } else {
        PluralCategory::Many
    }
}

fn arabic_rule(n: u64) -> PluralCategory {
    let mod100 = n % 100;
    match n {
        0 => PluralCategory::Zero,
        1 => PluralCategory::One,
        2 => PluralCategory::Two,
        _ if (3..=10).contains(&mod100) => PluralCategory::Few,
        _ if (11..=99).contains(&mod100) => PluralCategory::Many,
        _ => PluralCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_singular_plural() {
        let rule = PluralRule::English;
        assert_eq!(rule.categorize(0), PluralCategory::Other);
        assert_eq!(rule.categorize(1), PluralCategory::One);
        assert_eq!(rule.categorize(2), PluralCategory::Other);
        assert_eq!(rule.categorize(100), PluralCategory::Other);
    }

    #[test]
    fn french_zero_is_singular() {
        let rule = PluralRule::French;
        assert_eq!(rule.categorize(0), PluralCategory::One);
        assert_eq!(rule.categorize(1), PluralCategory::One);
        assert_eq!(rule.categorize(2), PluralCategory::Other);
    }

    #[test]
    fn russian_complex_rules() {
        let rule = PluralRule::Russian;
        assert_eq!(rule.categorize(1), PluralCategory::One);
        assert_eq!(rule.categorize(2), PluralCategory::Few);
        assert_eq!(rule.categorize(3), PluralCategory::Few);
        assert_eq!(rule.categorize(4), PluralCategory::Few);
        assert_eq!(rule.categorize(5), PluralCategory::Many);
        assert_eq!(rule.categorize(11), PluralCategory::Many);
        assert_eq!(rule.categorize(12), PluralCategory::Many);
        assert_eq!(rule.categorize(21), PluralCategory::One);
        assert_eq!(rule.categorize(22), PluralCategory::Few);
        assert_eq!(rule.categorize(25), PluralCategory::Many);
    }

    #[test]
    fn arabic_full_categories() {
        let rule = PluralRule::Arabic;
        assert_eq!(rule.categorize(0), PluralCategory::Zero);
        assert_eq!(rule.categorize(1), PluralCategory::One);
        assert_eq!(rule.categorize(2), PluralCategory::Two);
        assert_eq!(rule.categorize(5), PluralCategory::Few);
        assert_eq!(rule.categorize(11), PluralCategory::Many);
        assert_eq!(rule.categorize(100), PluralCategory::Other);
    }

    #[test]
    fn cjk_always_other() {
        let rule = PluralRule::CJK;
        for n in [0, 1, 2, 5, 100, 1000] {
            assert_eq!(rule.categorize(n), PluralCategory::Other);
        }
    }

    #[test]
    fn polish_rules() {
        let rule = PluralRule::Polish;
        assert_eq!(rule.categorize(1), PluralCategory::One);
        assert_eq!(rule.categorize(2), PluralCategory::Few);
        assert_eq!(rule.categorize(3), PluralCategory::Few);
        assert_eq!(rule.categorize(4), PluralCategory::Few);
        assert_eq!(rule.categorize(5), PluralCategory::Many);
        assert_eq!(rule.categorize(12), PluralCategory::Many);
        assert_eq!(rule.categorize(22), PluralCategory::Few);

        // CLDR `pl` diverges from `ru`: Polish `one` is ONLY exactly 1, so
        // `i % 10 == 1` values other than 1 are `many`, not `one`. (Russian
        // categorizes 21 as `one`; Polish categorizes it as `many`.) These
        // cases guard against "fixing" Polish to use the Russian rule.
        assert_eq!(rule.categorize(21), PluralCategory::Many);
        assert_eq!(rule.categorize(11), PluralCategory::Many);
        assert_eq!(rule.categorize(0), PluralCategory::Many);
        assert_eq!(rule.categorize(24), PluralCategory::Few);
        assert_eq!(rule.categorize(25), PluralCategory::Many);
    }

    #[test]
    fn locale_detection() {
        assert!(matches!(PluralRule::for_locale("en"), PluralRule::English));
        assert!(matches!(
            PluralRule::for_locale("en-US"),
            PluralRule::English
        ));
        assert!(matches!(PluralRule::for_locale("ru"), PluralRule::Russian));
        assert!(matches!(PluralRule::for_locale("fr"), PluralRule::French));
        assert!(matches!(PluralRule::for_locale("ar"), PluralRule::Arabic));
        assert!(matches!(PluralRule::for_locale("zh"), PluralRule::CJK));
        assert!(matches!(PluralRule::for_locale("ja"), PluralRule::CJK));
        assert!(matches!(
            PluralRule::for_locale("unknown"),
            PluralRule::English
        ));
    }

    #[test]
    fn plural_forms_select() {
        let forms = PluralForms {
            zero: Some("no items".into()),
            one: "1 item".into(),
            two: None,
            few: Some("a few items".into()),
            many: None,
            other: "many items".into(),
        };

        assert_eq!(forms.select(PluralCategory::Zero), "no items");
        assert_eq!(forms.select(PluralCategory::One), "1 item");
        assert_eq!(forms.select(PluralCategory::Two), "many items"); // falls back to other
        assert_eq!(forms.select(PluralCategory::Few), "a few items");
        assert_eq!(forms.select(PluralCategory::Many), "many items"); // falls back to other
        assert_eq!(forms.select(PluralCategory::Other), "many items");
    }

    #[test]
    fn custom_rule() {
        let rule = PluralRule::Custom(|n| {
            if n == 42 {
                PluralCategory::Few
            } else {
                PluralCategory::Other
            }
        });
        assert_eq!(rule.categorize(42), PluralCategory::Few);
        assert_eq!(rule.categorize(1), PluralCategory::Other);
    }

    #[test]
    fn negative_counts() {
        // Negative counts should use absolute value for built-in rules
        let rule = PluralRule::English;
        assert_eq!(rule.categorize(-1), PluralCategory::One);
        assert_eq!(rule.categorize(-2), PluralCategory::Other);
    }

    #[test]
    fn plural_category_display() {
        assert_eq!(PluralCategory::Zero.to_string(), "zero");
        assert_eq!(PluralCategory::One.to_string(), "one");
        assert_eq!(PluralCategory::Other.to_string(), "other");
    }
}
