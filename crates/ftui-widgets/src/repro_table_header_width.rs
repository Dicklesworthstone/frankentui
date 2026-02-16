#[cfg(test)]
mod tests {
    use super::*;
    use ftui_widgets::table::{Row, Table, TableState};
    use ftui_layout::{Constraint, Rect};
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;
    use ftui_widgets::{StatefulWidget, Widget};

    #[test]
    fn repro_fit_content_ignores_header() {
        // Column 0: FitContent.
        // Data: "1" (width 1).
        // Header: "LongHeader" (width 10).
        // Expected: Column width 10.
        // Actual (suspected): Column width 1.

        let rows = vec![Row::new(["1"])];
        let widths = vec![Constraint::FitContent];
        let header = Row::new(["LongHeader"]);

        let table = Table::new(rows, widths).header(header);

        // We can inspect the layout result by rendering and checking the buffer,
        // or by inspecting internal state if accessible.
        // Since we can't easily inspect internals without creating a Frame, let's render.

        let area = Rect::new(0, 0, 20, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 3, &mut pool);
        let mut state = TableState::default();

        StatefulWidget::render(&table, area, &mut frame, &mut state);

        // Check the header cell in the buffer.
        // Row 0 is header.
        // If width is 1, it will print "L" and truncate.
        // If width is 10, it will print "LongHeader".
        
        let cell_content = (0..10).map(|x| {
            frame.buffer.get(x, 0).unwrap().content.as_char().unwrap_or(' ')
        }).collect::<String>();
        
        println!("Header rendered: '{}'", cell_content);

        // If it renders "L         ", then width was 1.
        // If it renders "LongHeader", then width was 10.
        assert_eq!(cell_content.trim(), "LongHeader", "Column should expand to fit header");
    }
}
