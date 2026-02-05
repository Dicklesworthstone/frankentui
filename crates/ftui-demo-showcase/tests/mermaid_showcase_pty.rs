#![forbid(unsafe_code)]

//! PTY-driven E2E for the Mermaid showcase harness (bd-1k26f).
//!
//! Runs the deterministic mermaid harness with a fixed seed, verifies that
//! frame hashes are reproducible across runs, and validates JSONL structure.

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

fn has_harness_start(output: &[u8]) -> bool {
    let text = String::from_utf8_lossy(output);
    text.lines()
        .any(|l| l.contains("\"event\":\"mermaid_harness_start\""))
}

fn has_harness_done(output: &[u8]) -> bool {
    let text = String::from_utf8_lossy(output);
    text.lines()
        .any(|l| l.contains("\"event\":\"mermaid_harness_done\""))
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
        "frame count mismatch between runs"
    );

    let hashes_a = frame_hash_sequence(&frames_a);
    let hashes_b = frame_hash_sequence(&frames_b);

    if hashes_a != hashes_b {
        return Err(format!(
            "mermaid harness hashes diverged:\nA={:?}\nB={:?}",
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

    // Verify start event
    assert!(
        has_harness_start(&output),
        "expected mermaid_harness_start event"
    );

    // Verify done event
    assert!(
        has_harness_done(&output),
        "expected mermaid_harness_done event"
    );

    // Verify frame events
    let frames = extract_mermaid_frames(&output)?;
    assert!(
        frames.len() >= 5,
        "expected at least 5 mermaid frames, got {}",
        frames.len()
    );

    // Verify frame indices are monotonic
    for window in frames.windows(2) {
        assert!(
            window[1].frame > window[0].frame,
            "mermaid frame order not monotonic: {} -> {}",
            window[0].frame,
            window[1].frame
        );
    }

    // Verify sample indices are sequential
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(
            f.sample_idx, i as u64,
            "expected sample_idx {i}, got {}",
            f.sample_idx
        );
    }

    Ok(())
}

#[test]
fn pty_mermaid_harness_exits_cleanly() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    // Just verify the harness runs and exits without error.
    let _output = run_mermaid_harness(&demo_bin, MERMAID_SEED)?;
    Ok(())
}
