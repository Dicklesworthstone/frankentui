#[cfg(test)]
mod tests {
    use crate::{Constraint, Flex, Rect};

    #[test]
    fn repro_max_constraint_wastes_space() {
        // Scenario: Available width 100.
        // Item 1: Min(10), Max(20).
        // Item 2: Min(10).
        // Expected: Item 1 takes 20 (capped by Max). Item 2 takes 80 (takes all remaining).
        // Current implementation likely does:
        // 1. Min(10) allocated to both. Used 20. Rem 80.
        // 2. Both grow (weight 1:1). Each gets 40.
        //    Item 1: 10+40=50. Item 2: 10+40=50.
        // 3. Item 1 clamped to 20. Item 2 stays 50.
        // Total used: 20+50=70. Wasted: 30.

        let flex = Flex::horizontal().constraints([
            Constraint::Max(20),
            Constraint::Min(10),
        ]);
        
        // Add Min(10) to the first one implicitly? No, Max(20) implies 0..20.
        // Let's use [Constraint::Min(10), Constraint::Min(10)] but wrap one in a logic that limits it?
        // Flex doesn't support "Min AND Max" on same item directly in one Constraint enum.
        // But Constraint::Max(20) acts as "0..20".
        // Constraint::Min(10) acts as "10..inf".
        
        // Let's try [Constraint::Max(20), Constraint::Min(10)].
        // Item 1: Max(20). Init 0. Grow yes.
        // Item 2: Min(10). Init 10. Grow yes.
        // Rem: 90.
        // Weights equal. 45 each.
        // Item 1: 45. Clamped to 20.
        // Item 2: 10 + 45 = 55.
        // Total 75. Wasted 25.
        
        let rects = flex.split(Rect::new(0, 0, 100, 10));
        
        assert_eq!(rects[0].width, 20, "Item 1 should be capped at 20");
        
        // Strict expectation: Item 2 should take the rest (80).
        // If it fails, it confirms the "bug".
        assert_eq!(rects[1].width, 80, "Item 2 should take remaining space");
    }
}
