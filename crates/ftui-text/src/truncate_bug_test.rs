#[cfg(test)]
mod tests {
    use crate::text::Text;

    #[test]
    fn test_truncate_bug_small_width() {
        let mut text = Text::raw("hello world");
        // max_width (2) < ellipsis width (3)
        text.truncate(2, Some("..."));
        
        // Current implementation likely produces "he..." (width 5)
        // Expected: width <= 2
        assert!(text.width() <= 2, "Width {} exceeds max_width 2", text.width());
    }
}
