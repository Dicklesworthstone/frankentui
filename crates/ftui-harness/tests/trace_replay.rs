use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use ftui_harness::determinism::{JsonValue, TestJsonlLogger};
use ftui_harness::trace_replay::replay_trace;

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const EXPECTED_CHECKSUM_EMPTY_2X2: u64 = 0xc815b2ba593b90f5;
const EXPECTED_CHECKSUM_A_ONLY_2X2: u64 = 0x7960ba558452e6b4;
const EXPECTED_CHECKSUM_AB_2X2: u64 = 0x28f1067816e37544;

#[derive(Clone)]
struct CellData {
    kind: u8,
    char_code: u32,
    grapheme: Vec<u8>,
    fg: u32,
    bg: u32,
    attrs: u32,
}

impl Default for CellData {
    fn default() -> Self {
        Self {
            kind: 0,
            char_code: 0,
            grapheme: Vec::new(),
            fg: ftui_render::cell::PackedRgba::WHITE.0,
            bg: ftui_render::cell::PackedRgba::TRANSPARENT.0,
            attrs: 0,
        }
    }
}

fn fnv1a_update(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn checksum_grid(cells: &[CellData]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for cell in cells {
        fnv1a_update(&mut hash, &[cell.kind]);
        match cell.kind {
            0 | 3 => fnv1a_update(&mut hash, &0u16.to_le_bytes()),
            1 => {
                let ch = char::from_u32(cell.char_code).unwrap_or('\u{FFFD}');
                let mut buf = [0u8; 4];
                let encoded = ch.encode_utf8(&mut buf);
                let bytes = encoded.as_bytes();
                let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
                fnv1a_update(&mut hash, &len.to_le_bytes());
                fnv1a_update(&mut hash, bytes);
            }
            2 => {
                let len = u16::try_from(cell.grapheme.len()).unwrap_or(u16::MAX);
                fnv1a_update(&mut hash, &len.to_le_bytes());
                fnv1a_update(&mut hash, &cell.grapheme[..len as usize]);
            }
            _ => fnv1a_update(&mut hash, &0u16.to_le_bytes()),
        }
        fnv1a_update(&mut hash, &cell.fg.to_le_bytes());
        fnv1a_update(&mut hash, &cell.bg.to_le_bytes());
        fnv1a_update(&mut hash, &cell.attrs.to_le_bytes());
    }
    hash
}

fn write_diff_runs(path: &Path, width: u16, height: u16, runs: &[Run]) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(&width.to_le_bytes())?;
    file.write_all(&height.to_le_bytes())?;
    let run_count = runs.len() as u32;
    file.write_all(&run_count.to_le_bytes())?;
    for run in runs {
        file.write_all(&run.y.to_le_bytes())?;
        file.write_all(&run.x0.to_le_bytes())?;
        file.write_all(&run.x1.to_le_bytes())?;
        for cell in &run.cells {
            file.write_all(&[cell.kind])?;
            match cell.kind {
                0 | 3 => {}
                1 => file.write_all(&cell.char_code.to_le_bytes())?,
                2 => {
                    let len = u16::try_from(cell.grapheme.len()).unwrap_or(u16::MAX);
                    file.write_all(&len.to_le_bytes())?;
                    file.write_all(&cell.grapheme)?;
                }
                _ => {}
            }
            file.write_all(&cell.fg.to_le_bytes())?;
            file.write_all(&cell.bg.to_le_bytes())?;
            file.write_all(&cell.attrs.to_le_bytes())?;
        }
    }
    Ok(())
}

struct Run {
    y: u16,
    x0: u16,
    x1: u16,
    cells: Vec<CellData>,
}

fn unique_temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("ftui_trace_replay_{nanos}"))
}

fn logger() -> &'static TestJsonlLogger {
    static LOGGER: OnceLock<TestJsonlLogger> = OnceLock::new();
    LOGGER.get_or_init(|| {
        let mut logger = TestJsonlLogger::new("trace_replay", 1337);
        logger.add_context_str("suite", "trace_replay");
        logger
    })
}

#[test]
fn replay_trace_success_and_mismatch() {
    let base_dir = unique_temp_dir();
    let frames_dir = base_dir.join("frames");
    fs::create_dir_all(&frames_dir).expect("create temp dirs");

    logger().log_env();

    let width = 2u16;
    let height = 2u16;

    let mut grid = vec![CellData::default(); (width * height) as usize];
    let empty_checksum = checksum_grid(&grid);
    assert_eq!(
        empty_checksum, EXPECTED_CHECKSUM_EMPTY_2X2,
        "unexpected checksum for empty 2x2 grid"
    );

    let cell_a = CellData {
        kind: 1,
        char_code: 'A' as u32,
        ..Default::default()
    };
    grid[0] = cell_a.clone();

    let checksum0 = checksum_grid(&grid);
    assert_eq!(
        checksum0, EXPECTED_CHECKSUM_A_ONLY_2X2,
        "checksum stability regression for frame 0"
    );

    let cell_b = CellData {
        kind: 1,
        char_code: 'B' as u32,
        ..Default::default()
    };
    grid[1] = cell_b.clone();

    let checksum1 = checksum_grid(&grid);
    assert_eq!(
        checksum1, EXPECTED_CHECKSUM_AB_2X2,
        "checksum stability regression for frame 1"
    );

    logger().log(
        "trace_replay_frame",
        &[
            ("frame_idx", JsonValue::u64(0)),
            ("cols", JsonValue::u64(width as u64)),
            ("rows", JsonValue::u64(height as u64)),
            ("checksum", JsonValue::str(format!("{checksum0:016x}"))),
        ],
    );
    logger().log(
        "trace_replay_frame",
        &[
            ("frame_idx", JsonValue::u64(1)),
            ("cols", JsonValue::u64(width as u64)),
            ("rows", JsonValue::u64(height as u64)),
            ("checksum", JsonValue::str(format!("{checksum1:016x}"))),
        ],
    );

    let run0 = Run {
        y: 0,
        x0: 0,
        x1: 0,
        cells: vec![cell_a],
    };
    let run1 = Run {
        y: 0,
        x0: 1,
        x1: 1,
        cells: vec![cell_b],
    };

    let payload0 = frames_dir.join("frame_0000.bin");
    let payload1 = frames_dir.join("frame_0001.bin");
    write_diff_runs(&payload0, width, height, &[run0]).expect("write payload 0");
    write_diff_runs(&payload1, width, height, &[run1]).expect("write payload 1");

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(
        trace,
        r#"{{"event":"trace_header","schema_version":"render-trace-v1","run_id":"test","seed":0}}"#
    )
    .unwrap();
    writeln!(
        trace,
        r#"{{"event":"frame","frame_idx":0,"cols":2,"rows":2,"payload_kind":"diff_runs_v1","payload_path":"frames/frame_0000.bin","checksum":"{:016x}"}}"#,
        checksum0
    )
    .unwrap();
    writeln!(
        trace,
        r#"{{"event":"frame","frame_idx":1,"cols":2,"rows":2,"payload_kind":"diff_runs_v1","payload_path":"frames/frame_0001.bin","checksum":"{:016x}"}}"#,
        checksum1
    )
    .unwrap();
    writeln!(
        trace,
        r#"{{"event":"trace_summary","total_frames":2,"final_checksum_chain":"{:016x}","elapsed_ms":1}}"#,
        checksum1
    )
    .unwrap();

    let summary = replay_trace(&trace_path).expect("replay should succeed");
    assert_eq!(summary.frames, 2);
    assert_eq!(summary.last_checksum, Some(EXPECTED_CHECKSUM_AB_2X2));

    let summary_repeat = replay_trace(&trace_path).expect("replay should be deterministic");
    assert_eq!(summary_repeat.frames, summary.frames);
    assert_eq!(summary_repeat.last_checksum, summary.last_checksum);

    let bad_trace = base_dir.join("trace_bad.jsonl");
    let mut trace_bad = fs::File::create(&bad_trace).expect("create bad trace");
    writeln!(
        trace_bad,
        r#"{{"event":"frame","frame_idx":0,"cols":2,"rows":2,"payload_kind":"diff_runs_v1","payload_path":"frames/frame_0000.bin","checksum":"{:016x}"}}"#,
        checksum0
    )
    .unwrap();
    writeln!(
        trace_bad,
        r#"{{"event":"frame","frame_idx":1,"cols":2,"rows":2,"payload_kind":"diff_runs_v1","payload_path":"frames/frame_0001.bin","checksum":"{:016x}"}}"#,
        checksum1 ^ 1
    )
    .unwrap();

    let err = replay_trace(&bad_trace).expect_err("replay should fail");
    assert!(
        err.to_string().contains("checksum mismatch"),
        "unexpected error: {err}"
    );
}

fn write_full_buffer(
    path: &Path,
    width: u16,
    height: u16,
    cells: &[CellData],
) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(&width.to_le_bytes())?;
    file.write_all(&height.to_le_bytes())?;
    for cell in cells {
        file.write_all(&[cell.kind])?;
        match cell.kind {
            0 | 3 => {}
            1 => file.write_all(&cell.char_code.to_le_bytes())?,
            2 => {
                let len = u16::try_from(cell.grapheme.len()).unwrap_or(u16::MAX);
                file.write_all(&len.to_le_bytes())?;
                file.write_all(&cell.grapheme)?;
            }
            _ => {}
        }
        file.write_all(&cell.fg.to_le_bytes())?;
        file.write_all(&cell.bg.to_le_bytes())?;
        file.write_all(&cell.attrs.to_le_bytes())?;
    }
    Ok(())
}

#[test]
fn replay_full_buffer_payload() {
    let base_dir = unique_temp_dir();
    let frames_dir = base_dir.join("frames");
    fs::create_dir_all(&frames_dir).expect("create temp dirs");

    let width = 2u16;
    let height = 1u16;

    let cells = vec![
        CellData {
            kind: 1,
            char_code: 'H' as u32,
            ..Default::default()
        },
        CellData {
            kind: 1,
            char_code: 'i' as u32,
            ..Default::default()
        },
    ];
    let checksum = checksum_grid(&cells);

    let payload_path = frames_dir.join("frame_0000.bin");
    write_full_buffer(&payload_path, width, height, &cells).expect("write payload");

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(
        trace,
        r#"{{"event":"trace_header","schema_version":"render-trace-v1","run_id":"test","seed":0}}"#
    )
    .unwrap();
    writeln!(
        trace,
        r#"{{"event":"frame","frame_idx":0,"cols":2,"rows":1,"payload_kind":"full_buffer_v1","payload_path":"frames/frame_0000.bin","checksum":"{:016x}"}}"#,
        checksum
    )
    .unwrap();

    let summary = replay_trace(&trace_path).expect("replay should succeed");
    assert_eq!(summary.frames, 1);
    assert_eq!(summary.last_checksum, Some(checksum));
}

#[test]
fn replay_no_frames_fails() {
    let base_dir = unique_temp_dir();
    fs::create_dir_all(&base_dir).expect("create temp dir");

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(
        trace,
        r#"{{"event":"trace_header","schema_version":"render-trace-v1","run_id":"test","seed":0}}"#
    )
    .unwrap();

    let err = replay_trace(&trace_path).expect_err("no frames");
    assert!(
        err.to_string().contains("no frame records found"),
        "unexpected error: {err}"
    );
}

#[test]
fn replay_missing_event_field_fails() {
    let base_dir = unique_temp_dir();
    fs::create_dir_all(&base_dir).expect("create temp dir");

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(trace, r#"{{"no_event": true}}"#).unwrap();

    let err = replay_trace(&trace_path).expect_err("missing event");
    assert!(
        err.to_string().contains("missing event"),
        "unexpected error: {err}"
    );
}

#[test]
fn replay_invalid_json_fails() {
    let base_dir = unique_temp_dir();
    fs::create_dir_all(&base_dir).expect("create temp dir");

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(trace, "this is not json").unwrap();

    let err = replay_trace(&trace_path).expect_err("invalid json");
    assert!(
        err.to_string().contains("invalid JSONL"),
        "unexpected error: {err}"
    );
}

#[test]
fn replay_unsupported_payload_kind_fails() {
    let base_dir = unique_temp_dir();
    fs::create_dir_all(&base_dir).expect("create temp dir");

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(
        trace,
        r#"{{"event":"frame","frame_idx":0,"cols":2,"rows":2,"payload_kind":"unknown_v9","payload_path":"nope.bin","checksum":"0000000000000000"}}"#
    )
    .unwrap();

    let err = replay_trace(&trace_path).expect_err("unsupported payload");
    assert!(
        err.to_string().contains("unsupported payload_kind"),
        "unexpected error: {err}"
    );
}

#[test]
fn replay_none_payload_kind() {
    let base_dir = unique_temp_dir();
    fs::create_dir_all(&base_dir).expect("create temp dir");

    // A "none" payload means no disk payload — just validate the checksum of current grid.
    // For a fresh 1x1 grid, the checksum is deterministic.
    let grid = vec![CellData::default()];
    let checksum = checksum_grid(&grid);

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(
        trace,
        r#"{{"event":"frame","frame_idx":0,"cols":1,"rows":1,"payload_kind":"none","checksum":"{:016x}"}}"#,
        checksum
    )
    .unwrap();

    let summary = replay_trace(&trace_path).expect("replay should succeed with none payload");
    assert_eq!(summary.frames, 1);
    assert_eq!(summary.last_checksum, Some(checksum));
}

#[test]
fn replay_skips_blank_lines() {
    let base_dir = unique_temp_dir();
    fs::create_dir_all(&base_dir).expect("create temp dir");

    let grid = vec![CellData::default()];
    let checksum = checksum_grid(&grid);

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(trace).unwrap(); // blank line
    writeln!(
        trace,
        r#"{{"event":"trace_header","schema_version":"render-trace-v1"}}"#
    )
    .unwrap();
    writeln!(trace, "   ").unwrap(); // whitespace-only line
    writeln!(
        trace,
        r#"{{"event":"frame","frame_idx":0,"cols":1,"rows":1,"payload_kind":"none","checksum":"{:016x}"}}"#,
        checksum
    )
    .unwrap();

    let summary = replay_trace(&trace_path).expect("should skip blank lines");
    assert_eq!(summary.frames, 1);
}

#[test]
fn replay_grapheme_cell_in_full_buffer() {
    let base_dir = unique_temp_dir();
    let frames_dir = base_dir.join("frames");
    fs::create_dir_all(&frames_dir).expect("create temp dirs");

    let emoji_bytes = "🦀".as_bytes().to_vec();
    let cells = vec![
        CellData {
            kind: 2,
            char_code: 0,
            grapheme: emoji_bytes,
            ..Default::default()
        },
        CellData {
            kind: 3, // Continuation
            ..Default::default()
        },
    ];
    let checksum = checksum_grid(&cells);

    let payload_path = frames_dir.join("frame_0000.bin");
    write_full_buffer(&payload_path, 2, 1, &cells).expect("write payload");

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(
        trace,
        r#"{{"event":"frame","frame_idx":0,"cols":2,"rows":1,"payload_kind":"full_buffer_v1","payload_path":"frames/frame_0000.bin","checksum":"{:016x}"}}"#,
        checksum
    )
    .unwrap();

    let summary = replay_trace(&trace_path).expect("grapheme replay should succeed");
    assert_eq!(summary.frames, 1);
    assert_eq!(summary.last_checksum, Some(checksum));
}

#[test]
fn replay_resize_between_frames() {
    let base_dir = unique_temp_dir();
    let frames_dir = base_dir.join("frames");
    fs::create_dir_all(&frames_dir).expect("create temp dirs");

    // Frame 0: 1x1 grid with 'A'
    let cells_1x1 = vec![CellData {
        kind: 1,
        char_code: 'A' as u32,
        ..Default::default()
    }];
    let checksum0 = checksum_grid(&cells_1x1);
    let payload0 = frames_dir.join("frame_0000.bin");
    write_full_buffer(&payload0, 1, 1, &cells_1x1).expect("write");

    // Frame 1: resize to 2x1 with 'X','Y'
    let cells_2x1 = vec![
        CellData {
            kind: 1,
            char_code: 'X' as u32,
            ..Default::default()
        },
        CellData {
            kind: 1,
            char_code: 'Y' as u32,
            ..Default::default()
        },
    ];
    let checksum1 = checksum_grid(&cells_2x1);
    let payload1 = frames_dir.join("frame_0001.bin");
    write_full_buffer(&payload1, 2, 1, &cells_2x1).expect("write");

    let trace_path = base_dir.join("trace.jsonl");
    let mut trace = fs::File::create(&trace_path).expect("create trace");
    writeln!(
        trace,
        r#"{{"event":"frame","frame_idx":0,"cols":1,"rows":1,"payload_kind":"full_buffer_v1","payload_path":"frames/frame_0000.bin","checksum":"{:016x}"}}"#,
        checksum0
    )
    .unwrap();
    writeln!(
        trace,
        r#"{{"event":"frame","frame_idx":1,"cols":2,"rows":1,"payload_kind":"full_buffer_v1","payload_path":"frames/frame_0001.bin","checksum":"{:016x}"}}"#,
        checksum1
    )
    .unwrap();

    let summary = replay_trace(&trace_path).expect("resize replay should succeed");
    assert_eq!(summary.frames, 2);
    assert_eq!(summary.last_checksum, Some(checksum1));
}

#[test]
fn replay_nonexistent_file_fails() {
    let err = replay_trace("/tmp/nonexistent_trace_file_12345.jsonl")
        .expect_err("should fail for nonexistent file");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

mod frankenlab_replay {
    use super::*;
    use ftui_harness::determinism::{JsonValue, LabScenario};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct ScenarioFrame {
        width: u16,
        height: u16,
        cells: Vec<CellData>,
        checksum: u64,
    }

    #[derive(Clone)]
    struct ScenarioOutput {
        frames: Vec<ScenarioFrame>,
        async_completed: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TraceArtifacts {
        trace_jsonl: String,
        payloads: Vec<Vec<u8>>,
        frame_count: usize,
        last_checksum: u64,
        async_completed: bool,
        event_count: u64,
    }

    #[derive(Debug)]
    struct SyntheticAppState {
        width: u16,
        height: u16,
        timer_ticks: u64,
        animation_ticks: u64,
        resize_events: u64,
        last_now_ms: u64,
        keyboard: Vec<char>,
        animation_phase: u8,
    }

    impl Default for SyntheticAppState {
        fn default() -> Self {
            Self {
                width: 24,
                height: 4,
                timer_ticks: 0,
                animation_ticks: 0,
                resize_events: 0,
                last_now_ms: 0,
                keyboard: Vec::new(),
                animation_phase: 0,
            }
        }
    }

    fn write_line(cells: &mut [CellData], width: u16, height: u16, row: u16, text: &str) {
        if row >= height {
            return;
        }
        let width = width as usize;
        let row = row as usize;
        for (col, ch) in text.chars().take(width).enumerate() {
            let idx = row * width + col;
            cells[idx] = CellData {
                kind: 1,
                char_code: ch as u32,
                ..Default::default()
            };
        }
    }

    fn render_scenario_frame(state: &SyntheticAppState) -> ScenarioFrame {
        let mut cells = vec![CellData::default(); state.width as usize * state.height as usize];
        let keyboard: String = state.keyboard.iter().collect();
        let line0 = format!("tick:{} t={}ms", state.timer_ticks, state.last_now_ms);
        let line1 = format!("keys:{keyboard}");
        let line2 = format!(
            "resize:{} anim:{}",
            state.resize_events, state.animation_phase
        );
        let line3 = format!("anim_ticks:{}", state.animation_ticks);
        write_line(&mut cells, state.width, state.height, 0, &line0);
        write_line(&mut cells, state.width, state.height, 1, &line1);
        write_line(&mut cells, state.width, state.height, 2, &line2);
        write_line(&mut cells, state.width, state.height, 3, &line3);
        let checksum = checksum_grid(&cells);
        ScenarioFrame {
            width: state.width,
            height: state.height,
            cells,
            checksum,
        }
    }

    fn encode_full_buffer_payload(width: u16, height: u16, cells: &[CellData]) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            4 + cells
                .iter()
                .map(|cell| {
                    1 + match cell.kind {
                        1 => 4,
                        2 => 2 + cell.grapheme.len(),
                        _ => 0,
                    } + 12
                })
                .sum::<usize>(),
        );
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        for cell in cells {
            out.push(cell.kind);
            match cell.kind {
                0 | 3 => {}
                1 => out.extend_from_slice(&cell.char_code.to_le_bytes()),
                2 => {
                    let len = u16::try_from(cell.grapheme.len()).unwrap_or(u16::MAX);
                    out.extend_from_slice(&len.to_le_bytes());
                    out.extend_from_slice(&cell.grapheme);
                }
                _ => {}
            }
            out.extend_from_slice(&cell.fg.to_le_bytes());
            out.extend_from_slice(&cell.bg.to_le_bytes());
            out.extend_from_slice(&cell.attrs.to_le_bytes());
        }
        out
    }

    fn build_trace_artifacts(seed: u64) -> TraceArtifacts {
        let scenario =
            LabScenario::new_with("frankenlab_replay", "deterministic_full_app", seed, true, 1);
        let run = scenario.run(|ctx| {
            let mut state = SyntheticAppState::default();
            let mut frames = Vec::new();

            for step in 0_u16..12_u16 {
                state.timer_ticks = state.timer_ticks.saturating_add(1);
                state.last_now_ms = ctx.now_ms();
                ctx.log_info(
                    "lab.event.timer",
                    &[
                        ("step", JsonValue::u64(step as u64)),
                        ("tick", JsonValue::u64(state.timer_ticks)),
                    ],
                );

                let key = char::from(b'a'.wrapping_add(((seed + step as u64) % 26) as u8));
                state.keyboard.push(key);
                if state.keyboard.len() > 10 {
                    state.keyboard.remove(0);
                }
                ctx.log_info(
                    "lab.event.keyboard",
                    &[
                        ("step", JsonValue::u64(step as u64)),
                        ("key", JsonValue::str(key.to_string())),
                    ],
                );

                if step % 3 == 0 {
                    state.width = 20 + (((seed as u16) + step) % 5);
                    state.height = 3 + (((seed as u16) + step) % 2);
                    state.resize_events = state.resize_events.saturating_add(1);
                    ctx.log_info(
                        "lab.event.resize",
                        &[
                            ("step", JsonValue::u64(step as u64)),
                            ("cols", JsonValue::u64(state.width as u64)),
                            ("rows", JsonValue::u64(state.height as u64)),
                        ],
                    );
                }

                state.animation_ticks = state.animation_ticks.saturating_add(1);
                state.animation_phase =
                    ((state.animation_phase as u64 + seed + state.last_now_ms + step as u64) % 32)
                        as u8;
                ctx.log_info(
                    "lab.event.animation",
                    &[
                        ("step", JsonValue::u64(step as u64)),
                        ("phase", JsonValue::u64(state.animation_phase as u64)),
                    ],
                );

                frames.push(render_scenario_frame(&state));
            }

            ctx.log_info(
                "lab.event.async_complete",
                &[
                    ("done", JsonValue::bool(true)),
                    ("timer_events", JsonValue::u64(state.timer_ticks)),
                    ("animation_events", JsonValue::u64(state.animation_ticks)),
                ],
            );

            ScenarioOutput {
                frames,
                async_completed: true,
            }
        });

        let mut trace = format!(
            "{{\"event\":\"trace_header\",\"schema_version\":\"render-trace-v1\",\"run_id\":\"{}\",\"seed\":{}}}\n",
            run.result.run_id, run.result.seed
        );
        let mut payloads = Vec::new();
        for (frame_idx, frame) in run.output.frames.iter().enumerate() {
            payloads.push(encode_full_buffer_payload(
                frame.width,
                frame.height,
                &frame.cells,
            ));
            trace.push_str(&format!(
                "{{\"event\":\"frame\",\"frame_idx\":{frame_idx},\"cols\":{},\"rows\":{},\"payload_kind\":\"full_buffer_v1\",\"payload_path\":\"frames/frame_{frame_idx:04}.bin\",\"checksum\":\"{:016x}\"}}\n",
                frame.width, frame.height, frame.checksum
            ));
        }

        let last_checksum = run.output.frames.last().map_or(0, |frame| frame.checksum);
        trace.push_str(&format!(
            "{{\"event\":\"trace_summary\",\"total_frames\":{},\"final_checksum_chain\":\"{:016x}\"}}\n",
            run.output.frames.len(),
            last_checksum
        ));

        TraceArtifacts {
            trace_jsonl: trace,
            payloads,
            frame_count: run.output.frames.len(),
            last_checksum,
            async_completed: run.output.async_completed,
            event_count: run.result.event_count,
        }
    }

    fn write_trace_artifacts(
        base_dir: &Path,
        artifacts: &TraceArtifacts,
    ) -> std::io::Result<PathBuf> {
        let frames_dir = base_dir.join("frames");
        fs::create_dir_all(&frames_dir)?;
        for (idx, payload) in artifacts.payloads.iter().enumerate() {
            let path = frames_dir.join(format!("frame_{idx:04}.bin"));
            fs::write(path, payload)?;
        }
        let trace_path = base_dir.join("trace.jsonl");
        fs::write(&trace_path, &artifacts.trace_jsonl)?;
        Ok(trace_path)
    }

    #[derive(Default, Clone)]
    struct SpanCapture {
        spans: Arc<Mutex<Vec<String>>>,
    }

    struct SpanSubscriber {
        next_id: AtomicU64,
        capture: SpanCapture,
    }

    impl tracing::Subscriber for SpanSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            if attrs.metadata().name() == "lab.scenario" {
                self.capture
                    .spans
                    .lock()
                    .expect("span capture lock")
                    .push(attrs.metadata().name().to_string());
            }
            tracing::span::Id::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed))
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, _event: &tracing::Event<'_>) {}

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    fn capture_lab_scenario_spans(run: impl FnOnce()) -> Vec<String> {
        let capture = SpanCapture::default();
        let subscriber = SpanSubscriber {
            next_id: AtomicU64::new(1),
            capture: capture.clone(),
        };
        let _guard = tracing::subscriber::set_default(subscriber);
        run();
        capture.spans.lock().expect("span capture lock").clone()
    }

    #[test]
    fn frankenlab_full_application_replay_is_byte_identical_for_100_seeds() {
        let root = unique_temp_dir();
        fs::create_dir_all(&root).expect("create root temp dir");

        let mut divergence_count = 0usize;
        let spans = capture_lab_scenario_spans(|| {
            for seed in 0_u64..100_u64 {
                let artifacts_a = build_trace_artifacts(seed);
                let artifacts_b = build_trace_artifacts(seed);

                assert!(
                    artifacts_a.async_completed,
                    "seed {seed}: async did not complete"
                );
                assert!(
                    artifacts_b.async_completed,
                    "seed {seed}: replay async did not complete"
                );
                assert_eq!(
                    artifacts_a.event_count, artifacts_b.event_count,
                    "seed {seed}: event_count diverged"
                );
                assert_eq!(
                    artifacts_a.trace_jsonl, artifacts_b.trace_jsonl,
                    "seed {seed}: trace JSONL diverged"
                );
                assert_eq!(
                    artifacts_a.payloads, artifacts_b.payloads,
                    "seed {seed}: payload bytes diverged"
                );

                let seed_a = root.join(format!("seed_{seed:03}_a"));
                let seed_b = root.join(format!("seed_{seed:03}_b"));
                fs::create_dir_all(&seed_a).expect("create seed_a dir");
                fs::create_dir_all(&seed_b).expect("create seed_b dir");

                let trace_a = write_trace_artifacts(&seed_a, &artifacts_a).expect("write trace_a");
                let trace_b = write_trace_artifacts(&seed_b, &artifacts_b).expect("write trace_b");

                let summary_a = replay_trace(&trace_a).expect("replay trace_a");
                let summary_b = replay_trace(&trace_b).expect("replay trace_b");

                if summary_a.frames != summary_b.frames
                    || summary_a.last_checksum != summary_b.last_checksum
                {
                    divergence_count = divergence_count.saturating_add(1);
                }

                assert_eq!(
                    summary_a.frames, artifacts_a.frame_count,
                    "seed {seed}: unexpected frame count"
                );
                assert_eq!(
                    summary_a.last_checksum,
                    Some(artifacts_a.last_checksum),
                    "seed {seed}: replay checksum mismatch"
                );
                assert_eq!(
                    summary_a.frames, summary_b.frames,
                    "seed {seed}: replay frame count diverged"
                );
                assert_eq!(
                    summary_a.last_checksum, summary_b.last_checksum,
                    "seed {seed}: replay checksum diverged"
                );
            }
        });

        assert_eq!(
            divergence_count, 0,
            "expected zero divergences across 100 seeds"
        );
        assert_eq!(
            spans.len(),
            200,
            "expected one lab.scenario span per run (record + replay) for each seed"
        );
    }
}
