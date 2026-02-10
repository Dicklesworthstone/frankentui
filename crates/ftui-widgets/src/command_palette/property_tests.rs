#![cfg(test)]
use crate::command_palette::{CommandPalette, MatchType, scorer::BayesianScorer};
use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Property Tests
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn score_is_bounded_0_1(query in "\\PC*", title in "\\PC*") {
        let scorer = BayesianScorer::fast();
        let result = scorer.score(&query, &title);
        prop_assert!(result.score >= 0.0);
        prop_assert!(result.score <= 1.0);
    }

    #[test]
    fn exact_match_score_gt_partial(
        title in "[a-z]{5,10}" // Random lowercase title
    ) {
        let scorer = BayesianScorer::fast();
        // Exact match
        let exact = scorer.score(&title, &title);

        // Partial match (prefix)
        let prefix = &title[0..title.len()-1];
        let partial = scorer.score(prefix, &title);

        // Exact should score higher than prefix
        prop_assert!(exact.score > partial.score);
        prop_assert_eq!(exact.match_type, MatchType::Exact);
        prop_assert_ne!(partial.match_type, MatchType::Exact);
    }

    #[test]
    fn prefix_monotonicity(
        title in "[a-z]{10,20}",
        len1 in 1usize..5,
        len2 in 6usize..9
    ) {
        let scorer = BayesianScorer::fast();
        let prefix1 = &title[0..len1];
        let prefix2 = &title[0..len2];

        let score1 = scorer.score(prefix1, &title);
        let score2 = scorer.score(prefix2, &title);

        // Longer prefix should generally score higher or equal
        prop_assert!(score2.score >= score1.score);
    }

    #[test]
    fn navigation_never_panics(
        action_count in 1usize..50,
        ops in proptest::collection::vec(
            prop_oneof![
                Just(KeyCode::Up),
                Just(KeyCode::Down),
                Just(KeyCode::PageUp),
                Just(KeyCode::PageDown),
                Just(KeyCode::Home),
                Just(KeyCode::End)
            ],
            1..20
        )
    ) {
        let mut palette = CommandPalette::new();
        for i in 0..action_count {
            palette.register(format!("Action {}", i), None, &[]);
        }
        palette.open();

        for code in ops {
            let _ = palette.handle_event(&Event::Key(KeyEvent {
                code,
                modifiers: Modifiers::empty(),
                kind: KeyEventKind::Press,
            }));

            // Invariant: selected index is always valid
            let selected = palette.selected_index();
            let count = palette.result_count();
            if count > 0 {
                prop_assert!(selected < count);
            } else {
                prop_assert_eq!(selected, 0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit Tests for Specific Gaps
// ---------------------------------------------------------------------------

#[test]
fn contiguous_score_gt_scattered() {
    let scorer = BayesianScorer::new();
    let title = "Asset Manager";

    // "set" in "Asset" (contiguous substring)
    let contiguous = scorer.score("set", title);

    // "stm" in "Asset Manager" (scattered fuzzy: AsSeT Manager)
    // Note: "stm" matches 's', 't', 'm'
    let scattered = scorer.score("stm", title);

    assert_eq!(contiguous.match_type, MatchType::Substring);
    assert_eq!(scattered.match_type, MatchType::Fuzzy);

    assert!(
        contiguous.score > scattered.score,
        "Contiguous substring should score ({:.3}) higher than scattered fuzzy ({:.3})",
        contiguous.score,
        scattered.score
    );
}

#[test]
fn word_boundary_bonus_logic() {
    let scorer = BayesianScorer::new();
    // "git" matches start of "Git Commit"
    let word_start = scorer.score("git", "Git Commit");
    // "com" matches start of second word
    let second_word = scorer.score("com", "Git Commit");
    // "mit" matches inside "Commit"
    let mid_word = scorer.score("mit", "Git Commit");

    // Word start should beat mid-word
    assert!(word_start.score > mid_word.score);
    assert!(second_word.score > mid_word.score);
}

#[test]
fn page_up_down_viewport_movement() {
    let mut palette = CommandPalette::new().with_max_visible(5);
    for i in 0..20 {
        palette.register(format!("Item {:02}", i), None, &[]);
    }
    palette.open();

    // Initial state
    assert_eq!(palette.selected_index(), 0);

    // PageDown -> +5
    let pg_down = Event::Key(KeyEvent {
        code: KeyCode::PageDown,
        modifiers: Modifiers::empty(),
        kind: KeyEventKind::Press,
    });

    let _ = palette.handle_event(&pg_down);
    assert_eq!(palette.selected_index(), 5);

    let _ = palette.handle_event(&pg_down);
    assert_eq!(palette.selected_index(), 10);

    // PageUp -> -5
    let pg_up = Event::Key(KeyEvent {
        code: KeyCode::PageUp,
        modifiers: Modifiers::empty(),
        kind: KeyEventKind::Press,
    });

    let _ = palette.handle_event(&pg_up);
    assert_eq!(palette.selected_index(), 5);
}

#[test]
fn whitespace_query_behavior() {
    let scorer = BayesianScorer::new();
    // Space is a valid char, should match space in title
    let result = scorer.score(" ", "Hello World");
    assert_eq!(result.match_type, MatchType::Substring);
    assert!(!result.match_positions.is_empty());
}

#[test]
fn unicode_search_correctness() {
    let scorer = BayesianScorer::new();

    // Emoji match - the emoji follows a space, so it's at a word boundary (WordStart)
    let result = scorer.score("🚀", "Launch 🚀");
    assert_eq!(result.match_type, MatchType::WordStart);
    assert!(result.score > 0.5);

    // CJK match
    let result_cjk = scorer.score("世界", "Hello 世界 World");
    assert_eq!(result_cjk.match_type, MatchType::Substring);

    // Case folding for accents (if supported by to_lowercase)
    // Rust's to_lowercase handles basic unicode case folding
    // "Café".to_lowercase() is "café", "cafe" != "café"
    // Unless we use a dedicated fuzzy matcher that strips accents, this might be fuzzy or no match.
    // The current implementation uses simple to_lowercase(), so 'e' != 'é'.
    // Actually our fuzzy logic skips non-matching chars.

    let result_accent = scorer.score("caf", "Café");
    assert_eq!(result_accent.match_type, MatchType::Prefix);
}

#[test]
fn very_long_title_no_panic() {
    let scorer = BayesianScorer::new();
    let query = "find";
    let title = "a".repeat(1000) + "find" + &"b".repeat(1000);

    // Should verify it doesn't stack overflow or OOM
    let result = scorer.score(query, &title);
    assert!(result.score > 0.0);
}
