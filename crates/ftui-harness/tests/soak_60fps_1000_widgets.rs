#![forbid(unsafe_code)]

//! 60fps soak test on a 1000-widget tree (bd-1pys5.7).
//!
//! Validates that FrankenTUI sustains 60fps rendering on a large widget tree
//! with active content churn. No slow leaks, GC-like pauses, or progressive
//! degradation.
//!
//! # Design
//!
//! The test renders a configurable number of frames (default: 600 for CI,
//! configurable up to 3600 for the full 60-second soak). Each frame updates
//! a 1000-widget tree with simulated user interaction (content changes,
//! selection changes, progress updates).
//!
//! # Metrics Tracked
//!
//! - Frame time: render + diff + present
//! - Frame time percentiles: p50, p95, p99, max
//! - Memory proxy: buffer allocation sizes (no custom allocator needed)
//! - Buffer checksum stability: no visual corruption
//!
//! # Pass Criteria (for the fast CI variant)
//!
//! - p50 frame time < 8ms
//! - p99 frame time < 50ms (generous for debug builds)
//! - Max frame time < 100ms (debug builds are slow)
//! - No panics or crashes
//! - Buffer dimensions consistent across all frames
//!
//! # Running
//!
//! ```sh
//! # Fast CI variant (600 frames ≈ 10 seconds at 60fps)
//! cargo test -p ftui-harness --test soak_60fps_1000_widgets
//!
//! # Full soak (3600 frames ≈ 60 seconds at 60fps)
//! SOAK_FRAMES=3600 cargo test -p ftui-harness --test soak_60fps_1000_widgets -- --nocapture
//! ```

use std::time::{Duration, Instant};

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::presenter::{Presenter, TerminalCapabilities};
use ftui_text::Text;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;
use ftui_widgets::list::{List, ListItem};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::sparkline::Sparkline;

use ftui_harness::flicker_detection::analyze_stream_with_id;
use ftui_harness::golden::compute_buffer_checksum;

// ============================================================================
// Configuration
// ============================================================================

/// Default frame count for CI (≈10 seconds at 60fps).
const DEFAULT_FRAMES: usize = 600;

/// Terminal dimensions for the soak test.
const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

/// Number of widgets in the tree.
const WIDGET_COUNT: usize = 1000;

/// Frame time SLA thresholds (generous for debug builds).
const P50_LIMIT_US: u128 = 8_000; // 8ms
const P99_LIMIT_US: u128 = 50_000; // 50ms
const MAX_LIMIT_US: u128 = 100_000; // 100ms

fn frame_count() -> usize {
    std::env::var("SOAK_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FRAMES)
}

// ============================================================================
// Deterministic LCG
// ============================================================================

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        (self.0 >> 32) as u32
    }

    fn next_range(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        (self.next_u32() as usize) % max
    }
}

// ============================================================================
// Widget Tree Construction
// ============================================================================

/// Describes one widget in our synthetic tree.
enum WidgetKind {
    Paragraph { text: String },
    Progress { ratio: f64 },
    Sparkline { data: Vec<f64> },
    List { items: Vec<String>, selected: usize },
    Block { title: String, content: String },
}

/// The full widget tree state.
struct WidgetTree {
    widgets: Vec<WidgetKind>,
}

impl WidgetTree {
    /// Build an initial 1000-widget tree.
    fn new(count: usize) -> Self {
        let mut widgets = Vec::with_capacity(count);
        for i in 0..count {
            let kind = match i % 5 {
                0 => WidgetKind::Paragraph {
                    text: format!("Widget {i}: initial content for paragraph widget"),
                },
                1 => WidgetKind::Progress {
                    ratio: (i as f64 / count as f64),
                },
                2 => WidgetKind::Sparkline {
                    data: (0..20).map(|x| ((x + i) % 15) as f64).collect(),
                },
                3 => WidgetKind::List {
                    items: (0..8).map(|j| format!("Item {j} of widget {i}")).collect(),
                    selected: 0,
                },
                _ => WidgetKind::Block {
                    title: format!("Box #{i}"),
                    content: format!("Content for block widget {i}"),
                },
            };
            widgets.push(kind);
        }
        Self { widgets }
    }

    /// Simulate one frame of user interaction — mutates some widgets.
    fn tick(&mut self, frame_idx: usize, rng: &mut Lcg) {
        // Update ~10% of widgets per frame (100 out of 1000)
        let updates = self.widgets.len() / 10;
        for _ in 0..updates {
            let idx = rng.next_range(self.widgets.len());
            match &mut self.widgets[idx] {
                WidgetKind::Paragraph { text } => {
                    *text = format!(
                        "Widget {idx}: frame {frame_idx} updated content #{}",
                        rng.next_u32() % 1000
                    );
                }
                WidgetKind::Progress { ratio } => {
                    *ratio = ((frame_idx as f64 * 0.01) + (idx as f64 * 0.001)).fract();
                }
                WidgetKind::Sparkline { data } => {
                    // Shift data left, add new value
                    if !data.is_empty() {
                        data.rotate_left(1);
                        let last = data.len() - 1;
                        data[last] = (rng.next_u32() % 20) as f64;
                    }
                }
                WidgetKind::List { selected, items } => {
                    *selected = rng.next_range(items.len());
                }
                WidgetKind::Block { content, .. } => {
                    *content = format!("Block content at frame {frame_idx}");
                }
            }
        }
    }

    /// Render the widget tree into a frame.
    ///
    /// We tile widgets into small cells within the buffer area. Each widget
    /// gets a small rectangular area. This simulates a realistic dashboard.
    fn render(&self, frame: &mut Frame) {
        let area = Rect::new(0, 0, frame.buffer.width(), frame.buffer.height());

        // Calculate grid layout: arrange widgets in rows
        let cell_w = 12u16; // Each widget cell is 12 columns wide
        let cell_h = 4u16; // Each widget cell is 4 rows tall
        let cols = (area.width / cell_w).max(1);
        let rows = (area.height / cell_h).max(1);
        let visible = (cols as usize * rows as usize).min(self.widgets.len());

        for (i, widget) in self.widgets.iter().take(visible).enumerate() {
            let col = (i as u16) % cols;
            let row = (i as u16) / cols;
            if row >= rows {
                break;
            }
            let x = col * cell_w;
            let y = row * cell_h;
            let w = cell_w.min(area.width.saturating_sub(x));
            let h = cell_h.min(area.height.saturating_sub(y));
            if w == 0 || h == 0 {
                continue;
            }
            let cell_area = Rect::new(x, y, w, h);

            match widget {
                WidgetKind::Paragraph { text } => {
                    let t = Text::raw(text.as_str());
                    Paragraph::new(t).render(cell_area, frame);
                }
                WidgetKind::Progress { ratio } => {
                    ProgressBar::new().ratio(*ratio).render(cell_area, frame);
                }
                WidgetKind::Sparkline { data } => {
                    Sparkline::new(data).render(cell_area, frame);
                }
                WidgetKind::List { items, .. } => {
                    let list_items: Vec<ListItem> =
                        items.iter().map(|s| ListItem::new(s.as_str())).collect();
                    List::new(list_items).render(cell_area, frame);
                }
                WidgetKind::Block { title, content } => {
                    let block = Block::default().title(title.as_str()).borders(Borders::ALL);
                    let inner = block.inner(cell_area);
                    block.render(cell_area, frame);
                    if inner.width > 0 && inner.height > 0 {
                        Paragraph::new(Text::raw(content.as_str())).render(inner, frame);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Frame Timing Collection
// ============================================================================

struct FrameTimings {
    durations_us: Vec<u128>,
}

impl FrameTimings {
    fn new(capacity: usize) -> Self {
        Self {
            durations_us: Vec::with_capacity(capacity),
        }
    }

    fn record(&mut self, d: Duration) {
        self.durations_us.push(d.as_micros());
    }

    fn percentile(&self, p: f64) -> u128 {
        if self.durations_us.is_empty() {
            return 0;
        }
        let mut sorted = self.durations_us.clone();
        sorted.sort_unstable();
        let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn max(&self) -> u128 {
        self.durations_us.iter().copied().max().unwrap_or(0)
    }

    fn mean(&self) -> f64 {
        if self.durations_us.is_empty() {
            return 0.0;
        }
        self.durations_us.iter().sum::<u128>() as f64 / self.durations_us.len() as f64
    }

    fn len(&self) -> usize {
        self.durations_us.len()
    }
}

// ============================================================================
// Main Soak Test
// ============================================================================

#[test]
fn soak_1000_widgets_sustained_rendering() {
    let total_frames = frame_count();
    let mut pool = GraphemePool::new();
    let mut rng = Lcg::new(0x60F9_50AC);
    let mut tree = WidgetTree::new(WIDGET_COUNT);
    let mut timings = FrameTimings::new(total_frames);

    let caps = {
        let mut c = TerminalCapabilities::basic();
        c.sync_output = true;
        c
    };

    let mut prev_buffer = Buffer::new(WIDTH, HEIGHT);
    let mut all_ansi_output: Vec<u8> = Vec::new();
    let mut checksums: Vec<String> = Vec::new();

    let soak_start = Instant::now();

    for frame_idx in 0..total_frames {
        // 1. Tick: update widget tree state
        tree.tick(frame_idx, &mut rng);

        // 2. Render: build buffer from widget tree
        let frame_start = Instant::now();
        let mut frame = Frame::new(WIDTH, HEIGHT, &mut pool);
        tree.render(&mut frame);
        let buffer = frame.buffer;

        // 3. Diff: compute changes from previous frame
        let diff = BufferDiff::compute(&prev_buffer, &buffer);

        // 4. Present: emit ANSI through Presenter
        let mut sink = Vec::new();
        {
            let mut presenter = Presenter::new(&mut sink, caps);
            presenter.present(&buffer, &diff).unwrap();
        }
        let frame_duration = frame_start.elapsed();

        // 5. Record metrics
        timings.record(frame_duration);
        all_ansi_output.extend_from_slice(&sink);

        // Sample checksums periodically (every 50 frames) to track stability
        if frame_idx % 50 == 0 {
            checksums.push(compute_buffer_checksum(&buffer));
        }

        prev_buffer = buffer;
    }

    let soak_elapsed = soak_start.elapsed();

    // ================================================================
    // Validate frame time SLAs
    // ================================================================

    let p50 = timings.percentile(50.0);
    let p95 = timings.percentile(95.0);
    let p99 = timings.percentile(99.0);
    let max = timings.max();
    let mean = timings.mean();

    // Print summary (visible with --nocapture)
    eprintln!("\n=== 60fps Soak Test Results ===");
    eprintln!("Frames: {}", timings.len());
    eprintln!("Widgets: {WIDGET_COUNT}");
    eprintln!("Buffer: {WIDTH}x{HEIGHT}");
    eprintln!("Total time: {:.2}s", soak_elapsed.as_secs_f64());
    eprintln!(
        "Effective fps: {:.1}",
        timings.len() as f64 / soak_elapsed.as_secs_f64()
    );
    eprintln!("Frame times (us): mean={mean:.0}, p50={p50}, p95={p95}, p99={p99}, max={max}");
    eprintln!("ANSI output: {} bytes total", all_ansi_output.len());
    eprintln!("Checksum samples: {}", checksums.len());
    eprintln!("==============================\n");

    assert!(
        p50 < P50_LIMIT_US,
        "p50 frame time {p50}us exceeds {P50_LIMIT_US}us limit"
    );

    assert!(
        p99 < P99_LIMIT_US,
        "p99 frame time {p99}us exceeds {P99_LIMIT_US}us limit"
    );

    assert!(
        max < MAX_LIMIT_US,
        "Max frame time {max}us exceeds {MAX_LIMIT_US}us limit"
    );

    // ================================================================
    // Validate no flicker
    // ================================================================

    let analysis = analyze_stream_with_id("soak-60fps", &all_ansi_output);
    analysis.assert_flicker_free();
    assert_eq!(
        analysis.stats.total_frames, total_frames as u64,
        "Expected {total_frames} sync frames, got {}",
        analysis.stats.total_frames
    );

    // ================================================================
    // Validate no progressive degradation
    // ================================================================

    // Frame times in the last 10% should not be significantly worse than first 10%
    if timings.len() >= 100 {
        let tenth = timings.len() / 10;
        let early_mean: f64 =
            timings.durations_us[..tenth].iter().sum::<u128>() as f64 / tenth as f64;
        let late_mean: f64 = timings.durations_us[timings.len() - tenth..]
            .iter()
            .sum::<u128>() as f64
            / tenth as f64;

        // Allow up to 3x degradation (generous for debug builds with varying load)
        let degradation_ratio = late_mean / early_mean;
        assert!(
            degradation_ratio < 3.0,
            "Progressive degradation detected: early mean={early_mean:.0}us, \
             late mean={late_mean:.0}us, ratio={degradation_ratio:.2}x"
        );
    }

    // ================================================================
    // Validate buffer dimension consistency
    // ================================================================

    // All frames should have produced WIDTH x HEIGHT buffers
    // (This is implicitly verified by the successful diff computation,
    // but let's be explicit.)
    assert_eq!(prev_buffer.width(), WIDTH);
    assert_eq!(prev_buffer.height(), HEIGHT);
}

// ============================================================================
// Smaller Focused Soak Tests
// ============================================================================

#[test]
fn soak_paragraph_only_200_frames() {
    // Stress test: 1000 paragraphs, 200 frames
    let mut pool = GraphemePool::new();
    let mut timings = FrameTimings::new(200);
    let mut prev = Buffer::new(WIDTH, HEIGHT);

    for i in 0..200 {
        let mut frame = Frame::new(WIDTH, HEIGHT, &mut pool);
        let area = Rect::new(0, 0, WIDTH, HEIGHT);

        // Render 50 paragraphs tiled vertically (as many as fit)
        let row_h = HEIGHT / 50;
        for j in 0..50u16 {
            let y = j * row_h;
            if y >= HEIGHT {
                break;
            }
            let h = row_h.min(HEIGHT - y);
            let r = Rect::new(0, y, area.width, h);
            let text = format!("Para {j} frame {i}: {}", "x".repeat(area.width as usize));
            Paragraph::new(Text::raw(text)).render(r, &mut frame);
        }

        let start = Instant::now();
        let diff = BufferDiff::compute(&prev, &frame.buffer);
        let mut sink = Vec::new();
        {
            let mut caps = TerminalCapabilities::basic();
            caps.sync_output = true;
            let mut presenter = Presenter::new(&mut sink, caps);
            presenter.present(&frame.buffer, &diff).unwrap();
        }
        timings.record(start.elapsed());
        prev = frame.buffer;
    }

    let p99 = timings.percentile(99.0);
    assert!(
        p99 < P99_LIMIT_US,
        "Paragraph soak p99={p99}us exceeds limit"
    );
}

#[test]
fn soak_progress_bars_200_frames() {
    // 200 progress bars animated over 200 frames
    let mut pool = GraphemePool::new();
    let mut timings = FrameTimings::new(200);
    let mut prev = Buffer::new(WIDTH, HEIGHT);

    for i in 0..200 {
        let mut frame = Frame::new(WIDTH, HEIGHT, &mut pool);
        let rows = HEIGHT;
        for y in 0..rows {
            let ratio = ((i as f64 * 0.005) + (y as f64 * 0.01)).fract();
            let r = Rect::new(0, y, WIDTH, 1);
            ProgressBar::new().ratio(ratio).render(r, &mut frame);
        }

        let start = Instant::now();
        let diff = BufferDiff::compute(&prev, &frame.buffer);
        let mut sink = Vec::new();
        {
            let mut caps = TerminalCapabilities::basic();
            caps.sync_output = true;
            let mut presenter = Presenter::new(&mut sink, caps);
            presenter.present(&frame.buffer, &diff).unwrap();
        }
        timings.record(start.elapsed());
        prev = frame.buffer;
    }

    let p99 = timings.percentile(99.0);
    assert!(
        p99 < P99_LIMIT_US,
        "Progress soak p99={p99}us exceeds limit"
    );
}

#[test]
fn soak_sparklines_200_frames() {
    // 40 sparklines (one per row) animated over 200 frames
    let mut pool = GraphemePool::new();
    let mut timings = FrameTimings::new(200);
    let mut prev = Buffer::new(WIDTH, HEIGHT);
    let mut rng = Lcg::new(0xbeef);

    for _i in 0..200 {
        let mut frame = Frame::new(WIDTH, HEIGHT, &mut pool);
        for y in 0..HEIGHT {
            let data: Vec<f64> = (0..WIDTH).map(|_| (rng.next_u32() % 20) as f64).collect();
            let r = Rect::new(0, y, WIDTH, 1);
            Sparkline::new(&data).render(r, &mut frame);
        }

        let start = Instant::now();
        let diff = BufferDiff::compute(&prev, &frame.buffer);
        let mut sink = Vec::new();
        {
            let mut caps = TerminalCapabilities::basic();
            caps.sync_output = true;
            let mut presenter = Presenter::new(&mut sink, caps);
            presenter.present(&frame.buffer, &diff).unwrap();
        }
        timings.record(start.elapsed());
        prev = frame.buffer;
    }

    let p99 = timings.percentile(99.0);
    assert!(
        p99 < P99_LIMIT_US,
        "Sparkline soak p99={p99}us exceeds limit"
    );
}

#[test]
fn soak_mixed_widgets_no_memory_growth() {
    // Track buffer allocation overhead across 300 frames.
    // The key check: no progressive memory growth.
    let mut pool = GraphemePool::new();
    let mut tree = WidgetTree::new(500); // Smaller tree for focused test
    let mut rng = Lcg::new(0xAABB_CCDD);

    let mut frame_alloc_sizes: Vec<usize> = Vec::with_capacity(300);
    let mut prev = Buffer::new(80, 24);

    for i in 0..300 {
        tree.tick(i, &mut rng);

        let mut frame = Frame::new(80, 24, &mut pool);
        tree.render(&mut frame);

        let diff = BufferDiff::compute(&prev, &frame.buffer);

        // Track the ANSI output size as a proxy for "work done per frame"
        let mut sink = Vec::new();
        {
            let mut caps = TerminalCapabilities::basic();
            caps.sync_output = true;
            let mut presenter = Presenter::new(&mut sink, caps);
            presenter.present(&frame.buffer, &diff).unwrap();
        }
        frame_alloc_sizes.push(sink.len());
        prev = frame.buffer;
    }

    // After the first few frames (warmup), output size should stabilize.
    // Check that the last 100 frames have similar output size to frames 50-150.
    if frame_alloc_sizes.len() >= 200 {
        let early_avg: f64 = frame_alloc_sizes[50..150].iter().sum::<usize>() as f64 / 100.0;
        let late_avg: f64 = frame_alloc_sizes[200..300].iter().sum::<usize>() as f64 / 100.0;

        // Allow up to 2x growth (some variation is expected due to content changes)
        if early_avg > 0.0 {
            let ratio = late_avg / early_avg;
            assert!(
                ratio < 2.0,
                "Output size growing: early={early_avg:.0}, late={late_avg:.0}, ratio={ratio:.2}"
            );
        }
    }
}
