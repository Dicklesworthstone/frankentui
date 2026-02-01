
#[cfg(test)]
mod tests {
    use crate::{Flex, Rect, Constraint, Alignment};

    #[test]
    fn repro_space_between_remainder() {
        let flex = Flex::horizontal()
            .alignment(Alignment::SpaceBetween)
            .constraints([
                Constraint::Fixed(10),
                Constraint::Fixed(10),
                Constraint::Fixed(10),
            ]);
        
        // Items total 30.
        // Available 35.
        // Leftover 5.
        // Gaps: 2.
        // 5 / 2 = 2 remainder 1.
        
        // Expected:
        // Item 1: 0..10
        // Gap 1: 2 or 3
        // Item 2: ...
        // Gap 2: 3 or 2
        // Item 3: ..35 (flush with end)
        
        let rects = flex.split(Rect::new(0, 0, 35, 10));
        
        assert_eq!(rects[0].x, 0);
        // Last item should end at 35. width is 10. so x should be 25.
        assert_eq!(rects[2].x, 25, "Last item should be flush with end");
    }
}
