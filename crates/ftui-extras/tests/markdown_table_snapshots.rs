#![forbid(unsafe_code)]
//! Snapshot tests for markdown table rendering (bd-2k018.11).
//!
//! Run:
//!   cargo test -p ftui-extras --test markdown_table_snapshots --features markdown
//! Update snapshots:
//!   BLESS=1 cargo test -p ftui-extras --test markdown_table_snapshots --features markdown

#[cfg(feature = "markdown")]
use ftui_core::geometry::Rect;
#[cfg(feature = "markdown")]
use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
#[cfg(feature = "markdown")]
use ftui_harness::assert_snapshot_ansi;
#[cfg(feature = "markdown")]
use ftui_render::frame::Frame;
#[cfg(feature = "markdown")]
use ftui_render::grapheme_pool::GraphemePool;
#[cfg(feature = "markdown")]
use ftui_text::Text;
#[cfg(feature = "markdown")]
use ftui_widgets::Widget;
#[cfg(feature = "markdown")]
use ftui_widgets::paragraph::Paragraph;

#[cfg(feature = "markdown")]
fn render_markdown(markdown: &str, table_max_width: Option<u16>) -> Text {
    let renderer = MarkdownRenderer::new(MarkdownTheme::default());
    let renderer = match table_max_width {
        Some(width) => renderer.table_max_width(width),
        None => renderer,
    };
    renderer.render(markdown)
}

#[test]
#[cfg(feature = "markdown")]
fn snapshot_markdown_table_basic() {
    let md = "\
| Feature | Status | Notes |
|---|:---:|---:|
| Inline mode | OK | Scrollback preserved |
| Diff engine | OK | SIMD-friendly |
| Evidence logs | OK | JSONL output |
";
    let text = render_markdown(md, None);
    let width = text.width().max(1) as u16;
    let height = text.height().max(1) as u16;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    Paragraph::new(text).render(Rect::new(0, 0, width, height), &mut frame);
    assert_snapshot_ansi!("markdown_table_basic", &frame.buffer);
}

#[test]
#[cfg(feature = "markdown")]
fn snapshot_markdown_table_alignment() {
    let md = "\
| Left | Center | Right |
|:---|:---:|---:|
| L1 | C1 | R1 |
| L2 | C2 | R2 |
| L3 | C3 | R3 |
";
    let text = render_markdown(md, None);
    let width = text.width().max(1) as u16;
    let height = text.height().max(1) as u16;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    Paragraph::new(text).render(Rect::new(0, 0, width, height), &mut frame);
    assert_snapshot_ansi!("markdown_table_alignment", &frame.buffer);
}

#[test]
#[cfg(feature = "markdown")]
fn snapshot_markdown_table_max_width() {
    let md = "\
| Column | Description |
|---|---|
| Inline | This description is intentionally long to test truncation. |
| Diff | Another long description to exceed the max table width. |
";
    let text = render_markdown(md, Some(32));
    let width = 32u16;
    let height = text.height().max(1) as u16;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    Paragraph::new(text).render(Rect::new(0, 0, width, height), &mut frame);
    assert_snapshot_ansi!("markdown_table_max_width", &frame.buffer);
}
