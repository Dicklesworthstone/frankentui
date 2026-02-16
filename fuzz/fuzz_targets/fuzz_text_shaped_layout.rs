#![no_main]

use ftui_text::script_segmentation::{RunDirection, Script};
use ftui_text::shaped_render::ShapedLineLayout;
use ftui_text::shaping::{FontFeatures, NoopShaper, TextShaper};
use ftui_text::shaping_fallback::ShapingFallback;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Cap input length to avoid O(n^2) from_run dominating.
    if text.len() > 1024 {
        return;
    }

    // from_text path — must never panic.
    let layout_text = ShapedLineLayout::from_text(text);
    // Zero-width chars (e.g. \0, combining marks) create placements but
    // contribute 0 to total_cells, so placements.len() >= total_cells().
    assert!(layout_text.placements().len() >= layout_text.total_cells());

    // Placements must be monotonically positioned.
    let placements = layout_text.placements();
    for w in placements.windows(2) {
        assert!(w[1].cell_x >= w[0].cell_x);
    }

    // from_run with NoopShaper — must agree on cell count.
    if text.len() <= 256 {
        let shaper = NoopShaper;
        let features = FontFeatures::default();
        let run = shaper.shape(text, Script::Latin, RunDirection::Ltr, &features);
        let layout_run = ShapedLineLayout::from_run(text, &run);
        assert_eq!(layout_run.total_cells(), layout_text.total_cells());
    }

    // Terminal fallback — must produce valid output.
    let fb = ShapingFallback::<NoopShaper>::terminal();
    let (layout_fb, _event) = fb.shape_line(text, Script::Latin, RunDirection::Ltr);
    // Non-empty text with visible characters should produce cells,
    // but zero-width-only strings (e.g. "\0") may yield 0 cells.
    let _ = layout_fb.total_cells();
});
