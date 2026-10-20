#![forbid(unsafe_code)]
#![cfg(not(target_arch = "wasm32"))]

use std::time::{Duration, Instant};

use ftui::agent_shell::{Msg, StreamingHarness, StreamingOptions};
use ftui::core::terminal_capabilities::TerminalCapabilities;
use ftui::render::buffer::Buffer;
use ftui::render::frame::Frame;
use ftui::render::grapheme_pool::GraphemePool;
use ftui::runtime::{Model, ProcessEvent, ProgramSimulator, ScreenMode, TerminalWriter, UiAnchor};

fn count_subslice(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

#[test]
fn ten_thousand_lines_headless_under_budget() {
    let options =
        StreamingOptions::parse_agent_shell(["--", "cat"].map(str::to_owned), None, None).unwrap();
    let model = StreamingHarness::new(options);
    let generation = model.current_generation().expect("child generation");
    let mut sim = ProgramSimulator::new(model);

    let start = Instant::now();
    for idx in 1..=10_000usize {
        sim.send(Msg::Process {
            generation,
            event: ProcessEvent::Stdout(format!("line-{idx:05}")),
        });
    }
    let elapsed = start.elapsed();

    println!(
        r#"{{"test":"ten_thousand_lines_headless_under_budget","lines":{},"elapsed_ms":{}}}"#,
        sim.model().line_count(),
        elapsed.as_millis()
    );

    assert_eq!(sim.model().line_count(), 10_000);
    assert_eq!(sim.logs().len(), 10_000);
    assert_eq!(sim.logs().first().map(String::as_str), Some("line-00001"));
    assert_eq!(sim.logs().last().map(String::as_str), Some("line-10000"));
    assert!(
        elapsed < Duration::from_secs(10),
        "10,000 simulated stdout lines exceeded 10 s budget: {elapsed:?}"
    );
}

#[test]
fn inline_writer_never_clears_screen_for_agent_shell_stream() {
    let options =
        StreamingOptions::parse_agent_shell(["--", "cat"].map(str::to_owned), None, None).unwrap();
    let mut model = StreamingHarness::new(options);
    let generation = model.current_generation().expect("child generation");

    let mut caps = TerminalCapabilities::basic();
    caps.scroll_region = true;
    caps.sync_output = true;

    let mut writer = TerminalWriter::new(
        Vec::<u8>::new(),
        ScreenMode::Inline { ui_height: 3 },
        UiAnchor::Bottom,
        caps,
    );
    writer.set_size(80, 24);

    let mut pool = GraphemePool::new();
    for idx in 1..=10_000usize {
        let _ = model.update(Msg::Process {
            generation,
            event: ProcessEvent::Stdout(format!("line-{idx:05}")),
        });
        writer
            .write_log(&format!("line-{idx:05}\n"))
            .expect("queue log");
        if idx % 200 == 0 {
            let mut frame = Frame::new(80, 3, &mut pool);
            model.view(&mut frame);
            let mut full = Buffer::new(80, 24);
            for y in 0..3u16 {
                for x in 0..80u16 {
                    if let Some(cell) = frame.buffer.get(x, y) {
                        full.set_raw(x, y, *cell);
                    }
                }
            }
            writer
                .present_ui_owned(full, None, false)
                .expect("present inline frame");
        }
    }

    let bytes = writer.into_inner().expect("flush and teardown writer");
    let clear_2j = count_subslice(&bytes, b"\x1b[2J");
    let clear_3j = count_subslice(&bytes, b"\x1b[3J");
    let alt_1049h = count_subslice(&bytes, b"\x1b[?1049h");
    let decstbm_set = count_subslice(&bytes, b"\x1b[1;21r");
    let decstbm_reset = count_subslice(&bytes, b"\x1b[r");
    let sync_begin = count_subslice(&bytes, b"\x1b[?2026h");
    let sync_end = count_subslice(&bytes, b"\x1b[?2026l");

    let tallies = format!(
        r#"{{"clear_2j":{clear_2j},"clear_3j":{clear_3j},"alt_1049h":{alt_1049h},"decstbm_set":{decstbm_set},"decstbm_reset":{decstbm_reset},"sync_pairs":{sync_begin}}}"#
    );
    assert_eq!(clear_2j, 0, "inline stream must never emit ED2: {tallies}");
    assert_eq!(clear_3j, 0, "inline stream must never emit ED3: {tallies}");
    assert_eq!(
        alt_1049h, 0,
        "inline stream must never enter alt-screen: {tallies}"
    );
    assert_eq!(
        decstbm_set, 1,
        "scroll region should be set once across 50 presents: {tallies}"
    );
    assert_eq!(
        decstbm_reset, 1,
        "scroll region should be reset once at teardown: {tallies}"
    );
    assert_eq!(
        sync_begin, sync_end,
        "DEC 2026 sync brackets must be balanced: {tallies}"
    );
    assert!(sync_begin >= 50, "expected >= 50 sync presents: {tallies}");
}
