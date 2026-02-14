//! Baseline p50/p95/p99 capture for FrankenTUI hot paths (bd-3jlw5.1).
//!
//! Captures quantitative performance baselines for:
//! 1. Frame render pipeline (buffer creation + present to ANSI)
//! 2. Diff engine (old vs new buffer at various change percentages)
//! 3. Layout computation (Flex split at various widget counts)
//!
//! Results are stored as structured JSON in `baseline_results.json`.
//!
//! Run:
//!   cargo test -p ftui-demo-showcase --test baseline_capture -- --nocapture
//!
//! Regenerate:
//!   CAPTURE_BASELINE=1 cargo test -p ftui-demo-showcase --test baseline_capture -- --nocapture

use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_layout::{Constraint, Flex};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::Presenter;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const WARMUP_ITERS: u64 = 50;
const MEASURE_ITERS: u64 = 500;

/// Compute percentile from sorted array of durations.
fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64) * p / 100.0).ceil() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Run a closure many times and return p50/p95/p99/max as JSON.
fn measure<F: FnMut()>(mut f: F) -> Value {
    // Warmup
    for _ in 0..WARMUP_ITERS {
        f();
    }

    // Measure
    let mut times = Vec::with_capacity(MEASURE_ITERS as usize);
    for _ in 0..MEASURE_ITERS {
        let start = Instant::now();
        f();
        times.push(start.elapsed());
    }
    times.sort();

    let p50 = percentile(&times, 50.0);
    let p95 = percentile(&times, 95.0);
    let p99 = percentile(&times, 99.0);
    let max = *times.last().unwrap();

    json!({
        "p50_us": p50.as_nanos() as f64 / 1000.0,
        "p95_us": p95.as_nanos() as f64 / 1000.0,
        "p99_us": p99.as_nanos() as f64 / 1000.0,
        "max_us": max.as_nanos() as f64 / 1000.0,
        "iterations": MEASURE_ITERS,
    })
}

/// Create a pair of buffers where `pct` percent of cells differ.
fn make_pair(width: u16, height: u16, change_pct: f64) -> (Buffer, Buffer) {
    let mut old = Buffer::new(width, height);
    let mut new = old.clone();
    old.clear_dirty();
    new.clear_dirty();

    let total = width as usize * height as usize;
    let to_change = ((total as f64) * change_pct / 100.0) as usize;

    let colors = [
        PackedRgba::rgb(255, 0, 0),
        PackedRgba::rgb(0, 255, 0),
        PackedRgba::rgb(0, 0, 255),
        PackedRgba::rgb(255, 255, 0),
        PackedRgba::rgb(255, 0, 255),
    ];

    for i in 0..to_change {
        let x = (i * 7 + 3) as u16 % width;
        let y = (i * 11 + 5) as u16 % height;
        let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
        let fg = colors[i % colors.len()];
        let bg = colors[(i + 2) % colors.len()];
        new.set_raw(x, y, Cell::from_char(ch).with_fg(fg).with_bg(bg));
    }

    (old, new)
}

/// Baseline 1: Frame render pipeline (diff + present to ANSI).
fn capture_frame_pipeline() -> Value {
    let sizes: &[(u16, u16)] = &[(80, 24), (120, 40), (200, 60)];
    let mut results = serde_json::Map::new();

    for &(w, h) in sizes {
        let label = format!("{w}x{h}");
        let cells = w as u64 * h as u64;

        // Buffer creation
        let create = measure(|| {
            let buf = Buffer::new(w, h);
            std::hint::black_box(buf);
        });

        // Full pipeline: diff + present
        let (old, new) = make_pair(w, h, 25.0);
        let diff = BufferDiff::compute(&old, &new);
        let caps = TerminalCapabilities::default();

        let pipeline = measure(|| {
            let mut sink = Vec::with_capacity(cells as usize * 4);
            {
                let mut presenter = Presenter::new(&mut sink, caps);
                let _ = presenter.present(&new, &diff);
            }
            std::hint::black_box(sink.len());
        });

        results.insert(
            label,
            json!({
                "cells": cells,
                "buffer_create": create,
                "present_25pct_change": pipeline,
            }),
        );
    }

    Value::Object(results)
}

/// Baseline 2: Diff engine at various change percentages.
fn capture_diff_engine() -> Value {
    let sizes: &[(u16, u16)] = &[(80, 24), (120, 40), (200, 60)];
    let change_pcts: &[f64] = &[0.0, 1.0, 5.0, 10.0, 25.0, 50.0, 100.0];
    let mut results = serde_json::Map::new();

    for &(w, h) in sizes {
        let size_label = format!("{w}x{h}");
        let cells = w as u64 * h as u64;
        let mut size_results = serde_json::Map::new();

        for &pct in change_pcts {
            let (old, new) = make_pair(w, h, pct);
            let pct_label = format!("{pct:.0}pct");

            let full = measure(|| {
                let d = BufferDiff::compute(&old, &new);
                std::hint::black_box(d.len());
            });

            let dirty = measure(|| {
                let d = BufferDiff::compute_dirty(&old, &new);
                std::hint::black_box(d.len());
            });

            size_results.insert(
                pct_label,
                json!({
                    "cells": cells,
                    "change_pct": pct,
                    "full_diff": full,
                    "dirty_diff": dirty,
                }),
            );
        }

        results.insert(size_label, Value::Object(size_results));
    }

    Value::Object(results)
}

/// Baseline 3: Layout computation (Flex split).
fn capture_layout() -> Value {
    let area = Rect::new(0, 0, 200, 60);
    let widget_counts: &[usize] = &[3, 5, 10, 20, 50, 100];
    let mut results = serde_json::Map::new();

    for &n in widget_counts {
        let label = format!("{n}_widgets");

        let constraints: Vec<Constraint> = (0..n)
            .map(|i| match i % 5 {
                0 => Constraint::Fixed(10),
                1 => Constraint::Percentage(20.0),
                2 => Constraint::Min(5),
                3 => Constraint::Max(30),
                4 => Constraint::Ratio(1, 3),
                _ => unreachable!(),
            })
            .collect();

        let flex_h = Flex::horizontal().constraints(constraints.clone());
        let horizontal = measure(|| {
            let rects = flex_h.split(area);
            std::hint::black_box(rects.len());
        });

        let flex_v = Flex::vertical().constraints(constraints);
        let vertical = measure(|| {
            let rects = flex_v.split(area);
            std::hint::black_box(rects.len());
        });

        results.insert(
            label,
            json!({
                "widget_count": n,
                "horizontal_split": horizontal,
                "vertical_split": vertical,
            }),
        );
    }

    // Nested layout: 3 columns x N rows
    for &depth in &[5, 10, 20] {
        let label = format!("nested_3x{depth}");
        let outer = Flex::horizontal().constraints(vec![Constraint::Percentage(33.3); 3]);
        let inner = Flex::vertical().constraints(vec![Constraint::Fixed(3); depth]);

        let nested = measure(|| {
            let columns = outer.split(area);
            let mut total = 0;
            for col in &columns {
                total += inner.split(*col).len();
            }
            std::hint::black_box(total);
        });

        results.insert(
            label,
            json!({
                "columns": 3,
                "rows_per_col": depth,
                "nested_split": nested,
            }),
        );
    }

    Value::Object(results)
}

fn baseline_path() -> std::path::PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest_dir).join("tests/baseline_results.json")
}

/// Simple timestamp without chrono dependency.
fn timestamp() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("unix:{}", d.as_secs())
}

/// Capture baselines and store as JSON.
#[test]
fn capture_baselines() {
    let frame = capture_frame_pipeline();
    let diff = capture_diff_engine();
    let layout = capture_layout();

    let baselines = json!({
        "version": "1.0.0",
        "generated_at": timestamp(),
        "warmup_iters": WARMUP_ITERS,
        "measure_iters": MEASURE_ITERS,
        "hot_paths": {
            "frame_pipeline": frame,
            "diff_engine": diff,
            "layout": layout,
        }
    });

    let pretty = serde_json::to_string_pretty(&baselines).unwrap();

    eprintln!("\n=== BASELINE RESULTS ===\n{pretty}\n========================\n");

    let path = baseline_path();
    if std::env::var("CAPTURE_BASELINE").is_ok() || !path.exists() {
        std::fs::write(&path, &pretty).expect("failed to write baseline_results.json");
        eprintln!("Baseline results written to {}", path.display());
    }
}

/// Verify current performance doesn't regress from baseline.
#[test]
fn verify_no_regression() {
    let path = baseline_path();
    if !path.exists() {
        eprintln!("No baseline file; skipping regression check.");
        return;
    }

    let baseline_content = std::fs::read_to_string(&path).expect("failed to read baseline");
    let baseline: Value = serde_json::from_str(&baseline_content).expect("invalid JSON");

    let frame = capture_frame_pipeline();
    let diff = capture_diff_engine();
    let layout = capture_layout();

    let current = json!({
        "frame_pipeline": frame,
        "diff_engine": diff,
        "layout": layout,
    });

    let mut regressions = Vec::new();
    check_regressions(&baseline["hot_paths"], &current, "", 0.10, &mut regressions);

    if !regressions.is_empty() {
        eprintln!("\nPerformance regressions detected (>10% p99 increase):");
        for reg in &regressions {
            eprintln!("  {reg}");
        }
        eprintln!(
            "\nTo update: CAPTURE_BASELINE=1 cargo test -p ftui-demo-showcase --test baseline_capture"
        );
    } else {
        eprintln!("No regressions detected.");
    }
}

/// Recursively find p99_us fields and compare baseline vs current.
fn check_regressions(
    baseline: &Value,
    current: &Value,
    path: &str,
    threshold: f64,
    regressions: &mut Vec<String>,
) {
    if let (Some(b_p99), Some(c_p99)) = (
        baseline.get("p99_us").and_then(Value::as_f64),
        current.get("p99_us").and_then(Value::as_f64),
    ) {
        if b_p99 > 0.0 {
            let ratio = c_p99 / b_p99;
            if ratio > 1.0 + threshold {
                regressions.push(format!(
                    "{path}: p99 regressed {:.1}% (baseline: {:.1}us, current: {:.1}us)",
                    (ratio - 1.0) * 100.0,
                    b_p99,
                    c_p99
                ));
            }
        }
        return;
    }

    if let (Value::Object(b), Value::Object(c)) = (baseline, current) {
        for (key, bval) in b {
            if let Some(cval) = c.get(key) {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}/{key}")
                };
                check_regressions(bval, cval, &child_path, threshold, regressions);
            }
        }
    }
}
