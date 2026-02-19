#![forbid(unsafe_code)]

//! PTY-driven E2E for VisualEffects input handling (bd-l8x9.8.3).
//!
//! Drives real key sequences through a PTY to ensure the VisualEffects screen
//! can cycle effects/palettes without panicking and exits cleanly.

#![cfg(unix)]

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ftui_harness::determinism::{JsonValue, TestJsonlLogger};
use ftui_harness::golden::{
    GoldenOutcome, golden_checksum_path, is_bless_mode, is_golden_enforced, load_golden_checksums,
    save_golden_checksums, verify_checksums,
};
use ftui_pty::input_forwarding::{Key, KeyEvent, Modifiers, key_to_sequence};
use ftui_pty::{PtyConfig, spawn_command};
use portable_pty::CommandBuilder;

// ---------------------------------------------------------------------------
// JSONL Logging
// ---------------------------------------------------------------------------

fn logger() -> &'static TestJsonlLogger {
    static LOGGER: OnceLock<TestJsonlLogger> = OnceLock::new();
    LOGGER.get_or_init(|| {
        let mut logger = TestJsonlLogger::new("visual_effects_pty", 42);
        logger.add_context_str("suite", "visual_effects_pty");
        logger
    })
}

fn log_jsonl(event: &str, fields: &[(&str, JsonValue)]) {
    logger().log(event, fields);
}

#[derive(Debug, Clone, Copy)]
struct VfxCase {
    effect: &'static str,
    frames: u64,
    tick_ms: u64,
    cols: u16,
    rows: u16,
}

impl VfxCase {
    fn scenario_name(self, seed: u64) -> String {
        format!(
            "vfx_{}_{}x{}_{}ms_seed{}",
            self.effect, self.cols, self.rows, self.tick_ms, seed
        )
    }
}

const VFX_COLS: u16 = 120;
const VFX_ROWS: u16 = 40;
const VFX_TICK_MS: u64 = 16;
const VFX_FRAMES: u64 = 6;
const VFX_CASES: &[VfxCase] = &[
    VfxCase {
        effect: "metaballs",
        frames: VFX_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: VFX_COLS,
        rows: VFX_ROWS,
    },
    VfxCase {
        effect: "plasma",
        frames: VFX_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: VFX_COLS,
        rows: VFX_ROWS,
    },
    VfxCase {
        effect: "doom-e1m1",
        frames: VFX_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: VFX_COLS,
        rows: VFX_ROWS,
    },
    VfxCase {
        effect: "quake-e1m1",
        frames: VFX_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: VFX_COLS,
        rows: VFX_ROWS,
    },
    VfxCase {
        effect: "mandelbrot",
        frames: VFX_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: VFX_COLS,
        rows: VFX_ROWS,
    },
];
const VFX_PERF_FRAMES: u64 = 120;
const VFX_PERF_CASES: &[VfxCase] = &[
    VfxCase {
        effect: "metaballs",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 80,
        rows: 24,
    },
    VfxCase {
        effect: "metaballs",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 120,
        rows: 40,
    },
    VfxCase {
        effect: "plasma",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 80,
        rows: 24,
    },
    VfxCase {
        effect: "plasma",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 120,
        rows: 40,
    },
    VfxCase {
        effect: "doom-e1m1",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 80,
        rows: 24,
    },
    VfxCase {
        effect: "doom-e1m1",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 120,
        rows: 40,
    },
    VfxCase {
        effect: "quake-e1m1",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 80,
        rows: 24,
    },
    VfxCase {
        effect: "quake-e1m1",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 120,
        rows: 40,
    },
    VfxCase {
        effect: "mandelbrot",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 80,
        rows: 24,
    },
    VfxCase {
        effect: "mandelbrot",
        frames: VFX_PERF_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: 120,
        rows: 40,
    },
];

const VFX_PERF_CI_MULTIPLIER: f64 = 2.0;

#[derive(Debug, Clone, Copy)]
struct VfxPerfBudget {
    effect: &'static str,
    cols: u16,
    rows: u16,
    total_p50_ms: f64,
    total_p95_ms: f64,
    total_p99_ms: f64,
}

// Baselines captured via bd-3e1t.5.7 (seed=0, tick_ms=16, frames=120).
const VFX_PERF_BASELINES: &[VfxPerfBudget] = &[
    VfxPerfBudget {
        effect: "plasma",
        cols: 80,
        rows: 24,
        total_p50_ms: 0.979,
        total_p95_ms: 1.253,
        total_p99_ms: 1.495,
    },
    VfxPerfBudget {
        effect: "plasma",
        cols: 120,
        rows: 40,
        total_p50_ms: 2.204,
        total_p95_ms: 2.649,
        total_p99_ms: 3.545,
    },
    VfxPerfBudget {
        effect: "metaballs",
        cols: 80,
        rows: 24,
        total_p50_ms: 1.371,
        total_p95_ms: 1.668,
        total_p99_ms: 2.004,
    },
    VfxPerfBudget {
        effect: "metaballs",
        cols: 120,
        rows: 40,
        total_p50_ms: 3.214,
        total_p95_ms: 3.609,
        total_p99_ms: 3.860,
    },
    VfxPerfBudget {
        effect: "doom-e1m1",
        cols: 80,
        rows: 24,
        total_p50_ms: 0.554,
        total_p95_ms: 0.754,
        total_p99_ms: 0.895,
    },
    VfxPerfBudget {
        effect: "doom-e1m1",
        cols: 120,
        rows: 40,
        total_p50_ms: 1.337,
        total_p95_ms: 1.666,
        total_p99_ms: 2.318,
    },
    VfxPerfBudget {
        effect: "quake-e1m1",
        cols: 80,
        rows: 24,
        total_p50_ms: 2.631,
        total_p95_ms: 3.119,
        total_p99_ms: 3.831,
    },
    VfxPerfBudget {
        effect: "quake-e1m1",
        cols: 120,
        rows: 40,
        total_p50_ms: 4.760,
        total_p95_ms: 5.553,
        total_p99_ms: 5.906,
    },
    VfxPerfBudget {
        effect: "mandelbrot",
        cols: 80,
        rows: 24,
        total_p50_ms: 2.251,
        total_p95_ms: 2.905,
        total_p99_ms: 3.047,
    },
    VfxPerfBudget {
        effect: "mandelbrot",
        cols: 120,
        rows: 40,
        total_p50_ms: 5.561,
        total_p95_ms: 6.946,
        total_p99_ms: 7.502,
    },
];

fn vfx_golden_base_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn tail_output(output: &[u8], max_bytes: usize) -> String {
    let start = output.len().saturating_sub(max_bytes);
    String::from_utf8_lossy(&output[start..]).to_string()
}

fn send_key(
    session: &mut ftui_pty::PtySession,
    label: &str,
    key: Key,
    delay: Duration,
    last_key: &mut String,
) -> std::io::Result<()> {
    let seq = key_to_sequence(KeyEvent::new(key, Modifiers::NONE));
    *last_key = label.to_string();
    session.send_input(&seq)?;
    std::thread::sleep(delay);
    let _ = session.read_output_result();
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct VfxFrame {
    frame_idx: u64,
    hash: u64,
}

fn parse_u64_field(line: &str, key: &str) -> Option<u64> {
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse::<u64>().ok()
}

fn parse_f64_field(line: &str, key: &str) -> Option<f64> {
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok()
}

fn extract_vfx_frames(output: &[u8]) -> Result<Vec<VfxFrame>, String> {
    let text = String::from_utf8_lossy(output);
    let mut frames = Vec::new();

    for line in text.lines() {
        if !line.contains("\"event\":\"vfx_frame\"") {
            continue;
        }
        for key in [
            "\"seed\":",
            "\"cols\":",
            "\"rows\":",
            "\"tick_ms\":",
            "\"time\":",
        ] {
            if !line.contains(key) {
                return Err(format!("vfx_frame missing {key}: {line}"));
            }
        }
        let frame_idx = parse_u64_field(line, "\"frame_idx\":")
            .ok_or_else(|| format!("vfx_frame missing frame_idx: {line}"))?;
        let hash = parse_u64_field(line, "\"hash\":")
            .ok_or_else(|| format!("vfx_frame missing hash: {line}"))?;
        frames.push(VfxFrame { frame_idx, hash });
    }

    if frames.is_empty() {
        return Err("no vfx_frame entries found".to_string());
    }

    Ok(frames)
}

fn run_vfx_harness(demo_bin: &str, case: VfxCase, seed: u64) -> Result<Vec<VfxFrame>, String> {
    let output = run_vfx_harness_output(demo_bin, case, seed, false, &[])?;
    extract_vfx_frames(&output)
}

fn run_vfx_harness_output(
    demo_bin: &str,
    case: VfxCase,
    seed: u64,
    perf: bool,
    extra_env: &[(&str, &str)],
) -> Result<Vec<u8>, String> {
    let label = if perf {
        format!("vfx_harness_perf_{}", case.effect)
    } else {
        format!("vfx_harness_{}", case.effect)
    };
    let config = PtyConfig::default()
        .with_size(case.cols, case.rows)
        .with_test_name(label)
        .with_env("FTUI_DEMO_VFX_SEED", seed.to_string())
        .with_env("FTUI_DEMO_DETERMINISTIC", "1")
        .with_env("E2E_SEED", seed.to_string())
        .logging(false);
    let config = extra_env
        .iter()
        .fold(config, |cfg, (k, v)| cfg.with_env(*k, *v));

    let run_id = case.scenario_name(seed);
    let mut cmd = CommandBuilder::new(demo_bin);
    cmd.arg("--vfx-harness");
    cmd.arg(format!("--vfx-effect={}", case.effect));
    cmd.arg(format!("--vfx-tick-ms={}", case.tick_ms));
    cmd.arg(format!("--vfx-frames={}", case.frames));
    cmd.arg(format!("--vfx-cols={}", case.cols));
    cmd.arg(format!("--vfx-rows={}", case.rows));
    cmd.arg(format!("--vfx-seed={seed}"));
    cmd.arg("--vfx-jsonl=-");
    cmd.arg(format!("--vfx-run-id={run_id}"));
    cmd.arg("--exit-after-ms=4000");
    if perf {
        cmd.arg("--vfx-perf");
    }

    let mut session =
        spawn_command(config, cmd).map_err(|err| format!("spawn vfx harness: {err}"))?;
    let status = session
        .wait_and_drain(Duration::from_secs(6))
        .map_err(|err| format!("wait vfx harness: {err}"))?;
    let output = session.output().to_vec();

    if !status.success() {
        let tail = tail_output(&output, 4096);
        return Err(format!(
            "vfx harness exit failure: {status:?}\nTAIL:\n{tail}"
        ));
    }

    Ok(output)
}

fn extract_vfx_perf_frames(output: &[u8]) -> Result<Vec<u64>, String> {
    let text = String::from_utf8_lossy(output);
    let mut frames = Vec::new();
    for line in text.lines() {
        if !line.contains("\"event\":\"vfx_perf_frame\"") {
            continue;
        }
        for key in [
            "\"run_id\":",
            "\"effect\":",
            "\"frame_idx\":",
            "\"update_ms\":",
            "\"render_ms\":",
            "\"diff_ms\":",
            "\"present_ms\":",
            "\"total_ms\":",
            "\"cols\":",
            "\"rows\":",
            "\"tick_ms\":",
        ] {
            if !line.contains(key) {
                return Err(format!("vfx_perf_frame missing {key}: {line}"));
            }
        }
        let frame_idx = parse_u64_field(line, "\"frame_idx\":")
            .ok_or_else(|| format!("vfx_perf_frame missing frame_idx: {line}"))?;
        frames.push(frame_idx);
    }

    if frames.is_empty() {
        return Err("no vfx_perf_frame entries found".to_string());
    }

    Ok(frames)
}

#[derive(Debug, Clone, Copy)]
struct VfxPerfSummary {
    count: u64,
    total_p50_ms: f64,
    total_p95_ms: f64,
    total_p99_ms: f64,
}

fn extract_vfx_perf_summary(output: &[u8]) -> Result<VfxPerfSummary, String> {
    let text = String::from_utf8_lossy(output);
    for line in text.lines() {
        if !line.contains("\"event\":\"vfx_perf_summary\"") {
            continue;
        }
        for key in [
            "\"count\":",
            "\"total_ms_p50\":",
            "\"total_ms_p95\":",
            "\"total_ms_p99\":",
        ] {
            if !line.contains(key) {
                return Err(format!("vfx_perf_summary missing {key}: {line}"));
            }
        }
        let count = parse_u64_field(line, "\"count\":")
            .ok_or_else(|| format!("vfx_perf_summary missing count: {line}"))?;
        let total_p50_ms = parse_f64_field(line, "\"total_ms_p50\":")
            .ok_or_else(|| format!("vfx_perf_summary missing total_ms_p50: {line}"))?;
        let total_p95_ms = parse_f64_field(line, "\"total_ms_p95\":")
            .ok_or_else(|| format!("vfx_perf_summary missing total_ms_p95: {line}"))?;
        let total_p99_ms = parse_f64_field(line, "\"total_ms_p99\":")
            .ok_or_else(|| format!("vfx_perf_summary missing total_ms_p99: {line}"))?;
        return Ok(VfxPerfSummary {
            count,
            total_p50_ms,
            total_p95_ms,
            total_p99_ms,
        });
    }
    Err("no vfx_perf_summary entry found".to_string())
}

fn ensure_vfx_perf_summary(output: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(output);
    for line in text.lines() {
        if !line.contains("\"event\":\"vfx_perf_summary\"") {
            continue;
        }
        for key in [
            "\"count\":",
            "\"total_ms_p50\":",
            "\"total_ms_p95\":",
            "\"total_ms_p99\":",
            "\"update_ms_p50\":",
            "\"render_ms_p50\":",
            "\"diff_ms_p50\":",
            "\"present_ms_p50\":",
            "\"top_phase\":",
        ] {
            if !line.contains(key) {
                return Err(format!("vfx_perf_summary missing {key}: {line}"));
            }
        }
        return Ok(());
    }
    Err("no vfx_perf_summary entry found".to_string())
}

fn find_perf_budget(case: VfxCase) -> Option<&'static VfxPerfBudget> {
    VFX_PERF_BASELINES.iter().find(|budget| {
        budget.effect == case.effect && budget.cols == case.cols && budget.rows == case.rows
    })
}

fn perf_budget_limits(budget: &VfxPerfBudget, multiplier: f64) -> (f64, f64, f64) {
    (
        budget.total_p50_ms * multiplier,
        budget.total_p95_ms * multiplier,
        budget.total_p99_ms * multiplier,
    )
}

fn perf_within_budget(summary: VfxPerfSummary, budget: &VfxPerfBudget, multiplier: f64) -> bool {
    let (budget_p50, budget_p95, budget_p99) = perf_budget_limits(budget, multiplier);
    summary.total_p50_ms <= budget_p50
        && summary.total_p95_ms <= budget_p95
        && summary.total_p99_ms <= budget_p99
}

fn validate_frame_suite(frames: &[VfxFrame], case: VfxCase) -> Result<(), String> {
    let expected = case.frames as usize;
    if frames.len() != expected {
        return Err(format!(
            "vfx frame count mismatch for {}: expected {expected}, got {}",
            case.effect,
            frames.len()
        ));
    }
    let mut last = None;
    for frame in frames {
        if let Some(prev) = last
            && frame.frame_idx <= prev
        {
            return Err(format!(
                "vfx frame order not monotonic for {}: {} -> {}",
                case.effect, prev, frame.frame_idx
            ));
        }
        last = Some(frame.frame_idx);
    }
    Ok(())
}

fn frame_hash_sequence(frames: &[VfxFrame]) -> Vec<String> {
    frames
        .iter()
        .map(|frame| format!("{:03}:{:016x}", frame.frame_idx, frame.hash))
        .collect()
}

fn is_release_mode() -> bool {
    !cfg!(debug_assertions)
}

// ---------------------------------------------------------------------------
// PTY E2E: cycle effects/palettes without panic
// ---------------------------------------------------------------------------

#[test]
fn pty_visual_effects_input_no_panic() -> Result<(), String> {
    let start = Instant::now();
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    logger().log_env();
    log_jsonl(
        "env",
        &[
            ("test", JsonValue::str("pty_visual_effects_input_no_panic")),
            ("bin", JsonValue::str(&demo_bin)),
            ("cols", JsonValue::u64(120)),
            ("rows", JsonValue::u64(40)),
        ],
    );

    let config = PtyConfig::default()
        .with_size(120, 40)
        .with_test_name("vfx_pty_inputs")
        .with_env("FTUI_DEMO_EXIT_AFTER_MS", "2500")
        .with_env("FTUI_DEMO_SCREEN", "14")
        .logging(false);

    let mut cmd = CommandBuilder::new(demo_bin);
    cmd.arg("--screen=14");

    let mut session =
        spawn_command(config, cmd).map_err(|err| format!("spawn demo in PTY: {err}"))?;
    std::thread::sleep(Duration::from_millis(250));
    let _ = session.read_output_result();

    let mut last_key = "startup".to_string();
    let step_delay = Duration::from_millis(120);

    let steps: [(&str, Key); 7] = [
        ("space", Key::Char(' ')),
        ("right", Key::Right),
        ("right", Key::Right),
        ("left", Key::Left),
        ("palette", Key::Char('p')),
        ("space", Key::Char(' ')),
        ("palette", Key::Char('p')),
    ];

    for (label, key) in steps {
        log_jsonl("input", &[("key", JsonValue::str(label))]);
        if let Err(err) = send_key(&mut session, label, key, step_delay, &mut last_key) {
            let output = session.read_output();
            let tail = tail_output(&output, 2048);
            let msg = format!("PTY send failed at key={label}: {err}\nTAIL:\n{tail}");
            eprintln!("{msg}");
            return Err(msg);
        }
    }

    // Request clean exit
    log_jsonl("input", &[("key", JsonValue::str("quit"))]);
    let _ = send_key(
        &mut session,
        "quit",
        Key::Char('q'),
        step_delay,
        &mut last_key,
    );

    let result = session.wait_and_drain(Duration::from_secs(6));
    let output = session.output().to_vec();
    match result {
        Ok(status) if status.success() => {
            log_jsonl(
                "result",
                &[
                    ("case", JsonValue::str("pty_visual_effects_input_no_panic")),
                    ("outcome", JsonValue::str("pass")),
                    (
                        "elapsed_ms",
                        JsonValue::u64(start.elapsed().as_millis() as u64),
                    ),
                    ("last_key", JsonValue::str(&last_key)),
                    ("output_bytes", JsonValue::u64(output.len() as u64)),
                ],
            );
            Ok(())
        }
        Ok(status) => {
            let tail = tail_output(&output, 2048);
            let msg = format!("PTY exit status failure: {status:?}\nTAIL:\n{tail}");
            eprintln!("{msg}");
            Err(msg)
        }
        Err(err) => {
            let tail = tail_output(&output, 2048);
            let msg = format!("PTY wait_and_drain error: {err}\nTAIL:\n{tail}");
            eprintln!("{msg}");
            Err(msg)
        }
    }
}

// ---------------------------------------------------------------------------
// PTY E2E: deterministic VFX harness hashes
// ---------------------------------------------------------------------------

#[test]
fn pty_vfx_harness_deterministic_hashes() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    let seed = logger().fixture().seed();
    let case = *VFX_CASES
        .first()
        .ok_or_else(|| "missing VFX cases".to_string())?;

    logger().log_env();
    log_jsonl(
        "env",
        &[
            (
                "test",
                JsonValue::str("pty_vfx_harness_deterministic_hashes"),
            ),
            ("bin", JsonValue::str(&demo_bin)),
            ("effect", JsonValue::str(case.effect)),
            ("seed", JsonValue::u64(seed)),
        ],
    );

    let frames_a = run_vfx_harness(&demo_bin, case, seed)?;
    let frames_b = run_vfx_harness(&demo_bin, case, seed)?;

    validate_frame_suite(&frames_a, case)?;
    validate_frame_suite(&frames_b, case)?;

    let hashes_a = frame_hash_sequence(&frames_a);
    let hashes_b = frame_hash_sequence(&frames_b);

    if hashes_a != hashes_b {
        return Err(format!(
            "vfx harness hashes diverged for {}:\nA={:?}\nB={:?}",
            case.effect, hashes_a, hashes_b
        ));
    }

    log_jsonl(
        "result",
        &[
            (
                "case",
                JsonValue::str("pty_vfx_harness_deterministic_hashes"),
            ),
            ("effect", JsonValue::str(case.effect)),
            ("frames", JsonValue::u64(hashes_a.len() as u64)),
        ],
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// PTY E2E: VFX perf JSONL schema
// ---------------------------------------------------------------------------

#[test]
fn pty_vfx_perf_jsonl_schema() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    let seed = logger().fixture().seed();
    let case = *VFX_CASES
        .first()
        .ok_or_else(|| "missing VFX cases".to_string())?;

    logger().log_env();
    log_jsonl(
        "env",
        &[
            ("test", JsonValue::str("pty_vfx_perf_jsonl_schema")),
            ("bin", JsonValue::str(&demo_bin)),
            ("effect", JsonValue::str(case.effect)),
            ("seed", JsonValue::u64(seed)),
        ],
    );

    let output = run_vfx_harness_output(&demo_bin, case, seed, true, &[])?;
    let perf_frames = extract_vfx_perf_frames(&output)?;
    ensure_vfx_perf_summary(&output)?;

    let mut last = None;
    for frame_idx in perf_frames {
        if let Some(prev) = last
            && frame_idx <= prev
        {
            return Err(format!(
                "vfx perf frame order not monotonic for {}: {} -> {}",
                case.effect, prev, frame_idx
            ));
        }
        last = Some(frame_idx);
    }

    log_jsonl(
        "result",
        &[
            ("case", JsonValue::str("pty_vfx_perf_jsonl_schema")),
            ("effect", JsonValue::str(case.effect)),
        ],
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// PTY E2E: VFX perf regression guard (baseline + CI margin)
// ---------------------------------------------------------------------------

#[test]
fn pty_vfx_perf_regression_guard() -> Result<(), String> {
    if !is_release_mode() {
        eprintln!("SKIPPED: pty_vfx_perf_regression_guard (debug build - run with --release)");
        return Ok(());
    }

    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;
    let seed = 0u64;

    logger().log_env();
    log_jsonl(
        "env",
        &[
            ("test", JsonValue::str("pty_vfx_perf_regression_guard")),
            ("bin", JsonValue::str(&demo_bin)),
            ("seed", JsonValue::u64(seed)),
            ("frames", JsonValue::u64(VFX_PERF_FRAMES)),
            (
                "ci_multiplier",
                JsonValue::raw(format!("{:.2}", VFX_PERF_CI_MULTIPLIER)),
            ),
        ],
    );

    for case in VFX_PERF_CASES {
        let output = run_vfx_harness_output(&demo_bin, *case, seed, true, &[])?;
        let summary = extract_vfx_perf_summary(&output)?;
        let frames = extract_vfx_frames(&output)?;
        let budget = find_perf_budget(*case).ok_or_else(|| {
            format!(
                "missing VFX perf baseline for {} {}x{}",
                case.effect, case.cols, case.rows
            )
        })?;

        let (budget_p50, budget_p95, budget_p99) =
            perf_budget_limits(budget, VFX_PERF_CI_MULTIPLIER);
        let passed = perf_within_budget(summary, budget, VFX_PERF_CI_MULTIPLIER);
        let delta_p50 = summary.total_p50_ms - budget_p50;
        let delta_p95 = summary.total_p95_ms - budget_p95;
        let delta_p99 = summary.total_p99_ms - budget_p99;

        let scenario = case.scenario_name(seed);
        let checksum_path = golden_checksum_path(vfx_golden_base_dir(), &scenario);
        let expected_hashes = load_golden_checksums(&checksum_path).unwrap_or_default();
        let actual_hashes = frame_hash_sequence(&frames);
        if expected_hashes.is_empty() {
            log_jsonl(
                "vfx_perf_guard_hash",
                &[
                    ("scenario", JsonValue::str(&scenario)),
                    ("effect", JsonValue::str(case.effect)),
                    ("cols", JsonValue::u64(case.cols as u64)),
                    ("rows", JsonValue::u64(case.rows as u64)),
                    ("compared_frames", JsonValue::u64(0)),
                    ("outcome", JsonValue::str("missing")),
                ],
            );
        } else {
            if expected_hashes.len() > actual_hashes.len() {
                return Err(format!(
                    "VFX hash guard for {scenario} expected {} frames but got {}",
                    expected_hashes.len(),
                    actual_hashes.len()
                ));
            }
            let (outcome, mismatch) =
                verify_checksums(&actual_hashes[..expected_hashes.len()], &expected_hashes);
            let (mismatch_idx, mismatch_frame, expected_hash, actual_hash) =
                if let Some(idx) = mismatch {
                    (
                        JsonValue::u64(idx as u64),
                        JsonValue::u64(frames[idx].frame_idx),
                        JsonValue::str(&expected_hashes[idx]),
                        JsonValue::str(&actual_hashes[idx]),
                    )
                } else {
                    (
                        JsonValue::u64(0),
                        JsonValue::u64(0),
                        JsonValue::str(""),
                        JsonValue::str(""),
                    )
                };

            log_jsonl(
                "vfx_perf_guard_hash",
                &[
                    ("scenario", JsonValue::str(&scenario)),
                    ("effect", JsonValue::str(case.effect)),
                    ("cols", JsonValue::u64(case.cols as u64)),
                    ("rows", JsonValue::u64(case.rows as u64)),
                    (
                        "compared_frames",
                        JsonValue::u64(expected_hashes.len() as u64),
                    ),
                    (
                        "outcome",
                        JsonValue::str(match outcome {
                            GoldenOutcome::Pass => "pass",
                            GoldenOutcome::Fail => "fail",
                            GoldenOutcome::Skip => "skip",
                        }),
                    ),
                    ("mismatch_idx", mismatch_idx),
                    ("frame_idx", mismatch_frame),
                    ("expected_hash", expected_hash),
                    ("actual_hash", actual_hash),
                ],
            );

            if outcome != GoldenOutcome::Pass {
                return Err(format!("VFX hash guard failed for {scenario}"));
            }
        }

        log_jsonl(
            "vfx_perf_guard",
            &[
                ("effect", JsonValue::str(case.effect)),
                ("cols", JsonValue::u64(case.cols as u64)),
                ("rows", JsonValue::u64(case.rows as u64)),
                ("count", JsonValue::u64(summary.count)),
                (
                    "p50_ms",
                    JsonValue::raw(format!("{:.3}", summary.total_p50_ms)),
                ),
                (
                    "p95_ms",
                    JsonValue::raw(format!("{:.3}", summary.total_p95_ms)),
                ),
                (
                    "p99_ms",
                    JsonValue::raw(format!("{:.3}", summary.total_p99_ms)),
                ),
                (
                    "budget_p50_ms",
                    JsonValue::raw(format!("{:.3}", budget_p50)),
                ),
                (
                    "budget_p95_ms",
                    JsonValue::raw(format!("{:.3}", budget_p95)),
                ),
                (
                    "budget_p99_ms",
                    JsonValue::raw(format!("{:.3}", budget_p99)),
                ),
                ("delta_p50_ms", JsonValue::raw(format!("{:.3}", delta_p50))),
                ("delta_p95_ms", JsonValue::raw(format!("{:.3}", delta_p95))),
                ("delta_p99_ms", JsonValue::raw(format!("{:.3}", delta_p99))),
                ("passed", JsonValue::bool(passed)),
            ],
        );

        if !passed {
            return Err(format!(
                "VFX perf regression for {} {}x{}: p50 {:.3} (budget {:.3}, delta {:.3}), p95 {:.3} (budget {:.3}, delta {:.3}), p99 {:.3} (budget {:.3}, delta {:.3})",
                case.effect,
                case.cols,
                case.rows,
                summary.total_p50_ms,
                budget_p50,
                delta_p50,
                summary.total_p95_ms,
                budget_p95,
                delta_p95,
                summary.total_p99_ms,
                budget_p99,
                delta_p99
            ));
        }
    }

    Ok(())
}

#[test]
fn vfx_perf_budget_rejects_regression() {
    let budget = VFX_PERF_BASELINES
        .first()
        .expect("missing VFX perf baselines");
    let summary = VfxPerfSummary {
        count: VFX_PERF_FRAMES,
        total_p50_ms: budget.total_p50_ms * 3.0,
        total_p95_ms: budget.total_p95_ms * 3.0,
        total_p99_ms: budget.total_p99_ms * 3.0,
    };

    assert!(
        !perf_within_budget(summary, budget, VFX_PERF_CI_MULTIPLIER),
        "expected synthetic regression to violate budget"
    );
}

fn contains_seq(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn vfx_shape3d_smoke_runs() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    let seed = logger().fixture().seed();
    let case = VfxCase {
        effect: "shape3d",
        frames: VFX_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: VFX_COLS,
        rows: VFX_ROWS,
    };

    logger().log_env();
    let frames = run_vfx_harness(&demo_bin, case, seed)?;
    validate_frame_suite(&frames, case)?;
    Ok(())
}

#[test]
fn vfx_shape3d_wezterm_mux_policy_omits_sync_output_sequences() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    let seed = logger().fixture().seed();
    let case = VfxCase {
        effect: "shape3d",
        frames: VFX_FRAMES,
        tick_ms: VFX_TICK_MS,
        cols: VFX_COLS,
        rows: VFX_ROWS,
    };

    // Simulate a mixed environment: TERM_PROGRAM says "Ghostty" but WezTerm mux
    // markers are present. In this scenario `caps.sync_output` may be true,
    // but mux policy must suppress emitting DEC 2026 sequences.
    let extra_env = &[
        ("TERM_PROGRAM", "Ghostty"),
        ("WEZTERM_PANE", "1"),
        ("COLORTERM", "truecolor"),
    ];
    let output = run_vfx_harness_output(&demo_bin, case, seed, false, extra_env)?;

    assert!(
        !contains_seq(&output, b"\x1b[?2026h"),
        "must not emit DEC ?2026h under mux policy"
    );
    assert!(
        !contains_seq(&output, b"\x1b[?2026l"),
        "must not emit DEC ?2026l under mux policy"
    );
    Ok(())
}

/// Update goldens:
/// `BLESS=1 FTUI_VFX_BLESS_NOTE="reason" cargo test -p ftui-demo-showcase --test visual_effects_pty vfx_golden_hash_registry -- --nocapture`
#[test]
fn vfx_golden_hash_registry() -> Result<(), String> {
    let demo_bin = std::env::var("CARGO_BIN_EXE_ftui-demo-showcase").map_err(|err| {
        format!("CARGO_BIN_EXE_ftui-demo-showcase must be set for PTY tests: {err}")
    })?;

    let seed = logger().fixture().seed();
    let base_dir = vfx_golden_base_dir();
    let bless_note = std::env::var("FTUI_VFX_BLESS_NOTE").ok();

    logger().log_env();
    for case in VFX_CASES {
        let frames = run_vfx_harness(&demo_bin, *case, seed)?;
        validate_frame_suite(&frames, *case)?;
        let actual = frame_hash_sequence(&frames);

        let scenario = case.scenario_name(seed);
        let checksum_path = golden_checksum_path(base_dir, &scenario);
        let expected = load_golden_checksums(&checksum_path).unwrap_or_default();

        if is_bless_mode() {
            save_golden_checksums(&checksum_path, &actual)
                .map_err(|err| format!("save golden checksums failed for {scenario}: {err}"))?;
            log_jsonl(
                "vfx_golden",
                &[
                    ("scenario", JsonValue::str(&scenario)),
                    ("effect", JsonValue::str(case.effect)),
                    ("outcome", JsonValue::str("blessed")),
                    ("frames", JsonValue::u64(actual.len() as u64)),
                    ("seed", JsonValue::u64(seed)),
                    ("cols", JsonValue::u64(case.cols as u64)),
                    ("rows", JsonValue::u64(case.rows as u64)),
                    ("tick_ms", JsonValue::u64(case.tick_ms)),
                    (
                        "note",
                        JsonValue::str(bless_note.clone().unwrap_or_else(|| "none".to_string())),
                    ),
                ],
            );
            continue;
        }

        if expected.is_empty() {
            if is_golden_enforced() {
                return Err(format!(
                    "missing golden checksums for {scenario} (set BLESS=1 to generate)"
                ));
            }
            log_jsonl(
                "vfx_golden",
                &[
                    ("scenario", JsonValue::str(&scenario)),
                    ("effect", JsonValue::str(case.effect)),
                    ("outcome", JsonValue::str("first_run")),
                    ("frames", JsonValue::u64(actual.len() as u64)),
                ],
            );
            continue;
        }

        let (outcome, mismatch) = verify_checksums(&actual, &expected);
        assert_eq!(
            outcome,
            GoldenOutcome::Pass,
            "VFX golden hash mismatch for {scenario} at {mismatch:?}\nexpected: {expected:?}\nactual:   {actual:?}\nRun with BLESS=1 to update golden files."
        );
        log_jsonl(
            "vfx_golden",
            &[
                ("scenario", JsonValue::str(&scenario)),
                ("effect", JsonValue::str(case.effect)),
                ("outcome", JsonValue::str("pass")),
                ("frames", JsonValue::u64(actual.len() as u64)),
            ],
        );
    }

    Ok(())
}
