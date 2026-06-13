#![forbid(unsafe_code)]

//! Markdown and Rich Text screen — typography and text processing.
//!
//! Demonstrates:
//! - `MarkdownRenderer` with custom `MarkdownTheme`
//! - GFM auto-detection with `is_likely_markdown`
//! - Streaming/fragment rendering with `render_streaming`
//! - Text style attributes (bold, italic, underline, etc.)
//! - Unicode text with CJK and emoji in a `Table`
//! - `WrapMode` and `Alignment` cycling

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::sync::Arc;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::markdown::{
    MarkdownDetection, MarkdownRenderer, MarkdownTheme, is_likely_markdown,
};
use ftui_extras::syntax::SyntaxHighlighter;
use ftui_extras::visual_fx::{Backdrop, PlasmaFx, PlasmaPalette, Scrim, ThemeInputs};
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::{Cell as RenderCell, CellAttrs, CellContent, StyleFlags};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::text::{Line, Span, Text};
use ftui_text::{WrapMode, grapheme_width, graphemes};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::table::{Row, Table};

use super::{HelpEntry, Screen};
use crate::theme;

/// Simulated LLM streaming response with complex GFM content.
/// This demonstrates real-world markdown that an LLM might generate.
const STREAMING_MARKDOWN: &str = "\
# FrankenTUI Streaming Report — \"Galaxy Brain\" Edition

> [!NOTE]
> This stream simulates an LLM response rendered **incrementally** with full GFM support.

> [!TIP]
> Inline-first output keeps logs scrolling while the UI stays stable.

> [!WARNING]
> Rendering is deterministic. If you see flicker, it is a bug.

## TL;DR

- ✅ Zero-flicker rendering via **Buffer → Diff → Presenter**
- ✅ Evidence-ledger decisions (Bayes factors) for strategy selection
- ✅ Inline mode preserves scrollback
- ✅ 16-byte cells enable SIMD comparisons

### Roadmap (Live)

- [x] Deterministic renderer
- [x] Inline mode
- [x] Snapshot testing
- [ ] Dirty-span diff (interval union)
- [ ] Summed-area tile skip
- [ ] Conformal frame-time risk control

## Architecture Overview

```mermaid
graph TD
  A[Event Stream] --> B[Model Update]
  B --> C[Frame Buffer]
  C --> D[Diff Engine]
  D --> E[ANSI Presenter]
  E --> F[Terminal Writer]
```

## Runtime Config (YAML)

```yaml
runtime:
  screen_mode: inline
  ui_height: 12
  tick_ms: 16
  evidence_log: true
  budgets:
    render_ms: 16
    input_ms: 1
    diff_ms: 4
```

## Evidence Ledger (JSON)

```json
{
  \"event\": \"diff_decision\",
  \"strategy\": \"DirtyRows\",
  \"posterior_mean\": 0.032,
  \"expected_cost\": {
    \"full\": 1.23,
    \"dirty\": 0.41,
    \"redraw\": 2.02
  },
  \"tie_break\": \"stable\"
}
```

## SQL Query (Latency Scan)

```sql
SELECT
  frame_id,
  diff_cells,
  render_ms,
  budget_ms
FROM telemetry
WHERE render_ms > budget_ms
ORDER BY render_ms DESC
LIMIT 5;
```

## Rust Snippet (Renderer Core)

```rust
pub fn present(frame: &Frame, writer: &mut TerminalWriter) -> Result<()> {
    let diff = BufferDiff::compute(frame.prev(), frame.next());
    let spans = diff.coalesced_spans();
    writer.begin_sync()?;
    for span in spans {
        writer.move_to(span.x, span.y)?;
        writer.write_cells(span.cells)?;
    }
    writer.end_sync()?;
    Ok(())
}
```

## TypeScript Snippet (Log Parser)

```ts
type Event = { tick: number; render_ms: number; diff_cells: number };

export function p95(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.floor(0.95 * (sorted.length - 1));
  return sorted[idx] ?? 0;
}

export function summarize(events: Event[]) {
  const render = events.map((e) => e.render_ms);
  return { p95: p95(render), max: Math.max(...render) };
}
```

## Bash Harness

```bash
FTUI_DEMO_SCREEN=14 \
FTUI_DEMO_EXIT_AFTER_MS=1200 \
cargo run -p ftui-demo-showcase --release
```

## Diff Sample

```diff
- dirty_rows = 48
- strategy = \"Full\"
+ dirty_rows = 6
+ strategy = \"DirtyRows\"
```

## Data Table

| Metric | Value | Trend |
|:------ | ----: | :--- |
| Diff cells | 182 | ↘ |
| Render ms | 9.4 | ↘ |
| FPS | 59.7 | ↗ |

## Math Corner

Inline: $E = mc^2$ and $\\alpha + \\beta = \\gamma$.

Block:

$$P(R \\mid E) = \\frac{P(E\\mid R)P(R)}{P(E)}$$

---

*Press* <kbd>Space</kbd> *to toggle streaming, <kbd>r</kbd> to restart* 🚀
";

const SAMPLE_MARKDOWN: &str = "\
# GitHub-Flavored Markdown (Rich Demo)

## LaTeX + Symbols

Inline math: $E = mc^2$, $\\alpha + \\beta = \\gamma$, $\\Delta x \\approx 0.001$.

Block math:

$$\\sum_{i=1}^{n} x_i = \\frac{n(n+1)}{2}$$

$$\\int_{-\\infty}^{\\infty} e^{-x^2} dx = \\sqrt{\\pi}$$

## Admonitions

> [!NOTE]
> Information note with **rich emphasis**.

> [!TIP]
> Use <kbd>Tab</kbd> and <kbd>Shift+Tab</kbd> to navigate.

> [!WARNING]
> Unsafe mode is forbidden in this project.

## Task Lists + Links

- [x] Inline mode + scrollback
- [x] Deterministic output
- [ ] Time-travel diff heatmap
- [ ] Conformal frame-time predictor

Link: <https://example.com>

## Code Blocks

```rust
#[derive(Debug, Clone)]
pub enum Strategy { Full, DirtyRows, Redraw }

pub fn choose(costs: &[f64; 3]) -> Strategy {
    let (idx, _) = costs.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).unwrap();
    match idx { 0 => Strategy::Full, 1 => Strategy::DirtyRows, _ => Strategy::Redraw }
}
```

```python
from dataclasses import dataclass

@dataclass
class Span:
    x0: int
    x1: int
```

```json
{ \"screen\": \"dashboard\", \"fps\": 59.7, \"dirty_rows\": 6 }
```

```yaml
features:
  - inline
  - diff
  - evidence
```

## Tables

| Feature | Status | Notes |
|--------|:------:|------:|
| Inline mode | ✅ | Scrollback preserved |
| Diff engine | ✅ | SIMD-friendly |
| Evidence logs | ✅ | JSONL |

## Typography

**Bold**, *Italic*, ~~Strike~~, `Inline Code`

> \"Correctness over cleverness.\" — FrankenTUI

---

*Built with FrankenTUI 🦀*
";

const WRAP_MODES: &[WrapMode] = &[
    WrapMode::None,
    WrapMode::Word,
    WrapMode::Char,
    WrapMode::WordChar,
];

const ALIGNMENTS: &[Alignment] = &[Alignment::Left, Alignment::Center, Alignment::Right];

/// Base characters to advance per tick during streaming simulation.
const STREAM_CHARS_PER_TICK: usize = 3;
/// Global speed multiplier for the streaming demo.
const STREAM_SPEED_MULTIPLIER: usize = 81;
/// Turbo multiplier for fast streaming playback.
const STREAM_TURBO_MULTIPLIER: usize = 2;
/// Horizontal rule width for markdown rendering.
const RULE_WIDTH: u16 = 36;

struct MarkdownPanel<'a> {
    markdown: &'a str,
    scroll: u16,
    renderer: &'a MarkdownRenderer,
    render_cache: &'a RefCell<RenderedMarkdownCache>,
    border_style: Style,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarkdownViewportKey {
    width: u16,
    scroll: u16,
    height: u16,
}

#[derive(Default)]
struct RenderedMarkdownCache {
    width: Option<u16>,
    rendered: Option<Text<'static>>,
    viewport_key: Option<MarkdownViewportKey>,
    viewport: Option<Text<'static>>,
}

impl RenderedMarkdownCache {
    fn text(&mut self, width: u16, renderer: &MarkdownRenderer, markdown: &str) -> &Text<'static> {
        if self.width != Some(width) {
            self.width = Some(width);
            self.rendered = None;
            self.viewport_key = None;
            self.viewport = None;
        }

        self.rendered.get_or_insert_with(|| {
            renderer
                .clone()
                .rule_width(RULE_WIDTH.min(width))
                .table_max_width(width)
                .render(markdown)
        })
    }

    fn viewport(
        &mut self,
        width: u16,
        scroll: u16,
        height: u16,
        renderer: &MarkdownRenderer,
        markdown: &str,
    ) -> &Text<'static> {
        let key = MarkdownViewportKey {
            width,
            scroll,
            height,
        };

        if self.viewport_key != Some(key) || self.viewport.is_none() {
            let wrapped = {
                let rendered = self.text(width, renderer, markdown);
                wrap_markdown_for_viewport(rendered, width, scroll, height)
            };
            self.viewport_key = Some(key);
            self.viewport = Some(wrapped);
        }

        self.viewport
            .as_ref()
            .expect("viewport cache is populated after refresh")
    }

    fn clear(&mut self) {
        self.width = None;
        self.rendered = None;
        self.viewport_key = None;
        self.viewport = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamRenderKey {
    width: u16,
    position: usize,
    complete: bool,
}

struct StreamRenderEntry {
    text: Text<'static>,
    detection: MarkdownDetection,
    viewport_key: Option<MarkdownViewportKey>,
    viewport: Option<Text<'static>>,
}

#[derive(Default)]
struct StreamRenderCache {
    key: Option<StreamRenderKey>,
    entry: Option<StreamRenderEntry>,
}

impl StreamRenderCache {
    fn viewport_and_detection(
        &mut self,
        viewport_key: MarkdownViewportKey,
        position: usize,
        complete: bool,
        renderer: &MarkdownRenderer,
        fragment: &str,
    ) -> (&Text<'static>, MarkdownDetection) {
        let key = StreamRenderKey {
            width: viewport_key.width,
            position,
            complete,
        };
        if self.key != Some(key) {
            self.key = Some(key);
            self.entry = None;
        }

        let entry = self.entry.get_or_insert_with(|| {
            let mut text = renderer
                .clone()
                .rule_width(RULE_WIDTH.min(viewport_key.width))
                .table_max_width(viewport_key.width)
                .render_streaming(fragment);

            if !complete {
                let cursor = Span::styled("▌", Style::new().fg(theme::accent::PRIMARY).blink());
                text.push_span(cursor);
            }

            StreamRenderEntry {
                text,
                detection: is_likely_markdown(fragment),
                viewport_key: None,
                viewport: None,
            }
        });

        if entry.viewport_key != Some(viewport_key) || entry.viewport.is_none() {
            entry.viewport = Some(wrap_markdown_for_viewport(
                &entry.text,
                viewport_key.width,
                viewport_key.scroll,
                viewport_key.height,
            ));
            entry.viewport_key = Some(viewport_key);
        }

        (
            entry
                .viewport
                .as_ref()
                .expect("stream viewport cache is populated after refresh"),
            entry.detection,
        )
    }

    fn clear(&mut self) {
        self.key = None;
        self.entry = None;
    }
}

fn wrap_markdown_for_panel<'a>(text: &Text<'a>, width: u16) -> Text<'a> {
    wrap_markdown_for_viewport(text, width, 0, u16::MAX)
}

fn wrap_markdown_for_viewport<'a>(
    text: &Text<'a>,
    width: u16,
    scroll: u16,
    height: u16,
) -> Text<'a> {
    let width = usize::from(width);
    let start = usize::from(scroll);
    let end = start.saturating_add(usize::from(height));
    if width == 0 || start == end {
        return Text::from_lines([]);
    }

    let mut lines = Vec::with_capacity(usize::from(height).min(text.lines().len()));
    let mut visual_line = 0usize;
    for line in text.lines() {
        if visual_line >= end {
            break;
        }

        let line_width = line.width();
        if line_width <= width {
            push_visible_wrapped_line(&mut lines, &mut visual_line, start, end, line.clone());
            continue;
        }

        let plain = line.to_plain_text();
        let table_like = is_table_line(&plain) || is_table_like_line(&plain);
        if table_like {
            for wrapped in truncate_line_to_width(line, width) {
                push_visible_wrapped_line(&mut lines, &mut visual_line, start, end, wrapped);
                if visual_line >= end {
                    break;
                }
            }
            continue;
        }

        if let Some(prefix_width) = blockquote_prefix_width(&plain) {
            for wrapped in wrap_blockquote_line(line, width, prefix_width) {
                push_visible_wrapped_line(&mut lines, &mut visual_line, start, end, wrapped);
                if visual_line >= end {
                    break;
                }
            }
            continue;
        }

        for wrapped in wrap_line_to_width(line, width) {
            push_visible_wrapped_line(&mut lines, &mut visual_line, start, end, wrapped);
            if visual_line >= end {
                break;
            }
        }
    }

    Text::from_lines(lines)
}

fn push_visible_wrapped_line<'a>(
    lines: &mut Vec<Line<'a>>,
    visual_line: &mut usize,
    start: usize,
    end: usize,
    line: Line<'a>,
) {
    if *visual_line >= start && *visual_line < end {
        lines.push(line);
    }
    *visual_line = (*visual_line).saturating_add(1);
}

fn is_table_line(plain: &str) -> bool {
    let trimmed = plain.trim();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed
        .chars()
        .any(|c| matches!(c, '┌' | '┬' | '┐' | '├' | '┼' | '┤' | '└' | '┴' | '┘' | '─'))
    {
        return true;
    }

    trimmed.starts_with('│')
        && trimmed.ends_with('│')
        && trimmed.chars().filter(|&c| c == '│').count() >= 2
}

fn is_table_like_line(plain: &str) -> bool {
    let trimmed = plain.trim_start();
    if !trimmed.starts_with('|') {
        return false;
    }
    trimmed.chars().filter(|&c| c == '|').count() >= 2
}

fn wrap_line_to_width<'a>(line: &Line<'a>, width: usize) -> Vec<Line<'a>> {
    let mut wrapped_lines = Vec::new();
    for wrapped in line.wrap(width, WrapMode::Word) {
        if wrapped.width() <= width {
            wrapped_lines.push(wrapped);
        } else {
            wrapped_lines.extend(truncate_line_to_width(&wrapped, width));
        }
    }
    wrapped_lines
}

fn truncate_line_to_width<'a>(line: &Line<'a>, width: usize) -> Vec<Line<'a>> {
    let mut remaining = width;
    let mut spans = Vec::with_capacity(line.len());

    for span in line.spans() {
        if remaining == 0 {
            break;
        }

        let span_width = span.width();
        if span_width <= remaining {
            spans.push(span.clone());
            remaining -= span_width;
            continue;
        }

        let (head, _tail) = span.split_at_cell(remaining);
        if !head.is_empty() {
            spans.push(head);
        }
        break;
    }

    vec![Line::from_spans(spans)]
}

fn blockquote_prefix_width(plain: &str) -> Option<usize> {
    let mut prefix_bytes = 0usize;
    loop {
        let rest = &plain[prefix_bytes..];
        if rest.starts_with("│ ") || rest.starts_with("┃ ") {
            // Prefix markers are one box-drawing character + one trailing space.
            prefix_bytes += 4;
        } else {
            break;
        }
    }

    if prefix_bytes == 0 {
        return None;
    }

    Some(ftui_text::display_width(&plain[..prefix_bytes]))
}

fn wrap_blockquote_line<'a>(line: &Line<'a>, width: usize, prefix_width: usize) -> Vec<Line<'a>> {
    if prefix_width == 0 || prefix_width >= width {
        return truncate_line_to_width(line, width);
    }

    let (prefix, content) = split_line_at_cell(line, prefix_width);
    let content_width = width.saturating_sub(prefix_width).max(1);
    let wrapped_content = wrap_line_to_width(&content, content_width);

    wrapped_content
        .into_iter()
        .map(|body_line| {
            let mut merged = prefix.clone();
            for span in body_line.spans() {
                merged.push_span(span.clone());
            }
            merged
        })
        .collect()
}

fn split_line_at_cell<'a>(line: &Line<'a>, cell_pos: usize) -> (Line<'a>, Line<'a>) {
    if cell_pos == 0 {
        return (Line::new(), line.clone());
    }

    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut remaining = cell_pos;

    for span in line.spans() {
        if remaining == 0 {
            right.push(span.clone());
            continue;
        }

        let span_width = span.width();
        if span_width <= remaining {
            left.push(span.clone());
            remaining -= span_width;
            continue;
        }

        let (head, tail) = span.split_at_cell(remaining);
        if !head.is_empty() {
            left.push(head);
        }
        if !tail.is_empty() {
            right.push(tail);
        }
        remaining = 0;
    }

    (Line::from_spans(left), Line::from_spans(right))
}

impl Widget for MarkdownPanel<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Markdown Renderer")
            .title_alignment(Alignment::Center)
            .style(self.border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let max_width = inner.width.saturating_sub(1).max(1);
        {
            let mut cache = self.render_cache.borrow_mut();
            let wrapped = cache.viewport(
                max_width,
                self.scroll,
                inner.height,
                self.renderer,
                self.markdown,
            );
            render_cached_markdown_text(frame, inner, wrapped);
        }
    }
}

fn render_cached_markdown_text(frame: &mut Frame, area: Rect, text: &Text<'_>) {
    if area.is_empty() {
        return;
    }

    let degradation = frame.buffer.degradation;
    if !degradation.render_content() {
        clear_markdown_text_area(frame, area, Style::default());
        return;
    }

    clear_markdown_text_area(frame, area, Style::default());

    let mut y = area.y;
    for line in text.lines() {
        if y >= area.bottom() {
            break;
        }

        let mut x = area.x;
        for span in line.spans() {
            if x >= area.right() {
                break;
            }

            let span_style = if degradation.apply_styling() {
                span.style.unwrap_or_default()
            } else {
                Style::default()
            };
            x = draw_markdown_span(
                frame,
                x,
                y,
                span.content.as_ref(),
                span_style,
                area.right(),
                span.link.as_deref(),
            );
        }

        y = y.saturating_add(1);
    }
}

fn render_markdown_line_segments(frame: &mut Frame, area: Rect, segments: &[(&str, Style)]) {
    if area.is_empty() {
        return;
    }

    let degradation = frame.buffer.degradation;
    if !degradation.render_content() {
        clear_markdown_text_area(frame, area, Style::default());
        return;
    }

    clear_markdown_text_area(frame, area, Style::default());

    let mut x = area.x;
    for (content, style) in segments {
        if x >= area.right() {
            break;
        }

        let span_style = if degradation.apply_styling() {
            *style
        } else {
            Style::default()
        };
        x = draw_markdown_span(frame, x, area.y, content, span_style, area.right(), None);
    }
}

fn draw_markdown_repeated_char(
    frame: &mut Frame,
    mut x: u16,
    y: u16,
    ch: char,
    count: usize,
    style: Style,
    max_x: u16,
) -> u16 {
    let span_style = if frame.buffer.degradation.apply_styling() {
        style
    } else {
        Style::default()
    };

    for _ in 0..count {
        if x >= max_x {
            break;
        }

        let mut cell = inherited_markdown_text_cell(frame, x, y, CellContent::from_char(ch));
        apply_markdown_style(&mut cell, span_style);
        frame.buffer.set_fast(x, y, cell);
        x = x.saturating_add(1);
    }

    x
}

fn render_stream_progress_bar(frame: &mut Frame, area: Rect, progress: f64) {
    if area.is_empty() {
        return;
    }

    let degradation = frame.buffer.degradation;
    if !degradation.render_content() {
        clear_markdown_text_area(frame, area, Style::default());
        return;
    }

    clear_markdown_text_area(frame, area, Style::default());

    let progress = if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let bar_width = area.width.saturating_sub(4) as usize;
    let filled = (progress * bar_width as f64) as usize;
    let empty = bar_width.saturating_sub(filled);
    let max_x = area.right();
    let y = area.y;

    let mut x = draw_markdown_span(frame, area.x, y, "  ", Style::new(), max_x, None);
    x = draw_markdown_span(frame, x, y, "[", theme::muted(), max_x, None);
    x = draw_markdown_repeated_char(
        frame,
        x,
        y,
        '█',
        filled,
        Style::new().fg(theme::accent::SUCCESS),
        max_x,
    );
    x = draw_markdown_repeated_char(
        frame,
        x,
        y,
        '░',
        empty,
        Style::new().fg(theme::fg::MUTED).dim(),
        max_x,
    );
    draw_markdown_span(frame, x, y, "]", theme::muted(), max_x, None);
}

fn render_stream_detection_lines(
    frame: &mut Frame,
    area: Rect,
    detection: MarkdownDetection,
    stream_position: usize,
) {
    if area.is_empty() {
        return;
    }

    let degradation = frame.buffer.degradation;
    if !degradation.render_content() {
        clear_markdown_text_area(frame, area, Style::default());
        return;
    }

    clear_markdown_text_area(frame, area, Style::default());

    let detection_status = if detection.is_confident() {
        "Confident"
    } else if detection.is_likely() {
        "Likely"
    } else {
        "Uncertain"
    };

    if area.height >= 1 {
        let det_line1 = format!(
            "Detection: {} indicators | {}",
            detection.indicators, detection_status
        );
        render_markdown_line_segments(
            frame,
            Rect::new(area.x, area.y, area.width, 1),
            &[("  ", Style::new()), (det_line1.as_str(), theme::muted())],
        );
    }

    if area.height >= 2 {
        let det_line2 = format!(
            "Confidence: {:.0}% | Chars: {}/{}",
            detection.confidence() * 100.0,
            stream_position,
            STREAMING_MARKDOWN.len()
        );
        render_markdown_line_segments(
            frame,
            Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
            &[("  ", Style::new()), (det_line2.as_str(), theme::muted())],
        );
    }

    if area.height >= 3 {
        render_markdown_line_segments(
            frame,
            Rect::new(area.x, area.y.saturating_add(2), area.width, 1),
            &[
                ("  ", Style::new()),
                (
                    "Space: play/pause | r: restart | f: turbo | ↑↓: scroll stream",
                    Style::new().fg(theme::accent::INFO).dim(),
                ),
            ],
        );
    }
}

fn clear_markdown_text_area(frame: &mut Frame, area: Rect, style: Style) {
    if area.is_empty() {
        return;
    }

    let mut cell = RenderCell::from_char(' ');
    apply_markdown_style(&mut cell, style);
    frame.buffer.fill(area, cell);
}

fn draw_markdown_span(
    frame: &mut Frame,
    mut x: u16,
    y: u16,
    content: &str,
    style: Style,
    max_x: u16,
    link_url: Option<&str>,
) -> u16 {
    let link_id = link_url.map_or(0, |url| frame.register_link(url));

    for grapheme in graphemes(content) {
        if x >= max_x {
            break;
        }
        let width = grapheme_width(grapheme);
        if width == 0 {
            continue;
        }
        if x.saturating_add(width as u16) > max_x {
            break;
        }

        let content = if width > 1 || grapheme.chars().count() > 1 {
            let id = frame.intern_with_width(grapheme, width as u8);
            CellContent::from_grapheme(id)
        } else if let Some(ch) = grapheme.chars().next() {
            CellContent::from_char(ch)
        } else {
            continue;
        };

        let mut cell = inherited_markdown_text_cell(frame, x, y, content);
        apply_markdown_style(&mut cell, style);
        if link_id != 0 {
            cell.attrs = cell.attrs.with_link(link_id);
        }
        frame.buffer.set_fast(x, y, cell);

        x = x.saturating_add(width as u16);
    }

    x
}

fn inherited_markdown_text_cell(frame: &Frame, x: u16, y: u16, content: CellContent) -> RenderCell {
    let mut cell = frame.buffer.get(x, y).copied().unwrap_or_default();
    cell.content = content;
    cell.attrs = CellAttrs::new(cell.attrs.flags(), 0);
    cell
}

fn apply_markdown_style(cell: &mut RenderCell, style: Style) {
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg {
        match bg.a() {
            0 => {}
            255 => cell.bg = bg,
            _ => cell.bg = bg.over(cell.bg),
        }
    }
    if let Some(attrs) = style.attrs {
        cell.attrs = cell.attrs.merged_flags(StyleFlags::from(attrs));
    }
}

pub struct MarkdownRichText {
    md_scroll: u16,
    wrap_index: usize,
    align_index: usize,
    // Streaming simulation state
    stream_position: usize,
    stream_paused: bool,
    stream_turbo: bool,
    stream_scroll: u16,
    markdown_renderer: MarkdownRenderer,
    stream_renderer: MarkdownRenderer,
    rendered_markdown_cache: RefCell<RenderedMarkdownCache>,
    stream_render_cache: RefCell<StreamRenderCache>,
    style_sampler_cache: RefCell<Option<Text<'static>>>,
    tick_count: u64,
    markdown_backdrop: RefCell<Backdrop>,
    focus: FocusPanel,
    layout_markdown: Cell<Rect>,
    layout_stream: Cell<Rect>,
}

impl Default for MarkdownRichText {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPanel {
    Markdown,
    Stream,
}

impl MarkdownRichText {
    pub fn new() -> Self {
        let markdown_renderer = Self::build_renderer(Self::build_theme());
        let stream_renderer = Self::build_renderer(Self::build_theme());
        let theme_inputs = Self::current_fx_theme();
        let mut markdown_backdrop =
            Backdrop::new(Box::new(PlasmaFx::new(PlasmaPalette::Ocean)), theme_inputs);
        markdown_backdrop.set_effect_opacity(0.25);
        markdown_backdrop.set_scrim(Scrim::uniform(0.7));

        Self {
            md_scroll: 0,
            wrap_index: 1, // Start at Word
            align_index: 0,
            // Streaming starts active
            stream_position: 0,
            stream_paused: false,
            stream_turbo: false,
            stream_scroll: 0,
            markdown_renderer,
            stream_renderer,
            rendered_markdown_cache: RefCell::default(),
            stream_render_cache: RefCell::default(),
            style_sampler_cache: RefCell::default(),
            tick_count: 0,
            markdown_backdrop: RefCell::new(markdown_backdrop),
            focus: FocusPanel::Markdown,
            layout_markdown: Cell::new(Rect::default()),
            layout_stream: Cell::new(Rect::default()),
        }
    }

    pub fn apply_theme(&mut self) {
        self.markdown_renderer = Self::build_renderer(Self::build_theme());
        self.stream_renderer = Self::build_renderer(Self::build_theme());
        self.rendered_markdown_cache.borrow_mut().clear();
        self.stream_render_cache.borrow_mut().clear();
        *self.style_sampler_cache.borrow_mut() = None;
        let theme_inputs = Self::current_fx_theme();
        self.markdown_backdrop.borrow_mut().set_theme(theme_inputs);
    }

    fn build_renderer(theme: MarkdownTheme) -> MarkdownRenderer {
        let mut syntax_highlighter = SyntaxHighlighter::new();
        syntax_highlighter.set_theme(theme::syntax_theme());
        MarkdownRenderer::new(theme).with_syntax_highlighter(Arc::new(syntax_highlighter))
    }

    fn build_theme() -> MarkdownTheme {
        MarkdownTheme {
            h1: Style::new().fg(theme::fg::PRIMARY).bold(),
            h2: Style::new().fg(theme::accent::PRIMARY).bold(),
            h3: Style::new().fg(theme::accent::SECONDARY).bold(),
            h4: Style::new().fg(theme::accent::INFO).bold(),
            h5: Style::new().fg(theme::accent::SUCCESS).bold(),
            h6: Style::new().fg(theme::fg::SECONDARY).bold(),
            code_inline: Style::new()
                .fg(theme::accent::WARNING)
                .bg(theme::alpha::SURFACE),
            code_block: Style::new()
                .fg(theme::fg::SECONDARY)
                .bg(theme::alpha::SURFACE),
            blockquote: Style::new().fg(theme::fg::MUTED).italic(),
            link: Style::new().fg(theme::accent::LINK).underline(),
            emphasis: Style::new().italic(),
            strong: Style::new().bold(),
            strikethrough: Style::new().strikethrough(),
            list_bullet: Style::new().fg(theme::accent::PRIMARY),
            horizontal_rule: Style::new().fg(theme::fg::MUTED).dim(),
            table_theme: theme::table_theme_demo(),
            // GFM extensions - use themed colors
            task_done: Style::new().fg(theme::accent::SUCCESS),
            task_todo: Style::new().fg(theme::accent::INFO),
            math_inline: Style::new().fg(theme::accent::SECONDARY).italic(),
            math_block: Style::new().fg(theme::accent::SECONDARY).bold(),
            footnote_ref: Style::new().fg(theme::fg::MUTED).dim(),
            footnote_def: Style::new().fg(theme::fg::SECONDARY),
            admonition_note: Style::new().fg(theme::accent::INFO).bold(),
            admonition_tip: Style::new().fg(theme::accent::SUCCESS).bold(),
            admonition_important: Style::new().fg(theme::accent::SECONDARY).bold(),
            admonition_warning: Style::new().fg(theme::accent::WARNING).bold(),
            admonition_caution: Style::new().fg(theme::accent::ERROR).bold(),
        }
    }

    fn current_fx_theme() -> ThemeInputs {
        ThemeInputs::from(theme::palette(theme::current_theme()))
    }

    fn build_style_sampler_text() -> Text<'static> {
        Text::from_lines([
            Line::from_spans([
                Span::styled("Bold", theme::bold()),
                Span::raw("  "),
                Span::styled("Dim", theme::dim()),
                Span::raw("  "),
                Span::styled("Italic", theme::italic()),
                Span::raw("  "),
                Span::styled("Underline", theme::underline()),
            ]),
            Line::from_spans([
                Span::styled("Strikethrough", theme::strikethrough()),
                Span::raw("  "),
                Span::styled("Reverse", theme::reverse()),
                Span::raw("  "),
                Span::styled("Blink", theme::blink_style()),
            ]),
            Line::from_spans([
                Span::styled("Dbl-Underline", theme::double_underline()),
                Span::raw("  "),
                Span::styled("Curly-Underline", theme::curly_underline()),
                Span::raw("  "),
                Span::styled("[Hidden]", theme::hidden()),
            ]),
            Line::new(),
            Line::from_spans([
                Span::styled("Error", theme::error_style()),
                Span::raw("  "),
                Span::styled("Success", theme::success()),
                Span::raw("  "),
                Span::styled("Warning", theme::warning()),
                Span::raw("  "),
                Span::styled("Link", theme::link()),
                Span::raw("  "),
                Span::styled("Code", theme::code()),
            ]),
        ])
    }

    /// Advance the streaming simulation by one tick.
    ///
    /// Uses variable typing speed: faster for whitespace, slower for headings.
    fn tick_stream(&mut self) {
        if self.stream_paused {
            return;
        }
        let max_len = STREAMING_MARKDOWN.len();
        if self.stream_position < max_len {
            // Calculate variable speed based on content
            let mut speed = self.calculate_typing_speed();
            if self.stream_turbo {
                speed = speed.saturating_mul(STREAM_TURBO_MULTIPLIER);
            }

            // Advance by calculated characters, ensuring we land on a char boundary
            let mut new_pos = self.stream_position.saturating_add(speed);
            while new_pos < max_len && !STREAMING_MARKDOWN.is_char_boundary(new_pos) {
                new_pos += 1;
            }
            self.stream_position = new_pos.min(max_len);
        }
    }

    /// Calculate typing speed based on upcoming content.
    ///
    /// - Fast (5-6 chars): whitespace, simple punctuation
    /// - Medium (3 chars): regular text
    /// - Slow (1-2 chars): headings, code blocks, new sections
    fn calculate_typing_speed(&self) -> usize {
        let remaining = &STREAMING_MARKDOWN[self.stream_position..];
        if remaining.is_empty() {
            return STREAM_CHARS_PER_TICK * STREAM_SPEED_MULTIPLIER;
        }

        // Check what's coming up
        let first_char = remaining.chars().next().unwrap_or(' ');

        // Fast: whitespace sequences
        if first_char.is_whitespace() {
            // Count consecutive whitespace for burst typing
            let ws_count = remaining.chars().take_while(|c| c.is_whitespace()).count();
            return ws_count.clamp(1, 6) * STREAM_SPEED_MULTIPLIER;
        }

        // Check if we're at the start of a line
        let at_line_start = self.stream_position == 0
            || STREAMING_MARKDOWN.get(self.stream_position.saturating_sub(1)..self.stream_position)
                == Some("\n");

        if at_line_start {
            // Slow: headings (lines starting with #)
            if remaining.starts_with('#') {
                return STREAM_SPEED_MULTIPLIER;
            }
            // Slow: code blocks
            if remaining.starts_with("```") {
                return 2 * STREAM_SPEED_MULTIPLIER;
            }
            // Slow: list items and blockquotes
            if remaining.starts_with('-')
                || remaining.starts_with('>')
                || remaining.starts_with('|')
            {
                return 2 * STREAM_SPEED_MULTIPLIER;
            }
        }

        // Medium: regular text
        STREAM_CHARS_PER_TICK * STREAM_SPEED_MULTIPLIER
    }

    /// Get the current streaming fragment.
    fn current_stream_fragment(&self) -> &str {
        let end = self.stream_position.min(STREAMING_MARKDOWN.len());
        &STREAMING_MARKDOWN[..end]
    }

    /// Check if streaming is complete.
    fn stream_complete(&self) -> bool {
        self.stream_position >= STREAMING_MARKDOWN.len()
    }

    fn current_wrap(&self) -> WrapMode {
        WRAP_MODES[self.wrap_index]
    }

    fn current_alignment(&self) -> Alignment {
        ALIGNMENTS[self.align_index]
    }

    fn wrap_label(&self) -> &'static str {
        match self.current_wrap() {
            WrapMode::None => "None",
            WrapMode::Word => "Word",
            WrapMode::Char => "Char",
            WrapMode::WordChar => "WordChar",
            WrapMode::Optimal => "Optimal",
        }
    }

    fn alignment_label(&self) -> &'static str {
        match self.current_alignment() {
            Alignment::Left => "Left",
            Alignment::Center => "Center",
            Alignment::Right => "Right",
        }
    }

    // ---- Render panels ----

    fn render_markdown_panel(&self, frame: &mut Frame, area: Rect) {
        let panel = MarkdownPanel {
            markdown: SAMPLE_MARKDOWN,
            scroll: self.md_scroll,
            renderer: &self.markdown_renderer,
            render_cache: &self.rendered_markdown_cache,
            border_style: theme::panel_border_style(
                self.focus == FocusPanel::Markdown,
                theme::screen_accent::MARKDOWN,
            ),
        };

        // Quality is now derived automatically from frame.buffer.degradation
        // with area-based clamping inside Backdrop::render().
        let time_seconds = self.tick_count as f64 * 0.1;
        let theme_inputs = Self::current_fx_theme();

        let mut backdrop = self.markdown_backdrop.borrow_mut();
        backdrop.set_theme(theme_inputs);
        backdrop.set_time(self.tick_count, time_seconds);
        backdrop.render_with(area, frame, &panel);
    }

    fn render_style_sampler(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Style Sampler")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::MARKDOWN));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut cache = self.style_sampler_cache.borrow_mut();
        let styles_text = cache.get_or_insert_with(Self::build_style_sampler_text);
        render_cached_markdown_text(frame, inner, styles_text);
    }

    fn render_unicode_table(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Unicode Showcase")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::MARKDOWN));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let header =
            Row::new(["Text", "Type", "Cells"]).style(Style::new().fg(theme::fg::PRIMARY).bold());

        let rows = [
            Row::new(["Hello", "ASCII", "5"]),
            Row::new(["\u{4f60}\u{597d}\u{4e16}\u{754c}", "CJK", "8"]),
            Row::new(["\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}", "Hiragana", "10"]),
            Row::new(["\u{1f980}\u{1f525}\u{2728}", "Emoji", "6"]),
            Row::new(["caf\u{e9}", "Latin+accent", "4"]),
            Row::new(["\u{03b1} \u{03b2} \u{03b3} \u{03b4}", "Greek", "7"]),
            Row::new(["\u{2192} \u{2190} \u{2191} \u{2193}", "Arrows", "7"]),
            Row::new(["\u{2588}\u{2593}\u{2592}\u{2591}", "Block el.", "4"]),
        ];

        let widths = [
            Constraint::Min(12),
            Constraint::Min(12),
            Constraint::Fixed(6),
        ];

        Table::new(rows, widths)
            .header(header)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .theme(theme::table_theme_demo())
            .theme_phase(theme::table_theme_phase(self.tick_count))
            .column_spacing(theme::spacing::XS)
            .render(inner, frame);
    }

    fn render_wrap_demo(&self, frame: &mut Frame, area: Rect) {
        let title = format!(
            "Wrap: {} | Align: {}",
            self.wrap_label(),
            self.alignment_label()
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title.as_str())
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Stream,
                theme::screen_accent::MARKDOWN,
            ));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let controls = Rect::new(inner.x, inner.y, inner.width, 1);
        render_markdown_line_segments(
            frame,
            controls,
            &[("w: cycle wrap | a: cycle alignment", theme::muted())],
        );

        let demo_text = "The quick brown fox jumps over the lazy dog. \
             Supercalifragilisticexpialidocious is quite a long word \
             that tests character-level wrapping behavior. \
             \u{4f60}\u{597d}\u{4e16}\u{754c} contains CJK characters \
             that are double-width. \u{1f980} Ferris says hello!";

        let body = Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        );
        if body.is_empty() {
            return;
        }

        Paragraph::new(demo_text)
            .wrap(self.current_wrap())
            .alignment(self.current_alignment())
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(body, frame);
    }

    fn render_streaming_panel(&self, frame: &mut Frame, area: Rect) {
        // Build title with streaming status
        let progress_pct =
            (self.stream_position as f64 / STREAMING_MARKDOWN.len() as f64 * 100.0) as u8;
        let title: Cow<'static, str> = if self.stream_complete() {
            Cow::Borrowed("LLM Streaming Simulation | Complete")
        } else if self.stream_paused {
            Cow::Owned(format!(
                "LLM Streaming Simulation | Paused ({progress_pct}%)"
            ))
        } else if self.stream_turbo {
            Cow::Owned(format!(
                "LLM Streaming Simulation | Turbo... {progress_pct}%"
            ))
        } else {
            Cow::Owned(format!(
                "LLM Streaming Simulation | Streaming... {progress_pct}%"
            ))
        };

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title.as_ref())
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::MARKDOWN));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        // Split into content area, progress bar, and detection info
        let chunks = Flex::vertical()
            .constraints([
                Constraint::Min(5),
                Constraint::Fixed(1),
                Constraint::Fixed(3),
            ])
            .split(inner);

        // Render the streaming markdown fragment
        let fragment = self.current_stream_fragment();
        let stream_complete = self.stream_complete();
        let detection = {
            let mut cache = self.stream_render_cache.borrow_mut();
            let (stream_text, detection) = cache.viewport_and_detection(
                MarkdownViewportKey {
                    width: chunks[0].width,
                    scroll: self.stream_scroll,
                    height: chunks[0].height,
                },
                self.stream_position,
                stream_complete,
                &self.stream_renderer,
                fragment,
            );
            render_cached_markdown_text(frame, chunks[0], stream_text);
            detection
        };

        // Render mini progress bar
        let progress = self.stream_position as f64 / STREAMING_MARKDOWN.len() as f64;
        render_stream_progress_bar(frame, chunks[1], progress);

        // Detection status panel
        render_stream_detection_lines(frame, chunks[2], detection, self.stream_position);
    }
}

impl Screen for MarkdownRichText {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            match mouse.kind {
                ftui_core::event::MouseEventKind::Down(ftui_core::event::MouseButton::Left) => {
                    let markdown = self.layout_markdown.get();
                    let stream = self.layout_stream.get();
                    if markdown.contains(mouse.x, mouse.y) {
                        self.focus = FocusPanel::Markdown;
                    } else if stream.contains(mouse.x, mouse.y) {
                        self.focus = FocusPanel::Stream;
                    }
                }
                ftui_core::event::MouseEventKind::ScrollUp => {
                    let markdown = self.layout_markdown.get();
                    let stream = self.layout_stream.get();
                    if stream.contains(mouse.x, mouse.y) {
                        self.stream_scroll = self.stream_scroll.saturating_sub(1);
                    } else if markdown.contains(mouse.x, mouse.y) {
                        self.md_scroll = self.md_scroll.saturating_sub(1);
                    }
                }
                ftui_core::event::MouseEventKind::ScrollDown => {
                    let markdown = self.layout_markdown.get();
                    let stream = self.layout_stream.get();
                    if stream.contains(mouse.x, mouse.y) {
                        self.stream_scroll = self.stream_scroll.saturating_add(1);
                    } else if markdown.contains(mouse.x, mouse.y) {
                        self.md_scroll = self.md_scroll.saturating_add(1);
                    }
                }
                _ => {}
            }
            return Cmd::None;
        }

        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                // Markdown panel scrolling
                KeyCode::Up => match self.focus {
                    FocusPanel::Markdown => {
                        self.md_scroll = self.md_scroll.saturating_sub(1);
                    }
                    FocusPanel::Stream => {
                        self.stream_scroll = self.stream_scroll.saturating_sub(1);
                    }
                },
                KeyCode::Down => match self.focus {
                    FocusPanel::Markdown => {
                        self.md_scroll = self.md_scroll.saturating_add(1);
                    }
                    FocusPanel::Stream => {
                        self.stream_scroll = self.stream_scroll.saturating_add(1);
                    }
                },
                KeyCode::PageUp => match self.focus {
                    FocusPanel::Markdown => {
                        self.md_scroll = self.md_scroll.saturating_sub(10);
                    }
                    FocusPanel::Stream => {
                        self.stream_scroll = self.stream_scroll.saturating_sub(10);
                    }
                },
                KeyCode::PageDown => match self.focus {
                    FocusPanel::Markdown => {
                        self.md_scroll = self.md_scroll.saturating_add(10);
                    }
                    FocusPanel::Stream => {
                        self.stream_scroll = self.stream_scroll.saturating_add(10);
                    }
                },
                KeyCode::Home => match self.focus {
                    FocusPanel::Markdown => {
                        self.md_scroll = 0;
                    }
                    FocusPanel::Stream => {
                        self.stream_scroll = 0;
                    }
                },
                // Wrap/alignment controls
                KeyCode::Char('w') => {
                    self.wrap_index = (self.wrap_index + 1) % WRAP_MODES.len();
                }
                KeyCode::Char('a') => {
                    self.align_index = (self.align_index + 1) % ALIGNMENTS.len();
                }
                // Streaming controls
                KeyCode::Char(' ') => {
                    self.stream_paused = !self.stream_paused;
                }
                KeyCode::Char('f') => {
                    self.stream_turbo = !self.stream_turbo;
                }
                KeyCode::Char('r') => {
                    // Reset streaming
                    self.stream_position = 0;
                    self.stream_paused = false;
                    self.stream_scroll = 0;
                    self.stream_render_cache.borrow_mut().clear();
                }
                KeyCode::Char('[') => {
                    // Scroll stream panel up
                    self.stream_scroll = self.stream_scroll.saturating_sub(1);
                }
                KeyCode::Char(']') => {
                    // Scroll stream panel down
                    self.stream_scroll = self.stream_scroll.saturating_add(1);
                }
                _ => {}
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Clear the full area to avoid stale borders bleeding through gaps.
        Paragraph::new("")
            .style(Style::new().bg(theme::alpha::SURFACE))
            .render(area, frame);

        // Main layout: three columns - left markdown, center streaming, right panels
        let cols = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([
                Constraint::Percentage(35.0),
                Constraint::Percentage(35.0),
                Constraint::Fill,
            ])
            .split(area);

        // Left: Full GFM markdown demo
        self.layout_markdown.set(cols[0]);
        self.render_markdown_panel(frame, cols[0]);

        // Center: Streaming simulation
        self.layout_stream.set(cols[1]);
        self.render_streaming_panel(frame, cols[1]);

        // Right: Auxiliary panels
        let right_rows = Flex::vertical()
            .gap(theme::spacing::XS)
            .constraints([
                Constraint::Fixed(8),
                Constraint::Fixed(10), // Unicode table
                Constraint::Min(6),
            ])
            .split(cols[2]);

        self.render_style_sampler(frame, right_rows[0]);
        self.render_unicode_table(frame, right_rows[1]);
        self.render_wrap_demo(frame, right_rows[2]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "\u{2191}/\u{2193}",
                action: "Scroll focused panel",
            },
            HelpEntry {
                key: "[/]",
                action: "Scroll stream",
            },
            HelpEntry {
                key: "Mouse",
                action: "Focus panes + scroll",
            },
            HelpEntry {
                key: "Space",
                action: "Play/pause stream",
            },
            HelpEntry {
                key: "r",
                action: "Restart stream",
            },
            HelpEntry {
                key: "f",
                action: "Toggle turbo",
            },
            HelpEntry {
                key: "w/a",
                action: "Wrap/align mode",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Markdown and Rich Text"
    }

    fn tab_label(&self) -> &'static str {
        "Markdown"
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        // Advance streaming simulation on each tick
        self.tick_stream();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::budget::DegradationLevel;
    use ftui_render::grapheme_pool::GraphemePool;
    use ftui_render::link_registry::LinkRegistry;

    fn rendered_sample() -> Text<'static> {
        MarkdownRenderer::new(MarkdownTheme::default())
            .rule_width(RULE_WIDTH)
            .render(SAMPLE_MARKDOWN)
    }

    fn frame_row_text(frame: &Frame<'_>, y: u16, width: u16) -> String {
        (0..width)
            .map(|x| {
                frame
                    .buffer
                    .get(x, y)
                    .and_then(|cell| cell.content.as_char())
                    .unwrap_or(' ')
            })
            .collect()
    }

    fn assert_cached_markdown_text_matches_paragraph(
        text: Text<'static>,
        degradation: DegradationLevel,
    ) {
        let area = Rect::new(1, 1, 12, 2);
        let mut direct_pool = GraphemePool::new();
        let mut direct_links = LinkRegistry::new();
        let mut direct = Frame::new(14, 4, &mut direct_pool);
        direct.set_links(&mut direct_links);
        direct.set_degradation(degradation);

        let mut paragraph_pool = GraphemePool::new();
        let mut paragraph_links = LinkRegistry::new();
        let mut paragraph = Frame::new(14, 4, &mut paragraph_pool);
        paragraph.set_links(&mut paragraph_links);
        paragraph.set_degradation(degradation);

        render_cached_markdown_text(&mut direct, area, &text);
        Paragraph::from_static_text(text)
            .wrap(WrapMode::None)
            .render(area, &mut paragraph);

        assert_eq!(direct.buffer, paragraph.buffer);
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: ftui_core::event::Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn initial_state() {
        let screen = MarkdownRichText::new();
        assert_eq!(screen.md_scroll, 0);
        assert_eq!(screen.title(), "Markdown and Rich Text");
        assert_eq!(screen.tab_label(), "Markdown");
    }

    #[test]
    fn stream_progress_bar_renders_direct_progress_row() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(12, 1, &mut pool);

        render_stream_progress_bar(&mut frame, Rect::new(0, 0, 12, 1), 0.5);

        assert_eq!(frame_row_text(&frame, 0, 12), "  [████░░░░]");
    }

    #[test]
    fn stream_detection_lines_render_direct_rows() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(72, 3, &mut pool);
        let detection = is_likely_markdown("```rust\ncode\n```");

        render_stream_detection_lines(&mut frame, Rect::new(0, 0, 72, 3), detection, 123);

        assert!(frame_row_text(&frame, 0, 72).contains("Detection: 4 indicators | Confident"));
        assert!(frame_row_text(&frame, 1, 72).contains("Confidence: 67% | Chars: 123/"));
        assert!(frame_row_text(&frame, 2, 72).contains("Space: play/pause"));
    }

    #[test]
    fn wrap_demo_controls_render_fixed_direct_row() {
        let screen = MarkdownRichText::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(50, 8, &mut pool);

        screen.render_wrap_demo(&mut frame, Rect::new(0, 0, 50, 8));

        assert!(frame_row_text(&frame, 1, 50).contains("w: cycle wrap | a: cycle alignment"));
        assert!(frame_row_text(&frame, 2, 50).contains("The quick brown fox"));
    }

    #[test]
    fn markdown_renders_headings() {
        let rendered = rendered_sample();
        let plain: String = rendered
            .lines()
            .iter()
            .map(|l: &Line<'_>| l.to_plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plain.contains("GitHub-Flavored Markdown (Rich Demo)"));
        assert!(plain.contains("LaTeX + Symbols"));
        assert!(plain.contains("Task Lists + Links"));
    }

    #[test]
    fn markdown_renders_code_block() {
        let rendered = rendered_sample();
        let plain: String = rendered
            .lines()
            .iter()
            .map(|l: &Line<'_>| l.to_plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plain.contains("pub enum Strategy"));
        assert!(plain.contains("class Span"));
    }

    #[test]
    fn cached_markdown_text_renderer_matches_paragraph_full_styling() {
        let text = Text::from_lines([
            Line::from_spans([
                Span::styled("Bold ", Style::new().bold().fg(theme::accent::PRIMARY)),
                Span::raw("wide \u{1f980}"),
            ]),
            Line::from_spans([
                Span::raw("link ").link("https://example.com"),
                Span::styled("tail", Style::new().underline()),
            ]),
        ]);

        assert_cached_markdown_text_matches_paragraph(text, DegradationLevel::Full);
    }

    #[test]
    fn cached_markdown_text_renderer_matches_paragraph_no_styling() {
        let text = Text::from_lines([
            Line::from_spans([
                Span::styled("Muted", Style::new().fg(theme::fg::MUTED).italic()),
                Span::raw(" plain"),
            ]),
            Line::from_spans([Span::styled(
                "Alert",
                Style::new().fg(theme::accent::ERROR).bold(),
            )]),
        ]);

        assert_cached_markdown_text_matches_paragraph(text, DegradationLevel::NoStyling);
    }

    #[test]
    fn cached_style_sampler_renderer_matches_paragraph() {
        assert_cached_markdown_text_matches_paragraph(
            MarkdownRichText::build_style_sampler_text(),
            DegradationLevel::Full,
        );
    }

    #[test]
    fn markdown_renders_task_lists() {
        let rendered = rendered_sample();
        let plain: String = rendered
            .lines()
            .iter()
            .map(|l: &Line<'_>| l.to_plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        // Task list items should have checkbox markers
        assert!(plain.contains("Inline mode + scrollback"));
        assert!(plain.contains("Conformal frame-time predictor"));
    }

    #[test]
    fn truncate_line_to_width_preserves_styled_prefix() {
        let line = Line::from_spans([
            Span::styled("abcd", Style::new().bold()),
            Span::styled("efgh", Style::new().fg(theme::accent::PRIMARY)),
        ]);

        let truncated = truncate_line_to_width(&line, 6);

        assert_eq!(truncated.len(), 1);
        assert_eq!(truncated[0].to_plain_text(), "abcdef");
        assert!(truncated[0].width() <= 6);
        assert_eq!(truncated[0].spans()[0].style, Some(Style::new().bold()));
        assert_eq!(
            truncated[0].spans()[1].style,
            Some(Style::new().fg(theme::accent::PRIMARY))
        );
    }

    #[test]
    fn viewport_wrapping_matches_full_wrapping_visible_slice() {
        let text = Text::from_lines([
            Line::raw("short"),
            Line::raw("alpha beta gamma delta"),
            Line::raw("| table | row |"),
            Line::raw("tail"),
        ]);

        let full = wrap_markdown_for_panel(&text, 10);
        let viewport = wrap_markdown_for_viewport(&text, 10, 1, 3);
        let expected = Text::from_lines(full.lines()[1..4].iter().cloned());

        assert_eq!(viewport, expected);
    }

    #[test]
    fn markdown_viewport_cache_matches_direct_wrap() {
        let renderer = MarkdownRenderer::new(MarkdownTheme::default());
        let mut cache = RenderedMarkdownCache::default();
        let key = MarkdownViewportKey {
            width: 12,
            scroll: 1,
            height: 4,
        };

        let cached = cache
            .viewport(
                key.width,
                key.scroll,
                key.height,
                &renderer,
                SAMPLE_MARKDOWN,
            )
            .clone();
        let expected = wrap_markdown_for_viewport(
            cache
                .rendered
                .as_ref()
                .expect("rendered markdown is cached with the viewport"),
            key.width,
            key.scroll,
            key.height,
        );

        assert_eq!(cached, expected);
        assert_eq!(cache.viewport_key, Some(key));
        assert_eq!(
            cache
                .viewport(
                    key.width,
                    key.scroll,
                    key.height,
                    &renderer,
                    SAMPLE_MARKDOWN,
                )
                .clone(),
            cached
        );
    }

    #[test]
    fn stream_viewport_cache_keys_scroll_and_height() {
        let renderer = MarkdownRenderer::new(MarkdownTheme::default());
        let mut cache = StreamRenderCache::default();
        let fragment = &STREAMING_MARKDOWN[..STREAMING_MARKDOWN
            .find("## Architecture Overview")
            .expect("fixture contains architecture section")];
        let position = fragment.len();
        let first_key = MarkdownViewportKey {
            width: 20,
            scroll: 0,
            height: 5,
        };

        let (cached, detection) =
            cache.viewport_and_detection(first_key, position, false, &renderer, fragment);
        let cached = cached.clone();
        assert!(detection.is_likely());

        let entry = cache
            .entry
            .as_ref()
            .expect("stream render entry is cached with the viewport");
        assert_eq!(
            cached,
            wrap_markdown_for_viewport(
                &entry.text,
                first_key.width,
                first_key.scroll,
                first_key.height
            )
        );
        assert_eq!(entry.viewport_key, Some(first_key));

        let second_key = MarkdownViewportKey {
            scroll: 1,
            ..first_key
        };
        let _ = cache.viewport_and_detection(second_key, position, false, &renderer, fragment);
        assert_eq!(
            cache.entry.as_ref().and_then(|entry| entry.viewport_key),
            Some(second_key)
        );
        assert_eq!(
            cache.key,
            Some(StreamRenderKey {
                width: second_key.width,
                position,
                complete: false,
            })
        );
    }

    #[test]
    fn scroll_navigation() {
        let mut screen = MarkdownRichText::new();
        screen.update(&press(KeyCode::Down));
        assert_eq!(screen.md_scroll, 1);
        screen.update(&press(KeyCode::Down));
        assert_eq!(screen.md_scroll, 2);
        screen.update(&press(KeyCode::Up));
        assert_eq!(screen.md_scroll, 1);
        screen.update(&press(KeyCode::Home));
        assert_eq!(screen.md_scroll, 0);
        screen.update(&press(KeyCode::Up));
        assert_eq!(screen.md_scroll, 0);
    }

    #[test]
    fn page_scroll() {
        let mut screen = MarkdownRichText::new();
        screen.update(&press(KeyCode::PageDown));
        assert_eq!(screen.md_scroll, 10);
        screen.update(&press(KeyCode::PageUp));
        assert_eq!(screen.md_scroll, 0);
    }

    #[test]
    fn wrap_mode_cycles() {
        let mut screen = MarkdownRichText::new();
        assert_eq!(screen.wrap_label(), "Word");
        screen.update(&press(KeyCode::Char('w')));
        assert_eq!(screen.wrap_label(), "Char");
        screen.update(&press(KeyCode::Char('w')));
        assert_eq!(screen.wrap_label(), "WordChar");
        screen.update(&press(KeyCode::Char('w')));
        assert_eq!(screen.wrap_label(), "None");
        screen.update(&press(KeyCode::Char('w')));
        assert_eq!(screen.wrap_label(), "Word");
    }

    #[test]
    fn alignment_cycles() {
        let mut screen = MarkdownRichText::new();
        assert_eq!(screen.alignment_label(), "Left");
        screen.update(&press(KeyCode::Char('a')));
        assert_eq!(screen.alignment_label(), "Center");
        screen.update(&press(KeyCode::Char('a')));
        assert_eq!(screen.alignment_label(), "Right");
        screen.update(&press(KeyCode::Char('a')));
        assert_eq!(screen.alignment_label(), "Left");
    }

    #[test]
    fn stream_position_advances() {
        let mut screen = MarkdownRichText::new();
        let initial = screen.stream_position;
        screen.tick_stream();
        assert!(screen.stream_position > initial);
    }

    #[test]
    fn stream_completes_eventually() {
        let mut screen = MarkdownRichText::new();
        for _ in 0..10_000 {
            screen.tick_stream();
            if screen.stream_complete() {
                break;
            }
        }
        assert!(screen.stream_complete());
    }

    #[test]
    fn current_fragment_never_panics() {
        let mut screen = MarkdownRichText::new();
        for _ in 0..5_000 {
            let _ = screen.current_stream_fragment();
            screen.tick_stream();
        }
    }

    #[test]
    fn progress_in_valid_range() {
        let screen = MarkdownRichText::new();
        let progress = screen.stream_position as f64 / STREAMING_MARKDOWN.len() as f64;
        assert!((0.0..=1.0).contains(&progress));
    }

    #[test]
    fn keybindings_non_empty() {
        let screen = MarkdownRichText::new();
        assert!(!screen.keybindings().is_empty());
    }

    #[test]
    fn style_flags_all_represented() {
        let styles = [
            theme::bold(),
            theme::dim(),
            theme::italic(),
            theme::underline(),
            theme::strikethrough(),
            theme::reverse(),
            theme::blink_style(),
            theme::double_underline(),
            theme::curly_underline(),
        ];
        for style in &styles {
            assert_ne!(*style, Style::default());
        }
    }
}
