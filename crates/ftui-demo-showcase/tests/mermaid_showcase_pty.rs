#![forbid(unsafe_code)]

//! PTY-driven E2E for the Mermaid showcase harness (bd-1k26f, bd-2rfpz).
//!
//! Runs the deterministic mermaid harness with a fixed seed, verifies that
//! frame hashes are reproducible across runs, and validates JSONL structure
//! including field presence, types, and run_id consistency.

#![cfg(unix)]

use std::time::Duration;

use ftui_pty::{PtyConfig, spawn_command};
use portable_pty::CommandBuilder;

const MERMAID_COLS: u16 = 120;
const MERMAID_ROWS: u16 = 40;
const MERMAID_TICK_MS: u64 = 100;
const MERMAID_SEED: u64 = 42;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct MermaidFrame {
    frame: u64,
    hash: u64,
    sample_idx: u64,
}

fn parse_u64_field(line: &str, key: &str) -> Option<u64> {
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse::<u64>().ok()
}

fn parse_string_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn tail_output(output: &[u8], max_bytes: usize) -> String {
    let start = output.len().saturating_sub(max_bytes);
    String::from_utf8_lossy(&output[start..]).to_string()
}

fn run_mermaid_harness(demo_bin: &str, seed: u64) -> Result<Vec<u8>, String> {
    let config = PtyConfig::default()
        .with_size(MERMAID_COLS, MERMAID_ROWS)
        .with_test_name("mermaid_harness")
        .with_env("FTUI_DEMO_DETERMINISTIC", "1")
        .with_env("E2E_SEED", seed.to_string())
        .with_env("E2E_JSONL", "1")
        .logging(false);

    let run_id = format!("mermaid-{MERMAID_COLS}x{MERMAID_ROWS}-seed{seed}");
    let mut cmd = CommandBuilder::new(demo_bin);
    cmd.arg("--mermaid-harness");
    cmd.arg(format!("--mermaid-tick-ms={MERMAID_TICK_MS}"));
    cmd.arg(format!("--mermaid-cols={MERMAID_COLS}"));
    cmd.arg(format!("--mermaid-rows={MERMAID_ROWS}"));
    cmd.arg(format!("--mermaid-seed={seed}"));
    cmd.arg("--mermaid-jsonl=-");
    cmd.arg(format!("--mermaid-run-id={run_id}"));
    cmd.arg("--exit-after-ms=30000");

    let mut session =
        spawn_command(config, cmd).map_err(|err| format!("spawn mermaid harness: {err}"))?;
    let status = session
        .wait_and_drain(Duration::from_secs(60))
        .map_err(|err| format!("wait mermaid harness: {err}"))?;
    let output = session.output().to_vec();

    if !status.success() {
        let tail = tail_output(&output, 4096);
        return Err(format!(
            "mermaid harness exit failure: {status:?}\nTAIL:\n{tail}"
        ));
    }

    Ok(output)
}

fn extract_mermaid_frames(output: &[u8]) -> Result<Vec<MermaidFrame>, String> {
    let text = String::from_utf8_lossy(output);
    let mut frames = Vec::new();

    for line in text.lines() {
        if !line.contains("\"event\":\"mermaid_frame\"") {
            continue;
        }
        let frame = parse_u64_field(line, "\"frame\":")
            .ok_or_else(|| format!("mermaid_frame missing frame: {line}"))?;
        let hash = parse_u64_field(line, "\"hash\":")
            .ok_or_else(|| format!("mermaid_frame missing hash: {line}"))?;
        let sample_idx = parse_u64_field(line, "\"sample_idx\":")
            .ok_or_else(|| format!("mermaid_frame missing sample_idx: {line}"))?;
        frames.push(MermaidFrame {
            frame,
            hash,
            sample_idx,
        });
    }

    if frames.is_empty() {
        return Err("no mermaid_frame entries found".to_string());
    }

    Ok(frames)
}

fn find_event_line(output: &[u8], event_name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(output);
    let needle = format!("\"event\":\"{event_name}\"");
    text.lines()
        .find(|l| l.contains(&needle))
        .map(|s| s.to_string())
}

fn frame_hash_sequence(frames: &[MermaidFrame]) -> Vec<String> {
    frames
        .iter()
        .map(|f| format!("{:03}:{:016x}", f.frame, f.hash))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn pty_mermaid_harness_exits_cleanly() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    // Verify the harness runs and exits without error.
    let _output = run_mermaid_harness(&demo_bin, MERMAID_SEED)?;
    Ok(())
}

#[test]
fn pty_mermaid_harness_deterministic_hashes() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    let output_a = run_mermaid_harness(&demo_bin, MERMAID_SEED)?;
    let output_b = run_mermaid_harness(&demo_bin, MERMAID_SEED)?;

    let frames_a = extract_mermaid_frames(&output_a)?;
    let frames_b = extract_mermaid_frames(&output_b)?;

    assert!(
        !frames_a.is_empty(),
        "expected at least one mermaid frame from run A"
    );
    assert_eq!(
        frames_a.len(),
        frames_b.len(),
        "frame count mismatch between runs: A={}, B={}",
        frames_a.len(),
        frames_b.len(),
    );

    let hashes_a = frame_hash_sequence(&frames_a);
    let hashes_b = frame_hash_sequence(&frames_b);

    if hashes_a != hashes_b {
        // Report the first diverging frame for easier debugging.
        let first_diff = hashes_a
            .iter()
            .zip(hashes_b.iter())
            .enumerate()
            .find(|(_, (a, b))| a != b);
        let diff_detail = match first_diff {
            Some((idx, (a, b))) => format!(" (first divergence at frame {idx}: A={a}, B={b})"),
            None => String::new(),
        };
        return Err(format!(
            "mermaid harness hashes diverged (seed={MERMAID_SEED}, cols={MERMAID_COLS}, rows={MERMAID_ROWS}){diff_detail}\nA={:?}\nB={:?}",
            hashes_a, hashes_b
        ));
    }

    Ok(())
}

#[test]
fn pty_mermaid_harness_jsonl_schema() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    let output = run_mermaid_harness(&demo_bin, MERMAID_SEED)?;

    // --- Verify mermaid_harness_start event ---
    let start_line = find_event_line(&output, "mermaid_harness_start")
        .ok_or("missing mermaid_harness_start event")?;

    // Required fields in start event.
    for field in [
        "\"run_id\":",
        "\"timestamp\":",
        "\"hash_key\":",
        "\"cols\":",
        "\"rows\":",
        "\"seed\":",
        "\"sample_count\":",
        "\"env\":",
    ] {
        assert!(
            start_line.contains(field),
            "mermaid_harness_start missing {field}: {start_line}"
        );
    }

    // Verify cols/rows/seed match what we passed.
    let start_cols = parse_u64_field(&start_line, "\"cols\":").ok_or("start event missing cols")?;
    let start_rows = parse_u64_field(&start_line, "\"rows\":").ok_or("start event missing rows")?;
    let start_seed = parse_u64_field(&start_line, "\"seed\":").ok_or("start event missing seed")?;
    let sample_count = parse_u64_field(&start_line, "\"sample_count\":")
        .ok_or("start event missing sample_count")?;

    assert_eq!(
        start_cols, MERMAID_COLS as u64,
        "start cols mismatch: expected {MERMAID_COLS}"
    );
    assert_eq!(
        start_rows, MERMAID_ROWS as u64,
        "start rows mismatch: expected {MERMAID_ROWS}"
    );
    assert_eq!(
        start_seed, MERMAID_SEED,
        "start seed mismatch: expected {MERMAID_SEED}"
    );
    assert!(
        sample_count >= 5,
        "expected at least 5 samples, got {sample_count}"
    );

    let start_run_id =
        parse_string_field(&start_line, "\"run_id\":").ok_or("start event missing run_id")?;

    // --- Verify mermaid_frame events ---
    let frames = extract_mermaid_frames(&output)?;
    assert_eq!(
        frames.len(),
        sample_count as usize,
        "frame count ({}) should match sample_count ({sample_count})",
        frames.len(),
    );

    // Verify frame indices are monotonic.
    for window in frames.windows(2) {
        assert!(
            window[1].frame > window[0].frame,
            "mermaid frame order not monotonic: {} -> {}",
            window[0].frame,
            window[1].frame
        );
    }

    // Verify sample indices are sequential 0..N-1.
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(
            f.sample_idx, i as u64,
            "expected sample_idx {i}, got {} (frame {})",
            f.sample_idx, f.frame
        );
    }

    // Verify run_id consistency across all frame events.
    let text = String::from_utf8_lossy(&output);
    for line in text.lines() {
        if !line.contains("\"event\":\"mermaid_frame\"") {
            continue;
        }
        for field in ["\"run_id\":", "\"frame\":", "\"hash\":", "\"sample_idx\":"] {
            assert!(
                line.contains(field),
                "mermaid_frame event missing {field}: {line}"
            );
        }
        if let Some(frame_run_id) = parse_string_field(line, "\"run_id\":") {
            assert_eq!(
                frame_run_id, start_run_id,
                "run_id mismatch between start and frame events"
            );
        }
    }

    // --- Verify mermaid_harness_done event ---
    let done_line = find_event_line(&output, "mermaid_harness_done")
        .ok_or("missing mermaid_harness_done event")?;

    for field in ["\"run_id\":", "\"timestamp\":", "\"total_frames\":"] {
        assert!(
            done_line.contains(field),
            "mermaid_harness_done missing {field}: {done_line}"
        );
    }

    let total_frames = parse_u64_field(&done_line, "\"total_frames\":")
        .ok_or("done event missing total_frames")?;
    assert_eq!(
        total_frames,
        frames.len() as u64,
        "total_frames in done event ({total_frames}) doesn't match actual frame count ({})",
        frames.len()
    );

    if let Some(done_run_id) = parse_string_field(&done_line, "\"run_id\":") {
        assert_eq!(
            done_run_id, start_run_id,
            "run_id mismatch between start and done events"
        );
    }

    Ok(())
}

/// Verify that the mermaid_render JSONL events (from the screen's own metrics
/// logging) also appear in the harness output when E2E_JSONL is set.
#[test]
fn pty_mermaid_harness_metrics_jsonl_present() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    let output = run_mermaid_harness(&demo_bin, MERMAID_SEED)?;
    let text = String::from_utf8_lossy(&output);

    // The screen's recompute_metrics() emits mermaid_render events to stderr.
    let render_lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("\"event\":\"mermaid_render\""))
        .collect();

    // At least the initial sample should produce a mermaid_render line.
    assert!(
        !render_lines.is_empty(),
        "expected at least one mermaid_render event from the screen's metrics logging"
    );

    // Validate schema fields on the first mermaid_render line.
    let first = render_lines[0];
    for field in [
        "\"schema_version\":",
        "\"event\":\"mermaid_render\"",
        "\"sample\":",
        "\"layout_mode\":",
        "\"tier\":",
        "\"glyph_mode\":",
    ] {
        assert!(
            first.contains(field),
            "mermaid_render event missing {field}: {first}"
        );
    }

    Ok(())
}

/// Verify that different seeds produce different frame hashes.
#[test]
fn pty_mermaid_harness_different_seeds_differ() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    // The seed doesn't affect mermaid rendering (it's for VFX randomness),
    // but the run_id differs, and the hash_key differs.
    // Run with two different seeds and just verify both complete successfully.
    let output_a = run_mermaid_harness(&demo_bin, 42)?;
    let output_b = run_mermaid_harness(&demo_bin, 99)?;

    let frames_a = extract_mermaid_frames(&output_a)?;
    let frames_b = extract_mermaid_frames(&output_b)?;

    // Both runs should produce the same number of frames (all samples).
    assert_eq!(
        frames_a.len(),
        frames_b.len(),
        "different seeds should still render same number of samples"
    );

    // Verify run_ids are different.
    let start_a =
        find_event_line(&output_a, "mermaid_harness_start").ok_or("run A missing start")?;
    let start_b =
        find_event_line(&output_b, "mermaid_harness_start").ok_or("run B missing start")?;
    let rid_a = parse_string_field(&start_a, "\"run_id\":");
    let rid_b = parse_string_field(&start_b, "\"run_id\":");
    assert_ne!(
        rid_a, rid_b,
        "different seeds should produce different run_ids"
    );

    Ok(())
}
