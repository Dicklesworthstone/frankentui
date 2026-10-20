#![forbid(unsafe_code)]

//! Compare two recorded `shadow_frame` JSONL evidence files and emit a JSON
//! verdict report.
//!
//! Usage:
//!   shadow_compare <baseline.jsonl> <candidate.jsonl>
//!   shadow_compare --baseline <baseline.jsonl> --candidate <candidate.jsonl>

use std::path::PathBuf;
use std::process::ExitCode;

use ftui_harness::shadow_run::{ShadowRun, ShadowVerdict};

fn parse_args() -> Result<(PathBuf, PathBuf), String> {
    let mut baseline: Option<PathBuf> = None;
    let mut candidate: Option<PathBuf> = None;
    let mut positional: Vec<PathBuf> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--baseline" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--baseline requires a file path".to_string())?;
                baseline = Some(PathBuf::from(val));
            }
            "--candidate" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--candidate requires a file path".to_string())?;
                candidate = Some(PathBuf::from(val));
            }
            "--help" | "-h" => {
                return Err(
                    "Usage: shadow_compare [--baseline] <baseline.jsonl> [--candidate] <candidate.jsonl>"
                        .to_string(),
                );
            }
            other if other.starts_with("--baseline=") => {
                baseline = Some(PathBuf::from(&other["--baseline=".len()..]));
            }
            other if other.starts_with("--candidate=") => {
                candidate = Some(PathBuf::from(&other["--candidate=".len()..]));
            }
            other => {
                positional.push(PathBuf::from(other));
            }
        }
    }

    if baseline.is_none() && !positional.is_empty() {
        baseline = Some(positional.remove(0));
    }
    if candidate.is_none() && !positional.is_empty() {
        candidate = Some(positional.remove(0));
    }

    match (baseline, candidate) {
        (Some(b), Some(c)) => Ok((b, c)),
        _ => Err(
            "Usage: shadow_compare [--baseline] <baseline.jsonl> [--candidate] <candidate.jsonl>"
                .to_string(),
        ),
    }
}

fn main() -> ExitCode {
    let (baseline, candidate) = match parse_args() {
        Ok(pair) => pair,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    if !baseline.exists() {
        eprintln!("baseline file does not exist: {}", baseline.display());
        return ExitCode::from(1);
    }
    if !candidate.exists() {
        eprintln!("candidate file does not exist: {}", candidate.display());
        return ExitCode::from(1);
    }

    let result = ShadowRun::compare_recorded(&baseline, &candidate);
    let matched = result.verdict == ShadowVerdict::Match && result.frames_compared > 0;
    let verdict_str = if matched { "match" } else { "diverged" };
    let first_divergent = if result.frames_compared == 0 && result.first_divergence.is_none() {
        Some(0)
    } else {
        result.first_divergence
    };

    let report = serde_json::json!({
        "verdict": verdict_str,
        "frames_compared": result.frames_compared,
        "first_divergent_frame": first_divergent,
        "baseline": baseline.display().to_string(),
        "candidate": candidate.display().to_string(),
    });
    println!("{report}");

    if matched {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
