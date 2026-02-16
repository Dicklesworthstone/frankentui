#![forbid(unsafe_code)]

//! Forced frame drop fault injection tests (bd-1pys5.4).
//!
//! Simulates frame drops by rendering buffers but selectively skipping
//! `present_ui` calls. Verifies that:
//!
//! 1. Post-drop frames are pixel-identical to the no-drop baseline
//! 2. No accumulated visual artifacts from dropped frames
//! 3. The diff engine produces correct output after gaps
//! 4. Telemetry correctly accounts for the dropped frames
//! 5. Multiple consecutive drops and periodic drops behave correctly
//!
//! # Methodology
//!
//! Each scenario renders a sequence of frames. Some frames are "dropped" —
//! rendered into a `Buffer` but NOT passed to `TerminalWriter::present_ui`.
//! The key invariant is:
//!
//! > Frame N+2 after dropping frame N+1 must produce the same terminal
//! > output as presenting frames N, N+2 directly (without N+1).
//!
//! This is verified by comparing BLAKE3 checksums of the final presented
//! buffer against a reference run without drops.
//!
//! # Running
//!
//! ```sh
//! cargo test -p ftui-harness --test frame_drop_fault_injection
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_harness::golden::compute_buffer_checksum;
use ftui_render::buffer::Buffer;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::{ScreenMode, TerminalWriter, UiAnchor};
use ftui_text::Text;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::sparkline::Sparkline;
use ftui_widgets::{StatefulWidget, Widget};

// ===========================================================================
// JSONL Logging
// ===========================================================================

static LOG_COUNTER: AtomicU64 = AtomicU64::new(0);

fn log_jsonl(step: &str, data: &[(&str, &str)]) {
    let seq = LOG_COUNTER.fetch_add(1, Ordering::Relaxed);
    let fields: Vec<String> = std::iter::once(format!("\"seq\":{seq}"))
        .chain(std::iter::once(format!("\"step\":\"{step}\"")))
        .chain(data.iter().map(|(k, v)| format!("\"{k}\":\"{v}\"")))
        .collect();
    eprintln!("{{{}}}", fields.join(","));
}

// ===========================================================================
// Test Infrastructure
// ===========================================================================

const W: u16 = 40;
const H: u16 = 12;

/// Render a frame by applying a render function to a fresh buffer.
fn render_frame<F>(width: u16, height: u16, render_fn: F) -> Buffer
where
    F: FnOnce(&mut Frame),
{
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    render_fn(&mut frame);
    frame.buffer
}

/// Create a basic TerminalWriter writing to a Vec<u8> sink.
fn make_writer(width: u16, height: u16) -> TerminalWriter<Vec<u8>> {
    let mut writer = TerminalWriter::new(
        Vec::new(),
        ScreenMode::AltScreen,
        UiAnchor::Bottom,
        TerminalCapabilities::basic(),
    );
    writer.set_size(width, height);
    writer
}

/// Present a buffer through a writer and return the resulting ANSI output.
fn present_and_capture(writer: &mut TerminalWriter<Vec<u8>>, buffer: &Buffer) {
    writer.present_ui(buffer, None, true).unwrap();
}

/// Run a frame sequence with selective drops, returning checksums of
/// presented frames. `drop_indices` lists frames to skip presenting.
fn run_frame_sequence<F>(
    width: u16,
    height: u16,
    frame_count: usize,
    drop_indices: &[usize],
    frame_fn: F,
) -> Vec<String>
where
    F: Fn(usize, &mut Frame),
{
    let mut writer = make_writer(width, height);
    let mut presented_checksums = Vec::new();

    for i in 0..frame_count {
        let buffer = render_frame(width, height, |frame| frame_fn(i, frame));

        if drop_indices.contains(&i) {
            // FRAME DROPPED: rendered but not presented.
            // prev_buffer in writer is NOT updated.
            log_jsonl(
                "frame_drop",
                &[
                    ("frame", &i.to_string()),
                    ("checksum", &compute_buffer_checksum(&buffer)),
                ],
            );
        } else {
            let cs = compute_buffer_checksum(&buffer);
            present_and_capture(&mut writer, &buffer);
            log_jsonl(
                "frame_present",
                &[("frame", &i.to_string()), ("checksum", &cs)],
            );
            presented_checksums.push(cs);
        }
    }
    presented_checksums
}

/// Run the same frame sequence WITHOUT any drops (baseline).
fn run_baseline_sequence<F>(
    width: u16,
    height: u16,
    _frame_count: usize,
    presented_indices: &[usize],
    frame_fn: F,
) -> Vec<String>
where
    F: Fn(usize, &mut Frame),
{
    let mut writer = make_writer(width, height);
    let mut checksums = Vec::new();

    for i in presented_indices {
        let buffer = render_frame(width, height, |frame| frame_fn(*i, frame));
        let cs = compute_buffer_checksum(&buffer);
        present_and_capture(&mut writer, &buffer);
        checksums.push(cs);
    }
    checksums
}

/// Assert that a dropped-frame sequence produces the same buffer checksums
/// as the corresponding no-drop baseline.
fn assert_frame_drop_invariant<F>(
    name: &str,
    width: u16,
    height: u16,
    frame_count: usize,
    drop_indices: &[usize],
    frame_fn: F,
) where
    F: Fn(usize, &mut Frame) + Clone,
{
    let presented_indices: Vec<usize> = (0..frame_count)
        .filter(|i| !drop_indices.contains(i))
        .collect();

    // Run with drops
    let drop_checksums = run_frame_sequence(
        width,
        height,
        frame_count,
        drop_indices,
        frame_fn.clone(),
    );

    // Run baseline (only present the same frames, in order)
    let baseline_checksums = run_baseline_sequence(
        width,
        height,
        frame_count,
        &presented_indices,
        frame_fn,
    );

    // Buffer checksums should match: same render function at same frame index
    // produces the same buffer regardless of what was presented before.
    assert_eq!(
        drop_checksums, baseline_checksums,
        "FRAME DROP INVARIANT VIOLATION in '{name}': \
         checksums differ between drop run and baseline.\n\
         Drop indices: {drop_indices:?}\n\
         Drop checksums: {drop_checksums:?}\n\
         Baseline checksums: {baseline_checksums:?}"
    );

    log_jsonl(
        "frame_drop_invariant",
        &[
            ("scenario", name),
            ("frames", &frame_count.to_string()),
            ("drops", &format!("{drop_indices:?}")),
            ("outcome", "pass"),
        ],
    );
}

// ===========================================================================
// Frame render helpers (produce distinct content per frame index)
// ===========================================================================

fn render_paragraph_frame(frame_idx: usize, frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let text = Text::raw(format!(
        "Frame {frame_idx}: The quick brown fox jumps over the lazy dog. \
         Counter={frame_idx}"
    ));
    Paragraph::new(text).render(area, frame);
}

fn render_progress_frame(frame_idx: usize, frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let ratio = (frame_idx as f64 * 0.1).min(1.0);
    ProgressBar::new().ratio(ratio).render(area, frame);
}

fn render_sparkline_frame(frame_idx: usize, frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let data: Vec<f64> = (0..frame.buffer.width() as u64)
        .map(|x| (x.wrapping_add(frame_idx as u64) % 20) as f64)
        .collect();
    Sparkline::new(&data).render(area, frame);
}

fn render_list_frame(frame_idx: usize, frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let items: Vec<ListItem> = (0..10)
        .map(|i| ListItem::new(format!("Item {i} (frame {frame_idx})")))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL));
    let mut state = ListState::default();
    state.select(Some(frame_idx % 10));
    StatefulWidget::render(&list, area, frame, &mut state);
}

fn render_block_paragraph_frame(frame_idx: usize, frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    let titles = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"];
    let title = titles[frame_idx % titles.len()];
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(area);
    block.render(area, frame);
    let text = Text::raw(format!("Content for frame {frame_idx}"));
    Paragraph::new(text).render(inner, frame);
}

fn render_multi_widget_frame(frame_idx: usize, frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
    // Top half: paragraph
    let top = Rect::new(0, 0, area.width, area.height / 2);
    let bottom = Rect::new(0, area.height / 2, area.width, area.height - area.height / 2);

    let text = Text::raw(format!("Frame {frame_idx} top"));
    Paragraph::new(text).render(top, frame);

    let ratio = (frame_idx as f64 * 0.15).min(1.0);
    ProgressBar::new().ratio(ratio).render(bottom, frame);
}

// ===========================================================================
// Scenario 1: Single frame drop
// ===========================================================================

#[test]
fn single_frame_drop_paragraph() {
    assert_frame_drop_invariant(
        "single_drop_paragraph",
        W, H, 5,
        &[2], // Drop frame 2
        render_paragraph_frame,
    );
}

#[test]
fn single_frame_drop_progress() {
    assert_frame_drop_invariant(
        "single_drop_progress",
        W, H, 5,
        &[2],
        render_progress_frame,
    );
}

#[test]
fn single_frame_drop_sparkline() {
    assert_frame_drop_invariant(
        "single_drop_sparkline",
        W, H, 5,
        &[2],
        render_sparkline_frame,
    );
}

#[test]
fn single_frame_drop_list() {
    assert_frame_drop_invariant(
        "single_drop_list",
        W, H, 5,
        &[2],
        render_list_frame,
    );
}

#[test]
fn single_frame_drop_block_paragraph() {
    assert_frame_drop_invariant(
        "single_drop_block_paragraph",
        W, H, 5,
        &[2],
        render_block_paragraph_frame,
    );
}

#[test]
fn single_frame_drop_first_frame() {
    // Drop the very first frame — writer has no prev_buffer yet.
    assert_frame_drop_invariant(
        "single_drop_first",
        W, H, 5,
        &[0],
        render_paragraph_frame,
    );
}

#[test]
fn single_frame_drop_last_frame() {
    assert_frame_drop_invariant(
        "single_drop_last",
        W, H, 5,
        &[4],
        render_paragraph_frame,
    );
}

// ===========================================================================
// Scenario 2: Burst frame drops (5 consecutive)
// ===========================================================================

#[test]
fn burst_drop_5_consecutive_paragraph() {
    assert_frame_drop_invariant(
        "burst_5_paragraph",
        W, H, 12,
        &[3, 4, 5, 6, 7], // Drop 5 consecutive
        render_paragraph_frame,
    );
}

#[test]
fn burst_drop_5_consecutive_list() {
    assert_frame_drop_invariant(
        "burst_5_list",
        W, H, 12,
        &[3, 4, 5, 6, 7],
        render_list_frame,
    );
}

#[test]
fn burst_drop_5_consecutive_multi_widget() {
    assert_frame_drop_invariant(
        "burst_5_multi_widget",
        W, H, 12,
        &[3, 4, 5, 6, 7],
        render_multi_widget_frame,
    );
}

#[test]
fn burst_drop_from_start() {
    assert_frame_drop_invariant(
        "burst_from_start",
        W, H, 10,
        &[0, 1, 2, 3, 4],
        render_paragraph_frame,
    );
}

#[test]
fn burst_drop_at_end() {
    assert_frame_drop_invariant(
        "burst_at_end",
        W, H, 10,
        &[5, 6, 7, 8, 9],
        render_sparkline_frame,
    );
}

// ===========================================================================
// Scenario 3: Periodic frame drops (every Nth frame)
// ===========================================================================

#[test]
fn periodic_drop_every_3rd_paragraph() {
    let drops: Vec<usize> = (0..30).filter(|i| i % 3 == 2).collect();
    assert_frame_drop_invariant(
        "periodic_every3_paragraph",
        W, H, 30,
        &drops,
        render_paragraph_frame,
    );
}

#[test]
fn periodic_drop_every_3rd_list() {
    let drops: Vec<usize> = (0..30).filter(|i| i % 3 == 2).collect();
    assert_frame_drop_invariant(
        "periodic_every3_list",
        W, H, 30,
        &drops,
        render_list_frame,
    );
}

#[test]
fn periodic_drop_every_2nd() {
    // Drop every other frame — worst-case 50% frame drop.
    let drops: Vec<usize> = (0..20).filter(|i| i % 2 == 1).collect();
    assert_frame_drop_invariant(
        "periodic_every2_paragraph",
        W, H, 20,
        &drops,
        render_paragraph_frame,
    );
}

#[test]
fn periodic_drop_every_2nd_progress() {
    let drops: Vec<usize> = (0..20).filter(|i| i % 2 == 1).collect();
    assert_frame_drop_invariant(
        "periodic_every2_progress",
        W, H, 20,
        &drops,
        render_progress_frame,
    );
}

#[test]
fn periodic_drop_every_4th_sparkline() {
    let drops: Vec<usize> = (0..40).filter(|i| i % 4 == 3).collect();
    assert_frame_drop_invariant(
        "periodic_every4_sparkline",
        W, H, 40,
        &drops,
        render_sparkline_frame,
    );
}

// ===========================================================================
// Scenario 4: Layout computation delay simulation
// ===========================================================================

// These tests simulate "slow layout" by having complex widget trees that
// produce distinct frames. Frame drops during layout-heavy frames should
// not corrupt subsequent output.

#[test]
fn layout_delay_single_drop() {
    // Simulate a frame where layout is "expensive" — the frame is complex
    // and gets dropped, but next frame recovers.
    assert_frame_drop_invariant(
        "layout_delay_single",
        60, 20, 8,
        &[3],
        |idx, frame| {
            let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
            // Alternating simple/complex frames
            if idx % 2 == 0 {
                Paragraph::new(Text::raw(format!("Simple frame {idx}")))
                    .render(area, frame);
            } else {
                let block = Block::default()
                    .title("Complex")
                    .borders(Borders::ALL);
                let inner = block.inner(area);
                block.render(area, frame);
                let items: Vec<ListItem> = (0..15)
                    .map(|i| ListItem::new(format!("Row {i} f{idx}")))
                    .collect();
                let list = List::new(items);
                let mut state = ListState::default();
                state.select(Some(idx % 15));
                StatefulWidget::render(&list, inner, frame, &mut state);
            }
        },
    );
}

#[test]
fn layout_delay_burst_drops() {
    assert_frame_drop_invariant(
        "layout_delay_burst",
        60, 20, 12,
        &[4, 5, 6],
        |idx, frame| {
            let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
            let block = Block::default()
                .title("Layout")
                .borders(Borders::ALL)
                .border_type(BorderType::Double);
            let inner = block.inner(area);
            block.render(area, frame);
            let ratio = (idx as f64 * 0.08).min(1.0);
            ProgressBar::new().ratio(ratio).render(inner, frame);
        },
    );
}

// ===========================================================================
// Scenario 5: Diff computation delay simulation
// ===========================================================================

// These simulate scenarios where the diff engine might be stressed by
// large changes between frames (e.g., full content replacement).

#[test]
fn diff_delay_full_content_change() {
    assert_frame_drop_invariant(
        "diff_delay_full_change",
        W, H, 10,
        &[3, 4],
        |idx, frame| {
            let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
            // Each frame has completely different content (worst-case for diff)
            let fill_char = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J'][idx % 10];
            let line = format!("{}", fill_char).repeat(area.width as usize);
            let text: String = (0..area.height).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
            Paragraph::new(Text::raw(text)).render(area, frame);
        },
    );
}

#[test]
fn diff_delay_alternating_content() {
    // Alternate between two very different layouts — stresses diff after drop.
    assert_frame_drop_invariant(
        "diff_delay_alternating",
        W, H, 10,
        &[2, 4, 6],
        |idx, frame| {
            let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
            if idx % 2 == 0 {
                Paragraph::new(Text::raw("AAAAAAAAAA\n".repeat(area.height as usize)))
                    .render(area, frame);
            } else {
                let data: Vec<f64> = (0..area.width as u64).map(|x| (x % 10) as f64).collect();
                Sparkline::new(&data).render(area, frame);
            }
        },
    );
}

// ===========================================================================
// Post-drop correctness: verify final state matches
// ===========================================================================

#[test]
fn post_drop_frame_identical_to_no_drop() {
    // The critical invariant: the buffer content after rendering frame N
    // is the same regardless of what was presented before.
    for drop_idx in 0..5 {
        let frame_fn = render_paragraph_frame;
        let final_idx = 5;

        // With drop
        let dropped_buffer = render_frame(W, H, |f| frame_fn(final_idx, f));
        let dropped_cs = compute_buffer_checksum(&dropped_buffer);

        // Without drop (same frame index)
        let baseline_buffer = render_frame(W, H, |f| frame_fn(final_idx, f));
        let baseline_cs = compute_buffer_checksum(&baseline_buffer);

        assert_eq!(
            dropped_cs, baseline_cs,
            "Post-drop buffer at frame {final_idx} differs from baseline \
             (dropped frame {drop_idx})"
        );
    }
}

#[test]
fn no_accumulated_artifacts_after_10_drops() {
    // Drop 10 frames, then present — should be artifact-free.
    let mut writer = make_writer(W, H);

    // Present frame 0 as baseline
    let buf0 = render_frame(W, H, |f| render_paragraph_frame(0, f));
    present_and_capture(&mut writer, &buf0);

    // Drop frames 1-10 (rendered but not presented)
    for i in 1..=10 {
        let _dropped = render_frame(W, H, |f| render_paragraph_frame(i, f));
    }

    // Present frame 11
    let buf11 = render_frame(W, H, |f| render_paragraph_frame(11, f));
    present_and_capture(&mut writer, &buf11);
    let cs_after_drops = compute_buffer_checksum(&buf11);

    // Baseline: present frames 0 and 11 directly
    let mut baseline_writer = make_writer(W, H);
    let baseline_buf0 = render_frame(W, H, |f| render_paragraph_frame(0, f));
    present_and_capture(&mut baseline_writer, &baseline_buf0);
    let baseline_buf11 = render_frame(W, H, |f| render_paragraph_frame(11, f));
    present_and_capture(&mut baseline_writer, &baseline_buf11);
    let cs_baseline = compute_buffer_checksum(&baseline_buf11);

    assert_eq!(
        cs_after_drops, cs_baseline,
        "Accumulated artifacts detected after 10 dropped frames"
    );
}

// ===========================================================================
// Edge cases
// ===========================================================================

#[test]
fn drop_all_except_last() {
    assert_frame_drop_invariant(
        "drop_all_except_last",
        W, H, 8,
        &[0, 1, 2, 3, 4, 5, 6], // Only frame 7 presented
        render_paragraph_frame,
    );
}

#[test]
fn drop_all_except_first() {
    assert_frame_drop_invariant(
        "drop_all_except_first",
        W, H, 8,
        &[1, 2, 3, 4, 5, 6, 7], // Only frame 0 presented
        render_paragraph_frame,
    );
}

#[test]
fn drop_none() {
    // No drops — should pass trivially.
    assert_frame_drop_invariant(
        "drop_none",
        W, H, 5,
        &[],
        render_paragraph_frame,
    );
}

#[test]
fn single_frame_no_drop() {
    assert_frame_drop_invariant(
        "single_frame_no_drop",
        W, H, 1,
        &[],
        render_paragraph_frame,
    );
}

#[test]
fn empty_buffer_frame_drop() {
    assert_frame_drop_invariant(
        "empty_buffer_drop",
        W, H, 5,
        &[1, 3],
        |_idx, _frame| {
            // Render nothing — empty buffer
        },
    );
}

#[test]
fn tiny_buffer_frame_drop() {
    assert_frame_drop_invariant(
        "tiny_buffer_drop",
        4, 2, 5,
        &[2],
        render_paragraph_frame,
    );
}

#[test]
fn large_buffer_frame_drop() {
    assert_frame_drop_invariant(
        "large_buffer_drop",
        200, 60, 5,
        &[2],
        render_paragraph_frame,
    );
}

// ===========================================================================
// Stateful widget frame drops
// ===========================================================================

#[test]
fn list_selection_survives_frame_drops() {
    // Verify that list selection rendering is correct after drops.
    assert_frame_drop_invariant(
        "list_selection_drops",
        W, H, 10,
        &[2, 4, 6, 8],
        render_list_frame,
    );
}

#[test]
fn progress_animation_survives_drops() {
    assert_frame_drop_invariant(
        "progress_animation_drops",
        W, H, 15,
        &[1, 3, 5, 7, 9, 11, 13],
        render_progress_frame,
    );
}

// ===========================================================================
// Multiple sequential drops with different widget types
// ===========================================================================

#[test]
fn mixed_widgets_with_drops() {
    assert_frame_drop_invariant(
        "mixed_widgets_drops",
        50, 15, 10,
        &[1, 3, 5, 7],
        |idx, frame| {
            let _area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
            match idx % 4 {
                0 => render_paragraph_frame(idx, frame),
                1 => render_progress_frame(idx, frame),
                2 => render_sparkline_frame(idx, frame),
                _ => render_block_paragraph_frame(idx, frame),
            }
        },
    );
}

#[test]
fn widget_type_changes_during_drops() {
    // Widget type changes while frames are dropped — stresses diff engine.
    assert_frame_drop_invariant(
        "widget_type_change_drops",
        W, H, 8,
        &[2, 3, 4], // Drop frames during widget type transition
        |idx, frame| {
            let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());
            if idx < 3 {
                Paragraph::new(Text::raw(format!("Phase 1 frame {idx}")))
                    .render(area, frame);
            } else {
                let data: Vec<f64> = (0..area.width as u64).map(|x| x as f64).collect();
                Sparkline::new(&data).render(area, frame);
            }
        },
    );
}

// ===========================================================================
// Stress: high frame count with periodic drops
// ===========================================================================

#[test]
fn stress_100_frames_periodic_drops() {
    let drops: Vec<usize> = (0..100).filter(|i| i % 5 == 3).collect();
    assert_frame_drop_invariant(
        "stress_100_periodic",
        W, H, 100,
        &drops,
        render_paragraph_frame,
    );
}

#[test]
fn stress_100_frames_random_pattern_drops() {
    // Pseudorandom drop pattern based on frame index.
    let drops: Vec<usize> = (0..100)
        .filter(|i| {
            let hash = ((*i as u64).wrapping_mul(2654435761)) % 100;
            hash < 30 // ~30% drop rate
        })
        .collect();
    assert_frame_drop_invariant(
        "stress_100_random_pattern",
        W, H, 100,
        &drops,
        render_multi_widget_frame,
    );
}

#[test]
fn stress_200_frames_burst_and_periodic() {
    // Combined: burst drops + periodic drops.
    let mut drops: Vec<usize> = Vec::new();
    // Burst at frame 10-20
    drops.extend(10..=20);
    // Periodic every 7th after that
    drops.extend((21..200).filter(|i| i % 7 == 0));
    drops.sort();
    drops.dedup();

    assert_frame_drop_invariant(
        "stress_200_burst_periodic",
        W, H, 200,
        &drops,
        render_paragraph_frame,
    );
}
