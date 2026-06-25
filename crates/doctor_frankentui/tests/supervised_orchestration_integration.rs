//! Real-subprocess integration coverage for the supervised orchestration
//! substrate (bd-1dccp / bd-11ngr).
//!
//! The unit tests in `supervised_orchestration.rs` prove the pure decision core
//! deterministically with a mock clock. These tests prove the
//! [`supervise_subprocess`] supervisor against *real* child processes: success,
//! non-zero exit (with and without retry), output capture, deadline-driven kill,
//! and token cancellation -- all with bounded teardown so a stuck child can
//! never wedge a doctor lane.

use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use doctor_frankentui::supervised_orchestration::{
    AttemptOutcomeKind, Backoff, CancelReason, StepOutcome, SubprocessStdio, SupervisionBudget,
    supervise_subprocess,
};
use ftui_runtime::cancellation::CancellationSource;

fn budget(deadline_ms: u64, max_attempts: u32) -> SupervisionBudget {
    SupervisionBudget {
        deadline_ms,
        max_attempts,
    }
}

#[test]
fn subprocess_success_records_ok() {
    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "test",
        "true",
        budget(10_000, 3),
        Backoff::none(),
        &source.token(),
        true,
        &SubprocessStdio::Capture,
        || Command::new("true"),
    );
    assert_eq!(run.record.final_outcome, StepOutcome::Succeeded);
    assert_eq!(run.exit_code, Some(0));
    assert_eq!(run.record.attempts_used, 1);
    assert_eq!(run.record.attempts[0].outcome, AttemptOutcomeKind::Ok);
}

#[test]
fn subprocess_nonzero_without_retry_is_fatal() {
    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "test",
        "false",
        budget(10_000, 3),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::Capture,
        || Command::new("false"),
    );
    assert!(matches!(
        run.record.final_outcome,
        StepOutcome::Failed { .. }
    ));
    assert_eq!(run.exit_code, Some(1));
    assert_eq!(run.record.attempts_used, 1, "non-retryable must not retry");
    assert_eq!(run.record.attempts[0].outcome, AttemptOutcomeKind::Fatal);
}

#[test]
fn subprocess_nonzero_with_retry_exhausts_budget() {
    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "test",
        "false",
        budget(10_000, 3),
        Backoff::none(),
        &source.token(),
        true,
        &SubprocessStdio::Capture,
        || Command::new("false"),
    );
    assert!(matches!(
        run.record.final_outcome,
        StepOutcome::Failed { .. }
    ));
    assert_eq!(run.record.attempts_used, 3);
    assert!(
        run.record
            .attempts
            .iter()
            .all(|a| a.outcome == AttemptOutcomeKind::Retryable)
    );
}

#[test]
fn subprocess_captures_stdout() {
    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "test",
        "echo",
        budget(10_000, 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::Capture,
        || {
            let mut cmd = Command::new("echo");
            cmd.arg("hello-supervised");
            cmd
        },
    );
    assert_eq!(run.record.final_outcome, StepOutcome::Succeeded);
    assert!(
        run.stdout.contains("hello-supervised"),
        "stdout should be captured, got: {:?}",
        run.stdout
    );
}

#[test]
fn subprocess_deadline_kills_child_promptly() {
    let source = CancellationSource::new();
    let start = Instant::now();
    let run = supervise_subprocess(
        "test",
        "sleep",
        budget(400, 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::Capture,
        || {
            let mut cmd = Command::new("sleep");
            cmd.arg("60");
            cmd
        },
    );
    let elapsed = start.elapsed();
    assert_eq!(run.record.final_outcome, StepOutcome::TimedOut);
    assert_eq!(run.record.cancel_reason, Some(CancelReason::Deadline));
    assert!(
        elapsed < Duration::from_secs(3),
        "deadline must kill the child promptly, took {elapsed:?}"
    );
}

#[test]
fn subprocess_cancellation_kills_child_promptly() {
    let source = Arc::new(CancellationSource::new());
    let token = source.token();
    let canceller = Arc::clone(&source);
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        canceller.cancel();
    });

    let start = Instant::now();
    let run = supervise_subprocess(
        "test",
        "sleep",
        budget(60_000, 1),
        Backoff::none(),
        &token,
        false,
        &SubprocessStdio::Capture,
        || {
            let mut cmd = Command::new("sleep");
            cmd.arg("60");
            cmd
        },
    );
    let elapsed = start.elapsed();
    handle.join().expect("canceller thread");

    assert_eq!(
        run.record.final_outcome,
        StepOutcome::Cancelled {
            reason: CancelReason::Caller
        }
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "cancellation must kill the child promptly, took {elapsed:?}"
    );
}

#[test]
fn subprocess_retries_then_succeeds() {
    // A command that fails the first time and succeeds the second, driven by a
    // counter file so the retry path is exercised deterministically.
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("attempts");
    let counter_path = counter.to_string_lossy().into_owned();

    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "test",
        "flaky",
        budget(10_000, 3),
        Backoff::fixed(10),
        &source.token(),
        true,
        &SubprocessStdio::Capture,
        || {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(format!(
                "n=$(cat '{counter_path}' 2>/dev/null || echo 0); n=$((n+1)); \
                 echo $n > '{counter_path}'; [ $n -ge 2 ]"
            ));
            cmd
        },
    );

    assert_eq!(run.record.final_outcome, StepOutcome::Succeeded);
    assert_eq!(run.record.attempts_used, 2, "should fail once then succeed");
    assert_eq!(
        run.record.attempts[0].outcome,
        AttemptOutcomeKind::Retryable
    );
    assert_eq!(run.record.attempts[1].outcome, AttemptOutcomeKind::Ok);
}

#[test]
fn subprocess_record_evidence_is_serializable_and_triage_ready() {
    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "capture",
        "vhs_smoke",
        budget(10_000, 2),
        Backoff::fixed(10),
        &source.token(),
        true,
        &SubprocessStdio::Capture,
        || Command::new("true"),
    );

    let terms = run.record.evidence_terms();
    assert!(terms.contains(&"lane=capture".to_string()));
    assert!(terms.contains(&"step=vhs_smoke".to_string()));
    assert!(terms.contains(&"final_outcome=succeeded".to_string()));

    // The record (and a derived DecisionRecord) round-trips through JSON for the
    // evidence ledger.
    let json = serde_json::to_string(&run.record).expect("serialize record");
    let decoded: doctor_frankentui::supervised_orchestration::SupervisionRecord =
        serde_json::from_str(&json).expect("deserialize record");
    assert_eq!(decoded, run.record);

    let decision = run
        .record
        .to_decision_record("2026-06-22T00:00:00Z", "trace-x", "policy-x");
    assert!(!decision.fallback_active);
    let decision_json = serde_json::to_string(&decision).expect("serialize decision");
    assert!(decision_json.contains("supervise_capture"));
}

#[test]
fn subprocess_signal_termination_reports_shell_convention_exit_code() {
    // A child that signals itself (SIGTERM) has no exit *code*; the substrate must
    // report the shell convention `128 + signal` (143 for SIGTERM) rather than the
    // raw `None`, so signal deaths are triage-able and feed the retry classifier
    // the same way the capture / suite lanes' `exit_status_code` does.
    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "test",
        "self_sigterm",
        budget(10_000, 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::Capture,
        || {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg("kill -TERM $$");
            cmd
        },
    );
    assert_eq!(run.exit_code, Some(143), "SIGTERM must map to 128 + 15");
    assert!(matches!(
        run.record.final_outcome,
        StepOutcome::Failed { .. }
    ));
    assert_eq!(run.record.attempts[0].outcome, AttemptOutcomeKind::Fatal);
    assert!(
        run.record.attempts[0]
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("exit_code=143")),
        "evidence detail should carry the signal-aware code, got: {:?}",
        run.record.attempts[0].detail
    );
}

#[test]
fn subprocess_to_file_streams_combined_output_to_log() {
    // `ToFile` redirects stdout + stderr (interleaved) straight to disk and leaves
    // the in-memory capture buffers empty -- the contract the capture / suite lanes
    // need to route their on-disk logs through the substrate.
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("capture.log");

    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "capture",
        "to_file",
        budget(10_000, 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::ToFile(log_path.clone()),
        || {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg("echo out-line; echo err-line 1>&2");
            cmd
        },
    );

    assert_eq!(run.record.final_outcome, StepOutcome::Succeeded);
    assert_eq!(run.exit_code, Some(0));
    assert!(
        run.stdout.is_empty() && run.stderr.is_empty(),
        "ToFile must not populate the in-memory buffers, got stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );

    let logged = std::fs::read_to_string(&log_path).expect("read capture log");
    assert!(
        logged.contains("out-line") && logged.contains("err-line"),
        "both streams must be interleaved into the log, got: {logged:?}"
    );
}

#[test]
fn subprocess_to_file_truncates_log_each_attempt() {
    // Each retry must start from a fresh (truncated) log so the file reflects only
    // the final attempt's output, matching `run_capture_subprocess_to_log`.
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("retry.log");
    let counter = dir.path().join("attempts");
    let counter_path = counter.to_string_lossy().into_owned();

    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "capture",
        "to_file_retry",
        budget(10_000, 3),
        Backoff::fixed(10),
        &source.token(),
        true,
        &SubprocessStdio::ToFile(log_path.clone()),
        || {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(format!(
                "n=$(cat '{counter_path}' 2>/dev/null || echo 0); n=$((n+1)); \
                 echo $n > '{counter_path}'; echo attempt-$n; [ $n -ge 2 ]"
            ));
            cmd
        },
    );

    assert_eq!(run.record.final_outcome, StepOutcome::Succeeded);
    assert_eq!(run.record.attempts_used, 2);
    let logged = std::fs::read_to_string(&log_path).expect("read retry log");
    assert!(
        logged.contains("attempt-2"),
        "final attempt output must be present, got: {logged:?}"
    );
    assert!(
        !logged.contains("attempt-1"),
        "earlier attempts must be truncated, got: {logged:?}"
    );
}

#[test]
fn subprocess_to_files_splits_streams_into_separate_logs() {
    // `ToFiles` redirects stdout and stderr into their *own* on-disk logs (the
    // contract the capture lane's seed-demo thread needs: `seed.stdout.log` and
    // `seed.stderr.log` stay distinct) and leaves the in-memory buffers empty.
    let dir = tempfile::tempdir().expect("tempdir");
    let stdout_path = dir.path().join("seed.stdout.log");
    let stderr_path = dir.path().join("seed.stderr.log");

    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "capture",
        "to_files",
        budget(10_000, 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::ToFiles {
            stdout: stdout_path.clone(),
            stderr: stderr_path.clone(),
        },
        || {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg("echo out-line; echo err-line 1>&2");
            cmd
        },
    );

    assert_eq!(run.record.final_outcome, StepOutcome::Succeeded);
    assert_eq!(run.exit_code, Some(0));
    assert!(
        run.stdout.is_empty() && run.stderr.is_empty(),
        "ToFiles must not populate the in-memory buffers, got stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );

    let out = std::fs::read_to_string(&stdout_path).expect("read stdout log");
    let err = std::fs::read_to_string(&stderr_path).expect("read stderr log");
    assert!(
        out.contains("out-line") && !out.contains("err-line"),
        "stdout log must hold only stdout, got: {out:?}"
    );
    assert!(
        err.contains("err-line") && !err.contains("out-line"),
        "stderr log must hold only stderr, got: {err:?}"
    );
}

#[test]
fn subprocess_to_files_truncates_each_log_per_attempt() {
    // Each retry must restart from fresh (truncated) stdout/stderr logs so each
    // file reflects only the final attempt, matching the seed-demo thread's
    // create+truncate log setup.
    let dir = tempfile::tempdir().expect("tempdir");
    let stdout_path = dir.path().join("retry.stdout.log");
    let stderr_path = dir.path().join("retry.stderr.log");
    let counter = dir.path().join("attempts");
    let counter_path = counter.to_string_lossy().into_owned();

    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "capture",
        "to_files_retry",
        budget(10_000, 3),
        Backoff::fixed(10),
        &source.token(),
        true,
        &SubprocessStdio::ToFiles {
            stdout: stdout_path.clone(),
            stderr: stderr_path.clone(),
        },
        || {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(format!(
                "n=$(cat '{counter_path}' 2>/dev/null || echo 0); n=$((n+1)); \
                 echo $n > '{counter_path}'; echo out-$n; echo err-$n 1>&2; [ $n -ge 2 ]"
            ));
            cmd
        },
    );

    assert_eq!(run.record.final_outcome, StepOutcome::Succeeded);
    assert_eq!(run.record.attempts_used, 2);
    let out = std::fs::read_to_string(&stdout_path).expect("read stdout log");
    let err = std::fs::read_to_string(&stderr_path).expect("read stderr log");
    assert!(
        out.contains("out-2") && !out.contains("out-1"),
        "stdout log must hold only the final attempt, got: {out:?}"
    );
    assert!(
        err.contains("err-2") && !err.contains("err-1"),
        "stderr log must hold only the final attempt, got: {err:?}"
    );
}

#[test]
fn subprocess_null_discards_output_and_reports_exit() {
    // `Null` wires both streams to the null device (the doctor capture-smoke
    // probes that rely on the child's own artifacts, not its stdout/stderr): the
    // in-memory buffers stay empty and the child's exit code is still reported.
    let source = CancellationSource::new();
    let run = supervise_subprocess(
        "doctor",
        "null",
        budget(10_000, 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::Null,
        || {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg("echo out-line; echo err-line 1>&2; exit 3");
            cmd
        },
    );

    assert_eq!(run.exit_code, Some(3));
    assert_eq!(
        run.record.final_outcome,
        StepOutcome::Failed {
            reason: "exit_code=3".to_string(),
        }
    );
    assert!(
        run.stdout.is_empty() && run.stderr.is_empty(),
        "Null must not populate the in-memory buffers, got stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );
}
