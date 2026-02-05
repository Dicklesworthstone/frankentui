
#[cfg(test)]
mod tests {
    use ftui_harness::time_travel::{TimeTravel, FrameMetadata};
    use ftui_render::buffer::Buffer;
    use ftui_render::cell::Cell;
    use std::time::Duration;

    #[test]
    fn test_timetravel_capacity_one_eviction_corruption() {
        let mut tt = TimeTravel::new(1);
        let mut buf = Buffer::new(5, 1);

        // Frame 0
        buf.set(0, 0, Cell::from_char('A'));
        tt.record(&buf, FrameMetadata::new(0, Duration::ZERO));

        // Frame 1 (should evict F0)
        buf.set(1, 0, Cell::from_char('B'));
        tt.record(&buf, FrameMetadata::new(1, Duration::ZERO));

        // We expect Frame 1 to be retrievable and correct.
        // If the bug exists, Frame 1 is stored as a Delta from F0, but F0 is gone.
        // Reconstructing it from empty buffer yields " B" instead of "AB".
        let f1 = tt.get(0).expect("Should have 1 frame");
        
        assert_eq!(f1.get(0, 0).unwrap().content.as_char(), Some('A'), "Frame 1 should contain 'A' at (0,0)");
        assert_eq!(f1.get(1, 0).unwrap().content.as_char(), Some('B'), "Frame 1 should contain 'B' at (1,0)");
    }
}
