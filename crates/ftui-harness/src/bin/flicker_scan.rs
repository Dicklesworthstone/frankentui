#![forbid(unsafe_code)]

//! CLI front-end for the ANSI-stream flicker/tear detector.
//!
//! Feeds a captured terminal byte stream ([`FlickerDetector`]) and reports
//! whether the renderer produced any flicker-inducing sequences: unsynchronized
//! full-frame repaints, partial clears, or frames whose begin/end markers do
//! not pair up.
//!
//! Usage:
//!
//! ```text
//! flicker_scan [--run-id <id>] [STREAM_FILE]
//! ```
//!
//! The stream is read from `STREAM_FILE`, or stdin when omitted. Output is
//! zero or more flicker-event JSONL lines followed by exactly one
//! deterministic summary marker line:
//!
//! ```text
//! FLICKER_VERDICT {"bytes_total":1234,"bytes_in_sync":1200,"complete_frames":40,"flicker_free":true,"partial_clears":0,"sync_coverage":97.24,"sync_gaps":0,"total_frames":40}
//! ```
//!
//! Exit status: `0` when the stream is flicker-free, `1` when events were
//! detected, `2` on usage or I/O errors. Hosts (for example cross-project E2E
//! lanes) can shell out to this binary without depending on this crate.

use std::io::{Read, Write};
use std::process::ExitCode;

use ftui_harness::flicker_detection::FlickerDetector;

const USAGE: &str = "usage: flicker_scan [--run-id <id>] [STREAM_FILE]";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut run_id = String::from("flicker-scan");
    let mut stream_path: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--run-id" => match args.next() {
                Some(id) => run_id = id,
                None => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            },
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("unknown option: {other}\n{USAGE}");
                return ExitCode::from(2);
            }
            other => {
                if stream_path.is_some() {
                    eprintln!("unexpected extra argument: {other}\n{USAGE}");
                    return ExitCode::from(2);
                }
                stream_path = Some(other.to_string());
            }
        }
    }

    let mut bytes = Vec::new();
    let read_result = match stream_path.as_deref() {
        Some(path) => std::fs::File::open(path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|err| format!("read {path}: {err}")),
        None => std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|err| format!("read stdin: {err}")),
    };
    if let Err(err) = read_result {
        eprintln!("flicker_scan: {err}");
        return ExitCode::from(2);
    }

    let mut detector = FlickerDetector::new(run_id);
    detector.feed(&bytes);
    detector.finalize();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if !detector.events().is_empty() {
        let _ = writeln!(out, "{}", detector.to_jsonl());
    }
    let stats = detector.stats();
    let _ = writeln!(
        out,
        "FLICKER_VERDICT {{\"bytes_total\":{},\"bytes_in_sync\":{},\"complete_frames\":{},\"flicker_free\":{},\"partial_clears\":{},\"sync_coverage\":{:.2},\"sync_gaps\":{},\"total_frames\":{}}}",
        stats.bytes_total,
        stats.bytes_in_sync,
        stats.complete_frames,
        detector.is_flicker_free(),
        stats.partial_clears,
        stats.sync_coverage(),
        stats.sync_gaps,
        stats.total_frames,
    );
    let _ = out.flush();

    if detector.is_flicker_free() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
