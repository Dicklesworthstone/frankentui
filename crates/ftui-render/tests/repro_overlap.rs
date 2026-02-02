#[cfg(test)]
mod tests {
    use ftui_render::buffer::Buffer;
    use ftui_render::cell::Cell;

    #[test]
    fn overwrite_middle_of_wide_char_clears_head() {
        let mut buf = Buffer::new(10, 1);
        
        // Set wide char at 0 (width 2)
        // 0="中", 1=CONT
        buf.set(0, 0, Cell::from_char('中'));
        
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('中'));
        assert!(buf.get(1, 0).unwrap().is_continuation());
        
        // Overwrite index 1 (the tail) with a new char
        buf.set(1, 0, Cell::from_char('A'));
        
        // Expectation:
        // 0 should be cleared (empty) because its tail was stomped
        // 1 should be 'A'
        assert!(buf.get(0, 0).unwrap().is_empty(), "Head at 0 should be cleared");
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('A'), "Tail at 1 should be 'A'");
    }

    #[test]
    fn overwrite_start_of_wide_char_clears_tail() {
        let mut buf = Buffer::new(10, 1);
        buf.set(0, 0, Cell::from_char('中'));
        
        // Overwrite head with 'A'
        buf.set(0, 0, Cell::from_char('A'));
        
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert!(buf.get(1, 0).unwrap().is_empty(), "Tail at 1 should be cleared");
    }

    #[test]
    fn overlap_scan_stops_at_non_overlapping_head() {
        let mut buf = Buffer::new(10, 1);
        // 0="A", 1="中", 2=CONT, 3="B"
        buf.set(0, 0, Cell::from_char('A'));
        buf.set(1, 0, Cell::from_char('中'));
        buf.set(3, 0, Cell::from_char('B'));
        
        // Overwrite 2 (CONT of 中)
        buf.set(2, 0, Cell::from_char('X'));
        
        // Should clear 1 ("中")
        assert!(buf.get(1, 0).unwrap().is_empty());
        // Should NOT clear 0 ("A")
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('A'));
    }
}