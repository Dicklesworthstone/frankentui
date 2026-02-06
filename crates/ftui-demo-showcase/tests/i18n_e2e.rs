#![forbid(unsafe_code)]

//! i18n E2E Test Suite (bd-ic6i.6)
//!
//! End-to-end validation of the internationalization foundation:
//!
//! # Coverage
//! 1. **String Localization**: key lookup, fallback chains, interpolation, missing keys.
//! 2. **Pluralization**: English/Russian/Arabic/French/CJK/Polish rules, edge cases.
//! 3. **RTL Layout**: flow direction detection, logical alignment/sides, rect mirroring.
//! 4. **Coverage/Extraction**: `all_keys()`, `missing_keys()`, `coverage_report()`.
//! 5. **Demo Integration**: `I18nDemo` screen rendering, locale switching, determinism.
//! 6. **Performance**: catalog lookup latency, plural categorization throughput.
//!
//! # Invariants
//! - **Fallback terminates**: every lookup walks the chain exactly once.
//! - **Interpolation idempotent**: single-pass `{name}` substitution.
//! - **Plural totality**: every rule maps any `i64` to exactly one category.
//! - **RTL symmetry**: `resolve(Rtl)` mirrors `resolve(Ltr)`.
//! - **Rendering determinism**: same locale + size → identical buffer hash.
//!
//! # JSONL Logging
//! ```json
//! {"test":"string_lookup","check":"direct_hit","passed":true,"notes":""}
//! ```
//!
//! Run: `cargo test -p ftui-demo-showcase --test i18n_e2e -- --nocapture`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::screens::Screen;
use ftui_i18n::{LocaleStrings, PluralCategory, PluralForms, PluralRule, StringCatalog};
use ftui_layout::direction::{
    FlowDirection, LogicalAlignment, LogicalSides, mirror_rects_horizontal,
};
use ftui_layout::{Alignment, Sides};
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::locale::LocaleContext;
use ftui_text::bidi::{BidiSegment, Direction, ParagraphDirection, reorder};

// =============================================================================
// Test Utilities
// =============================================================================

fn log_jsonl(test: &str, check: &str, passed: bool, notes: &str) {
    eprintln!(
        "{{\"test\":\"{test}\",\"check\":\"{check}\",\"passed\":{passed},\"notes\":\"{notes}\"}}"
    );
}

fn is_coverage_run() -> bool {
    std::env::var("LLVM_PROFILE_FILE").is_ok() || std::env::var("CARGO_LLVM_COV").is_ok()
}

fn coverage_budget_ns(default_ns: u128) -> u128 {
    if is_coverage_run() {
        // Coverage instrumentation adds overhead and variability; relax perf budgets so
        // `cargo llvm-cov` runs remain stable while still logging timings.
        default_ns.saturating_mul(5)
    } else {
        default_ns
    }
}

fn coverage_budget_us(default_us: u128) -> u128 {
    if is_coverage_run() {
        default_us.saturating_mul(5)
    } else {
        default_us
    }
}

fn key_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        modifiers: Modifiers::empty(),
    })
}

fn render_lines(
    screen: &ftui_demo_showcase::screens::i18n_demo::I18nDemo,
    width: u16,
    height: u16,
) -> Vec<String> {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, width, height));

    let mut lines = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                line.push(ch);
            } else {
                line.push(' ');
            }
        }
        lines.push(line);
    }
    lines
}

fn buffer_hash(screen: &ftui_demo_showcase::screens::i18n_demo::I18nDemo, w: u16, h: u16) -> u64 {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(w, h, &mut pool);
    let area = Rect::new(0, 0, w, h);
    screen.view(&mut frame, area);

    let mut hasher = DefaultHasher::new();
    for y in 0..h {
        for x in 0..w {
            if let Some(cell) = frame.buffer.get(x, y) {
                if let Some(ch) = cell.content.as_char() {
                    ch.hash(&mut hasher);
                }
                cell.fg.hash(&mut hasher);
                cell.bg.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Build a multi-locale catalog for testing.
fn test_catalog() -> StringCatalog {
    let mut catalog = StringCatalog::new();

    let mut en = LocaleStrings::new();
    en.insert("greeting", "Hello");
    en.insert("welcome", "Welcome, {name}!");
    en.insert("farewell", "Goodbye, {name}. See you {when}.");
    en.insert("color", "Color");
    en.insert_plural(
        "items",
        PluralForms {
            one: "{count} item".into(),
            other: "{count} items".into(),
            ..Default::default()
        },
    );
    en.insert_plural(
        "files",
        PluralForms {
            one: "{count} file".into(),
            other: "{count} files".into(),
            ..Default::default()
        },
    );
    catalog.add_locale("en", en);

    let mut es = LocaleStrings::new();
    es.insert("greeting", "Hola");
    es.insert("welcome", "\u{a1}Bienvenido, {name}!");
    es.insert("farewell", "Adi\u{f3}s, {name}.");
    es.insert_plural(
        "items",
        PluralForms {
            one: "{count} elemento".into(),
            other: "{count} elementos".into(),
            ..Default::default()
        },
    );
    // "color" and "files" missing in es
    catalog.add_locale("es", es);

    let mut ru = LocaleStrings::new();
    ru.insert("greeting", "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}");
    ru.insert_plural(
        "files",
        PluralForms {
            one: "{count} \u{444}\u{430}\u{439}\u{43b}".into(),
            few: Some("{count} \u{444}\u{430}\u{439}\u{43b}\u{430}".into()),
            many: Some("{count} \u{444}\u{430}\u{439}\u{43b}\u{43e}\u{432}".into()),
            other: "{count} \u{444}\u{430}\u{439}\u{43b}\u{43e}\u{432}".into(),
            ..Default::default()
        },
    );
    // Many keys missing in ru
    catalog.add_locale("ru", ru);

    let mut ar = LocaleStrings::new();
    ar.insert("greeting", "\u{645}\u{631}\u{62d}\u{628}\u{627}\u{64b}");
    ar.insert_plural(
        "items",
        PluralForms {
            zero: Some("{count} \u{639}\u{646}\u{627}\u{635}\u{631}".into()),
            one: "\u{639}\u{646}\u{635}\u{631} \u{648}\u{627}\u{62d}\u{62f}".into(),
            two: Some("\u{639}\u{646}\u{635}\u{631}\u{627}\u{646}".into()),
            few: Some("{count} \u{639}\u{646}\u{627}\u{635}\u{631}".into()),
            many: Some("{count} \u{639}\u{646}\u{635}\u{631}\u{627}\u{64b}".into()),
            other: "{count} \u{639}\u{646}\u{635}\u{631}".into(),
        },
    );
    catalog.add_locale("ar", ar);

    let mut ja = LocaleStrings::new();
    ja.insert("greeting", "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}");
    ja.insert_plural(
        "items",
        PluralForms {
            one: "{count}\u{500b}".into(),
            other: "{count}\u{500b}".into(),
            ..Default::default()
        },
    );
    catalog.add_locale("ja", ja);

    catalog.set_fallback_chain(vec!["en".into()]);
    catalog
}

// =============================================================================
// 1. String Localization
// =============================================================================

#[test]
fn string_direct_lookup() {
    let catalog = test_catalog();

    assert_eq!(catalog.get("en", "greeting"), Some("Hello"));
    assert_eq!(catalog.get("es", "greeting"), Some("Hola"));
    assert_eq!(
        catalog.get("ru", "greeting"),
        Some("\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}")
    );
    log_jsonl("string_lookup", "direct_hit", true, "3 locales ok");
}

#[test]
fn string_fallback_chain() {
    let catalog = test_catalog();

    // "color" only in en; es/ru/ar/ja should fallback to en
    assert_eq!(catalog.get("es", "color"), Some("Color"));
    assert_eq!(catalog.get("ru", "color"), Some("Color"));
    assert_eq!(catalog.get("ar", "color"), Some("Color"));
    assert_eq!(catalog.get("ja", "color"), Some("Color"));

    log_jsonl("string_lookup", "fallback", true, "4 fallbacks ok");
}

#[test]
fn string_missing_everywhere() {
    let catalog = test_catalog();

    assert_eq!(catalog.get("en", "nonexistent"), None);
    assert_eq!(catalog.get("xx", "nonexistent"), None);

    log_jsonl("string_lookup", "missing", true, "");
}

#[test]
fn string_interpolation_single() {
    let catalog = test_catalog();

    assert_eq!(
        catalog.format("en", "welcome", &[("name", "Alice")]),
        Some("Welcome, Alice!".into())
    );
    assert_eq!(
        catalog.format("es", "welcome", &[("name", "Carlos")]),
        Some("\u{a1}Bienvenido, Carlos!".into())
    );

    log_jsonl("string_interpolation", "single_arg", true, "");
}

#[test]
fn string_interpolation_multiple() {
    let catalog = test_catalog();

    assert_eq!(
        catalog.format("en", "farewell", &[("name", "Bob"), ("when", "tomorrow")]),
        Some("Goodbye, Bob. See you tomorrow.".into())
    );

    log_jsonl("string_interpolation", "multi_arg", true, "");
}

#[test]
fn string_interpolation_missing_arg() {
    let catalog = test_catalog();

    // Missing arg leaves token as-is
    assert_eq!(
        catalog.format("en", "welcome", &[]),
        Some("Welcome, {name}!".into())
    );

    log_jsonl("string_interpolation", "missing_arg", true, "");
}

#[test]
fn string_locale_listing() {
    let catalog = test_catalog();
    let mut locales = catalog.locales();
    locales.sort_unstable();
    assert_eq!(locales, vec!["ar", "en", "es", "ja", "ru"]);
    log_jsonl(
        "string_locales",
        "list",
        true,
        &format!("count={}", locales.len()),
    );
}

// =============================================================================
// 2. Pluralization
// =============================================================================

#[test]
fn plural_english_rules() {
    let rule = PluralRule::English;

    assert_eq!(rule.categorize(0), PluralCategory::Other);
    assert_eq!(rule.categorize(1), PluralCategory::One);
    assert_eq!(rule.categorize(2), PluralCategory::Other);
    assert_eq!(rule.categorize(100), PluralCategory::Other);

    log_jsonl("plural", "english", true, "");
}

#[test]
fn plural_french_rules() {
    let rule = PluralRule::French;

    assert_eq!(rule.categorize(0), PluralCategory::One);
    assert_eq!(rule.categorize(1), PluralCategory::One);
    assert_eq!(rule.categorize(2), PluralCategory::Other);

    log_jsonl("plural", "french", true, "");
}

#[test]
fn plural_russian_rules() {
    let rule = PluralRule::Russian;

    assert_eq!(rule.categorize(1), PluralCategory::One);
    assert_eq!(rule.categorize(2), PluralCategory::Few);
    assert_eq!(rule.categorize(3), PluralCategory::Few);
    assert_eq!(rule.categorize(4), PluralCategory::Few);
    assert_eq!(rule.categorize(5), PluralCategory::Many);
    assert_eq!(rule.categorize(11), PluralCategory::Many);
    assert_eq!(rule.categorize(21), PluralCategory::One);
    assert_eq!(rule.categorize(22), PluralCategory::Few);
    assert_eq!(rule.categorize(100), PluralCategory::Many);

    log_jsonl("plural", "russian", true, "");
}

#[test]
fn plural_arabic_rules() {
    let rule = PluralRule::Arabic;

    assert_eq!(rule.categorize(0), PluralCategory::Zero);
    assert_eq!(rule.categorize(1), PluralCategory::One);
    assert_eq!(rule.categorize(2), PluralCategory::Two);
    assert_eq!(rule.categorize(5), PluralCategory::Few);
    assert_eq!(rule.categorize(11), PluralCategory::Many);
    assert_eq!(rule.categorize(100), PluralCategory::Other);

    log_jsonl("plural", "arabic", true, "");
}

#[test]
fn plural_cjk_rules() {
    let rule = PluralRule::CJK;

    // CJK always returns Other
    for n in [0, 1, 2, 5, 100] {
        assert_eq!(rule.categorize(n), PluralCategory::Other);
    }

    log_jsonl("plural", "cjk", true, "");
}

#[test]
fn plural_polish_rules() {
    let rule = PluralRule::Polish;

    assert_eq!(rule.categorize(1), PluralCategory::One);
    assert_eq!(rule.categorize(2), PluralCategory::Few);
    assert_eq!(rule.categorize(5), PluralCategory::Many);
    assert_eq!(rule.categorize(12), PluralCategory::Many);
    assert_eq!(rule.categorize(22), PluralCategory::Few);

    log_jsonl("plural", "polish", true, "");
}

#[test]
fn plural_custom_rule() {
    let rule = PluralRule::Custom(|n| {
        if n == 42 {
            PluralCategory::Zero
        } else {
            PluralCategory::Other
        }
    });

    assert_eq!(rule.categorize(42), PluralCategory::Zero);
    assert_eq!(rule.categorize(43), PluralCategory::Other);

    log_jsonl("plural", "custom", true, "");
}

#[test]
fn plural_negative_counts() {
    let rule = PluralRule::English;
    // Negative counts use unsigned_abs
    assert_eq!(rule.categorize(-1), PluralCategory::One);
    assert_eq!(rule.categorize(-2), PluralCategory::Other);

    log_jsonl("plural", "negative", true, "");
}

#[test]
fn plural_format_with_count() {
    let catalog = test_catalog();

    assert_eq!(
        catalog.format_plural("en", "items", 1, &[]),
        Some("1 item".into())
    );
    assert_eq!(
        catalog.format_plural("en", "items", 5, &[]),
        Some("5 items".into())
    );
    assert_eq!(
        catalog.format_plural("en", "files", 1, &[]),
        Some("1 file".into())
    );
    assert_eq!(
        catalog.format_plural("en", "files", 99, &[]),
        Some("99 files".into())
    );

    log_jsonl("plural", "format", true, "");
}

#[test]
fn plural_russian_format() {
    let catalog = test_catalog();

    let r1 = catalog.format_plural("ru", "files", 1, &[]);
    let r3 = catalog.format_plural("ru", "files", 3, &[]);
    let r5 = catalog.format_plural("ru", "files", 5, &[]);

    assert!(r1.is_some());
    assert!(r3.is_some());
    assert!(r5.is_some());

    // All three should be distinct (one/few/many)
    assert_ne!(r1, r3);
    assert_ne!(r3, r5);

    log_jsonl("plural", "russian_format", true, "");
}

#[test]
fn plural_rule_auto_detect() {
    assert!(matches!(PluralRule::for_locale("en"), PluralRule::English));
    assert!(matches!(PluralRule::for_locale("ru"), PluralRule::Russian));
    assert!(matches!(PluralRule::for_locale("ar"), PluralRule::Arabic));
    assert!(matches!(PluralRule::for_locale("fr"), PluralRule::French));
    assert!(matches!(PluralRule::for_locale("ja"), PluralRule::CJK));
    assert!(matches!(PluralRule::for_locale("pl"), PluralRule::Polish));
    // Unknown falls back to English
    assert!(matches!(PluralRule::for_locale("xx"), PluralRule::English));

    log_jsonl("plural", "auto_detect", true, "");
}

#[test]
fn plural_totality() {
    // Every rule must produce a valid category for any count
    let rules = [
        PluralRule::English,
        PluralRule::French,
        PluralRule::Russian,
        PluralRule::Arabic,
        PluralRule::CJK,
        PluralRule::Polish,
    ];

    let test_counts = [
        i64::MIN,
        -1000,
        -1,
        0,
        1,
        2,
        3,
        4,
        5,
        10,
        11,
        12,
        21,
        100,
        101,
        1000,
        i64::MAX,
    ];

    for rule in &rules {
        for &count in &test_counts {
            let cat = rule.categorize(count);
            // Just verify it returns a valid category (doesn't panic)
            let _display = format!("{cat}");
        }
    }

    log_jsonl(
        "plural",
        "totality",
        true,
        &format!("rules={} counts={}", rules.len(), test_counts.len()),
    );
}

// =============================================================================
// 3. RTL Layout
// =============================================================================

#[test]
fn rtl_locale_detection() {
    // RTL languages
    assert!(FlowDirection::locale_is_rtl("ar"));
    assert!(FlowDirection::locale_is_rtl("he"));
    assert!(FlowDirection::locale_is_rtl("fa"));
    assert!(FlowDirection::locale_is_rtl("ur"));
    assert!(FlowDirection::locale_is_rtl("ar-SA"));
    assert!(FlowDirection::locale_is_rtl("he-IL"));

    // LTR languages
    assert!(!FlowDirection::locale_is_rtl("en"));
    assert!(!FlowDirection::locale_is_rtl("es"));
    assert!(!FlowDirection::locale_is_rtl("ru"));
    assert!(!FlowDirection::locale_is_rtl("ja"));
    assert!(!FlowDirection::locale_is_rtl("en-US"));

    log_jsonl("rtl", "locale_detection", true, "");
}

#[test]
fn rtl_flow_direction_from_locale() {
    assert_eq!(FlowDirection::from_locale("en"), FlowDirection::Ltr);
    assert_eq!(FlowDirection::from_locale("ar"), FlowDirection::Rtl);
    assert_eq!(FlowDirection::from_locale("he"), FlowDirection::Rtl);
    assert_eq!(FlowDirection::from_locale("fr"), FlowDirection::Ltr);

    log_jsonl("rtl", "from_locale", true, "");
}

#[test]
fn rtl_logical_alignment_ltr() {
    assert_eq!(
        LogicalAlignment::Start.resolve(FlowDirection::Ltr),
        Alignment::Start
    );
    assert_eq!(
        LogicalAlignment::End.resolve(FlowDirection::Ltr),
        Alignment::End
    );
    assert_eq!(
        LogicalAlignment::Center.resolve(FlowDirection::Ltr),
        Alignment::Center
    );

    log_jsonl("rtl", "alignment_ltr", true, "");
}

#[test]
fn rtl_logical_alignment_rtl() {
    // Start and End swap in RTL
    assert_eq!(
        LogicalAlignment::Start.resolve(FlowDirection::Rtl),
        Alignment::End
    );
    assert_eq!(
        LogicalAlignment::End.resolve(FlowDirection::Rtl),
        Alignment::Start
    );
    // Center is invariant
    assert_eq!(
        LogicalAlignment::Center.resolve(FlowDirection::Rtl),
        Alignment::Center
    );

    log_jsonl("rtl", "alignment_rtl", true, "");
}

#[test]
fn rtl_logical_sides_ltr() {
    let logical = LogicalSides {
        top: 1,
        bottom: 2,
        start: 3,
        end: 4,
    };
    let physical = logical.resolve(FlowDirection::Ltr);

    assert_eq!(
        physical,
        Sides {
            top: 1,
            right: 4,
            bottom: 2,
            left: 3,
        }
    );

    log_jsonl("rtl", "sides_ltr", true, "");
}

#[test]
fn rtl_logical_sides_rtl() {
    let logical = LogicalSides {
        top: 1,
        bottom: 2,
        start: 3,
        end: 4,
    };
    let physical = logical.resolve(FlowDirection::Rtl);

    // In RTL: start→right, end→left
    assert_eq!(
        physical,
        Sides {
            top: 1,
            right: 3,
            bottom: 2,
            left: 4,
        }
    );

    log_jsonl("rtl", "sides_rtl", true, "");
}

#[test]
fn rtl_sides_roundtrip() {
    let original = LogicalSides {
        top: 5,
        bottom: 10,
        start: 3,
        end: 7,
    };

    for flow in [FlowDirection::Ltr, FlowDirection::Rtl] {
        let physical = original.resolve(flow);
        let recovered = LogicalSides::from_physical(physical, flow);
        assert_eq!(original, recovered, "roundtrip failed for {flow:?}");
    }

    log_jsonl("rtl", "sides_roundtrip", true, "");
}

#[test]
fn rtl_mirror_rects() {
    let area = Rect::new(0, 0, 100, 50);
    let mut rects = vec![
        Rect::new(0, 0, 30, 10),  // left-aligned
        Rect::new(70, 0, 30, 10), // right-aligned
    ];

    mirror_rects_horizontal(&mut rects, area);

    // After mirroring in a 100-wide area:
    // rect at x=0, w=30 → x=70
    // rect at x=70, w=30 → x=0
    assert_eq!(rects[0].x, 70);
    assert_eq!(rects[1].x, 0);

    log_jsonl("rtl", "mirror_rects", true, "");
}

#[test]
fn rtl_mirror_preserves_widths() {
    let area = Rect::new(10, 5, 80, 40);
    let mut rects = vec![Rect::new(10, 5, 20, 10), Rect::new(50, 5, 30, 10)];

    let original_widths: Vec<u16> = rects.iter().map(|r| r.width).collect();
    mirror_rects_horizontal(&mut rects, area);

    let mirrored_widths: Vec<u16> = rects.iter().map(|r| r.width).collect();
    assert_eq!(original_widths, mirrored_widths);

    log_jsonl("rtl", "mirror_widths", true, "");
}

#[test]
fn rtl_logical_sides_constructors() {
    let all = LogicalSides::all(5);
    assert_eq!(all.top, 5);
    assert_eq!(all.bottom, 5);
    assert_eq!(all.start, 5);
    assert_eq!(all.end, 5);

    let sym = LogicalSides::symmetric(2, 4);
    assert_eq!(sym.top, 2);
    assert_eq!(sym.bottom, 2);
    assert_eq!(sym.start, 4);
    assert_eq!(sym.end, 4);

    let inl = LogicalSides::inline(3, 7);
    assert_eq!(inl.top, 0);
    assert_eq!(inl.bottom, 0);
    assert_eq!(inl.start, 3);
    assert_eq!(inl.end, 7);

    let blk = LogicalSides::block(1, 9);
    assert_eq!(blk.top, 1);
    assert_eq!(blk.bottom, 9);
    assert_eq!(blk.start, 0);
    assert_eq!(blk.end, 0);

    log_jsonl("rtl", "constructors", true, "");
}

// =============================================================================
// 4. Coverage / Extraction
// =============================================================================

#[test]
fn extraction_all_keys() {
    let catalog = test_catalog();
    let keys = catalog.all_keys();

    // Should contain union of all locale keys, sorted
    assert!(keys.contains(&"greeting".to_string()));
    assert!(keys.contains(&"items".to_string()));
    assert!(keys.contains(&"color".to_string()));
    assert!(keys.contains(&"files".to_string()));

    // Should be sorted
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);

    log_jsonl(
        "extraction",
        "all_keys",
        true,
        &format!("count={}", keys.len()),
    );
}

#[test]
fn extraction_missing_keys_es() {
    let catalog = test_catalog();
    let all = catalog.all_keys();
    let ref_keys: Vec<&str> = all.iter().map(String::as_str).collect();

    // Spanish with fallback to en should have no missing keys
    let missing = catalog.missing_keys("es", &ref_keys);
    assert!(
        missing.is_empty(),
        "es should resolve all keys via fallback: {missing:?}"
    );

    log_jsonl("extraction", "missing_es", true, "");
}

#[test]
fn extraction_missing_keys_no_fallback() {
    let mut catalog = StringCatalog::new();

    let mut en = LocaleStrings::new();
    en.insert("a", "A");
    en.insert("b", "B");
    en.insert("c", "C");
    catalog.add_locale("en", en);

    let mut fr = LocaleStrings::new();
    fr.insert("a", "A-fr");
    catalog.add_locale("fr", fr);
    // No fallback

    let missing = catalog.missing_keys("fr", &["a", "b", "c"]);
    assert_eq!(missing, vec!["b", "c"]);

    log_jsonl("extraction", "missing_no_fallback", true, "count=2");
}

#[test]
fn extraction_coverage_report() {
    let mut catalog = StringCatalog::new();

    let mut en = LocaleStrings::new();
    en.insert("x", "X");
    en.insert("y", "Y");
    en.insert("z", "Z");
    catalog.add_locale("en", en);

    let mut de = LocaleStrings::new();
    de.insert("x", "X-de");
    de.insert("y", "Y-de");
    catalog.add_locale("de", de);
    // No fallback → de missing "z"

    let report = catalog.coverage_report();
    assert_eq!(report.total_keys, 3);
    assert_eq!(report.locales.len(), 2);

    let de_cov = report.locales.iter().find(|l| l.locale == "de").unwrap();
    assert_eq!(de_cov.present, 2);
    assert_eq!(de_cov.missing, vec!["z"]);
    assert!((de_cov.coverage_percent - 66.666_66).abs() < 0.01);

    let en_cov = report.locales.iter().find(|l| l.locale == "en").unwrap();
    assert_eq!(en_cov.present, 3);
    assert!(en_cov.missing.is_empty());
    assert!((en_cov.coverage_percent - 100.0).abs() < f32::EPSILON);

    log_jsonl("extraction", "coverage_report", true, "");
}

#[test]
fn extraction_coverage_empty() {
    let catalog = StringCatalog::new();
    let report = catalog.coverage_report();
    assert_eq!(report.total_keys, 0);
    assert!(report.locales.is_empty());

    log_jsonl("extraction", "coverage_empty", true, "");
}

#[test]
fn extraction_keys_iterator() {
    let mut ls = LocaleStrings::new();
    ls.insert("beta", "B");
    ls.insert("alpha", "A");
    ls.insert("gamma", "G");

    let mut keys: Vec<&str> = ls.keys().collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["alpha", "beta", "gamma"]);
    assert_eq!(ls.len(), 3);
    assert!(!ls.is_empty());

    log_jsonl("extraction", "keys_iter", true, "count=3");
}

// =============================================================================
// 5. Demo Integration
// =============================================================================

#[test]
fn integration_demo_initial_render() {
    use ftui_demo_showcase::screens::i18n_demo::I18nDemo;

    let screen = I18nDemo::new();
    let lines = render_lines(&screen, 120, 40);

    // Should have non-empty content
    let non_empty = lines.iter().filter(|l| !l.trim().is_empty()).count();
    assert!(non_empty > 10, "should have substantial content");

    log_jsonl(
        "integration",
        "initial_render",
        true,
        &format!("non_empty={non_empty}"),
    );
}

#[test]
fn integration_locale_switching() {
    use ftui_demo_showcase::screens::i18n_demo::I18nDemo;

    let mut screen = I18nDemo::new();

    let before = render_lines(&screen, 120, 40);

    // Press Right to switch locale
    let _ = screen.update(&key_press(KeyCode::Right));
    let after = render_lines(&screen, 120, 40);

    assert_ne!(before, after, "locale switch should change rendered output");

    log_jsonl("integration", "locale_switch", true, "");
}

#[test]
fn integration_locale_cycle_wraps() {
    use ftui_demo_showcase::screens::i18n_demo::I18nDemo;

    let mut screen = I18nDemo::new();

    let initial = render_lines(&screen, 120, 40);

    // Press Right 6 times (for 6 locales) should wrap back to initial
    for _ in 0..6 {
        let _ = screen.update(&key_press(KeyCode::Right));
    }
    let after_cycle = render_lines(&screen, 120, 40);

    assert_eq!(
        initial, after_cycle,
        "full cycle should return to initial locale"
    );

    log_jsonl("integration", "locale_cycle", true, "");
}

#[test]
fn integration_panel_switching() {
    use ftui_demo_showcase::screens::i18n_demo::I18nDemo;

    let mut screen = I18nDemo::new();

    let panel0 = render_lines(&screen, 120, 40);

    let _ = screen.update(&key_press(KeyCode::Tab));
    let panel1 = render_lines(&screen, 120, 40);

    let _ = screen.update(&key_press(KeyCode::Tab));
    let panel2 = render_lines(&screen, 120, 40);

    let _ = screen.update(&key_press(KeyCode::Tab));
    let panel3 = render_lines(&screen, 120, 40);

    // All panels should render different content
    assert_ne!(panel0, panel1, "panel 0 vs 1 should differ");
    assert_ne!(panel1, panel2, "panel 1 vs 2 should differ");
    assert_ne!(panel2, panel3, "panel 2 vs 3 should differ");

    log_jsonl("integration", "panel_switch", true, "");
}

#[test]
fn integration_render_determinism() {
    use ftui_demo_showcase::screens::i18n_demo::I18nDemo;

    let screen = I18nDemo::new();

    let h1 = buffer_hash(&screen, 120, 40);
    let h2 = buffer_hash(&screen, 120, 40);
    assert_eq!(h1, h2, "same state should produce identical hash");

    log_jsonl("integration", "determinism", true, &format!("hash={h1}"));
}

#[test]
fn integration_multiple_sizes() {
    use ftui_demo_showcase::screens::i18n_demo::I18nDemo;

    let screen = I18nDemo::new();

    let sizes: [(u16, u16); 3] = [(80, 24), (120, 40), (200, 50)];

    for (w, h) in sizes {
        let lines = render_lines(&screen, w, h);
        assert_eq!(lines.len(), h as usize);
        let non_empty = lines.iter().filter(|l| !l.trim().is_empty()).count();
        assert!(non_empty > 5, "size {w}x{h}: should render content");
        log_jsonl(
            "integration",
            &format!("size_{w}x{h}"),
            true,
            &format!("non_empty={non_empty}"),
        );
    }
}

#[test]
fn integration_plural_count_adjustment() {
    use ftui_demo_showcase::screens::i18n_demo::I18nDemo;

    let mut screen = I18nDemo::new();

    // Switch to plurals panel
    let _ = screen.update(&key_press(KeyCode::Tab));
    let before = render_lines(&screen, 120, 40);

    // Press Up to adjust plural count
    let _ = screen.update(&key_press(KeyCode::Up));
    let after = render_lines(&screen, 120, 40);

    assert_ne!(before, after, "count change should update plural display");

    log_jsonl("integration", "plural_count", true, "");
}

#[test]
fn integration_keybindings_documented() {
    use ftui_demo_showcase::screens::i18n_demo::I18nDemo;

    let screen = I18nDemo::new();
    let bindings = screen.keybindings();

    assert!(!bindings.is_empty(), "should have keybindings");

    let has_locale = bindings.iter().any(|h| h.action.contains("locale"));
    let has_panel = bindings.iter().any(|h| h.action.contains("panel"));

    assert!(has_locale, "should document locale switching");
    assert!(has_panel, "should document panel switching");

    log_jsonl(
        "integration",
        "keybindings",
        true,
        &format!("count={}", bindings.len()),
    );
}

// =============================================================================
// 6. Performance
// =============================================================================

#[test]
fn perf_catalog_lookup_latency() {
    let catalog = test_catalog();
    let iterations = if is_coverage_run() { 10_000 } else { 100_000 };

    let start = Instant::now();
    for _ in 0..iterations {
        let _ = catalog.get("en", "greeting");
        let _ = catalog.get("es", "welcome");
        let _ = catalog.get("ru", "color"); // fallback
        let _ = catalog.format("en", "welcome", &[("name", "Test")]);
    }
    let elapsed = start.elapsed();

    let avg_ns = elapsed.as_nanos() / (iterations as u128 * 4);

    log_jsonl(
        "perf",
        "lookup_latency",
        true,
        &format!("iterations={iterations} avg_ns={avg_ns}"),
    );

    let budget_ns = coverage_budget_ns(1000);
    // Budget: each lookup < 1μs on average (relaxed under coverage)
    assert!(
        avg_ns < budget_ns,
        "avg lookup latency {avg_ns}ns exceeds {budget_ns}ns budget"
    );
}

#[test]
fn perf_plural_categorization() {
    let rules = [
        PluralRule::English,
        PluralRule::Russian,
        PluralRule::Arabic,
        PluralRule::French,
        PluralRule::CJK,
        PluralRule::Polish,
    ];
    let iterations = if is_coverage_run() { 10_000 } else { 100_000 };
    let counts: Vec<i64> = (0..100).collect();

    let start = Instant::now();
    for _ in 0..iterations {
        for rule in &rules {
            for &count in &counts {
                let _ = rule.categorize(count);
            }
        }
    }
    let elapsed = start.elapsed();

    let total_ops = iterations as u128 * rules.len() as u128 * counts.len() as u128;
    let avg_ns = elapsed.as_nanos() / total_ops;

    log_jsonl(
        "perf",
        "plural_categorize",
        true,
        &format!("total_ops={total_ops} avg_ns={avg_ns}"),
    );

    let budget_ns = coverage_budget_ns(100);
    // Budget: < 100ns per categorization (relaxed under coverage)
    assert!(
        avg_ns < budget_ns,
        "avg categorization latency {avg_ns}ns exceeds {budget_ns}ns budget"
    );
}

#[test]
fn perf_coverage_report() {
    let catalog = test_catalog();
    let iterations = if is_coverage_run() { 1_000 } else { 10_000 };

    let start = Instant::now();
    for _ in 0..iterations {
        let _ = catalog.coverage_report();
    }
    let elapsed = start.elapsed();

    let avg_us = elapsed.as_micros() / iterations as u128;

    log_jsonl(
        "perf",
        "coverage_report",
        true,
        &format!("iterations={iterations} avg_us={avg_us}"),
    );

    let budget_us = coverage_budget_us(100);
    // Budget: < 100μs per report (relaxed under coverage)
    assert!(
        avg_us < budget_us,
        "avg coverage_report latency {avg_us}μs exceeds {budget_us}μs budget"
    );
}

// =============================================================================
// 7. BiDi Text Rendering
// =============================================================================

#[test]
fn bidi_pure_ltr_passthrough() {
    let text = "Hello, world!";
    let result = reorder(text, ParagraphDirection::Auto);
    assert_eq!(result, text);
    log_jsonl("bidi", "ltr_passthrough", true, "");
}

#[test]
fn bidi_pure_rtl_reorder() {
    // Pure Arabic text
    let text = "\u{645}\u{631}\u{62d}\u{628}\u{627}";
    let result = reorder(text, ParagraphDirection::Auto);
    assert!(!result.is_empty());
    log_jsonl("bidi", "rtl_reorder", true, "");
}

#[test]
fn bidi_mixed_ltr_rtl() {
    // Mixed Arabic and English
    let text = "\u{645}\u{631}\u{62d}\u{628}\u{627} Hello";
    let result = reorder(text, ParagraphDirection::Auto);
    assert!(result.contains("Hello"), "English part preserved");
    log_jsonl("bidi", "mixed_content", true, "");
}

#[test]
fn bidi_segment_ltr_identity() {
    let seg = BidiSegment::new("Hello", None);
    assert_eq!(seg.chars.len(), 5);
    // Pure LTR: visual == logical
    for i in 0..seg.chars.len() {
        assert_eq!(seg.visual_to_logical[i], i);
        assert_eq!(seg.logical_to_visual[i], i);
    }
    log_jsonl("bidi", "segment_ltr_identity", true, "");
}

#[test]
fn bidi_segment_rtl_roundtrip() {
    let seg = BidiSegment::new("\u{645}\u{631}\u{62d}\u{628}\u{627}", Some(Direction::Rtl));
    for i in 0..seg.chars.len() {
        let v = seg.logical_to_visual[i];
        let roundtrip = seg.visual_to_logical[v];
        assert_eq!(roundtrip, i, "roundtrip at logical {i}");
    }
    log_jsonl("bidi", "segment_rtl_roundtrip", true, "");
}

#[test]
fn bidi_segment_empty() {
    let seg = BidiSegment::new("", None);
    assert!(seg.chars.is_empty());
    assert!(seg.runs.is_empty());
    assert!(seg.visual_to_logical.is_empty());
    assert!(seg.logical_to_visual.is_empty());
    log_jsonl("bidi", "segment_empty", true, "");
}

#[test]
fn bidi_reorder_empty() {
    let result = reorder("", ParagraphDirection::Auto);
    assert!(result.is_empty());
    log_jsonl("bidi", "reorder_empty", true, "");
}

#[test]
fn bidi_paragraph_direction_forced() {
    let text = "Hello \u{645}\u{631}\u{62d}\u{628}\u{627}";
    let ltr = reorder(text, ParagraphDirection::Ltr);
    let rtl = reorder(text, ParagraphDirection::Rtl);
    // Both should produce non-empty output
    assert!(!ltr.is_empty());
    assert!(!rtl.is_empty());
    log_jsonl("bidi", "direction_forced", true, "");
}

#[test]
fn bidi_segment_runs() {
    // Pure LTR should be a single run
    let seg = BidiSegment::new("Hello", None);
    assert_eq!(seg.runs.len(), 1, "pure LTR = one run");
    assert_eq!(seg.runs[0].direction, Direction::Ltr);
    assert_eq!(seg.runs[0].start, 0);
    assert_eq!(seg.runs[0].end, 5);
    assert_eq!(seg.runs[0].len(), 5);
    assert!(!seg.runs[0].is_empty());
    log_jsonl("bidi", "segment_runs", true, "");
}

#[test]
fn bidi_hebrew_text() {
    // Hebrew text
    let text = "\u{5e9}\u{5dc}\u{5d5}\u{5dd}";
    let result = reorder(text, ParagraphDirection::Auto);
    assert!(!result.is_empty());
    let seg = BidiSegment::new(text, Some(Direction::Rtl));
    assert_eq!(seg.chars.len(), 4);
    log_jsonl("bidi", "hebrew", true, "");
}

#[test]
fn bidi_bracket_pairing() {
    // Brackets should pair correctly in mixed text
    let text = "Hello (\u{645}\u{631}\u{62d}\u{628}\u{627}) world";
    let result = reorder(text, ParagraphDirection::Ltr);
    // Result should contain all parts
    assert!(result.contains("Hello"));
    assert!(result.contains("world"));
    log_jsonl("bidi", "bracket_pairing", true, "");
}

#[test]
fn bidi_numbers_in_rtl() {
    // Numbers should remain LTR even in RTL context
    let text = "\u{645}\u{631}\u{62d}\u{628}\u{627} 123 \u{639}\u{646}\u{635}\u{631}";
    let result = reorder(text, ParagraphDirection::Rtl);
    assert!(result.contains("123"), "Numbers preserved in RTL");
    log_jsonl("bidi", "numbers_in_rtl", true, "");
}

// =============================================================================
// 8. Locale Context (Runtime)
// =============================================================================

#[test]
fn locale_context_basic_set_get() {
    let ctx = LocaleContext::new("en");
    assert_eq!(ctx.current_locale(), "en");
    assert_eq!(ctx.base_locale(), "en");

    ctx.set_locale("fr");
    assert_eq!(ctx.current_locale(), "fr");
    assert_eq!(ctx.base_locale(), "fr");

    log_jsonl("locale_ctx", "basic_set_get", true, "");
}

#[test]
fn locale_context_scoped_override_lifo() {
    let ctx = LocaleContext::new("en");

    {
        let _g1 = ctx.push_override("fr");
        assert_eq!(ctx.current_locale(), "fr");
        assert_eq!(ctx.base_locale(), "en", "base unchanged under override");

        {
            let _g2 = ctx.push_override("ru");
            assert_eq!(ctx.current_locale(), "ru");
            assert_eq!(ctx.base_locale(), "en");
        }
        // g2 dropped → back to fr
        assert_eq!(ctx.current_locale(), "fr");
    }
    // g1 dropped → back to en
    assert_eq!(ctx.current_locale(), "en");

    log_jsonl("locale_ctx", "scoped_override_lifo", true, "");
}

#[test]
fn locale_context_version_increments_on_base_change() {
    let ctx = LocaleContext::new("en");
    let v0 = ctx.version();

    ctx.set_locale("es");
    let v1 = ctx.version();
    assert!(v1 > v0, "version should increment on base locale change");

    // Override should NOT change version
    let _guard = ctx.push_override("ar");
    let v2 = ctx.version();
    assert_eq!(v1, v2, "override should not increment version");

    log_jsonl("locale_ctx", "version_tracking", true, "");
}

#[test]
fn locale_context_override_preserves_base() {
    let ctx = LocaleContext::new("en");
    let _guard = ctx.push_override("ja");

    assert_eq!(ctx.current_locale(), "ja");
    assert_eq!(ctx.base_locale(), "en");

    log_jsonl("locale_ctx", "override_preserves_base", true, "");
}

#[test]
fn locale_context_normalize_strips_encoding() {
    // Locales with encoding suffixes should be normalized
    let ctx = LocaleContext::new("en_US.UTF-8");
    let locale = ctx.current_locale();
    assert!(
        !locale.contains('.'),
        "encoding suffix should be stripped: got {locale}"
    );
    assert!(
        !locale.contains('_'),
        "underscore should be normalized: got {locale}"
    );

    log_jsonl("locale_ctx", "normalize", true, &locale);
}

#[test]
fn locale_context_c_posix_to_en() {
    let ctx_c = LocaleContext::new("C");
    assert_eq!(ctx_c.current_locale(), "en", "C should normalize to en");

    let ctx_posix = LocaleContext::new("POSIX");
    assert_eq!(
        ctx_posix.current_locale(),
        "en",
        "POSIX should normalize to en"
    );

    log_jsonl("locale_ctx", "c_posix", true, "");
}

#[test]
fn locale_context_empty_to_en() {
    let ctx = LocaleContext::new("");
    assert_eq!(ctx.current_locale(), "en", "empty should default to en");

    let ctx_ws = LocaleContext::new("   ");
    assert_eq!(
        ctx_ws.current_locale(),
        "en",
        "whitespace should default to en"
    );

    log_jsonl("locale_ctx", "empty_default", true, "");
}

#[test]
fn locale_context_triple_override_stack() {
    let ctx = LocaleContext::new("en");

    let g1 = ctx.push_override("fr");
    let g2 = ctx.push_override("de");
    let g3 = ctx.push_override("ja");

    assert_eq!(ctx.current_locale(), "ja");

    drop(g3);
    assert_eq!(ctx.current_locale(), "de");

    drop(g2);
    assert_eq!(ctx.current_locale(), "fr");

    drop(g1);
    assert_eq!(ctx.current_locale(), "en");

    log_jsonl("locale_ctx", "triple_stack", true, "");
}
