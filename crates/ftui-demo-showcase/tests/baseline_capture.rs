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
//!
//! # `verify_no_regression` is advisory unless you ask otherwise
//!
//! Read this before treating a green run as evidence of no regression. By
//! default the check is **inert in two separate ways**:
//!
//! 1. `baseline_results.json` is gitignored, so on a fresh checkout there is no
//!    baseline and the test returns before measuring anything.
//! 2. With a baseline present, a detected regression is printed and the test
//!    still passes.
//!
//! Both are deliberate, and `verify_no_regression`'s own doc comment has always
//! said so: the numbers are machine-specific, a 10% p99 delta on a shared box
//! measures the box (`bd-lbugy`), and **the enforced performance gate is
//! `scripts/perf_regression_gate.sh` against `tests/baseline.json`**, not this
//! test. This module doc exists because that explanation sits below the
//! function while the reassuring name sits in the test output, and a passing
//! `verify_no_regression` line is what most people will actually see.
//!
//! `FTUI_BASELINE_STRICT=1` makes a regression here fail too. It is a
//! convenience for a quiet host whose baseline was captured on that same host,
//! not a replacement for the real gate; anywhere else it will flake.

use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::{ColorDepth, TerminalCapabilities};
use ftui_layout::{Constraint, Flex};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::Presenter;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const WARMUP_ITERS: u64 = 50;
const MEASURE_ITERS: u64 = 500;
const BASELINE_COLOR_DEPTH: ColorDepth = ColorDepth::TrueColor;

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
        let caps = TerminalCapabilities::builder()
            .color_depth(BASELINE_COLOR_DEPTH)
            .build();

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

/// Where the baseline cache lives.
///
/// `FTUI_BASELINE_PATH` overrides it. Without that override this path is fixed
/// under `CARGO_MANIFEST_DIR`, which makes the load and strict-mode paths
/// untestable: exercising them means writing a baseline, and the only baseline
/// you could write is the developer's real one.
fn baseline_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("FTUI_BASELINE_PATH") {
        return std::path::PathBuf::from(path);
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest_dir).join("tests/baseline_results.json")
}

/// Format version of the baseline cache.
///
/// Bump when the recorded provenance or the shape of `hot_paths` changes. An
/// older file is reported as [`IgnoreReason::VersionMismatch`], skipped once,
/// and rewritten by `capture_baselines` — never compared against.
const BASELINE_VERSION: &str = "1.1.0";

/// Why a baseline file was not usable for comparison.
///
/// Each variant is a decision to *skip*, never to fail. A baseline that cannot
/// be trusted is worse than no baseline: comparing against one captured on
/// another machine, at another color depth, or in an older format produces
/// confident numbers about nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IgnoreReason {
    /// No file at the path.
    Missing,
    /// File exists but is not valid JSON — typically a run killed mid-write,
    /// which is what the atomic write in [`write_cache`] exists to prevent.
    InvalidJson,
    /// No `terminal_color_depth` key at all: a pre-provenance file.
    MissingProvenance,
    /// Captured at a different color depth, so the ANSI volume differs.
    DepthMismatch { found: String },
    /// Captured by an older format version.
    VersionMismatch { found: String },
    /// Captured on a different host. Machine-to-machine comparison is the
    /// single biggest source of false regressions here.
    HostMismatch { found: Option<String> },
}

impl IgnoreReason {
    /// One line, addressed to whoever is reading a skipped run.
    fn describe(&self) -> String {
        match self {
            Self::Missing => "no baseline file".to_string(),
            Self::InvalidJson => "baseline is not valid JSON".to_string(),
            Self::MissingProvenance => {
                "baseline predates color-depth provenance".to_string()
            }
            Self::DepthMismatch { found } => format!(
                "baseline captured at color depth {found}, running at {}",
                BASELINE_COLOR_DEPTH.as_str()
            ),
            Self::VersionMismatch { found } => {
                format!("baseline format {found}, expected {BASELINE_VERSION}")
            }
            Self::HostMismatch { found } => format!(
                "baseline captured on {}, running on {}",
                found.as_deref().unwrap_or("<unknown host>"),
                current_host().unwrap_or_else(|| "<unknown host>".to_string()),
            ),
        }
    }
}

/// Host identity for provenance, or `None` when it cannot be determined.
///
/// `None` on both sides compares equal: a machine that cannot name itself is
/// not evidence that two baselines came from different machines.
fn current_host() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
        })
}

/// Load a baseline, classifying every reason it might not be comparable.
fn load_cache(path: &std::path::Path) -> Result<Value, IgnoreReason> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Err(IgnoreReason::Missing);
    };
    let Ok(baseline) = serde_json::from_str::<Value>(&contents) else {
        return Err(IgnoreReason::InvalidJson);
    };

    // Version first: an older format may not have the keys checked below, and
    // reporting "missing provenance" for a file whose format simply predates it
    // sends the reader looking for the wrong problem.
    match baseline.get("version").and_then(Value::as_str) {
        Some(BASELINE_VERSION) => {}
        Some(found) => {
            return Err(IgnoreReason::VersionMismatch {
                found: found.to_string(),
            });
        }
        None => {
            return Err(IgnoreReason::VersionMismatch {
                found: "<none>".to_string(),
            });
        }
    }

    match baseline.get("terminal_color_depth").and_then(Value::as_str) {
        Some(found) if found == BASELINE_COLOR_DEPTH.as_str() => {}
        Some(found) => {
            return Err(IgnoreReason::DepthMismatch {
                found: found.to_string(),
            });
        }
        None => return Err(IgnoreReason::MissingProvenance),
    }

    let recorded_host = baseline
        .get("hostname")
        .and_then(Value::as_str)
        .map(str::to_string);
    if recorded_host != current_host() {
        return Err(IgnoreReason::HostMismatch {
            found: recorded_host,
        });
    }

    Ok(baseline)
}

/// Write the baseline so no reader can ever observe a partial file.
///
/// Write to a sibling temp path, then rename. `rename` within a directory is
/// atomic on every platform this runs on, so a killed run leaves either the old
/// baseline or the new one — never the half-written file that would otherwise
/// read as [`IgnoreReason::InvalidJson`] forever afterwards.
fn write_cache(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

fn validate_baseline_color_depth(baseline: &Value) -> Result<(), String> {
    let expected = BASELINE_COLOR_DEPTH.as_str();
    match baseline.get("terminal_color_depth").and_then(Value::as_str) {
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => Err(format!(
            "baseline terminal_color_depth mismatch: expected {expected}, found {actual}"
        )),
        None => Err(format!(
            "baseline missing terminal_color_depth provenance (expected {expected})"
        )),
    }
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
        "version": BASELINE_VERSION,
        "generated_at": timestamp(),
        "terminal_color_depth": BASELINE_COLOR_DEPTH.as_str(),
        "hostname": current_host(),
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

    // Rewrite when asked, and whenever the file on disk is not comparable —
    // that is the "ignored once, then rewritten" half of the provenance rule.
    // Without it a stale-format baseline is skipped by every future run and
    // never replaced, so the comparison stays silently disabled forever.
    let path = baseline_path();
    let unusable = load_cache(&path).err();
    if std::env::var("CAPTURE_BASELINE").is_ok() || unusable.is_some() {
        if let Some(reason) = &unusable {
            eprintln!("Replacing baseline ({}).", reason.describe());
        }
        write_cache(&path, &pretty).expect("failed to write baseline_results.json");
        eprintln!("Baseline results written to {}", path.display());
    }
}

/// Report whether current performance regressed from the locally captured
/// baseline.
///
/// This test is self-contained: it never depends on `capture_baselines`
/// having run first in the same binary, and a missing, unparsable, or
/// stale baseline file (one without the color-depth provenance that the
/// current capture format records) is logged and skipped rather than
/// failing the run. The file is gitignored and machine-specific, so a stale
/// copy on a developer box must not turn into a red test; the enforced
/// performance gate lives in `scripts/perf_regression_gate.sh` against
/// `tests/baseline.json`. Regressions found here are reported on stderr for
/// humans, not asserted, because wall-clock numbers on shared runners are
/// not a reliable pass/fail signal.
#[test]
fn verify_no_regression() {
    let path = baseline_path();
    let baseline = match load_cache(&path) {
        Ok(baseline) => baseline,
        Err(reason) => {
            eprintln!(
                "Skipping regression check: {} ({}). \
                 Regenerate with CAPTURE_BASELINE=1 cargo test -p ftui-demo-showcase --test baseline_capture",
                reason.describe(),
                path.display()
            );
            return;
        }
    };

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
        assert!(
            !strict_baselines(),
            "{} performance regression(s) against {}:\n  {}\n\
             Set FTUI_BASELINE_STRICT=0 to make this advisory again, or regenerate the \
             baseline with CAPTURE_BASELINE=1 if the change is intended.",
            regressions.len(),
            path.display(),
            regressions.join("\n  "),
        );
    } else {
        eprintln!("No regressions detected.");
    }
}

/// Whether a detected regression should fail the test.
///
/// **Advisory by default, and that is deliberate.** `baseline_results.json` is
/// gitignored and machine-specific, so on a shared or loaded box a 10% p99
/// delta measures the box as often as the code — the same trap that made
/// `perf_catalog_lookup_latency` a flake in the mandatory suite (`bd-lbugy`).
/// A gate that fails under this project's normal working conditions is not a
/// gate; it teaches people to re-run and shrug.
///
/// `FTUI_BASELINE_STRICT=1` turns the comparison into a real assertion, for a
/// perf lane on a quiet host where the baseline was captured on that same
/// host. That is the only configuration in which these numbers mean anything.
fn strict_baselines() -> bool {
    matches!(
        std::env::var("FTUI_BASELINE_STRICT").as_deref(),
        Ok("1") | Ok("true")
    )
}

#[test]
fn baseline_color_depth_provenance_accepts_expected_depth() {
    let baseline = json!({"terminal_color_depth": "truecolor"});

    assert_eq!(validate_baseline_color_depth(&baseline), Ok(()));
}

#[test]
fn baseline_color_depth_provenance_rejects_missing_and_mismatched_depth() {
    let missing = json!({});
    let mismatched = json!({"terminal_color_depth": "ansi256"});

    assert!(
        validate_baseline_color_depth(&missing)
            .expect_err("missing provenance must fail closed")
            .contains("missing terminal_color_depth provenance")
    );
    assert!(
        validate_baseline_color_depth(&mismatched)
            .expect_err("mismatched provenance must fail closed")
            .contains("expected truecolor, found ansi256")
    );
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

// ============================================================================
// Cache provenance (bd-g0k8b; the list bd-g00-root-epic-ewths.9.2 specifies)
//
// These write to paths under the crate's own `target/`, never to the real
// baseline: `load_cache` decisions are the thing under test, and the only
// baseline an un-overridable path could reach is a developer's. Nothing here
// deletes what it wrote — each case uses its own filename so runs do not
// collide, and the files are evidence if a case fails.
// ============================================================================

/// A distinct path per case, under `target/` so it is already gitignored.
fn fixture_path(case: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/baseline_fixtures");
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir.join(format!("{case}.json"))
}

/// A baseline that `load_cache` accepts, as the basis for mutating one field.
fn good_baseline() -> Value {
    json!({
        "version": BASELINE_VERSION,
        "generated_at": timestamp(),
        "terminal_color_depth": BASELINE_COLOR_DEPTH.as_str(),
        "hostname": current_host(),
        "hot_paths": {},
    })
}

fn write_fixture(case: &str, value: &Value) -> std::path::PathBuf {
    let path = fixture_path(case);
    write_cache(&path, &serde_json::to_string_pretty(value).unwrap()).expect("write fixture");
    path
}

#[test]
fn load_cache_missing_file() {
    // A name nothing else writes, so this needs no cleanup and cannot race.
    let path = fixture_path("never_written_by_anything");
    assert_eq!(load_cache(&path), Err(IgnoreReason::Missing));
}

#[test]
fn load_cache_invalid_json() {
    let path = fixture_path("invalid_json");
    write_cache(&path, "{").expect("write fixture");
    assert_eq!(load_cache(&path), Err(IgnoreReason::InvalidJson));
}

#[test]
fn load_cache_missing_provenance() {
    // Version is present and current, so the only thing wrong is the missing
    // depth key — otherwise this would report VersionMismatch and the test
    // would pass for the wrong reason.
    let mut baseline = good_baseline();
    baseline.as_object_mut().unwrap().remove("terminal_color_depth");
    let path = write_fixture("missing_provenance", &baseline);
    assert_eq!(load_cache(&path), Err(IgnoreReason::MissingProvenance));
}

#[test]
fn load_cache_depth_mismatch() {
    let mut baseline = good_baseline();
    baseline["terminal_color_depth"] = json!("ansi256");
    let path = write_fixture("depth_mismatch", &baseline);
    assert_eq!(
        load_cache(&path),
        Err(IgnoreReason::DepthMismatch {
            found: "ansi256".to_string()
        })
    );
}

#[test]
fn load_cache_version_mismatch() {
    let mut baseline = good_baseline();
    baseline["version"] = json!("1.0.0");
    let path = write_fixture("version_mismatch", &baseline);
    assert_eq!(
        load_cache(&path),
        Err(IgnoreReason::VersionMismatch {
            found: "1.0.0".to_string()
        })
    );

    // A file with no version at all is the same decision, not a different one.
    let mut unversioned = good_baseline();
    unversioned.as_object_mut().unwrap().remove("version");
    let path = write_fixture("version_absent", &unversioned);
    assert!(matches!(
        load_cache(&path),
        Err(IgnoreReason::VersionMismatch { .. })
    ));
}

#[test]
fn load_cache_host_mismatch() {
    let mut baseline = good_baseline();
    baseline["hostname"] = json!("some-other-host-that-is-not-this-one");
    let path = write_fixture("host_mismatch", &baseline);
    assert_eq!(
        load_cache(&path),
        Err(IgnoreReason::HostMismatch {
            found: Some("some-other-host-that-is-not-this-one".to_string())
        })
    );
}

#[test]
fn load_cache_accepts_matching_provenance() {
    // The positive case, so the six negatives above cannot all be passing
    // because `load_cache` rejects everything.
    let path = write_fixture("good", &good_baseline());
    assert!(load_cache(&path).is_ok(), "{:?}", load_cache(&path));
}

#[test]
fn write_cache_atomic_leaves_no_tmp() {
    let path = fixture_path("atomic");
    write_cache(&path, "{\"version\":\"x\"}").expect("write");
    assert!(path.exists(), "the baseline must exist after a write");
    assert!(
        !path.with_extension("json.tmp").exists(),
        "the temp file must be renamed away, not left beside the baseline"
    );
}

#[test]
fn write_cache_replaces_previous_contents_wholly() {
    // Rename-over, not truncate-and-write: a shorter payload must not leave a
    // tail of the previous one, which would parse as invalid JSON forever.
    let path = fixture_path("replace");
    write_cache(&path, &"x".repeat(4096)).expect("write long");
    write_cache(&path, "{}").expect("write short");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
}

#[test]
fn baseline_path_defaults_under_the_crate() {
    // Only the default is asserted. The `FTUI_BASELINE_PATH` override cannot be
    // tested here: `std::env::set_var` is `unsafe` in Rust 2024 and this
    // workspace is `#![forbid(unsafe_code)]`, and setting a process-global from
    // one test would race every other test in this binary regardless. The
    // override is exercised by callers that set it in the environment — that
    // is a real limit of this test, not a claim that the override is verified.
    let path = baseline_path();
    assert!(
        std::env::var("FTUI_BASELINE_PATH").is_ok()
            || path.ends_with("tests/baseline_results.json"),
        "unexpected default baseline path: {}",
        path.display()
    );
}
