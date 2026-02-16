#![forbid(unsafe_code)]

//! No-flicker invariant under all fault scenarios (bd-1pys5.5).
//!
//! End-to-end test that verifies the no-flicker invariant: under ALL fault
//! injection scenarios, the user never sees a partially-rendered or corrupted
//! frame.
//!
//! # Invariants Verified
//!
//! 1. Every sync bracket is complete (`?2026h` paired with `?2026l`)
//! 2. No visible content outside sync brackets (no sync gaps)
//! 3. No partial clear operations (ED/EL mode 0/1) inside frames
//! 4. No incomplete frames
//! 5. All checks hold under: frame drops, resize thrash, bursty input,
//!    capability mismatch, and combined faults
//!
//! # Running
//!
//! ```sh
//! cargo test -p ftui-harness --test no_flicker_fault_invariant
//! ```

use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::{Presenter, TerminalCapabilities};

use ftui_harness::flicker_detection::analyze_stream_with_id;

// ============================================================================
// Helpers
// ============================================================================

/// Deterministic LCG for repeatable pseudo-random tests.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        (self.0 >> 32) as u32
    }

    fn next_range(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        (self.next_u32() as usize) % max
    }

    fn next_char(&mut self) -> char {
        char::from_u32(b'A' as u32 + (self.next_u32() % 26)).unwrap()
    }

    fn next_color(&mut self) -> PackedRgba {
        let r = (self.next_u32() % 256) as u8;
        let g = (self.next_u32() % 256) as u8;
        let b = (self.next_u32() % 256) as u8;
        PackedRgba::rgb(r, g, b)
    }

    fn next_bool(&mut self) -> bool {
        self.next_u32().is_multiple_of(2)
    }
}

fn caps_synced() -> TerminalCapabilities {
    let mut caps = TerminalCapabilities::basic();
    caps.sync_output = true;
    caps
}

fn caps_no_sync() -> TerminalCapabilities {
    TerminalCapabilities::basic()
}

fn caps_truecolor_synced() -> TerminalCapabilities {
    let mut caps = TerminalCapabilities::basic();
    caps.sync_output = true;
    caps.true_color = true;
    caps
}

/// Render a full buffer diff through the presenter, returning raw ANSI bytes.
fn present_to_bytes(buffer: &Buffer, diff: &BufferDiff, caps: TerminalCapabilities) -> Vec<u8> {
    let mut sink = Vec::new();
    let mut presenter = Presenter::new(&mut sink, caps);
    presenter.present(buffer, diff).unwrap();
    drop(presenter);
    sink
}

/// Render a frame from blank → buffer with sync enabled.
fn present_frame_synced(buffer: &Buffer) -> Vec<u8> {
    let blank = Buffer::new(buffer.width(), buffer.height());
    let diff = BufferDiff::compute(&blank, buffer);
    present_to_bytes(buffer, &diff, caps_synced())
}

/// Render an incremental frame with sync enabled.
fn present_incremental(prev: &Buffer, next: &Buffer, caps: TerminalCapabilities) -> Vec<u8> {
    let diff = BufferDiff::compute(prev, next);
    present_to_bytes(next, &diff, caps)
}

/// Build a buffer with deterministic content.
fn build_buffer(width: u16, height: u16, seed: u64) -> Buffer {
    let mut rng = Lcg::new(seed);
    let mut buf = Buffer::new(width, height);
    let num_cells = (width as u64 * height as u64).min(200);
    for _ in 0..num_cells {
        let x = rng.next_range(width as usize) as u16;
        let y = rng.next_range(height as usize) as u16;
        buf.set_raw(
            x,
            y,
            Cell::from_char(rng.next_char()).with_fg(rng.next_color()),
        );
    }
    buf
}

/// Build a fully-filled buffer (worst-case diff scenario).
fn build_full_buffer(width: u16, height: u16, ch: char) -> Buffer {
    let mut buf = Buffer::new(width, height);
    for y in 0..height {
        for x in 0..width {
            buf.set_raw(x, y, Cell::from_char(ch));
        }
    }
    buf
}

/// Mutate a buffer with random changes.
fn mutate_buffer(buf: &mut Buffer, rng: &mut Lcg, num_changes: usize) {
    for _ in 0..num_changes {
        let x = rng.next_range(buf.width() as usize) as u16;
        let y = rng.next_range(buf.height() as usize) as u16;
        buf.set_raw(
            x,
            y,
            Cell::from_char(rng.next_char()).with_fg(rng.next_color()),
        );
    }
}

// ============================================================================
// 1. Frame Drop Fault Scenarios
// ============================================================================
//
// Simulate frame drops: render buffers but skip presenting some frames.
// Only the presented frames emit ANSI output → verify that output is
// flicker-free even when the prev_buffer may be stale.

#[test]
fn frame_drops_single_drop_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);

    for i in 0..10u64 {
        let next = build_buffer(80, 24, i);
        if i != 5 {
            // Present all except frame 5
            all_output.extend(present_incremental(&prev, &next, caps));
            prev = next;
        }
        // Frame 5 dropped: don't update prev, don't capture output
    }

    let analysis = analyze_stream_with_id("drop-single", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 9);
}

#[test]
fn frame_drops_burst_5_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(60, 20);

    for i in 0..15u64 {
        let next = build_buffer(60, 20, i);
        let dropped = (5..10).contains(&i); // Drop frames 5-9
        if !dropped {
            all_output.extend(present_incremental(&prev, &next, caps));
            prev = next;
        }
    }

    let analysis = analyze_stream_with_id("drop-burst-5", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 10);
}

#[test]
fn frame_drops_periodic_every_3rd_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(40, 12);

    for i in 0..30u64 {
        let next = build_buffer(40, 12, i);
        if i % 3 != 2 {
            all_output.extend(present_incremental(&prev, &next, caps));
            prev = next;
        }
    }

    let analysis = analyze_stream_with_id("drop-periodic-3rd", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 20);
}

#[test]
fn frame_drops_all_except_first_and_last_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let blank = Buffer::new(80, 24);

    let first = build_buffer(80, 24, 0);
    all_output.extend(present_incremental(&blank, &first, caps));

    let last = build_buffer(80, 24, 99);
    all_output.extend(present_incremental(&first, &last, caps));

    let analysis = analyze_stream_with_id("drop-all-middle", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 2);
}

#[test]
fn frame_drops_random_30pct_flicker_free() {
    let caps = caps_synced();
    let mut rng = Lcg::new(0xdead_beef);
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);
    let mut presented = 0u64;

    for i in 0..50u64 {
        let next = build_buffer(80, 24, i);
        let drop = rng.next_range(100) < 30; // ~30% drop rate
        if !drop {
            all_output.extend(present_incremental(&prev, &next, caps));
            prev = next;
            presented += 1;
        }
    }

    let analysis = analyze_stream_with_id("drop-random-30pct", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, presented);
}

// ============================================================================
// 2. Resize Thrash Scenarios
// ============================================================================
//
// Simulate rapid terminal resize by changing buffer dimensions between frames.
// After resize, prev_buffer is discarded (full redraw against blank).

#[test]
fn resize_oscillation_flicker_free() {
    let sizes: [(u16, u16); 4] = [(80, 24), (40, 12), (120, 40), (60, 20)];
    let mut all_output = Vec::new();

    for (i, &(w, h)) in sizes.iter().cycle().take(12).enumerate() {
        let buf = build_buffer(w, h, i as u64);
        all_output.extend(present_frame_synced(&buf));
    }

    let analysis = analyze_stream_with_id("resize-oscillation", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 12);
}

#[test]
fn resize_shrink_then_grow_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();

    // Start large
    let large = build_full_buffer(120, 40, '#');
    all_output.extend(present_frame_synced(&large));

    // Shrink to tiny
    let tiny = build_full_buffer(20, 5, '.');
    all_output.extend(present_frame_synced(&tiny));

    // Incremental updates at small size
    let mut prev = tiny;
    for i in 0..5u64 {
        let mut next = prev.clone();
        mutate_buffer(&mut next, &mut Lcg::new(i), 10);
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    // Grow back
    let grown = build_full_buffer(100, 30, '@');
    all_output.extend(present_frame_synced(&grown));

    let analysis = analyze_stream_with_id("resize-shrink-grow", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 8);
}

#[test]
fn resize_rapid_random_sizes_flicker_free() {
    let mut rng = Lcg::new(0xcafe_d00d);
    let mut all_output = Vec::new();

    for i in 0..20u64 {
        let w = 10 + rng.next_range(111) as u16; // 10-120
        let h = 5 + rng.next_range(36) as u16; // 5-40
        let buf = build_buffer(w, h, i);
        all_output.extend(present_frame_synced(&buf));
    }

    let analysis = analyze_stream_with_id("resize-rapid-random", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 20);
}

#[test]
fn resize_1x1_to_max_and_back_flicker_free() {
    let mut all_output = Vec::new();

    let tiny = build_full_buffer(1, 1, 'X');
    all_output.extend(present_frame_synced(&tiny));

    let max = build_full_buffer(200, 60, 'M');
    all_output.extend(present_frame_synced(&max));

    let tiny2 = build_full_buffer(1, 1, 'Y');
    all_output.extend(present_frame_synced(&tiny2));

    let analysis = analyze_stream_with_id("resize-1x1-max", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 3);
}

// ============================================================================
// 3. Bursty Input Scenarios
// ============================================================================
//
// Simulate rapid content changes: each frame has many changed cells.

#[test]
fn bursty_full_screen_rewrite_every_frame_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);

    for i in 0..20u64 {
        let ch = char::from_u32(b'A' as u32 + (i % 26) as u32).unwrap();
        let next = build_full_buffer(80, 24, ch);
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    let analysis = analyze_stream_with_id("bursty-full-rewrite", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 20);
}

#[test]
fn bursty_alternating_content_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let buf_a = build_full_buffer(60, 20, 'A');
    let buf_b = build_full_buffer(60, 20, 'B');
    let mut prev = Buffer::new(60, 20);

    for i in 0..30u64 {
        let next = if i % 2 == 0 { &buf_a } else { &buf_b };
        all_output.extend(present_incremental(&prev, next, caps));
        prev = next.clone();
    }

    let analysis = analyze_stream_with_id("bursty-alternating", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 30);
}

#[test]
fn bursty_random_high_churn_flicker_free() {
    let caps = caps_synced();
    let mut rng = Lcg::new(0xbeef_face);
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);

    for _ in 0..25 {
        let mut next = prev.clone();
        // Change 80% of cells
        let changes = (80 * 24 * 80) / 100;
        mutate_buffer(&mut next, &mut rng, changes);
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    let analysis = analyze_stream_with_id("bursty-high-churn", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 25);
}

#[test]
fn bursty_sparse_single_cell_per_frame_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);

    for i in 0..100u64 {
        let mut next = prev.clone();
        let x = (i * 7 + 3) as u16 % 80;
        let y = (i * 13 + 5) as u16 % 24;
        next.set_raw(x, y, Cell::from_char('*'));
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    let analysis = analyze_stream_with_id("bursty-sparse", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 100);
}

#[test]
fn bursty_styled_content_churn_flicker_free() {
    let caps = caps_truecolor_synced();
    let mut rng = Lcg::new(0x1234_5678);
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(60, 20);

    for _ in 0..15 {
        let mut next = prev.clone();
        // Each frame changes cells with different colors
        for _ in 0..200 {
            let x = rng.next_range(60) as u16;
            let y = rng.next_range(20) as u16;
            let fg = rng.next_color();
            let bg = rng.next_color();
            next.set_raw(
                x,
                y,
                Cell::from_char(rng.next_char()).with_fg(fg).with_bg(bg),
            );
        }
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    let analysis = analyze_stream_with_id("bursty-styled-churn", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 15);
}

// ============================================================================
// 4. Capability Mismatch Scenarios
// ============================================================================
//
// Verify behavior when terminal capabilities change or are mismatched.
// Without sync_output, content will be outside sync brackets → detected as gaps.
// The key invariant: with sync, ALWAYS flicker-free. Without sync, gaps detected.

#[test]
fn capability_sync_on_always_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);

    for i in 0..10u64 {
        let next = build_buffer(80, 24, i);
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    let analysis = analyze_stream_with_id("caps-sync-on", &all_output);
    analysis.assert_flicker_free();
}

#[test]
fn capability_sync_off_detects_gaps() {
    let caps = caps_no_sync();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(40, 10);

    for i in 0..5u64 {
        let next = build_buffer(40, 10, i);
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    let analysis = analyze_stream_with_id("caps-sync-off", &all_output);
    // Without sync, content is outside brackets → gaps expected
    assert!(!analysis.flicker_free);
    assert!(analysis.stats.sync_gaps > 0);
}

#[test]
fn capability_truecolor_with_sync_flicker_free() {
    let caps = caps_truecolor_synced();
    let mut rng = Lcg::new(42);
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);

    for _ in 0..8 {
        let mut next = prev.clone();
        mutate_buffer(&mut next, &mut rng, 100);
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    let analysis = analyze_stream_with_id("caps-truecolor-sync", &all_output);
    analysis.assert_flicker_free();
}

#[test]
fn capability_switch_mid_session_both_synced_flicker_free() {
    // Simulate: start with basic caps, then upgrade to truecolor (both synced)
    let caps_basic = caps_synced();
    let caps_true = caps_truecolor_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);

    // Phase 1: basic caps
    for i in 0..5u64 {
        let next = build_buffer(80, 24, i);
        all_output.extend(present_incremental(&prev, &next, caps_basic));
        prev = next;
    }

    // Phase 2: upgraded caps (full redraw after capability change)
    let fresh = build_buffer(80, 24, 100);
    all_output.extend(present_frame_synced(&fresh));
    prev = fresh;

    for i in 5..10u64 {
        let next = build_buffer(80, 24, i + 100);
        all_output.extend(present_incremental(&prev, &next, caps_true));
        prev = next;
    }

    let analysis = analyze_stream_with_id("caps-switch-mid", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 11); // 5 + 1 fresh + 5
}

// ============================================================================
// 5. Combined Fault Scenarios
// ============================================================================
//
// Apply multiple faults simultaneously.

#[test]
fn combined_frame_drops_plus_resize_flicker_free() {
    let caps = caps_synced();
    let sizes: [(u16, u16); 3] = [(80, 24), (40, 12), (120, 40)];
    let mut all_output = Vec::new();

    for (cycle, &(w, h)) in sizes.iter().enumerate() {
        let mut prev = Buffer::new(w, h);

        for i in 0..8u64 {
            let next = build_buffer(w, h, cycle as u64 * 100 + i);
            let dropped = i == 3 || i == 6; // Drop some frames per resize cycle
            if !dropped {
                if i == 0 {
                    // First frame after resize: full redraw
                    all_output.extend(present_frame_synced(&next));
                } else {
                    all_output.extend(present_incremental(&prev, &next, caps));
                }
                prev = next;
            }
        }
    }

    let analysis = analyze_stream_with_id("combined-drops-resize", &all_output);
    analysis.assert_flicker_free();
}

#[test]
fn combined_bursty_plus_frame_drops_flicker_free() {
    let caps = caps_synced();
    let mut rng = Lcg::new(0xabcd_1234);
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);

    for i in 0..40u64 {
        let mut next = prev.clone();
        // High churn: change 50% of cells
        mutate_buffer(&mut next, &mut rng, 80 * 24 / 2);

        let dropped = i % 5 == 3; // Drop every 5th frame
        if !dropped {
            all_output.extend(present_incremental(&prev, &next, caps));
            prev = next;
        }
    }

    let analysis = analyze_stream_with_id("combined-bursty-drops", &all_output);
    analysis.assert_flicker_free();
}

#[test]
fn combined_resize_plus_bursty_plus_drops_flicker_free() {
    let caps = caps_synced();
    let mut rng = Lcg::new(0x9876_fedc);
    let mut all_output = Vec::new();
    let mut presented_frames = 0u64;

    for phase in 0..4u64 {
        let w = 20 + rng.next_range(101) as u16; // 20-120
        let h = 5 + rng.next_range(36) as u16; // 5-40
        let mut prev = Buffer::new(w, h);

        for i in 0..10u64 {
            let mut next = prev.clone();
            let churn = 10 + rng.next_range(w as usize * h as usize / 2);
            mutate_buffer(&mut next, &mut rng, churn);

            let dropped = rng.next_range(100) < 25; // ~25% drop rate
            if !dropped {
                if i == 0 {
                    all_output.extend(present_frame_synced(&next));
                } else {
                    all_output.extend(present_incremental(&prev, &next, caps));
                }
                prev = next;
                presented_frames += 1;
            }
        }

        // Sanity: phase counter advances
        assert!(phase < 100);
    }

    let analysis = analyze_stream_with_id("combined-all-faults", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, presented_frames);
}

#[test]
fn combined_capability_switch_plus_resize_plus_drops_flicker_free() {
    let caps_a = caps_synced();
    let caps_b = caps_truecolor_synced();
    let mut rng = Lcg::new(0x5555_aaaa);
    let mut all_output = Vec::new();

    for phase in 0..6u64 {
        let caps = if phase % 2 == 0 { caps_a } else { caps_b };
        let w = 30 + rng.next_range(91) as u16;
        let h = 8 + rng.next_range(33) as u16;

        // First frame: full redraw after resize/cap change
        let first = build_buffer(w, h, phase * 100);
        all_output.extend({
            let blank = Buffer::new(w, h);
            let diff = BufferDiff::compute(&blank, &first);
            present_to_bytes(&first, &diff, caps)
        });
        let mut prev = first;

        for i in 1..8u64 {
            let mut next = prev.clone();
            mutate_buffer(&mut next, &mut rng, 50);
            let dropped = rng.next_bool() && i % 3 == 0;
            if !dropped {
                all_output.extend(present_incremental(&prev, &next, caps));
                prev = next;
            }
        }
    }

    let analysis = analyze_stream_with_id("combined-caps-resize-drops", &all_output);
    analysis.assert_flicker_free();
}

// ============================================================================
// 6. Stress: Large Frame Count Under Faults
// ============================================================================

#[test]
fn stress_200_frames_mixed_faults_flicker_free() {
    let caps = caps_synced();
    let mut rng = Lcg::new(0x1337_beef);
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);
    let mut frame_count = 0u64;

    for i in 0..200u64 {
        // Occasional resize
        if i % 50 == 0 && i > 0 {
            let w = 40 + rng.next_range(81) as u16;
            let h = 10 + rng.next_range(31) as u16;
            let fresh = build_buffer(w, h, i * 1000);
            all_output.extend({
                let blank = Buffer::new(w, h);
                let diff = BufferDiff::compute(&blank, &fresh);
                present_to_bytes(&fresh, &diff, caps)
            });
            prev = fresh;
            frame_count += 1;
            continue;
        }

        let mut next = prev.clone();
        let churn = 1 + rng.next_range(80);
        mutate_buffer(&mut next, &mut rng, churn);

        // ~20% drop rate
        let dropped = rng.next_range(100) < 20;
        if !dropped {
            all_output.extend(present_incremental(&prev, &next, caps));
            prev = next;
            frame_count += 1;
        }
    }

    let analysis = analyze_stream_with_id("stress-200-mixed", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, frame_count);
    assert!(analysis.stats.sync_coverage() > 70.0);
}

#[test]
fn stress_100_frames_full_rewrite_periodic_drops_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(80, 24);
    let mut frame_count = 0u64;

    for i in 0..100u64 {
        let ch = char::from_u32(b'A' as u32 + (i % 26) as u32).unwrap();
        let next = build_full_buffer(80, 24, ch);

        // Drop every 4th frame
        if i % 4 != 3 {
            all_output.extend(present_incremental(&prev, &next, caps));
            prev = next;
            frame_count += 1;
        }
    }

    let analysis = analyze_stream_with_id("stress-100-full-periodic", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, frame_count);
}

// ============================================================================
// 7. Edge Cases Under Faults
// ============================================================================

#[test]
fn edge_empty_buffer_after_drop_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();

    // Frame 1: content
    let filled = build_full_buffer(40, 10, '#');
    all_output.extend(present_frame_synced(&filled));

    // Frame 2: dropped

    // Frame 3: empty buffer
    let empty = Buffer::new(40, 10);
    all_output.extend(present_incremental(&filled, &empty, caps));

    let analysis = analyze_stream_with_id("edge-empty-after-drop", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 2);
}

#[test]
fn edge_single_cell_buffer_resize_flicker_free() {
    let mut all_output = Vec::new();

    let big = build_full_buffer(80, 24, 'X');
    all_output.extend(present_frame_synced(&big));

    let tiny = build_full_buffer(1, 1, 'Y');
    all_output.extend(present_frame_synced(&tiny));

    let big2 = build_full_buffer(80, 24, 'Z');
    all_output.extend(present_frame_synced(&big2));

    let analysis = analyze_stream_with_id("edge-1x1-resize", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 3);
}

#[test]
fn edge_no_changes_between_frames_flicker_free() {
    let caps = caps_synced();
    let buf = build_buffer(40, 10, 42);
    let mut all_output = Vec::new();

    // Present the same buffer 10 times (no changes → empty diff)
    all_output.extend(present_frame_synced(&buf));
    for _ in 0..9 {
        let diff = BufferDiff::compute(&buf, &buf);
        assert!(diff.is_empty());
        all_output.extend(present_to_bytes(&buf, &diff, caps));
    }

    let analysis = analyze_stream_with_id("edge-no-changes", &all_output);
    analysis.assert_flicker_free();
    assert_eq!(analysis.stats.total_frames, 10);
    assert_eq!(analysis.stats.complete_frames, 10);
}

#[test]
fn edge_wide_buffer_single_row_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(200, 1);

    for i in 0..20u64 {
        let mut next = prev.clone();
        let x = (i * 13) as u16 % 200;
        next.set_raw(x, 0, Cell::from_char('*'));
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    let analysis = analyze_stream_with_id("edge-wide-single-row", &all_output);
    analysis.assert_flicker_free();
}

#[test]
fn edge_tall_buffer_single_column_flicker_free() {
    let caps = caps_synced();
    let mut all_output = Vec::new();
    let mut prev = Buffer::new(1, 60);

    for i in 0..20u64 {
        let mut next = prev.clone();
        let y = (i * 7) as u16 % 60;
        next.set_raw(0, y, Cell::from_char('#'));
        all_output.extend(present_incremental(&prev, &next, caps));
        prev = next;
    }

    let analysis = analyze_stream_with_id("edge-tall-single-col", &all_output);
    analysis.assert_flicker_free();
}

// ============================================================================
// 8. Determinism: Same inputs → same flicker analysis
// ============================================================================

#[test]
fn deterministic_analysis_across_runs() {
    let caps = caps_synced();

    let collect_output = || -> Vec<u8> {
        let mut rng2 = Lcg::new(0x4242_4242);
        let mut prev2 = Buffer::new(80, 24);
        let mut out = Vec::new();
        for _ in 0..10 {
            let mut next = prev2.clone();
            mutate_buffer(&mut next, &mut rng2, 50);
            out.extend(present_incremental(&prev2, &next, caps));
            prev2 = next;
        }
        out
    };

    let run1 = collect_output();
    let run2 = collect_output();
    assert_eq!(run1, run2, "Deterministic output should be identical");

    let analysis1 = analyze_stream_with_id("det-1", &run1);
    let analysis2 = analyze_stream_with_id("det-2", &run2);
    assert_eq!(analysis1.stats.total_frames, analysis2.stats.total_frames);
    assert_eq!(
        analysis1.stats.complete_frames,
        analysis2.stats.complete_frames
    );
    assert_eq!(analysis1.stats.sync_gaps, analysis2.stats.sync_gaps);
    assert_eq!(
        analysis1.stats.partial_clears,
        analysis2.stats.partial_clears
    );
    analysis1.assert_flicker_free();
    analysis2.assert_flicker_free();
}
