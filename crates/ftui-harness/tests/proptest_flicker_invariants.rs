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
use ftui_pty::virtual_terminal::VirtualTerminal;
use ftui_render::{
    buffer::Buffer,
    cell::{Cell, CellAttrs, StyleFlags},
};
use proptest::prelude::*;
use std::{cell::RefCell, io, ops::Range, rc::Rc};

#[derive(Clone, Default)]
struct RecordedOutput(Rc<RefCell<Vec<u8>>>);

impl io::Write for RecordedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum EraseOperation {
    Present,
    Log,
    Other,
}

/// Observe actual erase positions, without trusting the writer's region state.
/// Each operation must establish an absolute position before erasing. In
/// particular, a fresh model after resize does not pretend to emulate reflow.
fn check_erases(
    model: &mut VirtualTerminal,
    bytes: &[u8],
    size: (u16, u16),
    pending: &Range<u16>,
    displayed: Option<&Range<u16>>,
    operation: EraseOperation,
) -> Result<usize, String> {
    let (width, height) = size;
    let in_ui = |row| pending.contains(&row) || displayed.is_some_and(|r| r.contains(&row));
    let mut known_position = false;
    let mut erases = 0;
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset..].starts_with(b"\x1b[") {
            let start = offset;
            let final_offset = bytes[offset + 2..]
                .iter()
                .position(|b| (0x40..=0x7e).contains(b))
                .map(|n| offset + 2 + n)
                .ok_or_else(|| format!("incomplete CSI at byte {start}"))?;
            let params = &bytes[offset + 2..final_offset];
            let command = bytes[final_offset];
            if matches!(command, b'H' | b'f') {
                let numbers = std::str::from_utf8(params)
                    .map_err(|e| e.to_string())?
                    .split(';')
                    .map(|s| {
                        if s.is_empty() {
                            Ok(1)
                        } else {
                            s.parse::<u16>().map(|n| n.max(1))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                let row = numbers[0];
                let col = numbers.get(1).copied().unwrap_or(1);
                // Check the wire coordinates before the model clamps them.
                if numbers.len() > 2 || row > height || col > width {
                    return Err(format!(
                        "out-of-bounds CUP {row};{col} for {size:?} at byte {start}"
                    ));
                }
                known_position = true;
            } else if matches!(command, b'J' | b'K') {
                if !known_position {
                    return Err(format!("erase without known cursor at byte {start}"));
                }
                let row = model.cursor().1;
                let valid = match (operation, command, params) {
                    (EraseOperation::Present, b'J', b"" | b"0") => (row..height).all(in_ui),
                    (EraseOperation::Present, b'K', b"" | b"0" | b"1" | b"2") => in_ui(row),
                    (EraseOperation::Log, b'K', b"" | b"0" | b"1" | b"2") => !in_ui(row),
                    _ => false,
                };
                if !valid {
                    return Err(format!(
                        "erase {:?} on row {row} outside permitted scope at byte {start}; pending={pending:?}, displayed={displayed:?}",
                        &bytes[start..=final_offset]
                    ));
                }
                erases += 1;
            } else if matches!(command, b'r' | b'u') {
                known_position = false;
            }
            model.feed(&bytes[start..=final_offset]);
            offset = final_offset + 1;
        } else if bytes[offset] == 0x1b {
            let command = *bytes.get(offset + 1).ok_or("incomplete ESC")?;
            // Sanitized log output cannot contain control strings. Reject one
            // instead of interpreting an embedded CSI as a terminal command.
            if matches!(command, b']' | b'P' | b'_' | b'^' | b'X') {
                return Err(format!("unexpected control string at byte {offset}"));
            }
            if matches!(command, b'8' | b'c') {
                known_position = false;
            }
            model.feed(&bytes[offset..offset + 2]);
            offset += 2;
        } else {
            model.feed(&bytes[offset..offset + 1]);
            offset += 1;
        }
    }
    Ok(erases)
}

#[test]
fn erase_scope_observer_accepts_ui_erases_and_rejects_planted_violations() {
    let check = |bytes: &[u8], pending: Range<u16>, operation| {
        check_erases(
            &mut VirtualTerminal::new(20, 10),
            bytes,
            (20, 10),
            &pending,
            None,
            operation,
        )
    };
    assert_eq!(
        check(b"\x1b[8;1H\x1b[K\x1b[J", 7..10, EraseOperation::Present),
        Ok(2)
    );
    assert_eq!(
        check(b"\x1b[7;1H\x1b[2K", 7..10, EraseOperation::Log),
        Ok(1)
    );
    for bytes in [
        b"\x1b[7;1H\x1b[K".as_slice(), // Above the bottom UI.
        b"\x1b[0J",                    // No known cursor.
        b"\x1b[11;1H\x1b[K",           // Model would clamp to a valid UI row.
        b"\x1b[8;21H\x1b[K",           // Out-of-bounds column.
        b"\x1b[8;1H\x1b[2J",           // Whole screen.
    ] {
        assert!(
            check(bytes, 7..10, EraseOperation::Present).is_err(),
            "{bytes:?}"
        );
    }
    assert!(check(b"\x1b[1;1H\x1b[J", 0..3, EraseOperation::Present).is_err());
    assert!(check(b"\x1b[8;1H\x1b[K", 7..10, EraseOperation::Log).is_err());
    assert!(check(b"\x1b[1;1H\x1b[K", 7..10, EraseOperation::Other).is_err());
    // Shrinking leaves the old UI on screen until the next present. Its
    // obsolete rows may be erased by present, but not used for logs yet.
    for (operation, valid) in [
        (EraseOperation::Present, true),
        (EraseOperation::Log, false),
    ] {
        let result = check_erases(
            &mut VirtualTerminal::new(20, 10),
            b"\x1b[6;1H\x1b[K",
            (20, 10),
            &(8..10),
            Some(&(5..10)),
            operation,
        );
        assert_eq!(result.is_ok(), valid);
    }
}

/// Compare requested ASCII cells with the separately parsed terminal output.
/// Color quantization and Unicode shaping are outside this observer's scope.
fn check_frame(model: &VirtualTerminal, frame: &Buffer, start_row: u16) -> Result<(), String> {
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            let expected = frame.get_unchecked(x, y);
            let actual = model
                .cell_at(x, start_row + y)
                .ok_or_else(|| format!("missing terminal cell at ({x}, {})", start_row + y))?;
            let ch = expected.content.as_char().unwrap_or(' ');
            let bold = expected.attrs.flags().contains(StyleFlags::BOLD);
            if actual.ch != ch || actual.style.bold != bold {
                return Err(format!(
                    "cell ({x}, {}) expected {ch:?}, bold={bold}; actual={actual:?}",
                    start_row + y
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn frame_observer_rejects_missing_text_stale_cells_and_wrong_style() {
    let mut frame = Buffer::new(20, 2);
    frame.set(19, 1, Cell::from_char('X'));
    let mut model = VirtualTerminal::new(20, 10);
    assert!(check_frame(&model, &frame, 8).is_err());
    model.feed(b"\x1b[10;20HX");
    assert!(check_frame(&model, &frame, 8).is_ok());
    model.feed(b"\x1b[9;1Hstale");
    assert!(check_frame(&model, &frame, 8).is_err());
    model.feed(b"\x1b[9;1H\x1b[2K\x1b[10;20H\x1b[1mX");
    assert!(check_frame(&model, &frame, 8).is_err());
    frame.set(
        19,
        1,
        Cell::from_char('X').with_attrs(CellAttrs::new(StyleFlags::BOLD, 0)),
    );
    assert!(check_frame(&model, &frame, 8).is_ok());
}

// Exercise the actual writer across interleaved present/resize/log operations.
// Check both forbidden sequences and where each permitted erase takes effect.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn inline_never_clears_screen(
        profile in 0usize..3,
        top in any::<bool>(),
        auto in any::<bool>(),
        operations in proptest::collection::vec(
            (0u8..5, 20u16..=200, 5u16..=60, 1u16..=10,
             0usize..ftui_harness::ADVERSARIAL_PAYLOADS.len(),
             proptest::collection::vec(any::<u8>(), 0..32),
             proptest::collection::vec((0u16..200, 0u16..10, 0x20u8..=0x7e, any::<bool>()), 0..=32)),
            1..=200,
        ),
    ) {
        use ftui_core::terminal_capabilities::{TerminalCapabilities, TerminalProfile};
        use ftui_runtime::{ScreenMode, TerminalWriter, UiAnchor};

        let profiles = [TerminalProfile::Kitty, TerminalProfile::Xterm256Color, TerminalProfile::Tmux];
        let mode = if auto {
            ScreenMode::InlineAuto { min_height: 1, max_height: 10 }
        } else {
            ScreenMode::Inline { ui_height: 3 }
        };
        let anchor = if top { UiAnchor::Top } else { UiAnchor::Bottom };
        let output = RecordedOutput::default();
        let mut writer = TerminalWriter::new(
            output.clone(), mode, anchor, TerminalCapabilities::from_profile(profiles[profile]),
        );
        let mut width = 80;
        let mut terminal_rows = 24;
        let mut expected_height: u16 = if auto { 1 } else { 3 };
        let mut displayed = None;
        let mut displayed_frame = None;
        let mut model = VirtualTerminal::new(width, terminal_rows);
        let mut ranges = Vec::new();
        writer.set_size(width, terminal_rows);
        for (index, (kind, cols, rows, height, fragment, bytes, cells)) in operations.iter().enumerate() {
            let start = output.0.borrow().len();
            match kind {
                0 => {
                    let mut frame = Buffer::new(width, writer.ui_height());
                    for &(x, y, ch, bold) in cells {
                        let flags = if bold { StyleFlags::BOLD } else { StyleFlags::empty() };
                        frame.set(x % width, y % frame.height(),
                            Cell::from_char(char::from(ch)).with_attrs(CellAttrs::new(flags, 0)));
                    }
                    writer.present_ui(&frame, None, false)?;
                    displayed_frame = Some(frame);
                }
                1 => {
                    width = *cols;
                    terminal_rows = *rows;
                    if auto { expected_height = 1; }
                    displayed = None;
                    displayed_frame = None;
                    model = VirtualTerminal::new(width, terminal_rows);
                    writer.set_size(*cols, *rows);
                }
                2 | 3 => {
                    let text = format!("{}{}\n", ftui_harness::ADVERSARIAL_PAYLOADS[*fragment].0,
                        String::from_utf8_lossy(bytes));
                    if *kind == 2 {
                        writer.write_log(&text)?;
                    } else {
                        writer.write_log_sgr_only(&text)?;
                    }
                }
                _ => {
                    if auto { expected_height = (*height).clamp(1, 10.min(terminal_rows)); }
                    writer.set_auto_ui_height(*height);
                }
            }
            writer.flush()?;
            prop_assert_eq!(writer.ui_height(), expected_height);
            let height = expected_height.min(terminal_rows);
            let pending = if top { 0..height } else { terminal_rows - height..terminal_rows };
            let operation = match kind {
                0 => EraseOperation::Present,
                2 | 3 => EraseOperation::Log,
                _ => EraseOperation::Other,
            };
            let captured = output.0.borrow();
            ranges.push(start..captured.len());
            let result = check_erases(&mut model, &captured[start..], (width, terminal_rows),
                &pending, displayed.as_ref(), operation);
            prop_assert!(result.is_ok(), "{result:?}; operation={index}; ranges={ranges:?}; operations={operations:?}; bytes={:?}", &captured[start..]);
            if *kind == 0 { displayed = Some(pending); }
            if let (Some(frame), Some(region)) = (&displayed_frame, &displayed) {
                let result = check_frame(&model, frame, region.start);
                prop_assert!(result.is_ok(), "{result:?}; operation={index}; ranges={ranges:?}; operations={operations:?}; bytes={:?}", &captured[start..]);
            }
        }
        let cleanup_start = output.0.borrow().len();
        drop(writer);
        let output = output.0.borrow();
        let cleanup = check_erases(&mut model, &output[cleanup_start..], (width, terminal_rows),
            &(0..0), None, EraseOperation::Other);
        prop_assert!(cleanup.is_ok(), "cleanup={cleanup:?}; operations={operations:?}");
        for forbidden in [b"\x1b[2J".as_slice(), b"\x1b[3J", b"\x1b[?1049h", b"\x1b[?47h"] {
            prop_assert!(!output.windows(forbidden.len()).any(|window| window == forbidden),
                "forbidden {:?}; operations={:?}; output={:?}", forbidden, operations, output);
        }
    }
}

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
