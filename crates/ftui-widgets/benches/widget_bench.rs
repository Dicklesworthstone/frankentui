//! Benchmarks for widget rendering (bd-19x)
//!
//! Run with: cargo bench -p ftui-widgets

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use ftui_core::geometry::{Rect, Size};
use ftui_layout::Constraint;
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_style::Style;
use ftui_text::{Line, Span, Text};
use ftui_widgets::MeasurableWidget;
use ftui_widgets::SizeConstraints;
use ftui_widgets::StatefulWidget;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;
use ftui_widgets::command_palette::{BayesianScorer, IncrementalScorer};
use ftui_widgets::log_ring::LogRing;
use ftui_widgets::log_viewer::{LogViewer, LogViewerState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::table::{Row, Table};
use ftui_widgets::virtualized::Virtualized;
use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::hint::black_box;

// ============================================================================
// Block widget
// ============================================================================

fn bench_block_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("widget/block");

    let block_plain = Block::new();
    let block_bordered = Block::new().borders(Borders::ALL).title("Title");

    for (w, h) in [(40, 10), (80, 24), (200, 60)] {
        let area = Rect::from_size(w, h);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, h, &mut pool);

        group.bench_with_input(
            BenchmarkId::new("plain", format!("{w}x{h}")),
            &(),
            |b, _| {
                b.iter(|| {
                    frame.buffer.clear();
                    block_plain.render(area, &mut frame);
                    black_box(&frame.buffer);
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("bordered", format!("{w}x{h}")),
            &(),
            |b, _| {
                b.iter(|| {
                    frame.buffer.clear();
                    block_bordered.render(area, &mut frame);
                    black_box(&frame.buffer);
                })
            },
        );
    }

    group.finish();
}

// ============================================================================
// Paragraph widget
// ============================================================================

fn make_paragraph_text(chars: usize) -> Text<'static> {
    let content: String = "The quick brown fox jumps over the lazy dog. "
        .chars()
        .cycle()
        .take(chars)
        .collect();
    Text::raw(content)
}

fn bench_paragraph_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("widget/paragraph");

    for (chars, label) in [(50, "50ch"), (200, "200ch"), (1000, "1000ch")] {
        let text = make_paragraph_text(chars);
        let para = Paragraph::new(text);
        let area = Rect::from_size(80, 24);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        group.bench_with_input(BenchmarkId::new("no_wrap", label), &para, |b, para| {
            b.iter(|| {
                frame.buffer.clear();
                para.render(area, &mut frame);
                black_box(&frame.buffer);
            })
        });
    }

    group.finish();
}

fn bench_paragraph_wrapped(c: &mut Criterion) {
    let mut group = c.benchmark_group("widget/paragraph_wrap");

    for (chars, label) in [(200, "200ch"), (1000, "1000ch"), (5000, "5000ch")] {
        let text = make_paragraph_text(chars);
        let para = Paragraph::new(text).wrap(ftui_text::WrapMode::Word);
        let area = Rect::from_size(80, 24);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        group.bench_with_input(BenchmarkId::new("word_wrap", label), &para, |b, para| {
            b.iter(|| {
                frame.buffer.clear();
                para.render(area, &mut frame);
                black_box(&frame.buffer);
            })
        });
    }

    group.finish();
}

/// Separate the cost of populating Paragraph's metrics cache from a cache hit.
/// The production cache holds 256 FIFO entries. Cycling 512 distinct text hashes
/// ensures every cold access evicts its previous entry before it is revisited.
/// Cold iterations traverse the complete corpus; their elapsed time is per 512
/// paragraphs, while throughput is paragraphs per second. Hot iterations use one.
fn bench_paragraph_cache(c: &mut Criterion) {
    const CORPUS_LEN: usize = 512;
    let mut group = c.benchmark_group("widget/paragraph_cache");
    // Equal-duration batches avoid very short initial samples in the bounded
    // focused/broad measurement window. Every cold iteration still visits all
    // 512 entries; this changes sampling, not the measured corpus.
    group.sampling_mode(SamplingMode::Flat);
    let available = Size::new(80, 4);
    let area = Rect::from_size(available.width, available.height);

    for family in ["ascii", "ascii_controls", "mixed_unicode", "long_ascii"] {
        let mut hashes = BTreeSet::new();
        let paragraphs: Vec<Paragraph<'static>> = (0..CORPUS_LEN)
            .map(|index| {
                let prefix = format!("{index:03} ");
                let (text, min_width, preferred_width, height) = match family {
                    "ascii" => {
                        let word_len = 8 + index % 16;
                        let text = Text::from_line(Line::from_spans([
                            Span::raw(prefix),
                            Span::raw("alpha "),
                            Span::raw("x".repeat(word_len / 2)),
                            Span::styled("x".repeat(word_len - word_len / 2), Style::new().bold()),
                            Span::raw(" tail"),
                        ]));
                        // A word crossing a styled span boundary remains one word.
                        (text, word_len, 15 + word_len, 1)
                    }
                    "ascii_controls" => {
                        let word_len = 2 + index % 4;
                        let text = Text::raw(format!(
                            "{prefix}aa\0bb\u{7f}cc\t{}\u{b}eee\u{c}f\rg\nnext",
                            "d".repeat(word_len)
                        ));
                        // NUL/DEL are zero-width within a word; VT/FF break words
                        // with zero width. Tab/CR occupy one cell; LF splits lines.
                        (text, 6, 17 + word_len, 2)
                    }
                    "mixed_unicode" => {
                        let wide_chars = 2 + index % 3;
                        let text = Text::from_spans([
                            Span::raw(prefix),
                            Span::raw("e\u{301}"),
                            Span::styled("e\u{301} ", Style::new().bold()),
                            Span::raw("界".repeat(wide_chars)),
                            Span::raw(" 👩‍💻\u{2003}tail"),
                        ]);
                        (text, 2 * wide_chars, 15 + 2 * wide_chars, 1)
                    }
                    "long_ascii" => {
                        let word_len = 4096 + index % 64;
                        let text = Text::raw(format!("{prefix}{} tail", "x".repeat(word_len)));
                        (text, word_len, 9 + word_len, 1)
                    }
                    _ => unreachable!(),
                };
                // Paragraph keys its cache by this exact Text hash. Distinct
                // strings alone would not detect an accidental key collision.
                let mut hasher = DefaultHasher::new();
                text.hash(&mut hasher);
                assert!(
                    hashes.insert(hasher.finish()),
                    "duplicate {family} cache key"
                );
                let expected = SizeConstraints::at_least(
                    Size::new(u16::try_from(min_width).unwrap(), 1),
                    Size::new(u16::try_from(preferred_width).unwrap(), height),
                );
                let paragraph = Paragraph::from_static_text(text);
                assert_eq!(paragraph.measure(available), expected, "{family}/{index}");
                paragraph
            })
            .collect();
        assert_eq!(hashes.len(), CORPUS_LEN);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(available.width, available.height, &mut pool);
        for (index, paragraph) in paragraphs.iter().enumerate() {
            frame.buffer.clear();
            paragraph.render(area, &mut frame);
            for (x, digit) in format!("{index:03}").chars().enumerate() {
                assert_eq!(
                    frame
                        .buffer
                        .get(u16::try_from(x).unwrap(), 0)
                        .unwrap()
                        .content
                        .as_char(),
                    Some(digit),
                    "{family}/{index} rendered identity"
                );
            }
            if family == "ascii_controls" {
                assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('n'));
                assert_eq!(frame.buffer.get(3, 1).unwrap().content.as_char(), Some('t'));
            }
        }

        group.throughput(Throughput::Elements(CORPUS_LEN as u64));
        group.bench_function(BenchmarkId::new("render_cold_512", family), |b| {
            // Prime a complete sweep before EACH Criterion invocation, including
            // short warmup samples. Starting at zero then guarantees cache misses.
            for paragraph in &paragraphs {
                black_box(paragraph.measure(available));
            }
            b.iter(|| {
                for paragraph in &paragraphs {
                    frame.buffer.clear();
                    paragraph.render(area, &mut frame);
                    black_box(&frame.buffer);
                }
            });
        });
        group.throughput(Throughput::Elements(1));
        group.bench_function(BenchmarkId::new("render_hot_1", family), |b| {
            paragraphs[0].render(area, &mut frame);
            b.iter(|| {
                frame.buffer.clear();
                paragraphs[0].render(area, &mut frame);
                black_box(&frame.buffer);
            });
        });
        group.throughput(Throughput::Elements(CORPUS_LEN as u64));
        group.bench_function(BenchmarkId::new("measure_cold_512", family), |b| {
            for paragraph in &paragraphs {
                black_box(paragraph.measure(available));
            }
            b.iter(|| {
                for paragraph in &paragraphs {
                    black_box(paragraph.measure(black_box(available)));
                }
            });
        });
        group.throughput(Throughput::Elements(1));
        group.bench_function(BenchmarkId::new("measure_hot_1", family), |b| {
            black_box(paragraphs[0].measure(available));
            b.iter(|| black_box(paragraphs[0].measure(black_box(available))));
        });
    }

    group.finish();
}

// ============================================================================
// Table widget
// ============================================================================

fn make_table(row_count: usize, col_count: usize) -> (Table<'static>, Vec<Constraint>) {
    let widths: Vec<Constraint> = (0..col_count)
        .map(|_| Constraint::Percentage(100.0 / col_count as f32))
        .collect();

    let rows: Vec<Row> = (0..row_count)
        .map(|r| {
            let cells: Vec<String> = (0..col_count).map(|col| format!("R{r}C{col}")).collect();
            Row::new(cells)
        })
        .collect();

    let header_cells: Vec<String> = (0..col_count).map(|c| format!("Col {c}")).collect();
    let header = Row::new(header_cells).style(Style::new().fg(PackedRgba::rgb(255, 255, 0)));

    let table = Table::new(rows, widths.clone())
        .header(header)
        .block(Block::new().borders(Borders::ALL).title("Data"));

    (table, widths)
}

fn bench_table_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("widget/table");

    for (rows, cols, label) in [
        (10, 3, "10x3"),
        (50, 5, "50x5"),
        (100, 3, "100x3"),
        (100, 8, "100x8"),
    ] {
        let (table, _) = make_table(rows, cols);
        let area = Rect::from_size(120, 40);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);

        group.bench_with_input(BenchmarkId::new("render", label), &table, |b, table| {
            b.iter(|| {
                frame.buffer.clear();
                Widget::render(table, area, &mut frame);
                black_box(&frame.buffer);
            })
        });
    }

    group.finish();
}

// ============================================================================
// Virtualization benchmarks (bd-uo6v)
// ============================================================================

fn bench_log_ring_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/log_ring_push");

    for (capacity, label) in [(1_000, "1K"), (10_000, "10K"), (100_000, "100K")] {
        let mut ring: LogRing<String> = LogRing::new(capacity);

        group.bench_function(BenchmarkId::new("push", label), |b| {
            b.iter(|| {
                ring.push(black_box(
                    "Log line: operation completed successfully".to_string(),
                ));
            })
        });
    }

    group.finish();
}

fn bench_log_ring_push_at_capacity(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/log_ring_push_full");

    for (capacity, label) in [(1_000, "1K"), (10_000, "10K")] {
        // Pre-fill to capacity
        let mut ring: LogRing<String> = LogRing::new(capacity);
        for i in 0..capacity {
            ring.push(format!("Line {}", i));
        }

        group.bench_function(BenchmarkId::new("push_evict", label), |b| {
            b.iter(|| {
                ring.push(black_box("New line replacing oldest".to_string()));
            })
        });
    }

    group.finish();
}

fn bench_virtualized_scroll(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/scroll");

    for (count, label) in [(1_000, "1K"), (10_000, "10K"), (100_000, "100K")] {
        let mut virt: Virtualized<i32> = Virtualized::new(count);
        for i in 0..count {
            virt.push(i as i32);
        }
        virt.set_visible_count(24);

        group.bench_function(BenchmarkId::new("scroll_one", label), |b| {
            b.iter(|| {
                virt.scroll(black_box(1));
                black_box(virt.scroll_offset());
            })
        });

        group.bench_function(BenchmarkId::new("page_down", label), |b| {
            b.iter(|| {
                virt.page_down();
                black_box(virt.scroll_offset());
            })
        });

        group.bench_function(BenchmarkId::new("visible_range", label), |b| {
            b.iter(|| {
                black_box(virt.visible_range(24));
            })
        });
    }

    group.finish();
}

fn bench_log_viewer_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/log_viewer_render");

    for (count, label) in [
        (100, "100"),
        (1_000, "1K"),
        (10_000, "10K"),
        (100_000, "100K"),
    ] {
        let mut viewer = LogViewer::new(count);
        for i in 0..count {
            viewer.push(format!(
                "[{:>6}] INFO  app::module: Processing request #{}",
                i, i
            ));
        }

        let area = Rect::from_size(80, 24);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let mut state = LogViewerState::default();

        group.bench_function(BenchmarkId::new("render", label), |b| {
            b.iter(|| {
                frame.buffer.clear();
                viewer.render(area, &mut frame, &mut state);
                black_box(&frame.buffer);
            })
        });
    }

    group.finish();
}

fn bench_log_viewer_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("virtualized/log_viewer_search");

    for (count, label) in [(1_000, "1K"), (10_000, "10K")] {
        let mut viewer = LogViewer::new(count);
        for i in 0..count {
            if i % 100 == 0 {
                viewer.push(format!("[{:>6}] ERROR app::handler: Request failed", i));
            } else {
                viewer.push(format!("[{:>6}] INFO  app::handler: Request OK", i));
            }
        }

        group.bench_function(BenchmarkId::new("search", label), |b| {
            b.iter(|| {
                black_box(viewer.search(black_box("ERROR")));
            })
        });
    }

    group.finish();
}

// ============================================================================
// Command Palette scorer benchmarks (bd-39y4.13)
// ============================================================================

fn make_command_corpus(size: usize) -> Vec<String> {
    let base_commands = [
        "Open File",
        "Save File",
        "Close Tab",
        "Split Editor Right",
        "Split Editor Down",
        "Toggle Terminal",
        "Go to Line",
        "Find in Files",
        "Replace in Files",
        "Git: Commit",
        "Git: Push",
        "Git: Pull",
        "Debug: Start",
        "Debug: Stop",
        "Debug: Step Over",
        "Format Document",
        "Rename Symbol",
        "Go to Definition",
        "Find All References",
        "Toggle Sidebar",
        "Toggle Minimap",
        "Change Language Mode",
        "Select Color Theme",
        "Keyboard Shortcuts",
        "User Settings",
        "Extensions: Install",
        "Terminal: New",
        "View: Zoom In",
        "View: Zoom Out",
        "Editor: Font Size",
    ];
    base_commands
        .iter()
        .cycle()
        .take(size)
        .enumerate()
        .map(|(i, cmd)| {
            if i < base_commands.len() {
                (*cmd).to_string()
            } else {
                format!("{} ({})", cmd, i)
            }
        })
        .collect()
}

fn bench_scorer_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("command_palette/score_single");
    let scorer = BayesianScorer::new();
    let scorer_fast = BayesianScorer::fast();

    let cases = [
        ("exact", "Open File", "Open File"),
        ("prefix", "ope", "Open File"),
        ("word_start", "of", "Open File"),
        ("substring", "en f", "Open File"),
        ("fuzzy", "opfl", "Open File"),
        ("no_match", "xyz", "Open File"),
    ];

    for (label, query, title) in cases {
        group.bench_function(BenchmarkId::new("tracked", label), |b| {
            b.iter(|| black_box(scorer.score(black_box(query), black_box(title))))
        });
        group.bench_function(BenchmarkId::new("fast", label), |b| {
            b.iter(|| black_box(scorer_fast.score(black_box(query), black_box(title))))
        });
    }

    group.finish();
}

fn bench_scorer_corpus(c: &mut Criterion) {
    let mut group = c.benchmark_group("command_palette/score_corpus");
    let scorer = BayesianScorer::fast();

    for (size, label) in [
        (30, "30"),
        (100, "100"),
        (500, "500"),
        (1000, "1K"),
        (5000, "5K"),
    ] {
        let corpus = make_command_corpus(size);

        group.bench_function(BenchmarkId::new("full_scan", label), |b| {
            b.iter(|| {
                let query = black_box("git co");
                let mut results: Vec<_> = corpus
                    .iter()
                    .map(|title| scorer.score(query, title))
                    .filter(|r| r.score > 0.0)
                    .collect();
                results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
                black_box(&results);
            })
        });
    }

    group.finish();
}

fn compute_word_starts(title_lower: &str) -> Vec<usize> {
    let bytes = title_lower.as_bytes();
    title_lower
        .char_indices()
        .filter_map(|(i, _)| {
            let is_word_start = i == 0 || {
                let prev = bytes.get(i.saturating_sub(1)).copied().unwrap_or(b' ');
                prev == b' ' || prev == b'-' || prev == b'_'
            };
            is_word_start.then_some(i)
        })
        .collect()
}

fn bench_scorer_incremental_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("command_palette/incremental_query");
    let scorer = BayesianScorer::fast();
    let corpus = make_command_corpus(100);
    let corpus_lower: Vec<String> = corpus.iter().map(|s| s.to_lowercase()).collect();
    let corpus_word_starts: Vec<Vec<usize>> = corpus_lower
        .iter()
        .map(|title_lower| compute_word_starts(title_lower))
        .collect();

    // Simulate typing "git commit" one character at a time
    let queries = ["g", "gi", "git", "git ", "git c", "git co", "git com"];

    group.bench_function("naive_rescans", |b| {
        b.iter(|| {
            for query in &queries {
                let mut results: Vec<_> = corpus
                    .iter()
                    .map(|title| scorer.score(black_box(query), title))
                    .filter(|r| r.score > 0.0)
                    .collect();
                results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
                black_box(&results);
            }
        })
    });

    group.bench_function("incremental_scorer", |b| {
        b.iter(|| {
            let mut inc = IncrementalScorer::new();
            for query in &queries {
                let results = inc.score_corpus_with_lowered_and_words(
                    black_box(query),
                    &corpus,
                    &corpus_lower,
                    &corpus_word_starts,
                    None,
                );
                black_box(&results);
            }
        })
    });

    group.finish();
}

fn bench_scorer_incremental_corpus_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("command_palette/incremental_corpus_size");
    let queries = ["g", "gi", "git", "git ", "git c"];

    for (size, label) in [
        (30, "30"),
        (100, "100"),
        (500, "500"),
        (1000, "1K"),
        (5000, "5K"),
    ] {
        let corpus = make_command_corpus(size);
        let corpus_lower: Vec<String> = corpus.iter().map(|s| s.to_lowercase()).collect();
        let corpus_word_starts: Vec<Vec<usize>> = corpus_lower
            .iter()
            .map(|title_lower| compute_word_starts(title_lower))
            .collect();

        group.bench_function(BenchmarkId::new("incremental", label), |b| {
            b.iter(|| {
                let mut inc = IncrementalScorer::new();
                for query in &queries {
                    let results = inc.score_corpus_with_lowered_and_words(
                        black_box(query),
                        &corpus,
                        &corpus_lower,
                        &corpus_word_starts,
                        None,
                    );
                    black_box(&results);
                }
            })
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_block_render,
    bench_paragraph_render,
    bench_paragraph_wrapped,
    bench_paragraph_cache,
    bench_table_render,
    bench_log_ring_push,
    bench_log_ring_push_at_capacity,
    bench_virtualized_scroll,
    bench_log_viewer_render,
    bench_log_viewer_search,
    bench_scorer_single,
    bench_scorer_corpus,
    bench_scorer_incremental_query,
    bench_scorer_incremental_corpus_sizes,
);

criterion_main!(benches);
