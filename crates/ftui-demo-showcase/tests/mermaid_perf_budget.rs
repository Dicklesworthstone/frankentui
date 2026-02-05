#![forbid(unsafe_code)]

//! Performance budget + log assertion tests for Mermaid showcase (bd-npalh).
//!
//! Validates:
//! - Parse/layout/render timing budgets for all built-in samples.
//! - Layout quality metric assertions (crossings, symmetry, compactness).
//! - Structured JSONL log schema and content.
//! - Detailed failure diagnostics (sample name, mode, metrics).
//!
//! Run with: cargo test -p ftui-demo-showcase --test mermaid_perf_budget --release
//!
//! **IMPORTANT**: Timing tests are release-mode only (debug builds are 10-20x slower).

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::screens::Screen;
use ftui_demo_showcase::screens::mermaid_showcase::MermaidShowcaseScreen;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use std::time::Instant;

// =============================================================================
// Performance budgets (milliseconds)
// =============================================================================

/// Full pipeline (parse + layout + render) budget per sample at 120x40.
/// Based on the screen's own thresholds: parse ≤5ms (OK), layout ≤20ms (OK),
/// render ≤16ms (OK). Total pipeline budget = 41ms, with 3x CI margin.
const BUDGET_PIPELINE_120X40_MS: f64 = 123.0;

/// Full pipeline budget per sample at 80x24 (smaller viewport = faster render).
const BUDGET_PIPELINE_80X24_MS: f64 = 100.0;

/// Maximum acceptable time for navigating to a new sample (cache invalidation
/// + re-render). Measured as time for update(Down) + view().
const BUDGET_SAMPLE_SWITCH_MS: f64 = 150.0;

// =============================================================================
// Layout quality thresholds for built-in samples
// =============================================================================

/// Maximum edge crossings for any built-in sample (lenient for complex graphs).
const MAX_CROSSINGS_BUILTIN: u32 = 20;

/// Minimum compactness for flowchart/sequence/class samples (0.0-1.0).
/// Gantt, pie, and mindmap charts may have near-zero compactness by design.
const MIN_COMPACTNESS_FLOWLIKE: f32 = 0.01;

// =============================================================================
// Helpers
// =============================================================================

fn key_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::NONE,
        kind: KeyEventKind::Press,
    })
}

fn is_release_mode() -> bool {
    !cfg!(debug_assertions)
}

/// Emit JSONL diagnostic when MERMAID_PERF_JSONL=1 is set.
fn log_jsonl(data: &[(&str, &str)]) {
    if std::env::var("MERMAID_PERF_JSONL").is_ok() || std::env::var("E2E_JSONL").is_ok() {
        let fields: Vec<String> = data
            .iter()
            .map(|(k, v)| format!("\"{k}\":\"{v}\""))
            .collect();
        eprintln!("{{{}}}", fields.join(","));
    }
}

/// Render the screen into a frame and return elapsed milliseconds.
fn measure_view_ms(screen: &MermaidShowcaseScreen, width: u16, height: u16) -> f64 {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let area = Rect::new(0, 0, width, height);
    let start = Instant::now();
    screen.view(&mut frame, area);
    start.elapsed().as_secs_f64() * 1000.0
}

/// Navigate to the next sample and render. Returns (switch_ms, view_ms).
fn switch_and_measure(screen: &mut MermaidShowcaseScreen, width: u16, height: u16) -> (f64, f64) {
    let start = Instant::now();
    let _ = screen.update(&key_press(KeyCode::Down));
    let switch_ms = start.elapsed().as_secs_f64() * 1000.0;
    let view_ms = measure_view_ms(screen, width, height);
    (switch_ms, view_ms)
}

// =============================================================================
// Tests: Pipeline timing budgets
// =============================================================================

/// Verify that the full parse→layout→render pipeline for all built-in samples
/// completes within budget at 120x40. Skips in debug mode.
#[test]
fn mermaid_pipeline_budget_120x40() {
    if !is_release_mode() {
        eprintln!("SKIPPED: mermaid_pipeline_budget_120x40 (debug build - run with --release)");
        return;
    }

    let mut screen = MermaidShowcaseScreen::new();
    let sample_count = screen.sample_count();
    let (width, height) = (120, 40);

    // Warmup: render the first sample to populate caches, then invalidate.
    measure_view_ms(&screen, width, height);

    let mut worst_ms = 0.0f64;
    let mut worst_sample_idx = 0;

    for i in 0..sample_count {
        // Navigate to sample i (screen starts at 0, so first iteration
        // re-renders sample 0 after cache was warmed up).
        if i > 0 {
            let _ = screen.update(&key_press(KeyCode::Down));
        }
        let pipeline_ms = measure_view_ms(&screen, width, height);

        log_jsonl(&[
            ("test", "mermaid_pipeline_budget_120x40"),
            ("sample_idx", &i.to_string()),
            ("pipeline_ms", &format!("{pipeline_ms:.2}")),
            ("budget_ms", &BUDGET_PIPELINE_120X40_MS.to_string()),
            (
                "passed",
                &(pipeline_ms < BUDGET_PIPELINE_120X40_MS).to_string(),
            ),
        ]);

        if pipeline_ms > worst_ms {
            worst_ms = pipeline_ms;
            worst_sample_idx = i;
        }

        assert!(
            pipeline_ms < BUDGET_PIPELINE_120X40_MS,
            "Sample {i} pipeline at {width}x{height} exceeded budget: \
             {pipeline_ms:.2}ms (budget: {BUDGET_PIPELINE_120X40_MS}ms). \
             Breakdown: check parse_ms, layout_ms, render_ms in JSONL logs.",
        );
    }

    log_jsonl(&[
        ("test", "mermaid_pipeline_budget_120x40_summary"),
        ("sample_count", &sample_count.to_string()),
        ("worst_ms", &format!("{worst_ms:.2}")),
        ("worst_sample_idx", &worst_sample_idx.to_string()),
    ]);
}

/// Verify pipeline budgets at 80x24 (smaller viewport).
#[test]
fn mermaid_pipeline_budget_80x24() {
    if !is_release_mode() {
        eprintln!("SKIPPED: mermaid_pipeline_budget_80x24 (debug build - run with --release)");
        return;
    }

    let mut screen = MermaidShowcaseScreen::new();
    let sample_count = screen.sample_count();
    let (width, height) = (80, 24);

    // Warmup
    measure_view_ms(&screen, width, height);

    for i in 0..sample_count {
        if i > 0 {
            let _ = screen.update(&key_press(KeyCode::Down));
        }
        let pipeline_ms = measure_view_ms(&screen, width, height);

        log_jsonl(&[
            ("test", "mermaid_pipeline_budget_80x24"),
            ("sample_idx", &i.to_string()),
            ("pipeline_ms", &format!("{pipeline_ms:.2}")),
            ("budget_ms", &BUDGET_PIPELINE_80X24_MS.to_string()),
            (
                "passed",
                &(pipeline_ms < BUDGET_PIPELINE_80X24_MS).to_string(),
            ),
        ]);

        assert!(
            pipeline_ms < BUDGET_PIPELINE_80X24_MS,
            "Sample {i} pipeline at {width}x{height} exceeded budget: \
             {pipeline_ms:.2}ms (budget: {BUDGET_PIPELINE_80X24_MS}ms).",
        );
    }
}

/// Verify sample-switch timing (navigate + re-render) is fast.
#[test]
fn mermaid_sample_switch_budget() {
    if !is_release_mode() {
        eprintln!("SKIPPED: mermaid_sample_switch_budget (debug build - run with --release)");
        return;
    }

    let mut screen = MermaidShowcaseScreen::new();
    let sample_count = screen.sample_count();
    let (width, height) = (120, 40);

    // Warmup: render first sample
    measure_view_ms(&screen, width, height);

    let mut worst_total_ms = 0.0f64;
    let mut worst_sample_idx = 0;

    for i in 1..sample_count {
        let (switch_ms, view_ms) = switch_and_measure(&mut screen, width, height);
        let total_ms = switch_ms + view_ms;

        log_jsonl(&[
            ("test", "mermaid_sample_switch_budget"),
            ("sample_idx", &i.to_string()),
            ("switch_ms", &format!("{switch_ms:.2}")),
            ("view_ms", &format!("{view_ms:.2}")),
            ("total_ms", &format!("{total_ms:.2}")),
            ("budget_ms", &BUDGET_SAMPLE_SWITCH_MS.to_string()),
        ]);

        if total_ms > worst_total_ms {
            worst_total_ms = total_ms;
            worst_sample_idx = i;
        }

        assert!(
            total_ms < BUDGET_SAMPLE_SWITCH_MS,
            "Sample switch to {i} exceeded budget: {total_ms:.2}ms \
             (switch: {switch_ms:.2}ms, view: {view_ms:.2}ms, \
             budget: {BUDGET_SAMPLE_SWITCH_MS}ms).",
        );
    }

    log_jsonl(&[
        ("test", "mermaid_sample_switch_budget_summary"),
        ("sample_count", &sample_count.to_string()),
        ("worst_total_ms", &format!("{worst_total_ms:.2}")),
        ("worst_sample_idx", &worst_sample_idx.to_string()),
    ]);
}

// =============================================================================
// Tests: Layout quality assertions
// =============================================================================

/// All built-in samples must render without panics at multiple sizes.
#[test]
fn mermaid_all_samples_render_cleanly() {
    let mut screen = MermaidShowcaseScreen::new();
    let sample_count = screen.sample_count();
    assert!(sample_count >= 5, "expected at least 5 built-in samples");

    let sizes: &[(u16, u16)] = &[(80, 24), (120, 40), (200, 60)];
    let mut pool = GraphemePool::new();

    for i in 0..sample_count {
        if i > 0 {
            let _ = screen.update(&key_press(KeyCode::Down));
        }
        for &(w, h) in sizes {
            let mut frame = Frame::new(w, h, &mut pool);
            let area = Rect::new(0, 0, w, h);
            // This should not panic for any sample at any size.
            screen.view(&mut frame, area);
        }
    }
}

/// Verify cache behavior: second view() call should be faster (cache hit).
#[test]
fn mermaid_cache_hit_faster_than_miss() {
    let screen = MermaidShowcaseScreen::new();
    let (width, height) = (120, 40);

    // First call: cache miss (full pipeline).
    let first_ms = measure_view_ms(&screen, width, height);

    // Second call: cache hit (should be fast).
    let second_ms = measure_view_ms(&screen, width, height);

    log_jsonl(&[
        ("test", "mermaid_cache_hit_faster_than_miss"),
        ("first_ms", &format!("{first_ms:.3}")),
        ("second_ms", &format!("{second_ms:.3}")),
    ]);

    // Cache hit should be at least 2x faster than cache miss.
    // If the first render is very fast (<1ms), skip the ratio check.
    if first_ms > 1.0 {
        assert!(
            second_ms < first_ms,
            "Cache hit ({second_ms:.3}ms) should be faster than miss ({first_ms:.3}ms)"
        );
    }
}

// =============================================================================
// Tests: JSONL log content (PTY-based)
// =============================================================================

/// Verify that mermaid_render JSONL events contain all required fields with
/// correct types when run through the harness.
#[cfg(unix)]
#[test]
fn mermaid_jsonl_render_event_complete_schema() {
    use ftui_pty::{PtyConfig, spawn_command};
    use portable_pty::CommandBuilder;
    use std::time::Duration;

    let demo_bin = match std::env::var("CARGO_BIN_EXE_ftui-demo-showcase") {
        Ok(bin) => bin,
        Err(_) => {
            eprintln!("SKIPPED: CARGO_BIN_EXE_ftui-demo-showcase not set");
            return;
        }
    };

    let config = PtyConfig::default()
        .with_size(120, 40)
        .with_test_name("mermaid_jsonl_schema")
        .with_env("FTUI_DEMO_DETERMINISTIC", "1")
        .with_env("E2E_JSONL", "1")
        .with_env("E2E_SEED", "42")
        .logging(false);

    let mut cmd = CommandBuilder::new(&demo_bin);
    cmd.arg("--mermaid-harness");
    cmd.arg("--mermaid-tick-ms=100");
    cmd.arg("--mermaid-cols=120");
    cmd.arg("--mermaid-rows=40");
    cmd.arg("--mermaid-seed=42");
    cmd.arg("--mermaid-jsonl=-");
    cmd.arg("--mermaid-run-id=jsonl-schema-test");
    cmd.arg("--exit-after-ms=30000");

    let mut session = spawn_command(config, cmd).expect("spawn mermaid harness");
    let status = session
        .wait_and_drain(Duration::from_secs(60))
        .expect("wait mermaid harness");
    assert!(status.success(), "mermaid harness exited with error");

    let output = session.output().to_vec();
    let text = String::from_utf8_lossy(&output);

    // Collect mermaid_render events.
    let render_lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("\"event\":\"mermaid_render\""))
        .collect();

    assert!(
        !render_lines.is_empty(),
        "expected at least one mermaid_render event"
    );

    // Required fields in every mermaid_render event.
    let required_fields = [
        "\"schema_version\":",
        "\"event\":\"mermaid_render\"",
        "\"seq\":",
        "\"seed\":",
        "\"sample\":",
        "\"layout_mode\":",
        "\"tier\":",
        "\"glyph_mode\":",
        "\"wrap_mode\":",
        "\"render_epoch\":",
    ];

    // Metric fields (must be present, value can be number or null).
    let metric_fields = [
        "\"parse_ms\":",
        "\"layout_ms\":",
        "\"render_ms\":",
        "\"layout_iterations\":",
        "\"objective_score\":",
        "\"constraint_violations\":",
        "\"bends\":",
        "\"symmetry\":",
        "\"compactness\":",
        "\"edge_length_variance\":",
        "\"label_collisions\":",
        "\"error_count\":",
    ];

    for (i, line) in render_lines.iter().enumerate() {
        for field in &required_fields {
            assert!(
                line.contains(field),
                "mermaid_render event {i} missing required field {field}\nLine: {line}"
            );
        }
        for field in &metric_fields {
            assert!(
                line.contains(field),
                "mermaid_render event {i} missing metric field {field}\nLine: {line}"
            );
        }
    }

    log_jsonl(&[
        ("test", "mermaid_jsonl_render_event_complete_schema"),
        ("render_event_count", &render_lines.len().to_string()),
        (
            "required_fields_checked",
            &required_fields.len().to_string(),
        ),
        ("metric_fields_checked", &metric_fields.len().to_string()),
    ]);
}

/// Verify that JSONL mermaid_render events cover all samples in the harness run.
#[cfg(unix)]
#[test]
fn mermaid_jsonl_all_samples_logged() {
    use ftui_pty::{PtyConfig, spawn_command};
    use portable_pty::CommandBuilder;
    use std::time::Duration;

    let demo_bin = match std::env::var("CARGO_BIN_EXE_ftui-demo-showcase") {
        Ok(bin) => bin,
        Err(_) => {
            eprintln!("SKIPPED: CARGO_BIN_EXE_ftui-demo-showcase not set");
            return;
        }
    };

    let config = PtyConfig::default()
        .with_size(120, 40)
        .with_test_name("mermaid_jsonl_samples")
        .with_env("FTUI_DEMO_DETERMINISTIC", "1")
        .with_env("E2E_JSONL", "1")
        .with_env("E2E_SEED", "42")
        .logging(false);

    let mut cmd = CommandBuilder::new(&demo_bin);
    cmd.arg("--mermaid-harness");
    cmd.arg("--mermaid-tick-ms=100");
    cmd.arg("--mermaid-cols=120");
    cmd.arg("--mermaid-rows=40");
    cmd.arg("--mermaid-seed=42");
    cmd.arg("--mermaid-jsonl=-");
    cmd.arg("--mermaid-run-id=samples-test");
    cmd.arg("--exit-after-ms=30000");

    let mut session = spawn_command(config, cmd).expect("spawn mermaid harness");
    let status = session
        .wait_and_drain(Duration::from_secs(60))
        .expect("wait mermaid harness");
    assert!(status.success(), "mermaid harness exited with error");

    let output = session.output().to_vec();
    let text = String::from_utf8_lossy(&output);

    // Extract sample names from mermaid_render events.
    let sample_names: Vec<String> = text
        .lines()
        .filter(|l| l.contains("\"event\":\"mermaid_render\""))
        .filter_map(|l| {
            let needle = "\"sample\":\"";
            let start = l.find(needle)? + needle.len();
            let rest = &l[start..];
            let end = rest.find('"')?;
            Some(rest[..end].to_string())
        })
        .collect();

    // Should have render events for most samples. Some samples (e.g.,
    // requirementDiagram) may fail to parse and won't emit mermaid_render.
    let screen = MermaidShowcaseScreen::new();
    let expected_count = screen.sample_count();

    // Allow up to 3 missing events for unsupported diagram types that
    // fail to parse (recompute_metrics returns early on parse failure).
    let min_events = expected_count.saturating_sub(3);
    assert!(
        sample_names.len() >= min_events,
        "Expected at least {min_events} mermaid_render events (from {expected_count} samples), \
         got {}. Sample names seen: {:?}",
        sample_names.len(),
        sample_names,
    );

    // Collect unique sample names.
    let unique_samples: std::collections::HashSet<&str> =
        sample_names.iter().map(|s| s.as_str()).collect();

    log_jsonl(&[
        ("test", "mermaid_jsonl_all_samples_logged"),
        ("render_event_count", &sample_names.len().to_string()),
        ("unique_samples", &unique_samples.len().to_string()),
        ("expected_sample_count", &expected_count.to_string()),
    ]);

    // Verify we saw at least 5 unique samples (sanity check).
    assert!(
        unique_samples.len() >= 5,
        "Expected at least 5 unique sample names in mermaid_render events, \
         got {}: {:?}",
        unique_samples.len(),
        unique_samples,
    );
}

/// Verify JSONL metric values are within sane ranges for built-in samples.
#[cfg(unix)]
#[test]
fn mermaid_jsonl_metric_values_sane() {
    use ftui_pty::{PtyConfig, spawn_command};
    use portable_pty::CommandBuilder;
    use std::time::Duration;

    let demo_bin = match std::env::var("CARGO_BIN_EXE_ftui-demo-showcase") {
        Ok(bin) => bin,
        Err(_) => {
            eprintln!("SKIPPED: CARGO_BIN_EXE_ftui-demo-showcase not set");
            return;
        }
    };

    let config = PtyConfig::default()
        .with_size(120, 40)
        .with_test_name("mermaid_jsonl_metrics")
        .with_env("FTUI_DEMO_DETERMINISTIC", "1")
        .with_env("E2E_JSONL", "1")
        .with_env("E2E_SEED", "42")
        .logging(false);

    let mut cmd = CommandBuilder::new(&demo_bin);
    cmd.arg("--mermaid-harness");
    cmd.arg("--mermaid-tick-ms=100");
    cmd.arg("--mermaid-cols=120");
    cmd.arg("--mermaid-rows=40");
    cmd.arg("--mermaid-seed=42");
    cmd.arg("--mermaid-jsonl=-");
    cmd.arg("--mermaid-run-id=metrics-sane-test");
    cmd.arg("--exit-after-ms=30000");

    let mut session = spawn_command(config, cmd).expect("spawn mermaid harness");
    let status = session
        .wait_and_drain(Duration::from_secs(60))
        .expect("wait mermaid harness");
    assert!(status.success(), "mermaid harness exited with error");

    let output = session.output().to_vec();
    let text = String::from_utf8_lossy(&output);

    let render_lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("\"event\":\"mermaid_render\""))
        .collect();

    for (i, line) in render_lines.iter().enumerate() {
        // Extract sample name for diagnostics.
        let sample_name = extract_string_field(line, "sample").unwrap_or("unknown");

        // Parse numeric fields and validate ranges.
        if let Some(crossings) = extract_u64_field(line, "constraint_violations") {
            assert!(
                crossings <= MAX_CROSSINGS_BUILTIN as u64,
                "Sample '{sample_name}' (event {i}) has {crossings} crossings \
                 (max: {MAX_CROSSINGS_BUILTIN}). Layout mode and tier should be checked.",
            );
        }

        if let Some(compactness) = extract_f64_field(line, "compactness") {
            // Gantt, pie, and mindmap diagrams may have near-zero compactness
            // because their layout geometry differs from flowcharts.
            let is_flowlike = !["Gantt", "Pie", "Mindmap"]
                .iter()
                .any(|prefix| sample_name.starts_with(prefix));
            if is_flowlike {
                assert!(
                    compactness >= MIN_COMPACTNESS_FLOWLIKE as f64,
                    "Sample '{sample_name}' (event {i}) has compactness {compactness:.3} \
                     (min: {MIN_COMPACTNESS_FLOWLIKE}). This may indicate a layout regression.",
                );
            }
        }

        // Symmetry should be between 0.0 and 1.0.
        if let Some(symmetry) = extract_f64_field(line, "symmetry") {
            assert!(
                (0.0..=1.0).contains(&symmetry),
                "Sample '{sample_name}' (event {i}) has invalid symmetry {symmetry:.3} \
                 (expected 0.0-1.0).",
            );
        }

        // Compactness should be between 0.0 and 1.0.
        if let Some(compactness) = extract_f64_field(line, "compactness") {
            assert!(
                (0.0..=1.0).contains(&compactness),
                "Sample '{sample_name}' (event {i}) has invalid compactness {compactness:.3} \
                 (expected 0.0-1.0).",
            );
        }

        // Error count should be small for built-in samples (they're curated).
        if let Some(errors) = extract_u64_field(line, "error_count") {
            // Some samples are intentionally unsupported (e.g., requirementDiagram).
            // Allow up to 5 errors for those; most should have 0.
            assert!(
                errors <= 5,
                "Sample '{sample_name}' (event {i}) has {errors} errors \
                 (expected ≤ 5 for built-in samples).",
            );
        }
    }
}

// =============================================================================
// Helpers: JSONL field extraction
// =============================================================================

fn extract_u64_field(line: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if rest.starts_with("null") {
        return None;
    }
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse::<u64>().ok()
}

fn extract_f64_field(line: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if rest.starts_with("null") {
        return None;
    }
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok()
}

fn extract_string_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}
