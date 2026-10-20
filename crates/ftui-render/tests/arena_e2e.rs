//! E2E integration tests: per-frame arena allocation in the render pipeline (bd-2alzw.5)
//!
//! Validates that FrameArena integrates correctly with Frame rendering:
//! 1. Arena accessible from Frame via set_arena/arena() API.
//! 2. Arena-backed scratch allocations work during widget rendering simulation.
//! 3. Memory stays bounded across multi-frame reset cycles.
//! 4. Guardrails correctly account for arena memory.
//! 5. Complex nested allocation patterns don't leak or panic.
//!
//! All tests emit structured JSONL evidence to stdout.
//!
//! Run:
//!   cargo test -p ftui-render --test arena_e2e -- --nocapture

use ftui_render::arena::FrameArena;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_render::frame_guardrails::{
    FrameGuardrails, GuardrailsConfig, MemoryBudgetConfig, buffer_memory_bytes,
};
use ftui_render::grapheme_pool::GraphemePool;
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

// ── JSONL Evidence ──────────────────────────────────────────────────

#[derive(Serialize)]
struct Evidence {
    test: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screen: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arena_bytes_used: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arena_bytes_capacity: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arena_reset: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_match: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alloc_time_ns: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    pass: bool,
}

impl Evidence {
    fn new(test: &'static str) -> Self {
        Self {
            test,
            frame_id: None,
            screen: None,
            arena_bytes_used: None,
            arena_bytes_capacity: None,
            arena_reset: None,
            frame_hash: None,
            baseline_match: None,
            alloc_time_ns: None,
            detail: None,
            pass: true,
        }
    }

    fn emit(&self) {
        println!("{}", serde_json::to_string(self).unwrap());
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Hash the entire buffer content for frame comparison.
///
/// Cell doesn't implement Hash, so we extract the char content per cell.
fn hash_buffer(buf: &Buffer) -> u64 {
    let mut hasher = DefaultHasher::new();
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            if let Some(cell) = buf.get(x, y) {
                let ch = cell.content.as_char().unwrap_or('\0');
                ch.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Simulate widget rendering by writing characters + using arena for scratch data.
///
/// Splits arena allocation from buffer mutation to satisfy the borrow checker:
/// `frame.arena()` borrows `frame` immutably, so we collect data first, then write.
fn render_with_arena(frame: &mut Frame, label: &str) {
    // Phase 1: arena scratch allocations (immutable borrow of frame via arena())
    let (formatted_chars, coords) = {
        let arena = frame.arena().expect("arena should be set");
        let formatted = arena.alloc_fmt(format_args!("Screen: {}", label));
        let coords: &[u16] = arena.alloc_slice(&[0, 1, 2, 3, 4]);
        // Collect chars to a heap vec so we can release the borrow
        let chars: Vec<char> = formatted.chars().collect();
        let coords_vec: Vec<u16> = coords.to_vec();
        (chars, coords_vec)
    };

    // Phase 2: buffer writes (mutable borrow of frame.buffer)
    for (i, ch) in formatted_chars.iter().enumerate() {
        let x = i as u16;
        if x < frame.buffer.width() {
            frame.buffer.set_raw(x, 0, Cell::from_char(*ch));
        }
    }
    for &y in &coords {
        if y < frame.buffer.height() {
            for x in 0..frame.buffer.width().min(10) {
                frame.buffer.set_raw(x, y, Cell::from_char('#'));
            }
        }
    }
}

/// Simulate widget rendering WITHOUT arena (baseline).
fn render_without_arena(frame: &mut Frame, label: &str) {
    let formatted = format!("Screen: {}", label);
    let coords: Vec<u16> = vec![0, 1, 2, 3, 4];

    for (i, ch) in formatted.chars().enumerate() {
        let x = i as u16;
        if x < frame.buffer.width() {
            frame.buffer.set_raw(x, 0, Cell::from_char(ch));
        }
    }

    for &y in &coords {
        if y < frame.buffer.height() {
            for x in 0..frame.buffer.width().min(10) {
                frame.buffer.set_raw(x, y, Cell::from_char('#'));
            }
        }
    }
}

// ============================================================================
// Test 1: Arena accessible from Frame via set_arena/arena()
// ============================================================================

#[test]
fn arena_frame_accessor_roundtrip() {
    let arena = FrameArena::new(4096);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);

    // Before set_arena, arena() returns None
    assert!(frame.arena().is_none());

    frame.set_arena(&arena);
    assert!(frame.arena().is_some());

    // Can allocate through the accessor
    let s = frame.arena().unwrap().alloc_str("hello from frame");
    assert_eq!(s, "hello from frame");

    let mut ev = Evidence::new("arena_frame_accessor_roundtrip");
    ev.pass = true;
    ev.detail = Some("set_arena/arena() roundtrip works".into());
    ev.emit();
}

// ============================================================================
// Test 2: Arena-backed rendering matches baseline (zero visual diff)
// ============================================================================

#[test]
fn arena_rendering_matches_baseline() {
    let screens = ["dashboard", "settings", "help", "dataviz", "palette"];

    for screen in &screens {
        let arena = FrameArena::new(64 * 1024);

        // Render WITH arena
        let mut pool_a = GraphemePool::new();
        let mut frame_a = Frame::new(120, 40, &mut pool_a);
        frame_a.set_arena(&arena);
        render_with_arena(&mut frame_a, screen);
        let hash_arena = hash_buffer(&frame_a.buffer);

        // Render WITHOUT arena (baseline)
        let mut pool_b = GraphemePool::new();
        let mut frame_b = Frame::new(120, 40, &mut pool_b);
        render_without_arena(&mut frame_b, screen);
        let hash_baseline = hash_buffer(&frame_b.buffer);

        let matches = hash_arena == hash_baseline;
        assert!(matches, "Frame hash mismatch for screen '{screen}'");

        let mut ev = Evidence::new("arena_rendering_matches_baseline");
        ev.screen = Some(screen);
        ev.frame_hash = Some(format!("{:016x}", hash_arena));
        ev.baseline_match = Some(matches);
        ev.arena_bytes_used = Some(arena.allocated_bytes());
        ev.pass = matches;
        ev.emit();
    }
}

// ============================================================================
// Test 3: Multi-frame render cycle — memory stays bounded
// ============================================================================

#[test]
fn arena_multi_frame_memory_bounded() {
    let mut arena = FrameArena::new(64 * 1024);
    let num_frames: u64 = 100;
    let mut peak_bytes = 0usize;
    let mut first_frame_bytes = 0usize;

    for frame_id in 0..num_frames {
        {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            frame.set_arena(&arena);
            render_with_arena(&mut frame, "dashboard");
        }

        let used = arena.allocated_bytes();
        if frame_id == 0 {
            first_frame_bytes = used;
        }
        if used > peak_bytes {
            peak_bytes = used;
        }

        if frame_id % 20 == 0 {
            let mut ev = Evidence::new("arena_multi_frame_memory_bounded");
            ev.frame_id = Some(frame_id);
            ev.arena_bytes_used = Some(used);
            ev.arena_bytes_capacity = Some(arena.allocated_bytes_including_metadata());
            ev.arena_reset = Some(true);
            ev.emit();
        }

        // Frame boundary: reset arena (retains largest chunk for reuse without growth)
        arena.reset();
        assert!(
            arena.allocated_bytes() <= first_frame_bytes,
            "Frame {frame_id}: retained chunk bytes ({}) should not exceed first frame ({first_frame_bytes})",
            arena.allocated_bytes()
        );
    }

    // After 100 frames, capacity should not grow unboundedly.
    let final_capacity = arena.allocated_bytes_including_metadata();
    assert!(
        first_frame_bytes > 0,
        "First frame should allocate arena memory"
    );
    assert_eq!(
        peak_bytes, first_frame_bytes,
        "Per-frame peak bytes should stay constant across identical frames"
    );
    assert!(
        final_capacity <= 128 * 1024,
        "Arena capacity grew beyond 2x initial chunk across {num_frames} resets: {final_capacity}"
    );

    let mut ev = Evidence::new("arena_multi_frame_memory_bounded");
    ev.frame_id = Some(num_frames);
    ev.arena_bytes_capacity = Some(final_capacity);
    ev.detail = Some(format!(
        "first_frame_bytes={first_frame_bytes}, peak={peak_bytes}, final_capacity={final_capacity}"
    ));
    ev.pass = true;
    ev.emit();
}

// ============================================================================
// Test 4: Arena reset occurs exactly once per frame boundary
// ============================================================================

#[test]
fn arena_reset_once_per_frame() {
    let mut arena = FrameArena::new(16 * 1024);
    let num_frames = 10u64;
    let initial_chunk_bytes = arena.allocated_bytes();

    for frame_id in 0..num_frames {
        // Simulate rendering: multiple allocations within a single frame
        {
            let _s1 = arena.alloc_str("header text");
            let _s2 = arena.alloc_str("body text");
            let _slice = arena.alloc_slice(&[1u32, 2, 3, 4, 5]);
            let mut v = arena.new_vec::<u16>();
            for i in 0..20 {
                v.push(i);
            }
            // v is dropped here, releasing the immutable borrow on arena
        }

        let bytes_before_reset = arena.allocated_bytes();
        assert!(
            bytes_before_reset > 0,
            "Frame {frame_id}: arena should have allocations before reset"
        );

        // Single reset at frame boundary — retains initial chunk without allocating extra chunks
        arena.reset();
        assert_eq!(
            arena.allocated_bytes(),
            initial_chunk_bytes,
            "Frame {frame_id}: arena should retain only initial chunk after reset"
        );

        let mut ev = Evidence::new("arena_reset_once_per_frame");
        ev.frame_id = Some(frame_id);
        ev.arena_bytes_used = Some(bytes_before_reset);
        ev.arena_reset = Some(true);
        ev.pass = true;
        ev.emit();
    }
}

// ============================================================================
// Test 5: Complex nested widget tree (deep allocation stress)
// ============================================================================

#[test]
fn arena_deep_nested_widgets() {
    let arena = FrameArena::new(256 * 1024);
    let start = Instant::now();

    // Simulate 50+ depth nested widget tree
    let depth = 50;
    let mut labels: Vec<&str> = Vec::new();

    for d in 0..depth {
        // Each "widget" at depth d allocates scratch data
        let label = arena.alloc_fmt(format_args!("widget_depth_{d}"));
        labels.push(label);

        // Simulate coordinate scratch per widget
        let _coords = arena.alloc_slice(&[d as u16; 4]);

        // Simulate text wrapping scratch (scoped to avoid borrow conflict)
        {
            let mut line_breaks = arena.new_vec::<u16>();
            for col in (0..200).step_by(20) {
                line_breaks.push(col);
            }
        }
    }

    let elapsed = start.elapsed();

    // Verify all labels survived (no corruption from arena growth)
    for (d, label) in labels.iter().enumerate() {
        assert_eq!(
            *label,
            format!("widget_depth_{d}"),
            "Label corruption at depth {d}"
        );
    }

    let mut ev = Evidence::new("arena_deep_nested_widgets");
    ev.arena_bytes_used = Some(arena.allocated_bytes());
    ev.arena_bytes_capacity = Some(arena.allocated_bytes_including_metadata());
    ev.alloc_time_ns = Some(elapsed.as_nanos());
    ev.detail = Some(format!("depth={depth}, all labels intact"));
    ev.pass = true;
    ev.emit();
}

// ============================================================================
// Test 6: Arena + frame guardrails memory accounting
// ============================================================================

#[test]
fn arena_guardrails_memory_accounting() {
    // Use a small arena to get predictable allocated_bytes()
    let arena = FrameArena::new(4096);
    let width: u16 = 80;
    let height: u16 = 24;

    // Small allocation
    let _small = arena.alloc_slice(&[0u8; 1_000]);
    let buffer_mem = buffer_memory_bytes(width, height);
    let arena_mem = arena.allocated_bytes();
    let total_mem = buffer_mem + arena_mem;

    // Set soft limit well above the initial total
    let soft_limit = total_mem * 2;
    let config = GuardrailsConfig {
        memory: MemoryBudgetConfig {
            soft_limit_bytes: soft_limit,
            hard_limit_bytes: soft_limit * 2,
            emergency_limit_bytes: soft_limit * 4,
        },
        ..Default::default()
    };
    let mut guardrails = FrameGuardrails::new(config);

    let verdict = guardrails.check_frame(total_mem, 0);

    let mut ev = Evidence::new("arena_guardrails_memory_accounting");
    ev.arena_bytes_used = Some(arena_mem);
    ev.detail = Some(format!(
        "buffer_mem={buffer_mem}, arena_mem={arena_mem}, total={total_mem}, \
         soft_limit={soft_limit}, should_degrade={}, is_clear={}",
        verdict.should_degrade(),
        verdict.is_clear()
    ));
    ev.pass = true;
    ev.emit();

    assert!(
        verdict.is_clear(),
        "Expected clear verdict for {total_mem} bytes (soft limit {soft_limit})"
    );

    // Now allocate enough to exceed the soft limit
    let big = vec![0u8; soft_limit];
    let _more = arena.alloc_slice(&big);
    let arena_mem2 = arena.allocated_bytes();
    let total_mem2 = buffer_mem + arena_mem2;
    let verdict2 = guardrails.check_frame(total_mem2, 0);

    assert!(
        verdict2.should_degrade(),
        "Expected degrade verdict for {total_mem2} bytes (soft limit {soft_limit})"
    );

    let mut ev2 = Evidence::new("arena_guardrails_memory_accounting");
    ev2.arena_bytes_used = Some(arena_mem2);
    ev2.detail = Some(format!(
        "after big alloc: total={total_mem2}, should_degrade={}",
        verdict2.should_degrade()
    ));
    ev2.pass = true;
    ev2.emit();
}

// ============================================================================
// Test 7: from_buffer constructor also supports arena
// ============================================================================

#[test]
fn arena_with_from_buffer_constructor() {
    let arena = FrameArena::new(4096);
    let buf = Buffer::new(40, 10);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::from_buffer(buf, &mut pool);

    // Arena starts as None
    assert!(frame.arena().is_none());

    frame.set_arena(&arena);
    let s = frame.arena().unwrap().alloc_str("from_buffer works");
    assert_eq!(s, "from_buffer works");

    let mut ev = Evidence::new("arena_with_from_buffer_constructor");
    ev.pass = true;
    ev.detail = Some("from_buffer + set_arena works".into());
    ev.emit();
}

// ============================================================================
// Test 8: Multi-screen render cycle with hash comparison
// ============================================================================

#[test]
fn arena_multi_screen_render_cycle() {
    let screens = ["dashboard", "settings", "help", "dataviz", "palette"];
    let frames_per_screen: u64 = 10;
    let mut arena = FrameArena::new(64 * 1024);

    for screen in &screens {
        let mut baseline_hash: Option<u64> = None;

        for frame_id in 0..frames_per_screen {
            let start = Instant::now();

            {
                let mut pool = GraphemePool::new();
                let mut frame = Frame::new(120, 40, &mut pool);
                frame.set_arena(&arena);
                render_with_arena(&mut frame, screen);

                let elapsed = start.elapsed();
                let frame_hash = hash_buffer(&frame.buffer);
                let arena_used = arena.allocated_bytes();

                // All frames for the same screen should produce identical output
                let matches = match baseline_hash {
                    None => {
                        baseline_hash = Some(frame_hash);
                        true
                    }
                    Some(base) => frame_hash == base,
                };

                assert!(
                    matches,
                    "Frame {frame_id} of screen '{screen}' hash mismatch"
                );

                let mut ev = Evidence::new("arena_multi_screen_render_cycle");
                ev.frame_id = Some(frame_id);
                ev.screen = Some(screen);
                ev.arena_bytes_used = Some(arena_used);
                ev.arena_reset = Some(true);
                ev.frame_hash = Some(format!("{:016x}", frame_hash));
                ev.baseline_match = Some(matches);
                ev.alloc_time_ns = Some(elapsed.as_nanos());
                ev.pass = matches;
                ev.emit();
            }

            // Reset at frame boundary (after frame is dropped)
            arena.reset();
        }
    }
}

// ============================================================================
// Test 9: BumpVec scratch collections work across arena reset
// ============================================================================

#[test]
fn arena_bump_vec_scratch_lifecycle() {
    let mut arena = FrameArena::new(16 * 1024);

    for frame_id in 0..20u64 {
        {
            // Create scratch vecs for this frame
            let mut lines = arena.new_vec::<&str>();
            lines.push("line 1");
            lines.push("line 2");
            lines.push("line 3");
            assert_eq!(lines.len(), 3);
            assert_eq!(lines[0], "line 1");

            let mut coords = arena.new_vec_with_capacity::<(u16, u16)>(100);
            for i in 0..50 {
                coords.push((i, i * 2));
            }
            assert_eq!(coords.len(), 50);
            assert_eq!(coords[25], (25, 50));

            // alloc_iter collects into arena
            let squares = arena.alloc_iter((0..10u32).map(|x| x * x));
            assert_eq!(squares, &[0, 1, 4, 9, 16, 25, 36, 49, 64, 81]);

            if frame_id % 5 == 0 {
                let mut ev = Evidence::new("arena_bump_vec_scratch_lifecycle");
                ev.frame_id = Some(frame_id);
                ev.arena_bytes_used = Some(arena.allocated_bytes());
                ev.pass = true;
                ev.emit();
            }
        }
        // BumpVecs are dropped, releasing immutable borrow — now we can reset
        arena.reset();
    }
}

// ============================================================================
// Test 10: Degenerate inputs — tiny arena, large allocations
// ============================================================================

#[test]
fn arena_degenerate_inputs() {
    // Tiny arena that must grow
    let arena = FrameArena::new(1);
    let s = arena.alloc_str("this forces arena growth");
    assert_eq!(s, "this forces arena growth");

    let mut ev = Evidence::new("arena_degenerate_inputs");
    ev.detail = Some(format!(
        "tiny arena grew to {} bytes",
        arena.allocated_bytes()
    ));
    ev.pass = true;
    ev.emit();

    // Large allocation in arena
    let big_arena = FrameArena::new(1024 * 1024);
    let big_slice = big_arena.alloc_slice(&[42u8; 500_000]);
    assert_eq!(big_slice.len(), 500_000);
    assert_eq!(big_slice[0], 42);
    assert_eq!(big_slice[499_999], 42);

    let mut ev2 = Evidence::new("arena_degenerate_inputs");
    ev2.detail = Some(format!(
        "large alloc: {} bytes in arena with {} capacity",
        big_arena.allocated_bytes(),
        big_arena.allocated_bytes_including_metadata()
    ));
    ev2.pass = true;
    ev2.emit();
}

// ============================================================================
// Test 11: Memory report — arena vs heap allocation comparison
// ============================================================================

#[test]
fn arena_memory_report() {
    let num_frames = 50;
    let allocs_per_frame = 200;
    let mut arena = FrameArena::new(128 * 1024);

    let start_arena = Instant::now();
    for _ in 0..num_frames {
        for i in 0..allocs_per_frame {
            let _s = arena.alloc_fmt(format_args!("widget_{i}_label"));
            let _sl = arena.alloc_slice(&[i as u32; 8]);
        }
        arena.reset();
    }
    let arena_elapsed = start_arena.elapsed();

    let start_heap = Instant::now();
    for _ in 0..num_frames {
        let mut strs = Vec::with_capacity(allocs_per_frame);
        let mut slices = Vec::with_capacity(allocs_per_frame);
        for i in 0..allocs_per_frame {
            strs.push(format!("widget_{i}_label"));
            slices.push(vec![i as u32; 8]);
        }
        drop(strs);
        drop(slices);
    }
    let heap_elapsed = start_heap.elapsed();

    let speedup = if arena_elapsed.as_nanos() > 0 {
        heap_elapsed.as_nanos() as f64 / arena_elapsed.as_nanos() as f64
    } else {
        f64::INFINITY
    };

    let final_capacity = arena.allocated_bytes_including_metadata();
    assert!(
        arena.allocated_bytes() > 0 && arena.allocated_bytes() <= final_capacity,
        "Arena retained bytes ({}) should be positive and bounded by capacity ({final_capacity})",
        arena.allocated_bytes()
    );
    assert!(
        final_capacity <= 256 * 1024,
        "Arena capacity should remain bounded across {num_frames} frames (got {final_capacity})"
    );
    assert!(
        speedup.is_finite() && speedup > 0.0,
        "Speedup ratio should be positive and finite (got {speedup})"
    );

    let mut ev = Evidence::new("arena_memory_report");
    ev.alloc_time_ns = Some(arena_elapsed.as_nanos());
    ev.arena_bytes_capacity = Some(final_capacity);
    ev.detail = Some(format!(
        "frames={num_frames}, allocs_per_frame={allocs_per_frame}, \
         arena_ns={}, heap_ns={}, speedup={speedup:.2}x",
        arena_elapsed.as_nanos(),
        heap_elapsed.as_nanos()
    ));
    ev.pass = true;
    ev.emit();
}
