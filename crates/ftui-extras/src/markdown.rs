#![forbid(unsafe_code)]

//! Markdown renderer for FrankenTUI.
//!
//! Converts Markdown text into styled [`Text`] for rendering in terminal UIs.
//! Uses [pulldown-cmark] for parsing.
//!
//! # Example
//! ```
//! use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
//!
//! let renderer = MarkdownRenderer::new(MarkdownTheme::default());
//! let text = renderer.render("# Hello\n\nSome **bold** text.");
//! assert!(text.height() > 0);
//! ```

use ftui_render::cell::PackedRgba;
use ftui_style::Style;
use ftui_text::text::{Line, Span, Text};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Theme for Markdown rendering.
///
/// Each field controls the style applied to the corresponding Markdown element.
#[derive(Debug, Clone)]
pub struct MarkdownTheme {
    pub h1: Style,
    pub h2: Style,
    pub h3: Style,
    pub h4: Style,
    pub h5: Style,
    pub h6: Style,
    pub code_inline: Style,
    pub code_block: Style,
    pub blockquote: Style,
    pub link: Style,
    pub emphasis: Style,
    pub strong: Style,
    pub strikethrough: Style,
    pub list_bullet: Style,
    pub horizontal_rule: Style,
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        Self {
            h1: Style::new().fg(PackedRgba::rgb(255, 255, 255)).bold(),
            h2: Style::new().fg(PackedRgba::rgb(200, 200, 255)).bold(),
            h3: Style::new().fg(PackedRgba::rgb(180, 180, 230)).bold(),
            h4: Style::new().fg(PackedRgba::rgb(160, 160, 210)).bold(),
            h5: Style::new().fg(PackedRgba::rgb(140, 140, 190)).bold(),
            h6: Style::new().fg(PackedRgba::rgb(120, 120, 170)).bold(),
            code_inline: Style::new().fg(PackedRgba::rgb(230, 180, 80)),
            code_block: Style::new().fg(PackedRgba::rgb(200, 200, 200)),
            blockquote: Style::new().fg(PackedRgba::rgb(150, 150, 150)).italic(),
            link: Style::new().fg(PackedRgba::rgb(100, 150, 255)).underline(),
            emphasis: Style::new().italic(),
            strong: Style::new().bold(),
            strikethrough: Style::new().strikethrough(),
            list_bullet: Style::new().fg(PackedRgba::rgb(180, 180, 100)),
            horizontal_rule: Style::new().fg(PackedRgba::rgb(100, 100, 100)).dim(),
        }
    }
}

/// Markdown renderer that converts Markdown text into styled [`Text`].
#[derive(Debug, Clone)]
pub struct MarkdownRenderer {
    theme: MarkdownTheme,
    rule_width: u16,
}

impl MarkdownRenderer {
    /// Create a new renderer with the given theme.
    #[must_use]
    pub fn new(theme: MarkdownTheme) -> Self {
        Self {
            theme,
            rule_width: 40,
        }
    }

    /// Set the width for horizontal rules.
    #[must_use]
    pub fn rule_width(mut self, width: u16) -> Self {
        self.rule_width = width;
        self
    }

    /// Render a Markdown string into styled [`Text`].
    #[must_use]
    pub fn render(&self, markdown: &str) -> Text {
        let options = Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TABLES
            | Options::ENABLE_HEADING_ATTRIBUTES;
        let parser = Parser::new_ext(markdown, options);

        let mut builder = RenderState::new(&self.theme, self.rule_width);
        builder.process(parser);
        builder.finish()
    }
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new(MarkdownTheme::default())
    }
}

// ---------------------------------------------------------------------------
// Internal render state machine
// ---------------------------------------------------------------------------

/// Style stack entry tracking what Markdown context is active.
#[derive(Debug, Clone, Copy)]
enum StyleContext {
    Heading(HeadingLevel),
    Emphasis,
    Strong,
    Strikethrough,
    CodeBlock,
    Blockquote,
    Link,
}

/// Tracks list nesting and numbering.
#[derive(Debug, Clone)]
struct ListState {
    ordered: bool,
    next_number: u64,
}

struct RenderState<'t> {
    theme: &'t MarkdownTheme,
    rule_width: u16,
    lines: Vec<Line>,
    current_spans: Vec<Span<'static>>,
    style_stack: Vec<StyleContext>,
    list_stack: Vec<ListState>,
    /// Whether we're collecting text inside a code block.
    in_code_block: bool,
    code_block_lines: Vec<String>,
    /// Whether we're inside a blockquote.
    blockquote_depth: u16,
    /// Track if we need a blank line separator.
    needs_blank: bool,
}

impl<'t> RenderState<'t> {
    fn new(theme: &'t MarkdownTheme, rule_width: u16) -> Self {
        Self {
            theme,
            rule_width,
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: Vec::new(),
            list_stack: Vec::new(),
            in_code_block: false,
            code_block_lines: Vec::new(),
            blockquote_depth: 0,
            needs_blank: false,
        }
    }

    fn process<'a>(&mut self, parser: impl Iterator<Item = Event<'a>>) {
        for event in parser {
            match event {
                Event::Start(tag) => self.start_tag(tag),
                Event::End(tag) => self.end_tag(tag),
                Event::Text(text) => self.text(&text),
                Event::Code(code) => self.inline_code(&code),
                Event::SoftBreak => self.soft_break(),
                Event::HardBreak => self.hard_break(),
                Event::Rule => self.horizontal_rule(),
                // TaskListMarker, FootnoteReference, Html, InlineHtml, InlineMath, DisplayMath
                _ => {}
            }
        }
    }

    fn start_tag(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush_blank();
                self.style_stack.push(StyleContext::Heading(level));
            }
            Tag::Paragraph => {
                self.flush_blank();
            }
            Tag::Emphasis => {
                self.style_stack.push(StyleContext::Emphasis);
            }
            Tag::Strong => {
                self.style_stack.push(StyleContext::Strong);
            }
            Tag::Strikethrough => {
                self.style_stack.push(StyleContext::Strikethrough);
            }
            Tag::CodeBlock(_) => {
                self.flush_blank();
                self.in_code_block = true;
                self.code_block_lines.clear();
                self.style_stack.push(StyleContext::CodeBlock);
            }
            Tag::BlockQuote(_) => {
                self.flush_blank();
                self.blockquote_depth += 1;
                self.style_stack.push(StyleContext::Blockquote);
            }
            Tag::Link { dest_url, .. } => {
                self.style_stack.push(StyleContext::Link);
                // Store URL - we'll use it in end_tag if needed
                let _ = dest_url;
            }
            Tag::List(start) => match start {
                Some(n) => self.list_stack.push(ListState {
                    ordered: true,
                    next_number: n,
                }),
                None => self.list_stack.push(ListState {
                    ordered: false,
                    next_number: 0,
                }),
            },
            Tag::Item => {
                self.flush_line();
                let prefix = self.list_prefix();
                let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
                self.current_spans.push(Span::styled(
                    format!("{indent}{prefix}"),
                    self.theme.list_bullet,
                ));
            }
            Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {
                // Table support: we render as simple text with separators
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.style_stack.pop();
                self.flush_line();
                self.needs_blank = true;
            }
            TagEnd::Paragraph => {
                self.flush_line();
                self.needs_blank = true;
            }
            TagEnd::Emphasis => {
                self.style_stack.pop();
            }
            TagEnd::Strong => {
                self.style_stack.pop();
            }
            TagEnd::Strikethrough => {
                self.style_stack.pop();
            }
            TagEnd::CodeBlock => {
                self.style_stack.pop();
                self.flush_code_block();
                self.in_code_block = false;
                self.needs_blank = true;
            }
            TagEnd::BlockQuote(_) => {
                self.style_stack.pop();
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.flush_line();
                self.needs_blank = true;
            }
            TagEnd::Link => {
                self.style_stack.pop();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.flush_line();
                    self.needs_blank = true;
                }
            }
            TagEnd::Item => {
                self.flush_line();
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                self.flush_line();
            }
            TagEnd::TableCell => {
                self.current_spans.push(Span::raw(String::from(" | ")));
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_block_lines.push(text.to_string());
            return;
        }

        let style = self.current_style();
        let content = if self.blockquote_depth > 0 {
            let prefix = "│ ".repeat(self.blockquote_depth as usize);
            format!("{prefix}{text}")
        } else {
            text.to_string()
        };

        match style {
            Some(s) => self.current_spans.push(Span::styled(content, s)),
            None => self.current_spans.push(Span::raw(content)),
        }
    }

    fn inline_code(&mut self, code: &str) {
        self.current_spans
            .push(Span::styled(format!("`{code}`"), self.theme.code_inline));
    }

    fn soft_break(&mut self) {
        self.current_spans.push(Span::raw(String::from(" ")));
    }

    fn hard_break(&mut self) {
        self.flush_line();
    }

    fn horizontal_rule(&mut self) {
        self.flush_blank();
        let rule = "─".repeat(self.rule_width as usize);
        self.lines
            .push(Line::styled(rule, self.theme.horizontal_rule));
        self.needs_blank = true;
    }

    // -- helpers --

    fn current_style(&self) -> Option<Style> {
        let mut result: Option<Style> = None;
        for ctx in &self.style_stack {
            let s = match ctx {
                StyleContext::Heading(HeadingLevel::H1) => self.theme.h1,
                StyleContext::Heading(HeadingLevel::H2) => self.theme.h2,
                StyleContext::Heading(HeadingLevel::H3) => self.theme.h3,
                StyleContext::Heading(HeadingLevel::H4) => self.theme.h4,
                StyleContext::Heading(HeadingLevel::H5) => self.theme.h5,
                StyleContext::Heading(HeadingLevel::H6) => self.theme.h6,
                StyleContext::Emphasis => self.theme.emphasis,
                StyleContext::Strong => self.theme.strong,
                StyleContext::Strikethrough => self.theme.strikethrough,
                StyleContext::CodeBlock => self.theme.code_block,
                StyleContext::Blockquote => self.theme.blockquote,
                StyleContext::Link => self.theme.link,
            };
            result = Some(match result {
                Some(existing) => s.merge(&existing),
                None => s,
            });
        }
        result
    }

    fn list_prefix(&mut self) -> String {
        if let Some(list) = self.list_stack.last_mut() {
            if list.ordered {
                let n = list.next_number;
                list.next_number += 1;
                format!("{n}. ")
            } else {
                String::from("• ")
            }
        } else {
            String::from("• ")
        }
    }

    fn flush_line(&mut self) {
        if !self.current_spans.is_empty() {
            let spans = std::mem::take(&mut self.current_spans);
            self.lines.push(Line::from_spans(spans));
        }
    }

    fn flush_blank(&mut self) {
        self.flush_line();
        if self.needs_blank && !self.lines.is_empty() {
            self.lines.push(Line::new());
            self.needs_blank = false;
        }
    }

    fn flush_code_block(&mut self) {
        let code = std::mem::take(&mut self.code_block_lines).join("");
        let style = self.theme.code_block;
        for line_text in code.lines() {
            self.lines
                .push(Line::styled(format!("  {line_text}"), style));
        }
        // If the code block was empty or ended with newline, still show at least nothing
        if code.is_empty() {
            self.lines.push(Line::styled(String::from("  "), style));
        }
    }

    fn finish(mut self) -> Text {
        self.flush_line();
        if self.lines.is_empty() {
            return Text::new();
        }
        Text::from_lines(self.lines)
    }
}

// ---------------------------------------------------------------------------
// Convenience function
// ---------------------------------------------------------------------------

/// Render Markdown to styled [`Text`] using the default theme.
#[must_use]
pub fn render_markdown(markdown: &str) -> Text {
    MarkdownRenderer::default().render(markdown)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &Text) -> String {
        text.lines()
            .iter()
            .map(|l| l.to_plain_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn render_empty_string() {
        let text = render_markdown("");
        assert!(text.is_empty());
    }

    #[test]
    fn render_plain_paragraph() {
        let text = render_markdown("Hello, world!");
        let content = plain(&text);
        assert!(content.contains("Hello, world!"));
    }

    #[test]
    fn render_heading_h1() {
        let text = render_markdown("# Title");
        let content = plain(&text);
        assert!(content.contains("Title"));
        // H1 should be on its own line
        assert!(text.height() >= 1);
    }

    #[test]
    fn render_heading_levels() {
        let md = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("H1"));
        assert!(content.contains("H6"));
    }

    #[test]
    fn render_bold_text() {
        let text = render_markdown("Some **bold** text.");
        let content = plain(&text);
        assert!(content.contains("bold"));
    }

    #[test]
    fn render_italic_text() {
        let text = render_markdown("Some *italic* text.");
        let content = plain(&text);
        assert!(content.contains("italic"));
    }

    #[test]
    fn render_strikethrough() {
        let text = render_markdown("Some ~~struck~~ text.");
        let content = plain(&text);
        assert!(content.contains("struck"));
    }

    #[test]
    fn render_inline_code() {
        let text = render_markdown("Use `code` here.");
        let content = plain(&text);
        assert!(content.contains("`code`"));
    }

    #[test]
    fn render_code_block() {
        let md = "```rust\nfn main() {}\n```";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("fn main()"));
    }

    #[test]
    fn render_blockquote() {
        let text = render_markdown("> Quoted text");
        let content = plain(&text);
        assert!(content.contains("Quoted text"));
    }

    #[test]
    fn render_unordered_list() {
        let md = "- Item 1\n- Item 2\n- Item 3";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("• Item 1"));
        assert!(content.contains("• Item 2"));
        assert!(content.contains("• Item 3"));
    }

    #[test]
    fn render_ordered_list() {
        let md = "1. First\n2. Second\n3. Third";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("1. First"));
        assert!(content.contains("2. Second"));
        assert!(content.contains("3. Third"));
    }

    #[test]
    fn render_horizontal_rule() {
        let md = "Above\n\n---\n\nBelow";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Above"));
        assert!(content.contains("Below"));
        assert!(content.contains("─"));
    }

    #[test]
    fn render_link() {
        let text = render_markdown("[click here](https://example.com)");
        let content = plain(&text);
        assert!(content.contains("click here"));
    }

    #[test]
    fn render_nested_emphasis() {
        let text = render_markdown("***bold and italic***");
        let content = plain(&text);
        assert!(content.contains("bold and italic"));
    }

    #[test]
    fn render_nested_list() {
        let md = "- Outer\n  - Inner\n- Back";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("Outer"));
        assert!(content.contains("Inner"));
        assert!(content.contains("Back"));
    }

    #[test]
    fn render_multiple_paragraphs() {
        let md = "First paragraph.\n\nSecond paragraph.";
        let text = render_markdown(md);
        // Should have a blank line between paragraphs
        assert!(text.height() >= 3);
    }

    #[test]
    fn custom_theme() {
        let theme = MarkdownTheme {
            h1: Style::new().fg(PackedRgba::rgb(255, 0, 0)),
            ..Default::default()
        };
        let renderer = MarkdownRenderer::new(theme);
        let text = renderer.render("# Red Title");
        assert!(!text.is_empty());
    }

    #[test]
    fn custom_rule_width() {
        let renderer = MarkdownRenderer::default().rule_width(20);
        let text = renderer.render("---");
        let content = plain(&text);
        // Rule should be 20 chars wide
        let rule_line = content.lines().find(|l| l.contains('─')).unwrap();
        assert_eq!(rule_line.chars().filter(|&c| c == '─').count(), 20);
    }

    #[test]
    fn render_code_block_preserves_whitespace() {
        let md = "```\n  indented\n    more\n```";
        let text = render_markdown(md);
        let content = plain(&text);
        assert!(content.contains("  indented"));
        assert!(content.contains("    more"));
    }

    #[test]
    fn render_empty_code_block() {
        let md = "```\n```";
        let text = render_markdown(md);
        // Should still produce at least one line
        assert!(text.height() >= 1);
    }

    #[test]
    fn blockquote_has_bar_prefix() {
        let text = render_markdown("> quoted");
        let content = plain(&text);
        assert!(content.contains("│"));
    }
}
