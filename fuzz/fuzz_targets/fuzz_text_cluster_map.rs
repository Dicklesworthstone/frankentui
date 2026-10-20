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

    let total = map.total_cells();
    assert_eq!(map.total_bytes(), text.len());

    // Canonical byte → cell → byte and mid-cluster snapping oracle:
    // - For in-bounds visual columns (`cell < total_cells`), `cell_to_byte(cell)`
    //   must resolve to the FIRST cluster owning `cell` (`<= entry.byte_start`).
    // - For trailing or all-zero-width clusters at visual EOF (`cell == total_cells`),
    //   `entry.cell_width` must be 0 and `cell_to_byte(total_cells)` must return
    //   `total_bytes` (`text.len()`), while `cell_to_entry(total_cells)` is `None`.
    for entry in entries {
        let start = entry.byte_start as usize;
        let end = entry.byte_end as usize;
        let cell = map.byte_to_cell(start);
        assert_eq!(cell, entry.cell_start as usize);

        for b in start..end {
            assert_eq!(map.byte_to_cell(b), cell);
        }

        let back = map.cell_to_byte(cell);
        if cell < total {
            let first_owner = entries
                .iter()
                .find(|e| e.cell_start == entry.cell_start)
                .expect("in-bounds cluster cell_start must have a first owner");
            assert_eq!(back, first_owner.byte_start as usize);
            assert!(back <= start);
            assert_eq!(
                map.cell_to_entry(cell).map(|e| e.byte_start),
                Some(first_owner.byte_start)
            );
        } else {
            assert_eq!(cell, total);
            assert_eq!(entry.cell_width, 0);
            assert_eq!(back, text.len());
            assert!(map.cell_to_entry(cell).is_none());
        }
    }

    // Reverse lookup oracle for all visual cells including wide continuations.
    for c in 0..total.min(512) {
        let owner = map
            .cell_to_entry(c)
            .expect("in-bounds visual cell must resolve to a cluster entry");
        assert_eq!(map.cell_to_byte(c), owner.byte_start as usize);
        if c > owner.cell_start as usize {
            assert!(c < owner.cell_end() as usize);
        }
    }

    // Cell range must produce valid byte range.
    if total > 0 {
        let (bs, be) = map.cell_range_to_byte_range(0, total);
        assert_eq!(bs, 0);
        assert_eq!(be, text.len());
    }
});
