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

use std::cell::RefCell;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme, is_likely_markdown};
use ftui_extras::visual_fx::{Backdrop, PlasmaFx, PlasmaPalette, Scrim, ThemeInputs};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::WrapMode;
use ftui_text::text::{Line, Span, Text};
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
/// Horizontal rule width for markdown rendering.
const RULE_WIDTH: u16 = 36;

struct MarkdownPanel<'a> {
    markdown: &'a str,
    scroll: u16,
    theme: MarkdownTheme,
}

fn wrap_markdown_for_panel(text: &Text, width: u16) -> Text {
    let width = usize::from(width);
    if width == 0 {
        return text.clone();
    }

    let mut lines = Vec::new();
    for line in text.lines() {
        let plain = line.to_plain_text();
        let table_like = is_table_line(&plain) || is_table_like_line(&plain);
        if table_like || line.width() <= width {
            lines.push(line.clone());
            continue;
        }

        for wrapped in line.wrap(width, WrapMode::Word) {
            if wrapped.width() <= width {
                lines.push(wrapped);
            } else {
                let mut text = Text::from_lines([wrapped]);
                text.truncate(width, None);
                lines.extend(text.lines().iter().cloned());
            }
        }
    }

    Text::from_lines(lines)
}

fn is_table_line(plain: &str) -> bool {
    plain.chars().any(|c| {
        matches!(
            c,
            '┌' | '┬' | '┐' | '├' | '┼' | '┤' | '└' | '┴' | '┘' | '│' | '─'
        )
    })
}

fn is_table_like_line(plain: &str) -> bool {
    let trimmed = plain.trim_start();
    if !trimmed.starts_with('|') {
        return false;
    }
    trimmed.chars().filter(|&c| c == '|').count() >= 2
}

impl Widget for MarkdownPanel<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Markdown Renderer")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::MARKDOWN));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let renderer = MarkdownRenderer::new(self.theme.clone())
            .rule_width(RULE_WIDTH.min(inner.width))
            .table_max_width(inner.width);
        let rendered = renderer.render(self.markdown);
        let wrapped = wrap_markdown_for_panel(&rendered, inner.width);
        Paragraph::new(wrapped)
            .wrap(WrapMode::None)
            .scroll((self.scroll, 0))
            .render(inner, frame);
    }
}

pub struct MarkdownRichText {
    md_scroll: u16,
    wrap_index: usize,
    align_index: usize,
    // Streaming simulation state
    stream_position: usize,
    stream_paused: bool,
    stream_scroll: u16,
    md_theme: MarkdownTheme,
    tick_count: u64,
    markdown_backdrop: RefCell<Backdrop>,
}

impl Default for MarkdownRichText {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownRichText {
    pub fn new() -> Self {
        let md_theme = Self::build_theme();
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
            stream_scroll: 0,
            md_theme,
            tick_count: 0,
            markdown_backdrop: RefCell::new(markdown_backdrop),
        }
    }

    pub fn apply_theme(&mut self) {
        self.md_theme = Self::build_theme();
        let theme_inputs = Self::current_fx_theme();
        self.markdown_backdrop.borrow_mut().set_theme(theme_inputs);
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
            table_border: Style::new().fg(theme::fg::MUTED),
            table_header: Style::new()
                .fg(theme::accent::INFO)
                .bg(theme::alpha::SURFACE)
                .bold(),
            table_row: Style::new().fg(theme::fg::PRIMARY),
            table_row_alt: Style::new()
                .fg(theme::fg::PRIMARY)
                .bg(theme::alpha::SURFACE),
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
            let speed = self.calculate_typing_speed();

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

    /// Render the streaming fragment using streaming-aware rendering.
    ///
    /// Adds a visible blinking cursor at the end when still streaming.
    fn render_stream_fragment(&self, width: u16) -> Text {
        let fragment = self.current_stream_fragment();
        let renderer = MarkdownRenderer::new(self.md_theme.clone())
            .rule_width(RULE_WIDTH.min(width))
            .table_max_width(width);
        let mut text = renderer.render_streaming(fragment);

        // Add blinking cursor at end if still streaming
        if !self.stream_complete() {
            // Create cursor span with accent color and blink
            let cursor = Span::styled("▌", Style::new().fg(theme::accent::PRIMARY).blink());
            let mut lines: Vec<Line> = text.lines().to_vec();
            if let Some(last_line) = lines.last_mut() {
                last_line.push_span(cursor);
            } else {
                lines.push(Line::from_spans([cursor]));
            }
            text = Text::from_lines(lines);
        }

        text
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
            theme: self.md_theme.clone(),
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

        let styles_text = Text::from_lines([
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
        ]);

        Paragraph::new(styles_text).render(inner, frame);
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
            .style(Style::new().fg(theme::screen_accent::MARKDOWN));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let chunks = Flex::vertical()
            .constraints([Constraint::Fixed(1), Constraint::Min(1)])
            .split(inner);

        Paragraph::new("w: cycle wrap | a: cycle alignment")
            .style(theme::muted())
            .render(chunks[0], frame);

        let demo_text = "The quick brown fox jumps over the lazy dog. \
             Supercalifragilisticexpialidocious is quite a long word \
             that tests character-level wrapping behavior. \
             \u{4f60}\u{597d}\u{4e16}\u{754c} contains CJK characters \
             that are double-width. \u{1f980} Ferris says hello!";

        Paragraph::new(demo_text)
            .wrap(self.current_wrap())
            .alignment(self.current_alignment())
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(chunks[1], frame);
    }

    fn render_streaming_panel(&self, frame: &mut Frame, area: Rect) {
        // Build title with streaming status
        let progress_pct =
            (self.stream_position as f64 / STREAMING_MARKDOWN.len() as f64 * 100.0) as u8;
        let status = if self.stream_complete() {
            "Complete".to_string()
        } else if self.stream_paused {
            format!("Paused ({progress_pct}%)")
        } else {
            format!("Streaming... {progress_pct}%")
        };

        let title = format!("LLM Streaming Simulation | {status}");

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title.as_str())
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
        let stream_text = self.render_stream_fragment(chunks[0].width);
        let wrapped_stream = wrap_markdown_for_panel(&stream_text, chunks[0].width);
        Paragraph::new(wrapped_stream)
            .wrap(WrapMode::None)
            .scroll((self.stream_scroll, 0))
            .render(chunks[0], frame);

        // Render mini progress bar
        let progress = self.stream_position as f64 / STREAMING_MARKDOWN.len() as f64;
        let bar_width = chunks[1].width.saturating_sub(4) as usize;
        let filled = (progress * bar_width as f64) as usize;
        let empty = bar_width.saturating_sub(filled);

        let progress_bar = Line::from_spans([
            Span::styled("  ", Style::new()),
            Span::styled("[", theme::muted()),
            Span::styled("█".repeat(filled), Style::new().fg(theme::accent::SUCCESS)),
            Span::styled("░".repeat(empty), Style::new().fg(theme::fg::MUTED).dim()),
            Span::styled("]", theme::muted()),
        ]);
        Paragraph::new(Text::from_lines([progress_bar])).render(chunks[1], frame);

        // Detection status panel
        let fragment = self.current_stream_fragment();
        let detection = is_likely_markdown(fragment);
        let det_line1 = format!(
            "Detection: {} indicators | {}",
            detection.indicators,
            if detection.is_confident() {
                "Confident"
            } else if detection.is_likely() {
                "Likely"
            } else {
                "Uncertain"
            }
        );
        let det_line2 = format!(
            "Confidence: {:.0}% | Chars: {}/{}",
            detection.confidence() * 100.0,
            self.stream_position,
            STREAMING_MARKDOWN.len()
        );
        let det_line3 = "Space: play/pause | r: restart | ↑↓: scroll stream";

        let detection_text = Text::from_lines([
            Line::from_spans([
                Span::styled("  ", Style::new()),
                Span::styled(det_line1, theme::muted()),
            ]),
            Line::from_spans([
                Span::styled("  ", Style::new()),
                Span::styled(det_line2, theme::muted()),
            ]),
            Line::from_spans([
                Span::styled("  ", Style::new()),
                Span::styled(det_line3, Style::new().fg(theme::accent::INFO).dim()),
            ]),
        ]);

        Paragraph::new(detection_text).render(chunks[2], frame);
    }
}

impl Screen for MarkdownRichText {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                // Markdown panel scrolling
                KeyCode::Up => {
                    self.md_scroll = self.md_scroll.saturating_sub(1);
                }
                KeyCode::Down => {
                    self.md_scroll = self.md_scroll.saturating_add(1);
                }
                KeyCode::PageUp => {
                    self.md_scroll = self.md_scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    self.md_scroll = self.md_scroll.saturating_add(10);
                }
                KeyCode::Home => {
                    self.md_scroll = 0;
                }
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
                KeyCode::Char('r') => {
                    // Reset streaming
                    self.stream_position = 0;
                    self.stream_paused = false;
                    self.stream_scroll = 0;
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
        self.render_markdown_panel(frame, cols[0]);

        // Center: Streaming simulation
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
                action: "Scroll markdown",
            },
            HelpEntry {
                key: "[/]",
                action: "Scroll stream",
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

    fn rendered_sample() -> Text {
        MarkdownRenderer::new(MarkdownTheme::default())
            .rule_width(RULE_WIDTH)
            .render(SAMPLE_MARKDOWN)
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
    fn markdown_renders_headings() {
        let rendered = rendered_sample();
        let plain: String = rendered
            .lines()
            .iter()
            .map(|l| l.to_plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plain.contains("GitHub-Flavored Markdown"));
        assert!(plain.contains("Math & LaTeX Support"));
        assert!(plain.contains("Task Lists"));
    }

    #[test]
    fn markdown_renders_code_block() {
        let rendered = rendered_sample();
        let plain: String = rendered
            .lines()
            .iter()
            .map(|l| l.to_plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plain.contains("fn fibonacci"));
        assert!(plain.contains("def quicksort"));
    }

    #[test]
    fn markdown_renders_task_lists() {
        let rendered = rendered_sample();
        let plain: String = rendered
            .lines()
            .iter()
            .map(|l| l.to_plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        // Task list items should have checkbox markers
        assert!(plain.contains("Implement markdown parser"));
        assert!(plain.contains("Add syntax highlighting"));
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
