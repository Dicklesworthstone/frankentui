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
    let label = format!("vfx_harness_{}", case.effect);
    let config = PtyConfig::default()
        .with_size(case.cols, case.rows)
        .with_test_name(label)
        .with_env("FTUI_DEMO_VFX_SEED", seed.to_string())
        .with_env("FTUI_DEMO_DETERMINISTIC", "1")
        .with_env("E2E_SEED", seed.to_string())
        .logging(false);

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

    let mut session =
        spawn_command(config, cmd).map_err(|err| format!("spawn vfx harness: {err}"))?;
    let status = session
        .wait_and_drain(Duration::from_secs(6))
        .map_err(|err| format!("wait vfx harness: {err}"))?;
    let output = session.output().to_vec();
    let frames = extract_vfx_frames(&output)?;

    if !status.success() {
        let tail = tail_output(&output, 4096);
        return Err(format!(
            "vfx harness exit failure: {status:?}\nTAIL:\n{tail}"
        ));
    }

    Ok(frames)
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
