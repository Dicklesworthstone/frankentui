#[cfg(test)]
mod tests {
    use super::*;
    use ftui_widgets::sparkline::Sparkline;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;
    use ftui_core::geometry::Rect;
    use ftui_widgets::Widget;

    #[test]
    fn repro_baseline_ignored() {
        let data = vec![4.0, 6.0];
        // min=0, max=10. 
        // 4.0 is 40% -> index 3 (▃)
        // 6.0 is 60% -> index 5 (▅)
        // Baseline is 5.0. 4.0 < 5.0, so it should be ' ' (index 0).
        
        let sparkline = Sparkline::new(&data)
            .min(0.0)
            .max(10.0)
            .baseline(5.0);
            
        let area = Rect::new(0, 0, 2, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(2, 1, &mut pool);
        
        sparkline.render(area, &mut frame);
        
        let c0 = frame.buffer.get(0, 0).unwrap().content.as_char().unwrap();
        let c1 = frame.buffer.get(1, 0).unwrap().content.as_char().unwrap();
        
        println!("c0: '{}', c1: '{}'", c0, c1);
        
        // Expect c0 to be ' ' because 4.0 < baseline 5.0
        assert_eq!(c0, ' ', "Value below baseline should be empty");
        // Expect c1 to be '▅' (index 5) or similar
        assert_ne!(c1, ' ');
    }
}
