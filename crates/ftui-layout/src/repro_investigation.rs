#[cfg(test)]
mod tests {
    use crate::{Alignment, Constraint, Flex, Rect};

    #[test]
    fn investigate_ratio_behavior() {
        let area = Rect::new(0, 0, 100, 10);
        
        // Case 1: Ratio(1, 4) alone
        let flex_r = Flex::horizontal().constraints([Constraint::Ratio(1, 4)]);
        let rects_r = flex_r.split(area);
        println!("Ratio(1, 4) alone: {:?}", rects_r[0]);
        // Expected: width 25 (if interpreted as 1/4 of total) or 100 (if interpreted as weight 1/4 vs nothing)

        // Case 2: Ratio(1, 4) vs Fill
        let flex_rf = Flex::horizontal().constraints([Constraint::Ratio(1, 4), Constraint::Fill]);
        let rects_rf = flex_rf.split(area);
        println!("Ratio(1, 4) vs Fill: Ratio={:?}, Fill={:?}", rects_rf[0], rects_rf[1]);
        // Expected: Ratio=20, Fill=80 if Ratio is weight 0.25 and Fill is weight 1.0.
    }

    #[test]
    fn investigate_space_between() {
        let flex = Flex::horizontal()
            .alignment(Alignment::SpaceBetween)
            .constraints([
                Constraint::Fixed(10),
                Constraint::Fixed(10),
                Constraint::Fixed(10),
            ]);
        
        // 35 available, 30 used. 5 remainder. 2 gaps. 5/2 = 2.5
        let rects = flex.split(Rect::new(0, 0, 35, 10));
        println!("SpaceBetween: {:?}", rects);
        // Expected: [0..10], [12..22] or [13..23], [25..35]
    }

    #[test]
    fn investigate_space_around() {
         let flex = Flex::horizontal()
            .alignment(Alignment::SpaceAround)
            .constraints([Constraint::Fixed(2), Constraint::Fixed(2)]);

        // 10 available, 4 used. 6 remainder. 2 items * 2 = 4 slots. 6/4 = 1.5 per slot.
        let rects = flex.split(Rect::new(0, 0, 10, 10));
        println!("SpaceAround: {:?}", rects);
        
        let center0 = rects[0].x as f32 + 1.0;
        let center1 = rects[1].x as f32 + 1.0;
        let midpoint = (center0 + center1) / 2.0;
        println!("SpaceAround Midpoint: {}", midpoint);
    }
}
