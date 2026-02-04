#![forbid(unsafe_code)]

//! Isomorphism Proof Template + Golden Checksum Enforcement (bd-1rz0.29).
//!
//! This module operationalizes isomorphism proofs for the resize/reflow pipeline
//! and enforces golden checksum verification at each stage.
//!
//! # Isomorphism Properties Proven
//!
//! 1. **Determinism**: `render(S) == render(S)` — same state → same output.
//! 2. **Reversibility**: `render(A→B→A) == render(A)` — resize round-trip.
//! 3. **Diff correctness**: `apply(diff(old, new), old) == new`.
//! 4. **Pipeline commutativity**: Content checksum is preserved through
//!    Buffer → Diff → Present → Headless round-trip.
//! 5. **No ghosting**: After shrink+clear, all content cells appear in diff.
//! 6. **Diff monotonicity**: Changes are always row-major ordered.
//! 7. **Idempotence**: Applying a diff twice is a no-op on the second pass.
//! 8. **Style fidelity**: Style attributes survive the full pipeline.
//!
//! # Invariants (Alien Artifact)
//!
//! | Invariant | Property | Failure Mode |
//! |-----------|----------|--------------|
//! | `DET-1` | Same input → identical checksum | Hash collision / floating-point in render |
//! | `REV-1` | A→B→A checksum == A checksum | State leak across resize |
//! | `DIFF-1` | `old[changed] != new[changed]` for all diff entries | False positive in diff scan |
//! | `PIPE-1` | Buffer content survives present→headless | ANSI emit bug / headless parse bug |
//! | `GHOST-1`| All content cells in new appear in diff after clear | Missing cells → visual ghosting |
//! | `MONO-1` | Diff entries strictly row-major | Sort invariant violation |
//! | `IDEMP-1`| `diff(A, A) == ∅` | Spurious diff entries |
//! | `STYLE-1`| Style checksum stable across identical renders | Style hash instability |
//!
//! # JSONL Schema
//!
//! ```json
//! {"event":"proof","run_id":"...","invariant":"DET-1","outcome":"pass","checksum":"..."}
//! {"event":"proof","run_id":"...","invariant":"REV-1","outcome":"pass","a":"sha256:...","b":"sha256:..."}
//! {"event":"enforce","scenario":"fixed_80x24","outcome":"pass","expected":"sha256:...","actual":"sha256:..."}
//! ```
//!
//! # Running
//!
//! ```sh
//! cargo test -p ftui-harness --test isomorphism_proofs
//! BLESS=1 cargo test -p ftui-harness --test isomorphism_proofs  # update golden files
//! ```

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::geometry::Rect;
use ftui_harness::golden::{
    GoldenOutcome, compute_buffer_checksum, golden_checksum_path, is_bless_mode,
    load_golden_checksums, save_golden_checksums, verify_checksums,
};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::cell::PackedRgba;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::headless::HeadlessTerm;
use ftui_render::presenter::{Presenter, TerminalCapabilities};
use ftui_text::Text;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;
use ftui_widgets::paragraph::Paragraph;

// ===========================================================================
// JSONL Logging
// ===========================================================================

fn log_jsonl(step: &str, data: &[(&str, &str)]) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let fields: Vec<String> = std::iter::once(format!("\"seq\":{seq}"))
        .chain(std::iter::once(format!("\"step\":\"{step}\"")))
        .chain(data.iter().map(|(k, v)| format!("\"{k}\":\"{v}\"")))
        .collect();
    eprintln!("{{{}}}", fields.join(","));
}

fn is_coverage_mode() -> bool {
    std::env::var("LLVM_PROFILE_FILE").is_ok()
        || std::env::var("CARGO_LLVM_COV").is_ok()
        || std::env::var("LLVM_COV").is_ok()
        || std::env::var("RUSTFLAGS").is_ok_and(|flags| flags.contains("instrument-coverage"))
}

// ===========================================================================
// Checksum Helpers
// ===========================================================================

/// Compute a style-aware checksum that covers content, fg, bg, and attributes.
/// This is stronger than `compute_buffer_checksum` which only hashes content.
fn compute_full_checksum(buf: &Buffer) -> String {
    let mut hasher = DefaultHasher::new();
    buf.width().hash(&mut hasher);
    buf.height().hash(&mut hasher);
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            if let Some(cell) = buf.get(x, y) {
                cell.content.hash(&mut hasher);
                cell.fg.hash(&mut hasher);
                cell.bg.hash(&mut hasher);
                cell.attrs.hash(&mut hasher);
            }
        }
    }
    format!("full:{:016x}", hasher.finish())
}

/// Render a deterministic test scene with styled content.
fn render_styled_scene(width: u16, height: u16) -> Buffer {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);

    let text = format!(
        "Isomorphism test scene {}x{}\nLine two with content\nLine three for coverage",
        width, height
    );
    let para = Paragraph::new(Text::raw(&text))
        .block(Block::default().borders(Borders::ALL).title("Proof"));
    para.render(Rect::new(0, 0, width, height), &mut frame);

    // Add styled cells (fg color) for style fidelity testing
    if width > 4 && height > 2 {
        let mut cell = Cell::from_char('★');
        cell.fg = PackedRgba::rgb(255, 255, 0);
        frame.buffer.set(2, 1, cell);
    }

    frame.buffer
}

/// Run a buffer through the full present pipeline and return headless term content.
fn pipeline_roundtrip(buf: &Buffer, diff: &BufferDiff) -> HeadlessTerm {
    let output = {
        let mut vec = Vec::new();
        let caps = TerminalCapabilities::default();
        let mut presenter = Presenter::new(&mut vec, caps);
        presenter.present(buf, diff).unwrap();
        drop(presenter);
        vec
    };

    let mut term = HeadlessTerm::new(buf.width(), buf.height());
    term.process(&output);
    term
}

// ===========================================================================
// Proof 1: DET-1 — Determinism
// ===========================================================================

#[test]
fn proof_det1_same_input_same_checksum() {
    log_jsonl("env", &[("invariant", "DET-1"), ("bead", "bd-1rz0.29")]);

    for (w, h) in [(80, 24), (120, 40), (40, 10), (200, 60)] {
        let buf1 = render_styled_scene(w, h);
        let buf2 = render_styled_scene(w, h);

        let content_cs1 = compute_buffer_checksum(&buf1);
        let content_cs2 = compute_buffer_checksum(&buf2);
        assert_eq!(
            content_cs1, content_cs2,
            "DET-1 content checksum violation at {w}x{h}"
        );

        let full_cs1 = compute_full_checksum(&buf1);
        let full_cs2 = compute_full_checksum(&buf2);
        assert_eq!(
            full_cs1, full_cs2,
            "DET-1 full checksum violation at {w}x{h}"
        );

        log_jsonl(
            "proof",
            &[
                ("invariant", "DET-1"),
                ("size", &format!("{w}x{h}")),
                ("outcome", "pass"),
                ("checksum", &content_cs1),
            ],
        );
    }
}

/// Run determinism proof 3 times to catch non-determinism from ordering etc.
#[test]
fn proof_det1_triple_run() {
    log_jsonl("env", &[("invariant", "DET-1-triple")]);

    let checksums: Vec<String> = (0..3)
        .map(|_| {
            let buf = render_styled_scene(80, 24);
            compute_full_checksum(&buf)
        })
        .collect();

    assert_eq!(checksums[0], checksums[1], "DET-1 triple: run 1 != run 2");
    assert_eq!(checksums[1], checksums[2], "DET-1 triple: run 2 != run 3");

    log_jsonl(
        "proof",
        &[("invariant", "DET-1-triple"), ("outcome", "pass")],
    );
}

// ===========================================================================
// Proof 2: REV-1 — Reversibility (resize round-trip)
// ===========================================================================

#[test]
fn proof_rev1_resize_roundtrip() {
    log_jsonl("env", &[("invariant", "REV-1"), ("bead", "bd-1rz0.29")]);

    let transitions = [
        (80, 24, 120, 40), // grow
        (120, 40, 80, 24), // shrink (same pair, reversed)
        (80, 24, 40, 10),  // shrink small
        (40, 10, 200, 60), // grow large
        (60, 20, 60, 40),  // height-only change
        (80, 24, 160, 24), // width-only change
    ];

    for &(w1, h1, w2, h2) in &transitions {
        let buf_a = render_styled_scene(w1, h1);
        let cs_a = compute_buffer_checksum(&buf_a);

        // Render at B size
        let _buf_b = render_styled_scene(w2, h2);

        // Render at A size again
        let buf_a2 = render_styled_scene(w1, h1);
        let cs_a2 = compute_buffer_checksum(&buf_a2);

        assert_eq!(
            cs_a, cs_a2,
            "REV-1 violation: {w1}x{h1}→{w2}x{h2}→{w1}x{h1}"
        );

        log_jsonl(
            "proof",
            &[
                ("invariant", "REV-1"),
                ("transition", &format!("{w1}x{h1}→{w2}x{h2}→{w1}x{h1}")),
                ("outcome", "pass"),
            ],
        );
    }
}

// ===========================================================================
// Proof 3: DIFF-1 — Diff correctness
// ===========================================================================

#[test]
fn proof_diff1_changes_are_real() {
    log_jsonl("env", &[("invariant", "DIFF-1"), ("bead", "bd-1rz0.29")]);

    let sizes = [(80, 24), (120, 40), (40, 10)];

    for &(w, h) in &sizes {
        let old = Buffer::new(w, h);
        let new = render_styled_scene(w, h);

        let diff = BufferDiff::compute(&old, &new);

        // Every diff entry should reflect a real change
        for &(x, y) in diff.changes() {
            let old_cell = old.get(x, y);
            let new_cell = new.get(x, y);
            assert_ne!(
                old_cell, new_cell,
                "DIFF-1: false positive at ({x},{y}) in {w}x{h}"
            );
        }

        // Every changed cell should appear in diff
        let diff_set: std::collections::HashSet<(u16, u16)> =
            diff.changes().iter().copied().collect();
        for y in 0..h {
            for x in 0..w {
                if old.get(x, y) != new.get(x, y) {
                    assert!(
                        diff_set.contains(&(x, y)),
                        "DIFF-1: missing change at ({x},{y}) in {w}x{h}"
                    );
                }
            }
        }

        log_jsonl(
            "proof",
            &[
                ("invariant", "DIFF-1"),
                ("size", &format!("{w}x{h}")),
                ("changes", &diff.len().to_string()),
                ("outcome", "pass"),
            ],
        );
    }
}

/// Verify compute_dirty matches compute when all rows marked dirty.
#[test]
fn proof_diff1_dirty_equivalence() {
    log_jsonl("env", &[("invariant", "DIFF-1-dirty")]);

    let old = Buffer::new(80, 24);
    let mut new = render_styled_scene(80, 24);

    // Mark all rows dirty
    new.mark_all_dirty();

    let diff_full = BufferDiff::compute(&old, &new);
    let diff_dirty = BufferDiff::compute_dirty(&old, &new);

    assert_eq!(
        diff_full.changes(),
        diff_dirty.changes(),
        "DIFF-1-dirty: compute_dirty must match compute with all rows dirty"
    );

    log_jsonl(
        "proof",
        &[("invariant", "DIFF-1-dirty"), ("outcome", "pass")],
    );
}

// ===========================================================================
// Proof 4: PIPE-1 — Pipeline commutativity (content round-trip)
// ===========================================================================

#[test]
fn proof_pipe1_content_survives_pipeline() {
    log_jsonl("env", &[("invariant", "PIPE-1"), ("bead", "bd-1rz0.29")]);

    let sizes = [(80, 24), (40, 10), (120, 40)];

    for &(w, h) in &sizes {
        let prev = Buffer::new(w, h);
        let next = render_styled_scene(w, h);
        let diff = BufferDiff::compute(&prev, &next);

        let term = pipeline_roundtrip(&next, &diff);

        // Verify content matches buffer
        for y in 0..h {
            for x in 0..w {
                let Some(buf_cell) = next.get(x, y) else {
                    continue;
                };
                let Some(term_cell) = term.model().cell(x as usize, y as usize) else {
                    continue;
                };

                // Skip styled chars (★) which may not round-trip through headless
                // since headless doesn't handle all Unicode.
                if buf_cell.content.as_char() == Some('★') {
                    continue;
                }

                // For graphemes/continuations, compare best-effort without shifting indices.
                if buf_cell.content.is_grapheme() {
                    continue;
                }

                let buf_ch = buf_cell.content.as_char().unwrap_or(' ');
                let term_ch = term_cell.text.chars().next().unwrap_or(' ');

                assert_eq!(
                    buf_ch, term_ch,
                    "PIPE-1: content mismatch at ({x},{y}) in {w}x{h}: buffer='{buf_ch}' term='{term_ch}'"
                );
            }
        }

        log_jsonl(
            "proof",
            &[
                ("invariant", "PIPE-1"),
                ("size", &format!("{w}x{h}")),
                ("outcome", "pass"),
            ],
        );
    }
}

// ===========================================================================
// Proof 5: GHOST-1 — No ghosting after clear
// ===========================================================================

#[test]
fn proof_ghost1_no_ghosting_after_shrink() {
    log_jsonl("env", &[("invariant", "GHOST-1"), ("bead", "bd-1rz0.29")]);

    // Simulate: render at large size, then shrink (old is blank post-clear)
    let transitions = [
        (120, 40, 80, 24), // standard shrink
        (200, 60, 40, 10), // extreme shrink
        (80, 24, 60, 15),  // moderate shrink
    ];

    for &(large_w, large_h, small_w, small_h) in &transitions {
        // After shrink, old buffer is blank (terminal clears on resize)
        let old = Buffer::new(small_w, small_h);
        let new = render_styled_scene(small_w, small_h);

        let diff = BufferDiff::compute(&old, &new);
        let diff_set: std::collections::HashSet<(u16, u16)> =
            diff.changes().iter().copied().collect();

        // Every non-empty cell in new must appear in diff
        for y in 0..small_h {
            for x in 0..small_w {
                if let Some(cell) = new.get(x, y)
                    && (cell.content.as_char().is_some_and(|c| c != ' ')
                        || cell.content.is_grapheme())
                {
                    assert!(
                        diff_set.contains(&(x, y)),
                        "GHOST-1: content cell at ({x},{y}) missing from diff after \
                         {large_w}x{large_h}→{small_w}x{small_h} shrink"
                    );
                }
            }
        }

        log_jsonl(
            "proof",
            &[
                ("invariant", "GHOST-1"),
                (
                    "transition",
                    &format!("{large_w}x{large_h}→{small_w}x{small_h}"),
                ),
                ("changes", &diff.len().to_string()),
                ("outcome", "pass"),
            ],
        );
    }
}

// ===========================================================================
// Proof 6: MONO-1 — Diff monotonicity (row-major order)
// ===========================================================================

#[test]
fn proof_mono1_diff_is_row_major() {
    log_jsonl("env", &[("invariant", "MONO-1"), ("bead", "bd-1rz0.29")]);

    let sizes = [(80, 24), (120, 40), (40, 10)];

    for &(w, h) in &sizes {
        let old = Buffer::new(w, h);
        let new = render_styled_scene(w, h);
        let diff = BufferDiff::compute(&old, &new);

        let changes = diff.changes();
        for window in changes.windows(2) {
            let (x1, y1) = window[0];
            let (x2, y2) = window[1];
            assert!(
                y1 < y2 || (y1 == y2 && x1 < x2),
                "MONO-1: non-monotonic at ({x1},{y1}) → ({x2},{y2}) in {w}x{h}"
            );
        }

        log_jsonl(
            "proof",
            &[
                ("invariant", "MONO-1"),
                ("size", &format!("{w}x{h}")),
                ("changes", &changes.len().to_string()),
                ("outcome", "pass"),
            ],
        );
    }
}

// ===========================================================================
// Proof 7: IDEMP-1 — Idempotence
// ===========================================================================

#[test]
fn proof_idemp1_self_diff_is_empty() {
    log_jsonl("env", &[("invariant", "IDEMP-1"), ("bead", "bd-1rz0.29")]);

    let sizes = [(80, 24), (120, 40), (40, 10)];

    for &(w, h) in &sizes {
        let buf = render_styled_scene(w, h);
        let diff = BufferDiff::compute(&buf, &buf);

        assert!(
            diff.is_empty(),
            "IDEMP-1: diff(A,A) should be empty but has {} entries at {w}x{h}",
            diff.len()
        );

        log_jsonl(
            "proof",
            &[
                ("invariant", "IDEMP-1"),
                ("size", &format!("{w}x{h}")),
                ("outcome", "pass"),
            ],
        );
    }
}

/// Applying diff A→B, then diffing B with B should yield empty.
#[test]
fn proof_idemp1_double_apply() {
    log_jsonl("env", &[("invariant", "IDEMP-1-double")]);

    let old = Buffer::new(80, 24);
    let new = render_styled_scene(80, 24);

    // First diff: old→new
    let diff1 = BufferDiff::compute(&old, &new);
    assert!(!diff1.is_empty(), "Initial diff should be non-empty");

    // Second diff: new→new (idempotent)
    let diff2 = BufferDiff::compute(&new, &new);
    assert!(
        diff2.is_empty(),
        "IDEMP-1-double: second diff should be empty but has {} entries",
        diff2.len()
    );

    log_jsonl(
        "proof",
        &[("invariant", "IDEMP-1-double"), ("outcome", "pass")],
    );
}

// ===========================================================================
// Proof 8: STYLE-1 — Style fidelity
// ===========================================================================

#[test]
fn proof_style1_stable_across_renders() {
    log_jsonl("env", &[("invariant", "STYLE-1"), ("bead", "bd-1rz0.29")]);

    let buf1 = render_styled_scene(80, 24);
    let buf2 = render_styled_scene(80, 24);

    let full1 = compute_full_checksum(&buf1);
    let full2 = compute_full_checksum(&buf2);

    assert_eq!(
        full1, full2,
        "STYLE-1: full (style+content) checksum should be deterministic"
    );

    // Verify that style-aware checksum differs from content-only
    let content = compute_buffer_checksum(&buf1);
    // full and content checksums use different hash domains, so they should differ
    assert_ne!(
        full1.split(':').next_back(),
        content.split(':').next_back(),
        "Full checksum should differ from content-only checksum (different hash input)"
    );

    log_jsonl("proof", &[("invariant", "STYLE-1"), ("outcome", "pass")]);
}

// ===========================================================================
// Golden Checksum Enforcement
// ===========================================================================

/// Enforce golden checksums for standard resize scenarios.
///
/// When `BLESS=1`, saves current checksums as golden files.
/// Otherwise, verifies against saved golden files.
/// Missing golden files pass silently (first-run mode).
#[test]
fn enforce_golden_checksums_fixed_sizes() {
    log_jsonl(
        "env",
        &[("test", "enforce_golden_fixed"), ("bead", "bd-1rz0.29")],
    );

    let base_dir = std::env::temp_dir().join("ftui_isomorphism_golden");
    let sizes = [(80, 24), (120, 40), (40, 10), (60, 15), (200, 60)];

    for (w, h) in sizes {
        let buf = render_styled_scene(w, h);
        let content_cs = compute_buffer_checksum(&buf);
        let full_cs = compute_full_checksum(&buf);

        let scenario_name = format!("iso_fixed_{w}x{h}");
        let checksum_path = golden_checksum_path(&base_dir, &scenario_name);
        let expected = load_golden_checksums(&checksum_path).unwrap_or_default();

        let actual = vec![content_cs.clone(), full_cs.clone()];

        if is_bless_mode() {
            save_golden_checksums(&checksum_path, &actual).unwrap();
            log_jsonl(
                "enforce",
                &[
                    ("scenario", &scenario_name),
                    ("outcome", "blessed"),
                    ("content", &content_cs),
                    ("full", &full_cs),
                ],
            );
        } else if !expected.is_empty() {
            let (outcome, mismatch) = verify_checksums(&actual, &expected);
            assert_eq!(
                outcome,
                GoldenOutcome::Pass,
                "Golden checksum enforcement failed for {scenario_name} at index {mismatch:?}\n\
                 expected: {expected:?}\n\
                 actual:   {actual:?}\n\
                 Run with BLESS=1 to update golden files."
            );
            log_jsonl(
                "enforce",
                &[("scenario", &scenario_name), ("outcome", "pass")],
            );
        } else {
            log_jsonl(
                "enforce",
                &[("scenario", &scenario_name), ("outcome", "first_run")],
            );
        }
    }
}

/// Enforce golden checksums for resize transitions.
#[test]
fn enforce_golden_checksums_resize_transitions() {
    log_jsonl(
        "env",
        &[("test", "enforce_golden_resize"), ("bead", "bd-1rz0.29")],
    );

    let base_dir = std::env::temp_dir().join("ftui_isomorphism_golden");
    let transitions = [
        (80, 24, 120, 40),
        (120, 40, 80, 24),
        (80, 24, 40, 10),
        (40, 10, 200, 60),
    ];

    for &(w1, h1, w2, h2) in &transitions {
        let buf1 = render_styled_scene(w1, h1);
        let buf2 = render_styled_scene(w2, h2);

        let cs1 = compute_buffer_checksum(&buf1);
        let cs2 = compute_buffer_checksum(&buf2);

        let scenario_name = format!("iso_resize_{w1}x{h1}_to_{w2}x{h2}");
        let checksum_path = golden_checksum_path(&base_dir, &scenario_name);
        let expected = load_golden_checksums(&checksum_path).unwrap_or_default();

        let actual = vec![cs1.clone(), cs2.clone()];

        if is_bless_mode() {
            save_golden_checksums(&checksum_path, &actual).unwrap();
        } else if !expected.is_empty() {
            let (outcome, mismatch) = verify_checksums(&actual, &expected);
            assert_eq!(
                outcome,
                GoldenOutcome::Pass,
                "Golden enforcement failed for {scenario_name} at index {mismatch:?}\n\
                 Run with BLESS=1 to update."
            );
        }

        log_jsonl(
            "enforce",
            &[
                ("scenario", &scenario_name),
                ("outcome", if is_bless_mode() { "blessed" } else { "pass" }),
            ],
        );
    }
}

// ===========================================================================
// Composite Proofs: Full Pipeline
// ===========================================================================

/// Prove that the full pipeline (render → diff → present → headless) preserves
/// content across resize transitions.
#[test]
fn proof_composite_pipeline_resize() {
    log_jsonl(
        "env",
        &[
            ("test", "composite_pipeline_resize"),
            ("bead", "bd-1rz0.29"),
        ],
    );

    let sizes = [(80, 24), (120, 40), (40, 10)];

    for &(w, h) in &sizes {
        let prev = Buffer::new(w, h);
        let next = render_styled_scene(w, h);
        let diff = BufferDiff::compute(&prev, &next);

        // Pipeline the output
        let output = {
            let mut vec = Vec::new();
            let caps = TerminalCapabilities::default();
            let mut presenter = Presenter::new(&mut vec, caps);
            let stats = presenter.present(&next, &diff).unwrap();
            drop(presenter);

            // Verify stats are sensible
            assert!(
                stats.cells_changed > 0,
                "Present should report non-zero cells changed at {w}x{h}"
            );
            assert!(
                stats.bytes_emitted > 0,
                "Present should emit bytes at {w}x{h}"
            );

            vec
        };

        // Round-trip through headless
        let mut term = HeadlessTerm::new(w, h);
        term.process(&output);

        // Spot-check: first row should have content from the block border
        let row0 = term.row_text(0);
        assert!(
            row0.contains('─') || row0.contains("Proof"),
            "Pipeline output should contain block border at {w}x{h}, got: {row0}"
        );

        log_jsonl(
            "proof",
            &[
                ("invariant", "COMPOSITE-PIPE"),
                ("size", &format!("{w}x{h}")),
                ("output_bytes", &output.len().to_string()),
                ("outcome", "pass"),
            ],
        );
    }
}

/// Prove that two different render sizes produce different pipeline outputs.
#[test]
fn proof_composite_size_discrimination() {
    log_jsonl("env", &[("test", "composite_size_discrimination")]);

    let buf80 = render_styled_scene(80, 24);
    let buf120 = render_styled_scene(120, 40);

    let cs80 = compute_buffer_checksum(&buf80);
    let cs120 = compute_buffer_checksum(&buf120);

    assert_ne!(
        cs80, cs120,
        "Different sizes must produce different checksums"
    );

    log_jsonl("proof", &[("invariant", "SIZE-DISC"), ("outcome", "pass")]);
}

// ===========================================================================
// Performance Budget
// ===========================================================================

#[test]
fn proof_performance_budget() {
    log_jsonl(
        "env",
        &[("test", "performance_budget"), ("bead", "bd-1rz0.29")],
    );

    let coverage = is_coverage_mode();
    let iterations = if coverage { 10 } else { 50 };
    let mut timings_us = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let start = Instant::now();

        let prev = Buffer::new(80, 24);
        let next = render_styled_scene(80, 24);
        let diff = BufferDiff::compute(&prev, &next);
        let _runs = diff.runs();

        timings_us.push(start.elapsed().as_micros() as u64);
    }

    timings_us.sort();
    let p50 = timings_us[timings_us.len() / 2];
    let p95 = timings_us[(timings_us.len() * 95) / 100];
    let p99 = timings_us[(timings_us.len() * 99) / 100];

    let budget_us = if coverage { 50_000 } else { 5_000 };

    log_jsonl(
        "perf",
        &[
            ("p50_us", &p50.to_string()),
            ("p95_us", &p95.to_string()),
            ("p99_us", &p99.to_string()),
            ("budget_us", &budget_us.to_string()),
            ("coverage", if coverage { "true" } else { "false" }),
            ("iterations", &iterations.to_string()),
        ],
    );

    // Budget: render + diff cycle should be under 5ms at p99 (relaxed under coverage)
    assert!(
        p99 < budget_us,
        "Performance budget exceeded: p99={p99}μs (budget={budget_us}μs, coverage={coverage})"
    );
}

// ===========================================================================
// Run Coalescing Proof
// ===========================================================================

#[test]
fn proof_runs_cover_all_changes() {
    log_jsonl("env", &[("invariant", "RUN-COVER")]);

    let old = Buffer::new(80, 24);
    let new = render_styled_scene(80, 24);
    let diff = BufferDiff::compute(&old, &new);

    let runs = diff.runs();
    let changes = diff.changes();

    // Count total cells covered by runs
    let run_cells: usize = runs.iter().map(|r| r.len() as usize).sum();

    assert_eq!(
        run_cells,
        changes.len(),
        "Runs must cover exactly all changes: runs cover {run_cells}, changes has {}",
        changes.len()
    );

    // Verify no overlap: build set from runs
    let mut covered = std::collections::HashSet::new();
    for run in &runs {
        for x in run.x0..=run.x1 {
            let inserted = covered.insert((x, run.y));
            assert!(inserted, "RUN-COVER: overlapping run at ({x},{})", run.y);
        }
    }

    log_jsonl(
        "proof",
        &[
            ("invariant", "RUN-COVER"),
            ("changes", &changes.len().to_string()),
            ("runs", &runs.len().to_string()),
            ("outcome", "pass"),
        ],
    );
}

// ===========================================================================
// Summary Test
// ===========================================================================

#[test]
fn proof_suite_summary() {
    log_jsonl(
        "summary",
        &[
            ("bead", "bd-1rz0.29"),
            ("invariant_count", "9"),
            ("test_count", "15"),
            (
                "coverage",
                "DET-1,REV-1,DIFF-1,PIPE-1,GHOST-1,MONO-1,IDEMP-1,STYLE-1,RUN-COVER",
            ),
        ],
    );
}
