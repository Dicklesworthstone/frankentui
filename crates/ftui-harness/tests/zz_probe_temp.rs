//! Throwaway probe file — **awaiting owner permission to delete** (CrimsonElk,
//! 2026-09-19).
//!
//! I created this in the repo to measure per-stage render costs while wiring
//! `cost_surface` into the gauntlet. It should have gone in a scratch directory;
//! creating it here was my mistake. AGENTS.md Rule 1 forbids me from deleting a
//! file without express written permission, including one I created myself, so
//! it stays until the owner grants that.
//!
//! Nothing depends on it. Its measurements are recorded in
//! `docs/perf/cost_surface_stage_dominance_2026-09-19.md`, and the assertions
//! worth keeping live in `render_gauntlet_e2e.rs::cost_surface_stage_costs_stay_within_the_frame_total`.
//!
//! Requested deletion: `crates/ftui-harness/tests/zz_probe_temp.rs`

/// Verifies basic Frame buffer bounds until owner grants permission to remove this file.
#[test]
fn probe_superseded_by_render_gauntlet_e2e() {
    let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
    let frame = ftui_render::frame::Frame::new(80, 24, &mut pool);
    assert_eq!(frame.width(), 80);
    assert_eq!(frame.height(), 24);
    assert_eq!(frame.buffer.width(), 80);
    assert_eq!(frame.buffer.height(), 24);
}
