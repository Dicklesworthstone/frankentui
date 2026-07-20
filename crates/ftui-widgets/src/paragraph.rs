#![forbid(unsafe_code)]

use crate::block::{Alignment, Block};
use crate::measurable::{MeasurableWidget, SizeConstraints};
use crate::{Widget, clear_text_area, draw_text_span_scrolled, draw_text_span_with_link};
use ahash::AHashMap;
use ftui_core::geometry::{Rect, Size};
use ftui_render::frame::Frame;
use ftui_style::Style;
use ftui_text::{Line, Span, Text as FtuiText, WrapMode, display_width, graphemes};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

type Text = FtuiText<'static>;

const PARAGRAPH_METRICS_CACHE_CAPACITY: usize = 256;
const PARAGRAPH_WRAP_CACHE_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
struct CachedParagraphMetrics {
    text_width: usize,
    text_height: usize,
    min_width: usize,
    line_widths: Arc<[usize]>,
}

#[derive(Debug, Clone)]
struct CachedWrappedParagraph {
    lines: Arc<[Line<'static>]>,
    line_widths: Arc<[usize]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ParagraphWrapCacheKey {
    text_hash: u64,
    wrap_mode: WrapMode,
    width: usize,
}

#[derive(Debug, Default)]
struct ParagraphCacheState {
    metrics: AHashMap<u64, CachedParagraphMetrics>,
    metrics_fifo: VecDeque<u64>,
    wrapped: AHashMap<ParagraphWrapCacheKey, CachedWrappedParagraph>,
    wrapped_fifo: VecDeque<ParagraphWrapCacheKey>,
}

impl ParagraphCacheState {
    fn insert_metrics(&mut self, key: u64, value: CachedParagraphMetrics) {
        cache_insert(
            &mut self.metrics,
            &mut self.metrics_fifo,
            PARAGRAPH_METRICS_CACHE_CAPACITY,
            key,
            value,
        );
    }

    fn insert_wrapped(&mut self, key: ParagraphWrapCacheKey, value: CachedWrappedParagraph) {
        cache_insert(
            &mut self.wrapped,
            &mut self.wrapped_fifo,
            PARAGRAPH_WRAP_CACHE_CAPACITY,
            key,
            value,
        );
    }
}

thread_local! {
    static PARAGRAPH_CACHE: RefCell<ParagraphCacheState> = RefCell::new(ParagraphCacheState::default());
}

fn cache_insert<K, V>(
    map: &mut AHashMap<K, V>,
    fifo: &mut VecDeque<K>,
    capacity: usize,
    key: K,
    value: V,
) where
    K: Copy + Eq + Hash,
{
    if !map.contains_key(&key) {
        if map.len() >= capacity
            && let Some(oldest) = fifo.pop_front()
        {
            map.remove(&oldest);
        }
        fifo.push_back(key);
    }
    map.insert(key, value);
}

fn text_into_owned(text: FtuiText<'_>) -> FtuiText<'static> {
    FtuiText::from_lines(
        text.into_iter()
            .map(|line| Line::from_spans(line.into_iter().map(Span::into_owned))),
    )
}

/// A widget that renders multi-line styled text.
#[derive(Debug, Clone, Default)]
pub struct Paragraph<'a> {
    text: Text,
    block: Option<Block<'a>>,
    style: Style,
    wrap: Option<WrapMode>,
    alignment: Alignment,
    scroll: (u16, u16),
}

fn hash_value<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn line_min_width(line: &Line<'_>) -> usize {
    let mut max_word_width = 0;
    let mut current_word_width = 0;

    for span in line.spans() {
        for grapheme in graphemes(span.content.as_ref()) {
            let grapheme_width = display_width(grapheme);
            if grapheme.chars().all(char::is_whitespace) {
                max_word_width = max_word_width.max(current_word_width);
                current_word_width = 0;
            } else {
                current_word_width += grapheme_width;
            }
        }
    }

    max_word_width.max(current_word_width)
}

impl<'a> Paragraph<'a> {
    fn with_static_text(text: FtuiText<'static>) -> Self {
        Self {
            text,
            block: None,
            style: Style::default(),
            wrap: None,
            alignment: Alignment::Left,
            scroll: (0, 0),
        }
    }

    /// Create a new paragraph from the given text.
    #[must_use]
    pub fn new<'t>(text: impl Into<FtuiText<'t>>) -> Self {
        Self::with_static_text(text_into_owned(text.into()))
    }

    /// Create a new paragraph from text that is already valid for the widget's storage lifetime.
    #[must_use]
    pub fn from_static_text(text: FtuiText<'static>) -> Self {
        Self::with_static_text(text)
    }

    /// Create a new paragraph from a static string without copying the string contents.
    #[must_use]
    pub fn from_static_str(text: &'static str) -> Self {
        Self::from_static_text(FtuiText::raw(text))
    }

    /// Set the surrounding block.
    #[must_use]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Set the base text style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set the text wrapping mode.
    #[must_use]
    pub fn wrap(mut self, wrap: WrapMode) -> Self {
        self.wrap = Some(wrap);
        self
    }

    /// Set the text alignment.
    #[must_use]
    pub fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Set the scroll offset as (vertical, horizontal).
    #[must_use]
    pub fn scroll(mut self, offset: (u16, u16)) -> Self {
        self.scroll = offset;
        self
    }

    fn text_hash(&self) -> u64 {
        hash_value(&self.text)
    }

    fn cached_metrics(&self) -> CachedParagraphMetrics {
        let text_hash = self.text_hash();
        PARAGRAPH_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some(metrics) = cache.metrics.get(&text_hash) {
                return metrics.clone();
            }

            let mut text_width = 0usize;
            let mut min_width = 0usize;
            let mut line_widths = Vec::with_capacity(self.text.lines().len());

            for line in self.text.lines() {
                let width = line.width();
                text_width = text_width.max(width);
                min_width = min_width.max(line_min_width(line));
                line_widths.push(width);
            }

            let metrics = CachedParagraphMetrics {
                text_width,
                text_height: self.text.height(),
                min_width: if min_width == 0 {
                    text_width
                } else {
                    min_width
                },
                line_widths: Arc::from(line_widths),
            };

            cache.insert_metrics(text_hash, metrics.clone());
            metrics
        })
    }

    fn cached_wrapped_lines(&self, width: usize, wrap_mode: WrapMode) -> CachedWrappedParagraph {
        let key = ParagraphWrapCacheKey {
            text_hash: self.text_hash(),
            wrap_mode,
            width,
        };

        PARAGRAPH_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some(wrapped) = cache.wrapped.get(&key) {
                return wrapped.clone();
            }

            let mut lines = Vec::new();
            let mut line_widths = Vec::new();

            for line in self.text.lines() {
                let line_width = line.width();
                if wrap_mode == WrapMode::None || line_width <= width {
                    lines.push(line.clone());
                    line_widths.push(line_width);
                    continue;
                }

                let wrapped_lines = line.wrap(width, wrap_mode);
                if wrapped_lines.is_empty() {
                    lines.push(Line::new());
                    line_widths.push(0);
                    continue;
                }

                for wrapped_line in wrapped_lines {
                    line_widths.push(wrapped_line.width());
                    lines.push(wrapped_line);
                }
            }

            let wrapped = CachedWrappedParagraph {
                lines: Arc::from(lines),
                line_widths: Arc::from(line_widths),
            };

            cache.insert_wrapped(key, wrapped.clone());
            wrapped
        })
    }
}

impl Widget for Paragraph<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "Paragraph",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        let deg = frame.buffer.degradation;

        // Skeleton+: clear the owned area so previously rendered content does not linger.
        if !deg.render_content() {
            clear_text_area(frame, area, Style::default());
            return;
        }

        // Special-case: an empty Paragraph with no Block is commonly used as a screen-clear.
        // In that mode we must clear cell *content* (not just paint style), otherwise old
        // borders/characters can bleed through Flex gaps.
        let style = if deg.apply_styling() {
            self.style
        } else {
            Style::default()
        };
        if self.block.is_none() && self.text.is_empty() {
            clear_text_area(frame, area, style);
            return;
        }

        clear_text_area(frame, area, style);

        let text_area = match self.block {
            Some(ref b) => {
                b.render(area, frame);
                b.inner(area)
            }
            None => area,
        };

        if text_area.is_empty() {
            return;
        }

        // At NoStyling, render text without per-span styles
        // Background is already applied for the whole area via `set_style_area()`. When drawing
        // text we avoid re-applying the same background, otherwise semi-transparent BG colors
        // get composited multiple times.
        let mut text_style = style;
        text_style.bg = None;

        let mut y = text_area.y;
        let mut current_visual_line = 0;
        let scroll_offset = self.scroll.0 as usize;

        let mut render_line = |line: &ftui_text::Line, line_width: usize, y: u16| {
            let scroll_x = self.scroll.1;
            let start_x = align_x(text_area, line_width, self.alignment);

            // Let's iterate spans.
            // `span_visual_offset`: relative to line start.
            let mut span_visual_offset = 0;

            // Alignment offset relative to text_area.x
            let alignment_offset = start_x.saturating_sub(text_area.x);

            for span in line.spans() {
                let span_width = span.width();

                // Effective position of this span relative to text_area.x
                // pos = alignment_offset + span_visual_offset - scroll_x
                let line_rel_start = alignment_offset.saturating_add(span_visual_offset);

                // Check visibility
                if line_rel_start.saturating_add(span_width as u16) <= scroll_x {
                    // Fully scrolled out to the left
                    span_visual_offset = span_visual_offset.saturating_add(span_width as u16);
                    continue;
                }

                // Calculate actual draw position.
                let (draw_x, local_scroll) = if line_rel_start < scroll_x {
                    // Partially scrolled out left
                    (text_area.x, scroll_x - line_rel_start)
                } else {
                    // Start is visible
                    (text_area.x.saturating_add(line_rel_start - scroll_x), 0)
                };

                if draw_x >= text_area.right() {
                    // Fully clipped to the right
                    break;
                }

                // At NoStyling+, ignore span-level styles entirely
                let span_style = if deg.apply_styling() {
                    match span.style {
                        Some(s) => s.merge(&text_style),
                        None => text_style,
                    }
                } else {
                    text_style // Style::default() at NoStyling
                };

                if local_scroll > 0 {
                    draw_text_span_scrolled(
                        frame,
                        draw_x,
                        y,
                        span.content.as_ref(),
                        span_style,
                        text_area.right(),
                        local_scroll,
                        span.link.as_deref(),
                    );
                } else {
                    draw_text_span_with_link(
                        frame,
                        draw_x,
                        y,
                        span.content.as_ref(),
                        span_style,
                        text_area.right(),
                        span.link.as_deref(),
                    );
                }

                span_visual_offset = span_visual_offset.saturating_add(span_width as u16);
            }
        };

        let metrics = self.cached_metrics();
        let rendered_lines: Option<CachedWrappedParagraph> = self
            .wrap
            .map(|wrap_mode| self.cached_wrapped_lines(text_area.width as usize, wrap_mode));

        if let Some(wrapped) = rendered_lines {
            for (line, line_width) in wrapped.lines.iter().zip(wrapped.line_widths.iter()) {
                if current_visual_line < scroll_offset {
                    current_visual_line += 1;
                    continue;
                }
                if y >= text_area.bottom() {
                    break;
                }
                render_line(line, *line_width, y);
                y = y.saturating_add(1);
                current_visual_line += 1;
            }
        } else {
            for (line, line_width) in self.text.lines().iter().zip(metrics.line_widths.iter()) {
                if current_visual_line < scroll_offset {
                    current_visual_line += 1;
                    continue;
                }
                if y >= text_area.bottom() {
                    break;
                }
                render_line(line, *line_width, y);
                y = y.saturating_add(1);
                current_visual_line += 1;
            }
        }
    }
}
impl MeasurableWidget for Paragraph<'_> {
    fn measure(&self, available: Size) -> SizeConstraints {
        let metrics = self.cached_metrics();
        let text_width = metrics.text_width;
        let text_height = metrics.text_height;
        let min_width = metrics.min_width;

        // Get block chrome if present
        let (chrome_width, chrome_height) = self
            .block
            .as_ref()
            .map(|b| b.chrome_size())
            .unwrap_or((0, 0));

        // If wrapping is enabled, calculate wrapped height
        let (preferred_width, preferred_height) =
            if self.wrap.is_some_and(|mode| mode != WrapMode::None) {
                // When wrapping, preferred width is either the text width or available width
                let wrap_width = if available.width > chrome_width {
                    (available.width - chrome_width) as usize
                } else {
                    1
                };

                let wrapped_height = self
                    .wrap
                    .map(|wrap_mode| self.cached_wrapped_lines(wrap_width, wrap_mode).lines.len())
                    .unwrap_or(text_height);

                // Preferred width is min(text_width, available_width - chrome)
                let pref_w = text_width.min(wrap_width);
                (pref_w, wrapped_height)
            } else {
                // No wrapping: preferred is natural text dimensions
                (text_width, text_height)
            };

        // Convert to u16, saturating at MAX
        let min_w = (min_width as u16).saturating_add(chrome_width);
        // Only require 1 line minimum if there's actual content
        let min_h = if preferred_height > 0 {
            (1u16).saturating_add(chrome_height)
        } else {
            chrome_height
        };

        let pref_w = (preferred_width as u16).saturating_add(chrome_width);
        let pref_h = (preferred_height as u16).saturating_add(chrome_height);

        SizeConstraints {
            min: Size::new(min_w, min_h),
            preferred: Size::new(pref_w, pref_h),
            max: None, // Paragraph can use additional space for scrolling
        }
    }

    fn has_intrinsic_size(&self) -> bool {
        // Paragraph always has intrinsic size based on its text content
        true
    }
}

impl Paragraph<'_> {
    #[cfg_attr(not(test), allow(dead_code))]
    fn calculate_min_width(&self) -> usize {
        self.cached_metrics().min_width
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn estimate_wrapped_height(&self, wrap_width: usize) -> usize {
        if wrap_width == 0 {
            return self.cached_metrics().text_height;
        }

        self.wrap
            .map(|wrap_mode| self.cached_wrapped_lines(wrap_width, wrap_mode).lines.len())
            .unwrap_or_else(|| self.cached_metrics().text_height)
            .max(1)
    }
}

/// Calculate the starting x position for a line given alignment.
fn align_x(area: Rect, line_width: usize, alignment: Alignment) -> u16 {
    let line_width_u16 = u16::try_from(line_width).unwrap_or(u16::MAX);
    match alignment {
        Alignment::Left => area.x,
        Alignment::Center => area
            .x
            .saturating_add(area.width.saturating_sub(line_width_u16) / 2),
        Alignment::Right => area
            .x
            .saturating_add(area.width.saturating_sub(line_width_u16)),
    }
}

fn truncate_accessible_text(text: &str) -> String {
    const ACCESSIBLE_TEXT_LIMIT: usize = 200;
    const ACCESSIBLE_TEXT_PREFIX_LIMIT: usize = 197;

    if text.chars().count() <= ACCESSIBLE_TEXT_LIMIT {
        text.to_owned()
    } else {
        let mut prefix = String::new();
        let mut prefix_chars = 0usize;

        for grapheme in graphemes(text) {
            let grapheme_chars = grapheme.chars().count();
            if prefix_chars + grapheme_chars > ACCESSIBLE_TEXT_PREFIX_LIMIT {
                break;
            }
            prefix.push_str(grapheme);
            prefix_chars += grapheme_chars;
        }

        format!("{prefix}...")
    }
}

// ============================================================================
// Accessibility
// ============================================================================

impl ftui_a11y::Accessible for Paragraph<'_> {
    fn accessibility_nodes(&self, area: Rect) -> Vec<ftui_a11y::node::A11yNodeInfo> {
        use ftui_a11y::node::{A11yNodeInfo, A11yRole};

        let id = crate::a11y_node_id(area);

        // Extract the plain-text content for the accessible name.
        let name: String = self
            .text
            .lines()
            .iter()
            .map(|line| {
                line.spans()
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join(" ");

        let block_title = self.block.as_ref().and_then(|b| b.title_text());
        let truncated_name = truncate_accessible_text(&name);

        let mut node = A11yNodeInfo::new(id, A11yRole::Label, area);
        if let Some(title) = block_title {
            node = node.with_name(title);
            if !name.is_empty() {
                node = node.with_description(truncated_name);
            }
        } else if !name.is_empty() {
            node = node.with_name(truncated_name);
        }

        vec![node]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    fn raw_row_text(frame: &Frame, y: u16) -> String {
        let width = frame.buffer.width();
        let mut actual = String::new();
        for x in 0..width {
            let ch = frame
                .buffer
                .get(x, y)
                .and_then(|cell| cell.content.as_char())
                .unwrap_or(' ');
            actual.push(ch);
        }
        actual
    }

    #[test]
    fn render_simple_text() {
        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(frame.buffer.get(4, 0).unwrap().content.as_char(), Some('o'));
    }

    #[test]
    fn from_static_str_preserves_rendering_without_owning_static_spans() {
        let area = Rect::new(0, 0, 8, 2);

        let fast = Paragraph::from_static_str("Hi\nthere").style(Style::new().bold());
        let slow = Paragraph::new("Hi\nthere").style(Style::new().bold());

        let mut fast_pool = GraphemePool::new();
        let mut fast_frame = Frame::new(8, 2, &mut fast_pool);
        fast.render(area, &mut fast_frame);

        let mut slow_pool = GraphemePool::new();
        let mut slow_frame = Frame::new(8, 2, &mut slow_pool);
        slow.render(area, &mut slow_frame);

        assert_eq!(raw_row_text(&fast_frame, 0), raw_row_text(&slow_frame, 0));
        assert_eq!(raw_row_text(&fast_frame, 1), raw_row_text(&slow_frame, 1));

        let first_span = &fast.text.lines()[0].spans()[0];
        match &first_span.content {
            std::borrow::Cow::Borrowed(text) => assert_eq!(*text, "Hi"),
            std::borrow::Cow::Owned(_) => panic!("static paragraph text should stay borrowed"),
        }
    }

    #[test]
    fn render_multiline_text() {
        let para = Paragraph::new(Text::raw("AB\nCD"));
        let area = Rect::new(0, 0, 5, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 3, &mut pool);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(1, 0).unwrap().content.as_char(), Some('B'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('C'));
        assert_eq!(frame.buffer.get(1, 1).unwrap().content.as_char(), Some('D'));
    }

    #[test]
    fn render_centered_text() {
        let para = Paragraph::new(Text::raw("Hi")).alignment(Alignment::Center);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        para.render(area, &mut frame);

        // "Hi" is 2 wide, area is 10, so starts at (10-2)/2 = 4
        assert_eq!(frame.buffer.get(4, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(frame.buffer.get(5, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn render_with_scroll() {
        let para = Paragraph::new(Text::raw("Line1\nLine2\nLine3")).scroll((1, 0));
        let area = Rect::new(0, 0, 10, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 2, &mut pool);
        para.render(area, &mut frame);

        // Should skip Line1, show Line2 and Line3
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('L'));
        assert_eq!(frame.buffer.get(4, 0).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn render_empty_area() {
        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        para.render(area, &mut frame);
    }

    #[test]
    fn line_min_width_tracks_words_across_spans() {
        let line = Line::from_spans([
            Span::raw("alpha"),
            Span::styled(" ", Style::new().bold()),
            Span::raw("beta"),
            Span::raw("  "),
            Span::raw("gamma"),
        ]);

        assert_eq!(line_min_width(&line), 5);
    }

    #[test]
    fn measure_wrap_counts_cached_visual_lines() {
        let para = Paragraph::new(Text::raw("hello world from cache")).wrap(WrapMode::Word);
        let constraints = para.measure(Size::new(8, 10));

        assert_eq!(constraints.preferred.height, 4);
        assert_eq!(constraints.min.width, 5);
    }

    #[test]
    fn measure_wrap_none_preserves_natural_width() {
        let para = Paragraph::new(Text::raw("abcdef")).wrap(WrapMode::None);
        let constraints = para.measure(Size::new(3, 10));

        assert_eq!(constraints.preferred.width, 6);
        assert_eq!(constraints.preferred.height, 1);
    }

    #[test]
    fn render_empty_text_clears_content() {
        let para = Paragraph::new("");
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);

        // Seed with non-space content; an empty Paragraph render should clear it.
        frame
            .buffer
            .fill(area, ftui_render::cell::Cell::from_char('X'));

        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some(' '));
        assert_eq!(frame.buffer.get(2, 0).unwrap().content.as_char(), Some(' '));
    }

    #[test]
    fn render_right_aligned() {
        let para = Paragraph::new(Text::raw("Hi")).alignment(Alignment::Right);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        para.render(area, &mut frame);

        // "Hi" is 2 wide, area is 10, so starts at 10-2 = 8
        assert_eq!(frame.buffer.get(8, 0).unwrap().content.as_char(), Some('H'));
        assert_eq!(frame.buffer.get(9, 0).unwrap().content.as_char(), Some('i'));
    }

    #[test]
    fn render_with_word_wrap() {
        let para = Paragraph::new(Text::raw("hello world")).wrap(WrapMode::Word);
        let area = Rect::new(0, 0, 6, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(6, 3, &mut pool);
        para.render(area, &mut frame);

        // "hello " fits in 6, " world" wraps to next line
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('h'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('w'));
    }

    #[test]
    fn render_with_char_wrap() {
        let para = Paragraph::new(Text::raw("abcdefgh")).wrap(WrapMode::Char);
        let area = Rect::new(0, 0, 4, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 3, &mut pool);
        para.render(area, &mut frame);

        // First line: abcd
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('a'));
        assert_eq!(frame.buffer.get(3, 0).unwrap().content.as_char(), Some('d'));
        // Second line: efgh
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('e'));
    }

    #[test]
    fn scroll_past_all_lines() {
        let para = Paragraph::new(Text::raw("AB")).scroll((5, 0));
        let area = Rect::new(0, 0, 5, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 2, &mut pool);
        para.render(area, &mut frame);

        // All lines skipped, but the paragraph still owns and clears its area.
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some(' '));
    }

    #[test]
    fn render_shorter_text_clears_stale_suffix_and_extra_lines() {
        let area = Rect::new(0, 0, 8, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(8, 2, &mut pool);

        Paragraph::new(Text::raw("Hello\nWorld")).render(area, &mut frame);
        Paragraph::new(Text::raw("Hi")).render(area, &mut frame);

        assert_eq!(raw_row_text(&frame, 0), "Hi      ");
        assert_eq!(raw_row_text(&frame, 1), "        ");
    }

    #[test]
    fn render_clipped_at_area_height() {
        let para = Paragraph::new(Text::raw("A\nB\nC\nD\nE"));
        let area = Rect::new(0, 0, 5, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 2, &mut pool);
        para.render(area, &mut frame);

        // Only first 2 lines should render
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('B'));
    }

    #[test]
    fn render_clipped_at_area_width() {
        let para = Paragraph::new(Text::raw("ABCDEF"));
        let area = Rect::new(0, 0, 3, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 1, &mut pool);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(2, 0).unwrap().content.as_char(), Some('C'));
    }

    #[test]
    fn align_x_left() {
        let area = Rect::new(5, 0, 20, 1);
        assert_eq!(align_x(area, 10, Alignment::Left), 5);
    }

    #[test]
    fn align_x_center() {
        let area = Rect::new(0, 0, 20, 1);
        // line_width=6, area=20, so (20-6)/2 = 7
        assert_eq!(align_x(area, 6, Alignment::Center), 7);
    }

    #[test]
    fn align_x_right() {
        let area = Rect::new(0, 0, 20, 1);
        // line_width=5, area=20, so 20-5 = 15
        assert_eq!(align_x(area, 5, Alignment::Right), 15);
    }

    #[test]
    fn align_x_wide_line_saturates() {
        let area = Rect::new(0, 0, 10, 1);
        // line wider than area: should saturate to area.x
        assert_eq!(align_x(area, 20, Alignment::Right), 0);
        assert_eq!(align_x(area, 20, Alignment::Center), 0);
    }

    #[test]
    fn builder_methods_chain() {
        let para = Paragraph::new(Text::raw("test"))
            .style(Style::default())
            .wrap(WrapMode::Word)
            .alignment(Alignment::Center)
            .scroll((1, 2));
        // Verify it builds without panic
        let area = Rect::new(0, 0, 10, 5);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        para.render(area, &mut frame);
    }

    #[test]
    fn render_at_offset_area() {
        let para = Paragraph::new(Text::raw("X"));
        let area = Rect::new(3, 4, 5, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 10, &mut pool);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(3, 4).unwrap().content.as_char(), Some('X'));
        // Cell at (0,0) should be empty
        assert!(frame.buffer.get(0, 0).unwrap().is_empty());
    }

    #[test]
    fn wrap_clipped_at_area_bottom() {
        // Long wrapped text should stop at area height
        let para = Paragraph::new(Text::raw("abcdefghijklmnop")).wrap(WrapMode::Char);
        let area = Rect::new(0, 0, 4, 2);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(4, 2, &mut pool);
        para.render(area, &mut frame);

        // Only 2 rows of 4 chars each
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('a'));
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('e'));
    }

    // --- Degradation tests ---

    #[test]
    fn degradation_skeleton_skips_content() {
        use ftui_render::budget::DegradationLevel;

        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        Paragraph::new(Text::raw("Stale")).render(area, &mut frame);
        frame.set_degradation(DegradationLevel::Skeleton);
        para.render(area, &mut frame);

        // Skeleton clears previously rendered content instead of leaving it behind.
        assert_eq!(raw_row_text(&frame, 0), "          ");
    }

    #[test]
    fn degradation_full_renders_content() {
        use ftui_render::budget::DegradationLevel;

        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.set_degradation(DegradationLevel::Full);
        para.render(area, &mut frame);

        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('H'));
    }

    #[test]
    fn degradation_essential_only_still_renders_text() {
        use ftui_render::budget::DegradationLevel;

        let para = Paragraph::new(Text::raw("Hello"));
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.set_degradation(DegradationLevel::EssentialOnly);
        para.render(area, &mut frame);

        // EssentialOnly still renders content (< Skeleton)
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('H'));
    }

    #[test]
    fn degradation_no_styling_ignores_span_styles() {
        use ftui_render::budget::DegradationLevel;
        use ftui_render::cell::PackedRgba;
        use ftui_text::{Line, Span};

        // Create text with a styled span
        let styled_span = Span::styled("Hello", Style::new().fg(PackedRgba::RED));
        let line = Line::from_spans([styled_span]);
        let text = Text::from(line);
        let para = Paragraph::new(text);
        let area = Rect::new(0, 0, 10, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        frame.set_degradation(DegradationLevel::NoStyling);
        para.render(area, &mut frame);

        // Text should render but span style should be ignored
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('H'));
        // Foreground color should NOT be red
        assert_ne!(
            frame.buffer.get(0, 0).unwrap().fg,
            PackedRgba::RED,
            "Span fg color should be ignored at NoStyling"
        );
    }

    // --- MeasurableWidget tests ---

    use crate::MeasurableWidget;
    use ftui_core::geometry::Size;

    #[test]
    fn measure_simple_text() {
        let para = Paragraph::new(Text::raw("Hello"));
        let constraints = para.measure(Size::MAX);

        // "Hello" is 5 chars wide, 1 line tall
        assert_eq!(constraints.preferred, Size::new(5, 1));
        assert_eq!(constraints.min.height, 1);
        // Min width is the longest word = "Hello" = 5
        assert_eq!(constraints.min.width, 5);
    }

    #[test]
    fn measure_multiline_text() {
        let para = Paragraph::new(Text::raw("Line1\nLine22\nL3"));
        let constraints = para.measure(Size::MAX);

        // Max width is "Line22" = 6, height = 3 lines
        assert_eq!(constraints.preferred, Size::new(6, 3));
        assert_eq!(constraints.min.height, 1);
        // Min width is longest word = "Line22" = 6
        assert_eq!(constraints.min.width, 6);
    }

    #[test]
    fn measure_with_block() {
        let block = crate::block::Block::bordered();
        let para = Paragraph::new(Text::raw("Hi")).block(block);
        let constraints = para.measure(Size::MAX);

        // "Hi" = 2 wide, 1 tall, plus chrome (borders + padding) = 4 on each axis.
        assert_eq!(constraints.preferred, Size::new(6, 5));
        assert_eq!(constraints.min.width, 6);
        assert_eq!(constraints.min.height, 5);
    }

    #[test]
    fn measure_with_word_wrap() {
        let para = Paragraph::new(Text::raw("hello world")).wrap(WrapMode::Word);
        // Measure with narrow available width
        let constraints = para.measure(Size::new(6, 10));

        // With 6 chars available, "hello" fits, "world" wraps
        // Preferred width = 6 (available), height = 2 lines
        assert_eq!(constraints.preferred.height, 2);
        // Min width is longest word = "hello" = 5
        assert_eq!(constraints.min.width, 5);
    }

    #[test]
    fn measure_empty_text() {
        let para = Paragraph::new(Text::raw(""));
        let constraints = para.measure(Size::MAX);

        // Empty text: 0 width, 0 height (no lines)
        assert_eq!(constraints.preferred.width, 0);
        assert_eq!(constraints.preferred.height, 0);
        // Min height is 0 for empty text (no content to display)
        // This ensures min <= preferred invariant holds
        assert_eq!(constraints.min.height, 0);
    }

    #[test]
    fn calculate_min_width_single_long_word() {
        let para = Paragraph::new(Text::raw("supercalifragilistic"));
        assert_eq!(para.calculate_min_width(), 20);
    }

    #[test]
    fn calculate_min_width_multiple_words() {
        let para = Paragraph::new(Text::raw("the quick brown fox"));
        // Longest word is "quick" or "brown" = 5
        assert_eq!(para.calculate_min_width(), 5);
    }

    #[test]
    fn calculate_min_width_multiline() {
        let para = Paragraph::new(Text::raw("short\nlongword\na"));
        // Longest word is "longword" = 8
        assert_eq!(para.calculate_min_width(), 8);
    }

    #[test]
    fn estimate_wrapped_height_no_wrap_needed() {
        let para = Paragraph::new(Text::raw("short")).wrap(WrapMode::Word);
        // Width 10 is enough for "short" (5 chars)
        assert_eq!(para.estimate_wrapped_height(10), 1);
    }

    #[test]
    fn estimate_wrapped_height_needs_wrap() {
        let para = Paragraph::new(Text::raw("hello world")).wrap(WrapMode::Word);
        // Width 6: "hello " fits (6 chars), "world" (5 chars) wraps
        assert_eq!(para.estimate_wrapped_height(6), 2);
    }

    #[test]
    fn has_intrinsic_size() {
        let para = Paragraph::new(Text::raw("test"));
        assert!(para.has_intrinsic_size());
    }

    #[test]
    fn measure_is_pure() {
        let para = Paragraph::new(Text::raw("Hello World"));
        let a = para.measure(Size::new(100, 50));
        let b = para.measure(Size::new(100, 50));
        assert_eq!(a, b);
    }

    #[test]
    fn accessibility_truncates_long_unicode_without_panicking() {
        use ftui_a11y::Accessible;

        let para = Paragraph::new(Text::raw("界".repeat(210)));
        let nodes = para.accessibility_nodes(Rect::new(0, 0, 10, 1));
        let name = nodes[0]
            .name
            .as_deref()
            .expect("paragraph should have a name");

        assert!(name.ends_with("..."));
        assert_eq!(name.chars().count(), 200);
    }

    #[test]
    fn accessibility_truncates_description_when_block_title_present() {
        use ftui_a11y::Accessible;

        let para =
            Paragraph::new(Text::raw("界".repeat(210))).block(Block::bordered().title("Body"));
        let nodes = para.accessibility_nodes(Rect::new(0, 0, 10, 1));
        let node = &nodes[0];

        assert_eq!(node.name.as_deref(), Some("Body"));
        let description = node
            .description
            .as_deref()
            .expect("paragraph should have a description");
        assert!(description.ends_with("..."));
        assert_eq!(description.chars().count(), 200);
    }

    #[test]
    fn accessibility_preserves_exactly_200_chars_without_ellipsis() {
        use ftui_a11y::Accessible;

        let para = Paragraph::new(Text::raw("界".repeat(200)));
        let nodes = para.accessibility_nodes(Rect::new(0, 0, 10, 1));
        let name = nodes[0]
            .name
            .as_deref()
            .expect("paragraph should have a name");

        assert!(!name.ends_with("..."));
        assert_eq!(name.chars().count(), 200);
    }

    #[test]
    fn accessibility_truncates_on_grapheme_boundaries() {
        use ftui_a11y::Accessible;

        let para = Paragraph::new(Text::raw("e\u{301}".repeat(210)));
        let nodes = para.accessibility_nodes(Rect::new(0, 0, 10, 1));
        let name = nodes[0]
            .name
            .as_deref()
            .expect("paragraph should have a name");

        let prefix = name
            .strip_suffix("...")
            .expect("paragraph should be truncated");
        assert!(name.chars().count() <= 200);
        assert_eq!(ftui_text::graphemes(prefix).count(), 98);
        assert!(prefix.ends_with("e\u{301}"));
    }
}
