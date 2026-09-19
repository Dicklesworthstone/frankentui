#![forbid(unsafe_code)]

//! Snapshot Player E2E and Regression Tests (bd-3sa7.2).
//!
//! Tests playback correctness, determinism, and ordering with verbose JSONL logging.
//!
//! # Invariants
//!
//! 1. **Playback determinism**: Same initial state + same tick sequence = identical frame sequence
//! 2. **Progress bounds**: `0 <= current_frame < frame_count` always
//! 3. **Checksum chain integrity**: Each frame's checksum contributes to chain
//! 4. **Looping correctness**: At end, playback wraps to frame 0
//! 5. **Pause stability**: When paused, ticks don't advance frames
//!
//! # Failure Modes
//!
//! - Frame index out of bounds (panic)
//! - Checksum mismatch on replay (data corruption)
//! - Non-deterministic frame progression (timing issues)
//! - Marker state corruption after clear

use std::sync::OnceLock;
use std::time::Instant;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::screens::Screen;
use ftui_demo_showcase::screens::snapshot_player::{
    PlaybackState, SnapshotPlayer, SnapshotPlayerConfig,
};
use ftui_demo_showcase::test_logging::JsonlLogger;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;

// ---------------------------------------------------------------------------
// JSONL Logging Helpers
// ---------------------------------------------------------------------------

fn jsonl_logger() -> &'static JsonlLogger {
    static LOGGER: OnceLock<JsonlLogger> = OnceLock::new();
    LOGGER.get_or_init(|| JsonlLogger::new("snapshot_player_e2e").with_context("suite", "snapshot"))
}

fn log_case(
    case: &str,
    outcome: &str,
    frame_count: usize,
    final_frame: usize,
    checksum_chain: u64,
    elapsed_ms: u128,
    notes: &str,
) {
    let frame_count = frame_count.to_string();
    let final_frame = final_frame.to_string();
    let checksum_chain = format!("0x{:016x}", checksum_chain);
    let elapsed_ms = elapsed_ms.to_string();
    jsonl_logger().log(
        "case_complete",
        &[
            ("case", case),
            ("outcome", outcome),
            ("frame_count", &frame_count),
            ("final_frame", &final_frame),
            ("checksum_chain", &checksum_chain),
            ("elapsed_ms", &elapsed_ms),
            ("notes", notes),
        ],
    );
}

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::NONE,
        kind: KeyEventKind::Press,
    })
}

// ---------------------------------------------------------------------------
// Playback Determinism Tests
// ---------------------------------------------------------------------------

/// Invariant: Same tick sequence produces identical frame progression.
#[test]
fn playback_determinism_same_ticks() {
    let start = Instant::now();

    // Create two identical players
    let config = SnapshotPlayerConfig {
        max_frames: 100,
        playback_speed: 1,
        auto_generate_demo: true,
        demo_frame_count: 20,
    };

    let mut player1 = SnapshotPlayer::with_config(config.clone());
    let mut player2 = SnapshotPlayer::with_config(config);

    // Start playback on both
    player1.toggle_playback();
    player2.toggle_playback();

    // Apply identical tick sequence
    let mut frames1 = Vec::new();
    let mut frames2 = Vec::new();

    for tick in 0..50 {
        player1.tick(tick * 2);
        player2.tick(tick * 2);

        frames1.push(player1.current_frame());
        frames2.push(player2.current_frame());
    }

    // Verify determinism
    assert_eq!(frames1, frames2, "Frame sequences must be identical");

    let notes = format!(
        "Verified {} ticks produce identical sequence",
        frames1.len()
    );
    log_case(
        "playback_determinism_same_ticks",
        "pass",
        player1.frame_count(),
        player1.current_frame(),
        player1.checksum_chain(),
        start.elapsed().as_millis(),
        &notes,
    );
}

/// Invariant: Replay from same state produces same checksums.
#[test]
fn checksum_chain_determinism() {
    let start = Instant::now();

    let config = SnapshotPlayerConfig {
        max_frames: 50,
        playback_speed: 1,
        auto_generate_demo: true,
        demo_frame_count: 15,
    };

    let player1 = SnapshotPlayer::with_config(config.clone());
    let player2 = SnapshotPlayer::with_config(config);

    // Checksum chains should be identical for identical generated content
    assert_eq!(
        player1.checksum_chain(),
        player2.checksum_chain(),
        "Checksum chains must match for identical content"
    );

    // Individual frame checksums should match
    for i in 0..player1.frame_count() {
        let info1 = &player1.frame_info()[i];
        let info2 = &player2.frame_info()[i];
        assert_eq!(
            info1.checksum, info2.checksum,
            "Frame {} checksum mismatch",
            i
        );
    }

    let notes = format!("Verified {} frame checksums match", player1.frame_count());
    log_case(
        "checksum_chain_determinism",
        "pass",
        player1.frame_count(),
        player1.current_frame(),
        player1.checksum_chain(),
        start.elapsed().as_millis(),
        &notes,
    );
}

// ---------------------------------------------------------------------------
// Progress Bounds Tests
// ---------------------------------------------------------------------------

/// Invariant: current_frame is always within valid bounds.
#[test]
fn frame_index_bounds_invariant() {
    let start = Instant::now();

    let mut player = SnapshotPlayer::new();
    let frame_count = player.frame_count();

    // Test rapid navigation doesn't break bounds
    for _ in 0..1000 {
        player.step_forward();
        assert!(
            player.current_frame() < frame_count,
            "Frame index {} exceeds count {}",
            player.current_frame(),
            frame_count
        );
    }

    for _ in 0..1000 {
        player.step_backward();
        assert!(
            player.current_frame() < frame_count,
            "Frame index {} exceeds count {}",
            player.current_frame(),
            frame_count
        );
    }

    // Home/End navigation
    player.go_to_end();
    assert_eq!(player.current_frame(), frame_count - 1);
    player.go_to_start();
    assert_eq!(player.current_frame(), 0);

    log_case(
        "frame_index_bounds_invariant",
        "pass",
        frame_count,
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        "Verified 2000+ navigation ops maintain bounds",
    );
}

/// Invariant: Empty player handles navigation gracefully.
#[test]
fn empty_player_navigation_safety() {
    let start = Instant::now();

    let config = SnapshotPlayerConfig {
        max_frames: 10,
        playback_speed: 1,
        auto_generate_demo: false,
        demo_frame_count: 0,
    };

    let mut player = SnapshotPlayer::with_config(config);
    assert_eq!(player.frame_count(), 0);

    // All navigation should be safe on empty player
    player.step_forward();
    player.step_backward();
    player.go_to_start();
    player.go_to_end();
    player.toggle_playback();
    player.tick(100);

    assert_eq!(player.current_frame(), 0);

    log_case(
        "empty_player_navigation_safety",
        "pass",
        0,
        0,
        0,
        start.elapsed().as_millis(),
        "Empty player handles all navigation safely",
    );
}

// ---------------------------------------------------------------------------
// Looping Correctness Tests
// ---------------------------------------------------------------------------

/// Invariant: Playback loops correctly from end to start.
#[test]
fn playback_loop_regression() {
    let start = Instant::now();

    let config = SnapshotPlayerConfig {
        max_frames: 50,
        playback_speed: 1,
        auto_generate_demo: true,
        demo_frame_count: 10,
    };

    let mut player = SnapshotPlayer::with_config(config);
    player.go_to_end();
    let last_frame = player.current_frame();

    player.toggle_playback(); // Start playing

    // Tick should advance and loop
    player.tick(2);
    assert_eq!(
        player.current_frame(),
        0,
        "Should loop to frame 0, got {}",
        player.current_frame()
    );

    // Continue playing should advance normally
    player.tick(4);
    assert_eq!(
        player.current_frame(),
        1,
        "Should advance to frame 1 after loop"
    );

    let notes = format!("Verified loop from frame {} to 0", last_frame);
    log_case(
        "playback_loop_regression",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        &notes,
    );
}

// ---------------------------------------------------------------------------
// Pause Stability Tests
// ---------------------------------------------------------------------------

/// Invariant: When paused, ticks do not advance frames.
#[test]
fn pause_stability_regression() {
    let start = Instant::now();

    let mut player = SnapshotPlayer::new();
    assert_eq!(player.playback_state(), PlaybackState::Paused);

    let initial_frame = player.current_frame();

    // Many ticks while paused should not change frame
    for tick in 0..100 {
        player.tick(tick * 2);
    }

    assert_eq!(
        player.current_frame(),
        initial_frame,
        "Paused player should not advance"
    );

    log_case(
        "pause_stability_regression",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        "Verified 100 ticks while paused don't advance",
    );
}

/// Invariant: Manual step pauses playback.
#[test]
fn manual_step_pauses_playback() {
    let start = Instant::now();

    let mut player = SnapshotPlayer::new();
    player.toggle_playback();
    assert_eq!(player.playback_state(), PlaybackState::Playing);

    // Manual step should pause
    player.update(&press(KeyCode::Right));
    assert_eq!(
        player.playback_state(),
        PlaybackState::Paused,
        "Manual step should pause playback"
    );

    player.toggle_playback();
    player.update(&press(KeyCode::Left));
    assert_eq!(
        player.playback_state(),
        PlaybackState::Paused,
        "Manual step backward should pause"
    );

    log_case(
        "manual_step_pauses_playback",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        "Manual navigation correctly pauses playback",
    );
}

// ---------------------------------------------------------------------------
// Marker State Tests
// ---------------------------------------------------------------------------

/// Invariant: Markers persist correctly across navigation.
#[test]
fn marker_persistence_regression() {
    let start = Instant::now();

    let mut player = SnapshotPlayer::new();

    // Add markers at various frames
    let marker_positions = vec![0, 5, 10, 25, 49];
    for &pos in &marker_positions {
        player.set_current_frame(pos.min(player.frame_count().saturating_sub(1)));
        player.toggle_marker();
    }

    // Navigate around
    player.go_to_start();
    player.go_to_end();
    for _ in 0..20 {
        player.step_forward();
        player.step_backward();
    }

    // Verify markers persisted
    for &pos in &marker_positions {
        let pos = pos.min(player.frame_count() - 1);
        assert!(
            player.markers().contains(&pos),
            "Marker at {} should persist",
            pos
        );
    }

    let notes = format!("Verified {} markers persist", marker_positions.len());
    log_case(
        "marker_persistence_regression",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        &notes,
    );
}

/// Invariant: Clear removes all markers.
#[test]
fn clear_removes_markers_regression() {
    let start = Instant::now();

    let mut player = SnapshotPlayer::new();

    // Add several markers
    for i in 0..10 {
        player.set_current_frame(i * 5);
        player.toggle_marker();
    }
    let markers_before = player.markers().len();

    // Clear
    player.clear();

    assert!(
        player.markers().is_empty(),
        "Clear should remove all markers"
    );
    assert_eq!(player.frame_count(), 0);
    assert_eq!(player.current_frame(), 0);

    let notes = format!("Cleared {} markers", markers_before);
    log_case(
        "clear_removes_markers_regression",
        "pass",
        0,
        0,
        player.checksum_chain(),
        start.elapsed().as_millis(),
        &notes,
    );
}

// ---------------------------------------------------------------------------
// Recording Regression Tests
// ---------------------------------------------------------------------------

/// Invariant: Recording adds frames with correct metadata.
#[test]
fn recording_metadata_regression() {
    let start = Instant::now();

    let config = SnapshotPlayerConfig {
        max_frames: 100,
        playback_speed: 1,
        auto_generate_demo: false,
        demo_frame_count: 0,
    };

    let mut player = SnapshotPlayer::with_config(config);
    assert_eq!(player.frame_count(), 0);

    // Record some frames
    use ftui_render::buffer::Buffer;
    for i in 0..5 {
        let mut buf = Buffer::new(10, 5);
        buf.set(
            i as u16,
            0,
            ftui_render::cell::Cell::from_char((b'A' + i as u8) as char),
        );
        player.record_frame(&buf);
    }

    assert_eq!(player.frame_count(), 5);

    // Verify metadata
    for (i, info) in player.frame_info().iter().enumerate() {
        assert_eq!(info.index, i);
        assert_eq!(info.width, 10);
        assert_eq!(info.height, 5);
        assert!(info.checksum != 0, "Frame {} should have valid checksum", i);
    }

    log_case(
        "recording_metadata_regression",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        "Recording produces correct metadata",
    );
}

/// Invariant: Recording respects max_frames limit.
#[test]
fn recording_max_frames_regression() {
    let start = Instant::now();

    let config = SnapshotPlayerConfig {
        max_frames: 5,
        playback_speed: 1,
        auto_generate_demo: false,
        demo_frame_count: 0,
    };

    let mut player = SnapshotPlayer::with_config(config);

    use ftui_render::buffer::Buffer;
    // Record more than max
    for _ in 0..10 {
        let buf = Buffer::new(5, 5);
        player.record_frame(&buf);
    }

    assert_eq!(player.frame_count(), 5, "Should cap at max_frames");

    // Verify indices are re-numbered
    for (i, info) in player.frame_info().iter().enumerate() {
        assert_eq!(info.index, i, "Frame {} should have correct index", i);
    }

    log_case(
        "recording_max_frames_regression",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        "Max frames limit enforced correctly",
    );
}

// ---------------------------------------------------------------------------
// Rendering Regression Tests
// ---------------------------------------------------------------------------

/// Invariant: Rendering at various sizes doesn't panic.
#[test]
fn render_stress_regression() {
    let start = Instant::now();

    let player = SnapshotPlayer::new();
    let sizes = [
        (1, 1),
        (10, 5),
        (40, 10),
        (80, 24),
        (120, 40),
        (200, 50),
        (300, 100),
    ];

    for (w, h) in sizes {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, h, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, w, h));
    }

    let notes = format!("Rendered at {} different sizes without panic", sizes.len());
    log_case(
        "render_stress_regression",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        &notes,
    );
}

/// Invariant: Rendering during playback is stable.
#[test]
fn render_during_playback_regression() {
    let start = Instant::now();

    let mut player = SnapshotPlayer::new();
    player.toggle_playback();

    for tick in 0..100 {
        player.tick(tick);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        player.view(&mut frame, Rect::new(0, 0, 80, 24));
    }

    log_case(
        "render_during_playback_regression",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        "100 render cycles during playback succeeded",
    );
}

// ---------------------------------------------------------------------------
// State Machine Tests
// ---------------------------------------------------------------------------

/// Invariant: State transitions follow expected patterns.
#[test]
fn playback_state_machine_regression() {
    let start = Instant::now();

    let mut player = SnapshotPlayer::new();

    // Initial state
    assert_eq!(player.playback_state(), PlaybackState::Paused);

    // Paused -> Playing
    player.toggle_playback();
    assert_eq!(player.playback_state(), PlaybackState::Playing);

    // Playing -> Paused
    player.toggle_playback();
    assert_eq!(player.playback_state(), PlaybackState::Paused);

    // Paused -> Recording
    player.toggle_recording();
    assert_eq!(player.playback_state(), PlaybackState::Recording);

    // Recording -> Paused
    player.toggle_recording();
    assert_eq!(player.playback_state(), PlaybackState::Paused);

    // Recording -> Playing (via toggle_playback)
    player.toggle_recording();
    player.toggle_playback();
    assert_eq!(player.playback_state(), PlaybackState::Playing);

    log_case(
        "playback_state_machine_regression",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        "All state transitions verified",
    );
}

/// Invariant: Scrubbing to an index renders that frame.
#[test]
fn scrub_to_index_renders_that_frame() {
    let start = Instant::now();
    let mut player = SnapshotPlayer::new();
    assert!(player.frame_count() >= 10);
    player.set_current_frame(5);
    assert_eq!(player.current_frame(), 5);
    let expected = player.time_travel.get(5);
    assert!(expected.is_some());
    assert_eq!(player.current_buffer(), expected);

    log_case(
        "scrub_to_index_renders_that_frame",
        "pass",
        player.frame_count(),
        player.current_frame(),
        player.checksum_chain(),
        start.elapsed().as_millis(),
        "Scrubber rendered frame matches time_travel.get(5)",
    );
}
