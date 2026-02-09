//! Property-based invariant tests for the i18n subsystem.
//!
//! Verifies structural guarantees of plural rules, interpolation, and catalog:
//!
//! 1.  Every built-in plural rule always returns a valid PluralCategory
//! 2.  Plural rules are deterministic: same count → same category
//! 3.  CJK always returns Other for any count
//! 4.  English: One for ±1, Other otherwise
//! 5.  French: One for 0..=1, Other otherwise
//! 6.  Negative counts use absolute value for built-in rules
//! 7.  Interpolation with no placeholders is identity
//! 8.  Interpolation is idempotent (no recursive substitution)
//! 9.  Missing args leave placeholder tokens intact
//! 10. PluralForms::select always returns a non-empty str for category One/Other
//! 11. Catalog: missing key always returns None
//! 12. Catalog: format_plural auto-injects {count}
//! 13. for_locale never panics on arbitrary strings
//! 14. Coverage report coverage_percent is in [0, 100]

use ftui_i18n::catalog::{LocaleStrings, StringCatalog};
use ftui_i18n::plural::{PluralCategory, PluralForms, PluralRule};
use proptest::prelude::*;

// ── Helpers ──────────────────────────────────────────────────────────

fn all_built_in_rules() -> Vec<PluralRule> {
    vec![
        PluralRule::English,
        PluralRule::Russian,
        PluralRule::Arabic,
        PluralRule::French,
        PluralRule::CJK,
        PluralRule::Polish,
    ]
}

fn is_valid_category(cat: PluralCategory) -> bool {
    matches!(
        cat,
        PluralCategory::Zero
            | PluralCategory::One
            | PluralCategory::Two
            | PluralCategory::Few
            | PluralCategory::Many
            | PluralCategory::Other
    )
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Every built-in rule returns a valid category
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn all_rules_return_valid_category(count in any::<i64>()) {
        for rule in all_built_in_rules() {
            let cat = rule.categorize(count);
            prop_assert!(
                is_valid_category(cat),
                "rule {:?} returned invalid category {:?} for count {}",
                rule, cat, count
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Plural rules are deterministic
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn plural_rules_deterministic(count in any::<i64>()) {
        for rule in all_built_in_rules() {
            let a = rule.categorize(count);
            let b = rule.categorize(count);
            prop_assert_eq!(a, b, "rule {:?} non-deterministic for count {}", rule, count);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. CJK always returns Other
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn cjk_always_other(count in any::<i64>()) {
        let cat = PluralRule::CJK.categorize(count);
        prop_assert_eq!(cat, PluralCategory::Other, "CJK should always return Other");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. English: One for ±1, Other for everything else
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn english_one_or_other(count in any::<i64>()) {
        let cat = PluralRule::English.categorize(count);
        if count == 1 || count == -1 {
            prop_assert_eq!(cat, PluralCategory::One);
        } else {
            prop_assert_eq!(cat, PluralCategory::Other);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. French: One for |n| <= 1
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn french_zero_and_one_are_singular(count in any::<i64>()) {
        let cat = PluralRule::French.categorize(count);
        let abs = count.unsigned_abs();
        if abs <= 1 {
            prop_assert_eq!(cat, PluralCategory::One, "French: |{}| <= 1 should be One", count);
        } else {
            prop_assert_eq!(cat, PluralCategory::Other, "French: |{}| > 1 should be Other", count);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Negative counts use absolute value
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn negative_matches_positive(count in 0i64..=100_000) {
        for rule in all_built_in_rules() {
            let pos = rule.categorize(count);
            let neg = rule.categorize(-count);
            prop_assert_eq!(
                pos, neg,
                "rule {:?}: categorize({}) != categorize({})",
                rule, count, -count
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Interpolation with no placeholders is identity
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn interpolation_no_placeholders_identity(
        text in "[a-zA-Z0-9 .,!?]*"
    ) {
        // Text without braces should pass through unchanged
        let mut catalog = StringCatalog::new();
        let mut en = LocaleStrings::new();
        en.insert("test", text.as_str());
        catalog.add_locale("en", en);
        let result = catalog.format("en", "test", &[]);
        prop_assert_eq!(result.as_deref(), Some(text.as_str()));
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. Interpolation is idempotent (no recursive substitution)
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn interpolation_not_recursive() {
    let mut catalog = StringCatalog::new();
    let mut en = LocaleStrings::new();
    en.insert("test", "Hello {name}!");
    catalog.add_locale("en", en);

    // Provide a value that itself contains a placeholder
    let result = catalog.format("en", "test", &[("name", "{name}")]);
    assert_eq!(result, Some("Hello {name}!".into()));

    // The {name} in the replacement should NOT be re-expanded
    let result2 = catalog.format("en", "test", &[("name", "{other}")]);
    assert_eq!(result2, Some("Hello {other}!".into()));
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Missing args leave placeholder tokens intact
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn missing_args_preserve_tokens(
        name in "[a-z]{1,10}",
    ) {
        let template = format!("Value: {{{name}}}");
        let mut catalog = StringCatalog::new();
        let mut en = LocaleStrings::new();
        en.insert("test", template.as_str());
        catalog.add_locale("en", en);
        let result = catalog.format("en", "test", &[]);
        // Should contain the original {name} token
        prop_assert_eq!(result, Some(template.clone()));
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. PluralForms::select always returns a string for One/Other
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn plural_forms_select_never_empty(
        one in "[a-z]{1,20}",
        other in "[a-z]{1,20}",
    ) {
        let forms = PluralForms {
            one: one.clone(),
            other: other.clone(),
            ..Default::default()
        };
        let categories = [
            PluralCategory::Zero,
            PluralCategory::One,
            PluralCategory::Two,
            PluralCategory::Few,
            PluralCategory::Many,
            PluralCategory::Other,
        ];
        for cat in categories {
            let selected = forms.select(cat);
            prop_assert!(!selected.is_empty(), "select({:?}) returned empty", cat);
            // One always returns the `one` field, other categories fall back to `other`
            match cat {
                PluralCategory::One => prop_assert_eq!(selected, one.as_str()),
                PluralCategory::Other => prop_assert_eq!(selected, other.as_str()),
                _ => prop_assert_eq!(selected, other.as_str(), "missing form should fall back to other"),
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. Missing key returns None
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn missing_key_returns_none(key in "[a-z]{1,20}") {
        let catalog = StringCatalog::new();
        prop_assert_eq!(catalog.get("en", &key), None);
        prop_assert_eq!(catalog.get_plural("en", &key, 1), None);
        prop_assert_eq!(catalog.format("en", &key, &[]), None);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. format_plural auto-injects {count}
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn format_plural_injects_count(count in -1000i64..=1000) {
        let mut catalog = StringCatalog::new();
        let mut en = LocaleStrings::new();
        en.insert_plural("items", PluralForms {
            one: "{count} item".into(),
            other: "{count} items".into(),
            ..Default::default()
        });
        catalog.add_locale("en", en);

        let result = catalog.format_plural("en", "items", count, &[]);
        prop_assert!(result.is_some());
        let text = result.unwrap();
        // The count value should appear in the output
        prop_assert!(
            text.contains(&count.to_string()),
            "format_plural result '{}' should contain count '{}'",
            text, count
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 13. for_locale never panics on arbitrary strings
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn for_locale_never_panics(locale in ".*") {
        let _rule = PluralRule::for_locale(&locale);
        // Just verify no panic
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. Coverage report percentage is bounded
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn coverage_percent_bounded(
        n_keys in 0usize..=10,
        n_locales in 1usize..=3,
    ) {
        let mut catalog = StringCatalog::new();
        for locale_idx in 0..n_locales {
            let locale = format!("l{}", locale_idx);
            let mut ls = LocaleStrings::new();
            // Each locale gets a subset of keys
            for k in 0..n_keys {
                if k % (locale_idx + 1) == 0 {
                    ls.insert(format!("key_{k}"), format!("val_{k}"));
                }
            }
            catalog.add_locale(&locale, ls);
        }

        let report = catalog.coverage_report();
        for lc in &report.locales {
            prop_assert!(
                lc.coverage_percent >= 0.0 && lc.coverage_percent <= 100.0,
                "coverage {} out of bounds for locale {}",
                lc.coverage_percent, lc.locale
            );
            prop_assert!(
                lc.present + lc.missing.len() == report.total_keys,
                "present ({}) + missing ({}) != total ({})",
                lc.present, lc.missing.len(), report.total_keys
            );
        }
    }
}
