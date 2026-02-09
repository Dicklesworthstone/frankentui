//! Property-based invariant tests for the flicker detection harness.
//!
//! Verifies structural guarantees of the `FlickerDetector` state machine:
//!
//! 1.  Never panics on arbitrary byte input
//! 2.  Determinism: same bytes → same events and stats
//! 3.  complete_frames <= total_frames
//! 4.  bytes_in_sync <= bytes_total
//! 5.  sync_coverage always in [0.0, 100.0]
//! 6.  Properly bracketed frames are always flicker-free
//! 7.  Unsynced visible content triggers sync_gap
//! 8.  Frame IDs are strictly monotonically increasing
//! 9.  Finalize always emits AnalysisComplete as last event
//! 10. Incomplete frame detected when sync starts but never ends
//! 11. Multiple frames accumulate correctly
//! 12. Empty stream is flicker-free with zero stats
//! 13. Chunked feeding matches single-shot feeding
//! 14. Partial erase (ED/EL mode 0/1) in sync frame increments partial_clears
//! 15. Full erase (ED/EL mode 2) in sync frame does NOT increment partial_clears

use ftui_harness::flicker_detection::{AnalysisStats, EventType, FlickerDetector, analyze_stream};
use proptest::prelude::*;

// ── Helpers ──────────────────────────────────────────────────────────

const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";

fn make_synced_frame(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SYNC_BEGIN.len() + content.len() + SYNC_END.len());
    out.extend_from_slice(SYNC_BEGIN);
    out.extend_from_slice(content);
    out.extend_from_slice(SYNC_END);
    out
}

/// Generate printable ASCII that doesn't contain ESC (0x1b).
fn arb_safe_content() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(0x20u8..=0x7e, 0..=100)
        .prop_filter("no ESC bytes", |v| !v.contains(&0x1b))
}

// ═════════════════════════════════════════════════════════════════════════
// 1. Never panics on arbitrary input
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..=500)) {
        let mut detector = FlickerDetector::new("fuzz");
        detector.feed(&bytes);
        detector.finalize();
        // If we reach here, no panic occurred
        let _ = detector.stats();
        let _ = detector.events();
        let _ = detector.to_jsonl();
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. Determinism: same bytes → same stats
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn deterministic(bytes in proptest::collection::vec(any::<u8>(), 0..=200)) {
        let a = analyze_stream(&bytes);
        let b = analyze_stream(&bytes);
        prop_assert_eq!(a.stats.total_frames, b.stats.total_frames);
        prop_assert_eq!(a.stats.complete_frames, b.stats.complete_frames);
        prop_assert_eq!(a.stats.sync_gaps, b.stats.sync_gaps);
        prop_assert_eq!(a.stats.partial_clears, b.stats.partial_clears);
        prop_assert_eq!(a.stats.bytes_total, b.stats.bytes_total);
        prop_assert_eq!(a.stats.bytes_in_sync, b.stats.bytes_in_sync);
        prop_assert_eq!(a.flicker_free, b.flicker_free);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. complete_frames <= total_frames
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn complete_le_total(bytes in proptest::collection::vec(any::<u8>(), 0..=300)) {
        let analysis = analyze_stream(&bytes);
        prop_assert!(
            analysis.stats.complete_frames <= analysis.stats.total_frames,
            "complete {} > total {}",
            analysis.stats.complete_frames,
            analysis.stats.total_frames
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 4. bytes_in_sync <= bytes_total
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn bytes_in_sync_le_total(bytes in proptest::collection::vec(any::<u8>(), 0..=300)) {
        let analysis = analyze_stream(&bytes);
        prop_assert!(
            analysis.stats.bytes_in_sync <= analysis.stats.bytes_total,
            "bytes_in_sync {} > bytes_total {}",
            analysis.stats.bytes_in_sync,
            analysis.stats.bytes_total
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. sync_coverage always in [0.0, 100.0]
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn sync_coverage_bounded(
        total in 0usize..=100_000,
        in_sync in 0usize..=100_000,
    ) {
        let stats = AnalysisStats {
            bytes_total: total,
            bytes_in_sync: in_sync.min(total),
            ..Default::default()
        };
        let cov = stats.sync_coverage();
        prop_assert!(cov >= 0.0, "coverage {} < 0", cov);
        prop_assert!(cov <= 100.0 + 1e-9, "coverage {} > 100", cov);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 6. Properly bracketed frames are flicker-free
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn bracketed_frames_flicker_free(
        contents in proptest::collection::vec(arb_safe_content(), 1..=5),
    ) {
        let mut stream = Vec::new();
        for content in &contents {
            stream.extend(make_synced_frame(content));
        }
        let analysis = analyze_stream(&stream);
        prop_assert!(
            analysis.flicker_free,
            "properly bracketed frames should be flicker-free, but got {} sync_gaps, {} partial_clears, {} incomplete",
            analysis.stats.sync_gaps,
            analysis.stats.partial_clears,
            analysis.stats.total_frames - analysis.stats.complete_frames
        );
        prop_assert_eq!(analysis.stats.total_frames, contents.len() as u64);
        prop_assert_eq!(analysis.stats.complete_frames, contents.len() as u64);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 7. Unsynced visible content triggers sync_gap
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn unsynced_visible_triggers_gap(
        gap in proptest::collection::vec(0x20u8..=0x7e, 1..=50),
    ) {
        // Only visible bytes (no ESC) before a synced frame
        let mut stream = gap.clone();
        stream.extend(make_synced_frame(b"ok"));
        let analysis = analyze_stream(&stream);
        prop_assert!(
            analysis.stats.sync_gaps > 0,
            "visible content before sync should cause gap"
        );
        prop_assert!(!analysis.flicker_free);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 8. Frame IDs are strictly monotonically increasing
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn frame_ids_monotonic(
        frame_count in 1usize..=10,
        content_len in 1usize..=20,
    ) {
        let mut stream = Vec::new();
        for _ in 0..frame_count {
            let content: Vec<u8> = (0..content_len).map(|i| b'A' + (i % 26) as u8).collect();
            stream.extend(make_synced_frame(&content));
        }

        let mut detector = FlickerDetector::new("mono");
        detector.feed(&stream);
        detector.finalize();

        let frame_ids: Vec<u64> = detector
            .events()
            .iter()
            .filter(|e| matches!(e.event_type, EventType::FrameStart))
            .map(|e| e.context.frame_id)
            .collect();

        for window in frame_ids.windows(2) {
            prop_assert!(
                window[1] > window[0],
                "frame IDs not monotonic: {} >= {}",
                window[0],
                window[1]
            );
        }
        prop_assert_eq!(frame_ids.len(), frame_count);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 9. Finalize always emits AnalysisComplete as last event
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn finalize_emits_analysis_complete(bytes in proptest::collection::vec(any::<u8>(), 0..=200)) {
        let mut detector = FlickerDetector::new("final");
        detector.feed(&bytes);
        detector.finalize();

        let events = detector.events();
        prop_assert!(!events.is_empty(), "finalize should emit at least one event");
        prop_assert!(
            matches!(events.last().unwrap().event_type, EventType::AnalysisComplete),
            "last event should be AnalysisComplete, got {:?}",
            events.last().unwrap().event_type
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 10. Incomplete frame detected when sync starts but never ends
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn incomplete_frame_detected(content in arb_safe_content()) {
        let mut stream = Vec::new();
        stream.extend_from_slice(SYNC_BEGIN);
        stream.extend_from_slice(&content);
        // No SYNC_END
        let analysis = analyze_stream(&stream);
        prop_assert!(!analysis.flicker_free, "incomplete frame should not be flicker-free");
        prop_assert!(
            analysis.issues.iter().any(|e| matches!(e.event_type, EventType::IncompleteFrame)),
            "should detect incomplete frame"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 11. Multiple frames accumulate correctly
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn frame_count_accurate(n in 1u64..=20) {
        let mut stream = Vec::new();
        for _ in 0..n {
            stream.extend(make_synced_frame(b"X"));
        }
        let analysis = analyze_stream(&stream);
        prop_assert_eq!(analysis.stats.total_frames, n);
        prop_assert_eq!(analysis.stats.complete_frames, n);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 12. Empty stream is flicker-free with zero stats
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn empty_stream_zero_stats() {
    let analysis = analyze_stream(b"");
    assert!(analysis.flicker_free);
    assert_eq!(analysis.stats.total_frames, 0);
    assert_eq!(analysis.stats.complete_frames, 0);
    assert_eq!(analysis.stats.sync_gaps, 0);
    assert_eq!(analysis.stats.partial_clears, 0);
    assert_eq!(analysis.stats.bytes_total, 0);
    assert_eq!(analysis.stats.bytes_in_sync, 0);
}

// ═════════════════════════════════════════════════════════════════════════
// 13. Chunked feeding matches single-shot feeding
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn chunked_matches_single(
        bytes in proptest::collection::vec(any::<u8>(), 1..=200),
        split_point in 0usize..=200,
    ) {
        let split = split_point.min(bytes.len());

        // Single-shot
        let single = analyze_stream(&bytes);

        // Chunked
        let mut chunked = FlickerDetector::new("analysis");
        chunked.feed(&bytes[..split]);
        chunked.feed(&bytes[split..]);
        chunked.finalize();

        prop_assert_eq!(
            single.stats.total_frames,
            chunked.stats().total_frames,
            "total_frames mismatch"
        );
        prop_assert_eq!(
            single.stats.complete_frames,
            chunked.stats().complete_frames,
            "complete_frames mismatch"
        );
        prop_assert_eq!(
            single.stats.bytes_total,
            chunked.stats().bytes_total,
            "bytes_total mismatch"
        );
        prop_assert_eq!(
            single.stats.bytes_in_sync,
            chunked.stats().bytes_in_sync,
            "bytes_in_sync mismatch"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 14. Partial erase in sync frame increments partial_clears
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn partial_erase_increments(mode in 0u8..=1) {
        // ED mode 0 (to end) or 1 (to start) inside frame
        let ed_seq = format!("\x1b[{}J", mode);
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(ed_seq.as_bytes());
        frame.extend_from_slice(b"content");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        prop_assert!(
            analysis.stats.partial_clears >= 1,
            "ED mode {} in frame should be a partial clear",
            mode
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 15. Full erase (mode 2) in sync does NOT increment partial_clears
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn full_erase_not_partial() {
    // ED mode 2 (clear all) and EL mode 2 (clear entire line) in frame
    for seq in ["\x1b[2J", "\x1b[2K"] {
        let mut frame = Vec::new();
        frame.extend_from_slice(SYNC_BEGIN);
        frame.extend_from_slice(seq.as_bytes());
        frame.extend_from_slice(b"content");
        frame.extend_from_slice(SYNC_END);

        let analysis = analyze_stream(&frame);
        assert_eq!(
            analysis.stats.partial_clears, 0,
            "{} in frame should not be partial clear",
            seq
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 16. bytes_total always equals input length
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn bytes_total_equals_input_length(bytes in proptest::collection::vec(any::<u8>(), 0..=500)) {
        let analysis = analyze_stream(&bytes);
        prop_assert_eq!(
            analysis.stats.bytes_total,
            bytes.len(),
            "bytes_total should match input length"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 17. is_flicker_free iff sync_gaps == 0 && partial_clears == 0 && complete == total
// ═════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn flicker_free_iff_conditions(bytes in proptest::collection::vec(any::<u8>(), 0..=300)) {
        let analysis = analyze_stream(&bytes);
        let expected = analysis.stats.sync_gaps == 0
            && analysis.stats.partial_clears == 0
            && analysis.stats.total_frames == analysis.stats.complete_frames;
        prop_assert_eq!(
            analysis.flicker_free,
            expected,
            "flicker_free={} but gaps={}, clears={}, total={}, complete={}",
            analysis.flicker_free,
            analysis.stats.sync_gaps,
            analysis.stats.partial_clears,
            analysis.stats.total_frames,
            analysis.stats.complete_frames
        );
    }
}
