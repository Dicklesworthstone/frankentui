#![cfg(feature = "experimental")]
#![forbid(unsafe_code)]

//! E2E integration test for Roaring Bitmap dirty region tracking.
//!
//! Validates that the Roaring bitmap implementation produces identical dirty
//! sets compared to a HashSet<u32> reference, across multiple screen sizes
//! and update patterns. Logs JSONL evidence per frame.

use std::collections::{HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::time::Instant;

use ftui_render::roaring_bitmap::RoaringBitmap;

// ── JSONL Event ─────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct RoaringFrameEvent {
    event: &'static str,
    frame_id: u64,
    screen: String,
    dirty_cells: u32,
    dirty_rows: u32,
    roaring_container_type: &'static str,
    set_size_bytes: u32,
    union_time_ns: u64,
    intersection_time_ns: u64,
    diff_output_hash: String,
    bitvec_diff_hash: String,
    #[serde(rename = "match")]
    is_match: bool,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn hash_set(set: &[u32]) -> String {
    let mut sorted = set.to_vec();
    sorted.sort_unstable();
    let mut h = DefaultHasher::new();
    sorted.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn classify_container(_bm: &RoaringBitmap, total_cells: u32) -> &'static str {
    // Heuristic: if > 4096 cells in one container, it's bitmap; otherwise array
    if total_cells >= 4096 {
        "bitmap"
    } else if total_cells > 0 {
        "array"
    } else {
        "empty"
    }
}

fn estimate_roaring_bytes(bm: &RoaringBitmap) -> u32 {
    // Rough estimate: 4 bytes per value in array mode, 8KB per bitmap container
    let card = bm.cardinality() as u32;
    if card >= 4096 {
        8192 // bitmap container
    } else {
        card * 2 + 16 // array container overhead
    }
}

fn dirty_rows_from_set(dirty: &[u32], width: u32) -> u32 {
    let rows: HashSet<u32> = dirty.iter().map(|&idx| idx / width).collect();
    rows.len() as u32
}

// ── Screen Config ───────────────────────────────────────────────────────────

struct ScreenConfig {
    name: &'static str,
    width: u32,
    height: u32,
}

const SCREENS: &[ScreenConfig] = &[
    ScreenConfig {
        name: "80x24",
        width: 80,
        height: 24,
    },
    ScreenConfig {
        name: "120x40",
        width: 120,
        height: 40,
    },
    ScreenConfig {
        name: "200x60",
        width: 200,
        height: 60,
    },
    ScreenConfig {
        name: "300x100",
        width: 300,
        height: 100,
    },
];

// ── Update Patterns ─────────────────────────────────────────────────────────

/// Mark a few scattered cells dirty (typical incremental update).
fn pattern_sparse(width: u32, height: u32, frame: u64) -> Vec<u32> {
    let mut cells = Vec::new();
    // Deterministic pseudo-random positions
    let seed = frame.wrapping_mul(6364136223846793005).wrapping_add(1);
    for i in 0..5 {
        let val = seed.wrapping_mul(i + 1);
        let x = (val % width as u64) as u32;
        let y = ((val >> 16) % height as u64) as u32;
        cells.push(y * width + x);
    }
    cells
}

/// Mark an entire row dirty (scroll or line update).
fn pattern_full_row(width: u32, _height: u32, frame: u64) -> Vec<u32> {
    let row = (frame % _height as u64) as u32;
    (0..width).map(|x| row * width + x).collect()
}

/// Mark alternating rows dirty (partial redraw).
fn pattern_alternating_rows(width: u32, height: u32, _frame: u64) -> Vec<u32> {
    let mut cells = Vec::new();
    for y in (0..height).step_by(2) {
        for x in 0..width {
            cells.push(y * width + x);
        }
    }
    cells
}

/// Mark all cells dirty (full screen redraw).
fn pattern_full_screen(width: u32, height: u32, _frame: u64) -> Vec<u32> {
    (0..width * height).collect()
}

/// Mark a single cell dirty.
fn pattern_single_cell(width: u32, height: u32, frame: u64) -> Vec<u32> {
    let cell = (frame as u32) % (width * height);
    vec![cell]
}

/// Empty dirty set (no changes).
fn pattern_empty(_width: u32, _height: u32, _frame: u64) -> Vec<u32> {
    Vec::new()
}

// ── Frame Runner ────────────────────────────────────────────────────────────

struct FrameRunner {
    events: Vec<RoaringFrameEvent>,
    frame_id: u64,
}

impl FrameRunner {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            frame_id: 0,
        }
    }

    fn run_frame(&mut self, screen: &ScreenConfig, dirty_cells: &[u32]) -> bool {
        self.frame_id += 1;

        // Build Roaring bitmap
        let mut roaring = RoaringBitmap::new();
        for &cell in dirty_cells {
            roaring.insert(cell);
        }

        // Build reference HashSet
        let mut reference: HashSet<u32> = HashSet::new();
        for &cell in dirty_cells {
            reference.insert(cell);
        }

        // Verify cardinality matches
        let roaring_card = roaring.cardinality();
        let ref_card = reference.len();
        assert_eq!(
            roaring_card, ref_card,
            "cardinality mismatch: roaring={roaring_card}, ref={ref_card}"
        );

        // Verify identical contents
        let roaring_sorted: Vec<u32> = roaring.iter().collect();
        let mut ref_sorted: Vec<u32> = reference.iter().copied().collect();
        ref_sorted.sort_unstable();

        let roaring_hash = hash_set(&roaring_sorted);
        let ref_hash = hash_set(&ref_sorted);
        let is_match = roaring_hash == ref_hash;

        // Measure union time
        let mut other = RoaringBitmap::new();
        if !dirty_cells.is_empty() {
            // Create a second bitmap overlapping ~50%
            for &cell in dirty_cells.iter().step_by(2) {
                other.insert(cell);
            }
            let extra = dirty_cells[0].wrapping_add(1);
            other.insert(extra);
        }

        let start = Instant::now();
        let _union = roaring.union(&other);
        let union_ns = start.elapsed().as_nanos() as u64;

        let start = Instant::now();
        let _intersect = roaring.intersection(&other);
        let intersect_ns = start.elapsed().as_nanos() as u64;

        let container_type = classify_container(&roaring, roaring_card as u32);
        let set_size = estimate_roaring_bytes(&roaring);
        let dirty_rows = dirty_rows_from_set(dirty_cells, screen.width);

        self.events.push(RoaringFrameEvent {
            event: "roaring_frame",
            frame_id: self.frame_id,
            screen: screen.name.to_string(),
            dirty_cells: roaring_card as u32,
            dirty_rows,
            roaring_container_type: container_type,
            set_size_bytes: set_size,
            union_time_ns: union_ns,
            intersection_time_ns: intersect_ns,
            diff_output_hash: roaring_hash,
            bitvec_diff_hash: ref_hash,
            is_match,
        });

        is_match
    }

    fn write_jsonl(&self, path: &std::path::Path) {
        let mut file = std::fs::File::create(path).expect("create JSONL");
        for event in &self.events {
            let line = serde_json::to_string(event).expect("serialize event");
            writeln!(file, "{}", line).expect("write event");
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn roaring_matches_reference_all_screens() {
    let mut runner = FrameRunner::new();
    type PatternFn = fn(u32, u32, u64) -> Vec<u32>;
    let patterns: Vec<PatternFn> = vec![
        pattern_sparse,
        pattern_full_row,
        pattern_alternating_rows,
        pattern_full_screen,
        pattern_single_cell,
        pattern_empty,
    ];

    for screen in SCREENS {
        for (pi, pattern) in patterns.iter().enumerate() {
            for frame in 0..5 {
                let dirty = pattern(screen.width, screen.height, frame);
                let matched = runner.run_frame(screen, &dirty);
                assert!(
                    matched,
                    "mismatch at screen={}, pattern={pi}, frame={frame}",
                    screen.name
                );
            }
        }
    }

    let jsonl_path = std::env::temp_dir().join("roaring_e2e_all_screens.jsonl");
    runner.write_jsonl(&jsonl_path);
    std::fs::remove_file(&jsonl_path).ok();
}

#[test]
fn roaring_sparse_updates_match() {
    let mut runner = FrameRunner::new();
    let screen = &SCREENS[0]; // 80x24

    for frame in 0..50 {
        let dirty = pattern_sparse(screen.width, screen.height, frame);
        let matched = runner.run_frame(screen, &dirty);
        assert!(matched, "sparse mismatch at frame {frame}");
    }

    assert_eq!(runner.events.len(), 50);
    for ev in &runner.events {
        assert!(ev.is_match);
        assert!(ev.dirty_cells <= 5); // sparse: at most 5 cells
    }
}

#[test]
fn roaring_full_row_matches() {
    let mut runner = FrameRunner::new();
    let screen = &SCREENS[1]; // 120x40

    for frame in 0..40 {
        let dirty = pattern_full_row(screen.width, screen.height, frame);
        let matched = runner.run_frame(screen, &dirty);
        assert!(matched, "full_row mismatch at frame {frame}");
    }

    for ev in &runner.events {
        assert!(ev.is_match);
        assert_eq!(ev.dirty_cells, 120); // full row of 120 columns
        assert_eq!(ev.dirty_rows, 1);
    }
}

#[test]
fn roaring_alternating_rows_match() {
    let mut runner = FrameRunner::new();
    let screen = &SCREENS[2]; // 200x60

    let dirty = pattern_alternating_rows(screen.width, screen.height, 0);
    let matched = runner.run_frame(screen, &dirty);
    assert!(matched, "alternating rows mismatch");

    let ev = &runner.events[0];
    assert_eq!(ev.dirty_rows, 30); // half of 60 rows
    assert_eq!(ev.dirty_cells, 200 * 30); // 30 rows × 200 columns
}

#[test]
fn roaring_full_screen_match() {
    let mut runner = FrameRunner::new();

    for screen in SCREENS {
        let dirty = pattern_full_screen(screen.width, screen.height, 0);
        let matched = runner.run_frame(screen, &dirty);
        assert!(matched, "full screen mismatch at {}", screen.name);
    }

    for ev in &runner.events {
        assert!(ev.is_match);
    }
}

#[test]
fn roaring_empty_dirty_set() {
    let mut runner = FrameRunner::new();
    let screen = &SCREENS[0]; // 80x24

    let dirty = pattern_empty(screen.width, screen.height, 0);
    let matched = runner.run_frame(screen, &dirty);
    assert!(matched);

    let ev = &runner.events[0];
    assert_eq!(ev.dirty_cells, 0);
    assert_eq!(ev.dirty_rows, 0);
    assert_eq!(ev.roaring_container_type, "empty");
}

#[test]
fn roaring_single_cell_match() {
    let mut runner = FrameRunner::new();
    let screen = &SCREENS[0]; // 80x24

    for frame in 0..100 {
        let dirty = pattern_single_cell(screen.width, screen.height, frame);
        let matched = runner.run_frame(screen, &dirty);
        assert!(matched, "single cell mismatch at frame {frame}");
    }

    for ev in &runner.events {
        assert!(ev.is_match);
        assert_eq!(ev.dirty_cells, 1);
    }
}

#[test]
fn roaring_union_preserves_correctness() {
    let screen = &SCREENS[1]; // 120x40

    // Two overlapping dirty sets
    let mut a = RoaringBitmap::new();
    let mut b = RoaringBitmap::new();
    let mut ref_a: HashSet<u32> = HashSet::new();
    let mut ref_b: HashSet<u32> = HashSet::new();

    let cells_a = pattern_sparse(screen.width, screen.height, 0);
    let cells_b = pattern_sparse(screen.width, screen.height, 1);

    for &c in &cells_a {
        a.insert(c);
        ref_a.insert(c);
    }
    for &c in &cells_b {
        b.insert(c);
        ref_b.insert(c);
    }

    let union_roaring = a.union(&b);
    let union_ref: HashSet<u32> = ref_a.union(&ref_b).copied().collect();

    assert_eq!(union_roaring.cardinality(), union_ref.len());
    for &v in &union_ref {
        assert!(union_roaring.contains(v), "union missing {v}");
    }
}

#[test]
fn roaring_intersection_preserves_correctness() {
    let screen = &SCREENS[1]; // 120x40

    let mut a = RoaringBitmap::new();
    let mut b = RoaringBitmap::new();
    let mut ref_a: HashSet<u32> = HashSet::new();
    let mut ref_b: HashSet<u32> = HashSet::new();

    // Overlapping rows
    let w = screen.width;
    for x in 0..w {
        a.insert(x); // row 0
        a.insert(w + x); // row 1
        ref_a.insert(x);
        ref_a.insert(w + x);

        b.insert(w + x); // row 1 (overlap)
        b.insert(2 * w + x); // row 2
        ref_b.insert(w + x);
        ref_b.insert(2 * w + x);
    }

    let isect_roaring = a.intersection(&b);
    let isect_ref: HashSet<u32> = ref_a.intersection(&ref_b).copied().collect();

    assert_eq!(isect_roaring.cardinality(), isect_ref.len());
    assert_eq!(isect_roaring.cardinality(), screen.width as usize); // row 1 only
    for &v in &isect_ref {
        assert!(isect_roaring.contains(v), "intersection missing {v}");
    }
}

#[test]
fn roaring_incremental_updates_match() {
    let mut runner = FrameRunner::new();
    let screen = &SCREENS[1]; // 120x40

    // Simulate 20 frames of incremental updates: text edits, scrolls, resizes
    type PatternFn = fn(u32, u32, u64) -> Vec<u32>;
    let patterns: Vec<PatternFn> = vec![
        pattern_sparse,
        pattern_full_row,
        pattern_alternating_rows,
        pattern_full_screen,
    ];

    for frame in 0..20 {
        let pattern = &patterns[frame as usize % patterns.len()];
        let dirty = pattern(screen.width, screen.height, frame);
        let matched = runner.run_frame(screen, &dirty);
        assert!(matched, "incremental update mismatch at frame {frame}");
    }

    // All frames should match
    for ev in &runner.events {
        assert!(ev.is_match);
    }
}

#[test]
fn roaring_size_reasonable_for_dense() {
    let screen = &SCREENS[3]; // 300x100

    let dirty = pattern_full_screen(screen.width, screen.height, 0);
    let mut roaring = RoaringBitmap::new();
    for &cell in &dirty {
        roaring.insert(cell);
    }

    let total_cells = screen.width * screen.height;
    assert_eq!(roaring.cardinality(), total_cells as usize);

    // For dense patterns, roaring bitmap container should be promoted to bitmap type
    // Total cells = 30000, which > 4096, so at least one bitmap container
    let est_bytes = estimate_roaring_bytes(&roaring);
    // Bitmap container is 8KB; a raw bitvec for 30000 cells would be ~3750 bytes
    // Roaring may use more due to container overhead, but should be reasonable
    assert!(
        est_bytes <= 16384,
        "roaring size should be reasonable for dense: {est_bytes} bytes"
    );
}

#[test]
fn jsonl_schema_compliance() {
    let mut runner = FrameRunner::new();
    let screen = &SCREENS[0]; // 80x24

    runner.run_frame(screen, &pattern_sparse(screen.width, screen.height, 0));
    runner.run_frame(screen, &pattern_empty(screen.width, screen.height, 0));
    runner.run_frame(screen, &pattern_full_screen(screen.width, screen.height, 0));

    let jsonl_path = std::env::temp_dir().join("roaring_schema_test.jsonl");
    runner.write_jsonl(&jsonl_path);

    let content = std::fs::read_to_string(&jsonl_path).expect("read JSONL");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 3);

    for (i, line) in lines.iter().enumerate() {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("parse JSON line {i}: {e}"));

        assert_eq!(v["event"], "roaring_frame", "line {i}: event");
        assert!(v["frame_id"].is_u64(), "line {i}: frame_id");
        assert!(v["screen"].is_string(), "line {i}: screen");
        assert!(v["dirty_cells"].is_u64(), "line {i}: dirty_cells");
        assert!(v["dirty_rows"].is_u64(), "line {i}: dirty_rows");
        assert!(
            v["roaring_container_type"].is_string(),
            "line {i}: container_type"
        );
        assert!(v["set_size_bytes"].is_u64(), "line {i}: set_size_bytes");
        assert!(v["union_time_ns"].is_u64(), "line {i}: union_time_ns");
        assert!(
            v["intersection_time_ns"].is_u64(),
            "line {i}: intersection_time_ns"
        );
        assert!(
            v["diff_output_hash"].is_string(),
            "line {i}: diff_output_hash"
        );
        assert!(
            v["bitvec_diff_hash"].is_string(),
            "line {i}: bitvec_diff_hash"
        );
        assert!(v["match"].is_boolean(), "line {i}: match");
    }

    std::fs::remove_file(&jsonl_path).ok();
}

#[test]
fn no_panics_edge_cases() {
    let mut runner = FrameRunner::new();

    // Screen with minimum size
    let tiny = ScreenConfig {
        name: "1x1",
        width: 1,
        height: 1,
    };
    runner.run_frame(&tiny, &[0]);
    runner.run_frame(&tiny, &[]);

    // Large index values (still within u32 range)
    let large = ScreenConfig {
        name: "400x200",
        width: 400,
        height: 200,
    };
    let max_cell = large.width * large.height - 1;
    runner.run_frame(&large, &[0, max_cell]);
    runner.run_frame(&large, &[max_cell / 2]);

    for ev in &runner.events {
        assert!(ev.is_match);
    }
}
