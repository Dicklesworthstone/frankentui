#![no_main]

use ftui_text::cluster_map::ClusterMap;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Only process valid UTF-8.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    // Build cluster map — must never panic.
    let map = ClusterMap::from_text(text);

    // Verify structural invariants.
    let entries = map.entries();
    if entries.is_empty() {
        return;
    }

    // First entry must start at byte 0.
    assert_eq!(entries[0].byte_start, 0);

    // Last entry must end at text length.
    assert_eq!(entries.last().unwrap().byte_end as usize, text.len());

    // Entries must be monotonically non-decreasing in cell offset.
    for w in entries.windows(2) {
        assert!(w[1].cell_start >= w[0].cell_start);
        assert!(w[1].byte_start >= w[0].byte_end);
    }

    // Round-trip: byte → cell → byte must snap to cluster start.
    for entry in entries {
        let cell = map.byte_to_cell(entry.byte_start as usize);
        let back = map.cell_to_byte(cell);
        assert!(back <= entry.byte_start as usize);
    }

    // Cell range must produce valid byte range.
    let total = map.total_cells();
    if total > 0 {
        let (bs, be) = map.cell_range_to_byte_range(0, total);
        assert!(bs <= be);
        assert!(be <= text.len());
    }
});
