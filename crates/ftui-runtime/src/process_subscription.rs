// SPDX-License-Identifier: Apache-2.0
//! Process subscription for spawning and monitoring external processes.
//!
//! [`ProcessSubscription`] wraps [`std::process::Command`] as a first-class
//! runtime [`Subscription`]. It spawns a child process, captures stdout
//! line-by-line, and sends messages to the model. When the subscription is
//! stopped (via [`StopSignal`]), the child process is killed.
//!
//! # Migration rationale
//!
//! Web Worker APIs and child-process patterns in source frameworks translate
//! to process-based subscriptions in the terminal context. This provides a
//! clean target for the migration code emitter.
//!
//! # Example
//!
//! ```ignore
//! use ftui_runtime::process_subscription::{ProcessSubscription, ProcessEvent};
//! use std::time::Duration;
//!
//! #[derive(Debug)]
//! enum Msg {
//!     ProcessOutput(ProcessEvent),
//!     // ...
//! }
//!
//! fn subscriptions() -> Vec<Box<dyn Subscription<Msg>>> {
//!     vec![Box::new(
//!         ProcessSubscription::new("tail", Msg::ProcessOutput)
//!             .arg("-f")
//!             .arg("/var/log/syslog")
//!             .timeout(Duration::from_secs(60))
//!     )]
//! }
//! ```

#![forbid(unsafe_code)]

use crate::subscription::{StopSignal, SubId, Subscription};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use web_time::{Duration, Instant};

/// Events emitted by a [`ProcessSubscription`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEvent {
    /// A line of stdout output from the process.
    Stdout(String),
    /// A line of stderr output from the process.
    Stderr(String),
    /// The process exited with a status code.
    Exited(i32),
    /// The process was terminated by a Unix signal.
    Signaled(i32),
    /// The process was killed by the subscription (stop signal or timeout).
    Killed,
    /// An error occurred spawning or monitoring the process.
    Error(String),
}

/// A subscription that spawns and monitors an external process.
///
/// Captures stdout/stderr line-by-line and sends [`ProcessEvent`] messages.
/// The process is killed when the subscription's [`StopSignal`] fires or
/// when the optional timeout expires.
pub struct ProcessSubscription<M: Send + 'static> {
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    timeout: Option<Duration>,
    id: SubId,
    explicit_id: bool,
    make_msg: std::sync::Arc<dyn Fn(ProcessEvent) -> M + Send + Sync>,
}

const PROCESS_READER_JOIN_TIMEOUT: Duration = Duration::from_millis(250);
const PROCESS_READER_JOIN_POLL: Duration = Duration::from_millis(5);

impl<M: Send + 'static> ProcessSubscription<M> {
    fn computed_id(
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Option<Duration>,
    ) -> SubId {
        let mut h = DefaultHasher::new();
        "ProcessSubscription".hash(&mut h);
        program.hash(&mut h);
        args.hash(&mut h);
        env.hash(&mut h);
        timeout.map(|duration| duration.as_nanos()).hash(&mut h);
        h.finish()
    }

    fn refresh_id(&mut self) {
        if !self.explicit_id {
            self.id = Self::computed_id(&self.program, &self.args, &self.env, self.timeout);
        }
    }

    /// Create a new process subscription for the given program.
    ///
    /// The `make_msg` closure converts [`ProcessEvent`] into your model's
    /// message type.
    pub fn new(
        program: impl Into<String>,
        make_msg: impl Fn(ProcessEvent) -> M + Send + Sync + 'static,
    ) -> Self {
        let program = program.into();
        let id = Self::computed_id(&program, &[], &[], None);
        Self {
            program,
            args: Vec::new(),
            env: Vec::new(),
            timeout: None,
            id,
            explicit_id: false,
            make_msg: std::sync::Arc::new(make_msg),
        }
    }

    /// Add a command-line argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self.refresh_id();
        self
    }

    /// Add multiple command-line arguments.
    #[must_use]
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        for a in args {
            self = self.arg(a);
        }
        self
    }

    /// Set an environment variable for the child process.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self.refresh_id();
        self
    }

    /// Set a timeout after which the process is killed.
    #[must_use]
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self.refresh_id();
        self
    }

    /// Override the subscription ID (for explicit deduplication control).
    #[must_use]
    pub fn with_id(mut self, id: SubId) -> Self {
        self.id = id;
        self.explicit_id = true;
        self
    }
}

impl<M: Send + 'static> Subscription<M> for ProcessSubscription<M> {
    fn id(&self) -> SubId {
        self.id
    }

    fn run(&self, sender: mpsc::Sender<M>, stop: StopSignal) {
        fn forward_lines<R, M>(
            reader: std::io::BufReader<R>,
            sender: mpsc::Sender<M>,
            make_msg: impl Fn(String) -> M,
        ) where
            R: Read,
            M: Send + 'static,
        {
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if sender.send(make_msg(line)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }

        let spawn_start = web_time::Instant::now();
        let sub_id = self.id;

        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => {
                tracing::debug!(
                    target: "ftui.process",
                    sub_id,
                    program = %self.program,
                    args = ?self.args,
                    spawn_us = spawn_start.elapsed().as_micros() as u64,
                    "process spawned"
                );
                c
            }
            Err(e) => {
                tracing::warn!(
                    target: "ftui.process",
                    sub_id,
                    program = %self.program,
                    error = %e,
                    "process spawn failed"
                );
                let _ = sender.send((self.make_msg.as_ref())(ProcessEvent::Error(format!(
                    "Failed to spawn '{}': {}",
                    self.program, e
                ))));
                return;
            }
        };

        let deadline = self.timeout.map(|t| web_time::Instant::now() + t);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let make_msg_ref = std::sync::Arc::clone(&self.make_msg);
        // Use the cancellation token for cooperative stop coordination.
        let token = stop.cancellation_token().clone();
        let poll_interval = Duration::from_millis(50);
        let stdout_handle = stdout.map(|stdout| {
            let sender_out = sender.clone();
            let make_msg_out = std::sync::Arc::clone(&make_msg_ref);
            std::thread::spawn(move || {
                forward_lines(std::io::BufReader::new(stdout), sender_out, |line| {
                    (make_msg_out.as_ref())(ProcessEvent::Stdout(line))
                });
            })
        });
        let stderr_handle = stderr.map(|stderr| {
            let sender_err = sender.clone();
            let make_msg_err = std::sync::Arc::clone(&make_msg_ref);
            std::thread::spawn(move || {
                forward_lines(std::io::BufReader::new(stderr), sender_err, |line| {
                    (make_msg_err.as_ref())(ProcessEvent::Stderr(line))
                });
            })
        });

        let final_event = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let event = process_exit_event(status);
                    if let ProcessEvent::Exited(code) = &event {
                        tracing::debug!(
                            target: "ftui.process",
                            sub_id,
                            exit_code = *code,
                            elapsed_ms = spawn_start.elapsed().as_millis() as u64,
                            "process exited"
                        );
                    } else if let ProcessEvent::Signaled(signal) = &event {
                        tracing::debug!(
                            target: "ftui.process",
                            sub_id,
                            signal = *signal,
                            elapsed_ms = spawn_start.elapsed().as_millis() as u64,
                            "process terminated by signal"
                        );
                    }
                    break event;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        target: "ftui.process",
                        sub_id,
                        error = %e,
                        "process wait error"
                    );
                    break ProcessEvent::Error(format!("wait error: {e}"));
                }
            }

            if let Some(dl) = deadline
                && web_time::Instant::now() >= dl
            {
                tracing::debug!(
                    target: "ftui.process",
                    sub_id,
                    elapsed_ms = spawn_start.elapsed().as_millis() as u64,
                    reason = "timeout",
                    "killing process"
                );
                let _ = child.kill();
                let _ = child.wait();
                break ProcessEvent::Killed;
            }

            if token.wait_timeout(poll_interval) {
                tracing::debug!(
                    target: "ftui.process",
                    sub_id,
                    elapsed_ms = spawn_start.elapsed().as_millis() as u64,
                    reason = "cancellation",
                    "killing process"
                );
                let _ = child.kill();
                let _ = child.wait();
                break ProcessEvent::Killed;
            }
        };

        if let Some(handle) = stdout_handle {
            join_reader_thread_bounded(handle, "stdout", sub_id);
        }
        if let Some(handle) = stderr_handle {
            join_reader_thread_bounded(handle, "stderr", sub_id);
        }

        let _ = sender.send((make_msg_ref.as_ref())(final_event));
    }
}

fn process_exit_event(status: std::process::ExitStatus) -> ProcessEvent {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        if let Some(signal) = status.signal() {
            return ProcessEvent::Signaled(signal);
        }
    }

    ProcessEvent::Exited(status.code().unwrap_or(-1))
}

fn join_reader_thread_bounded(
    handle: std::thread::JoinHandle<()>,
    stream: &'static str,
    sub_id: SubId,
) {
    let start = Instant::now();
    while !handle.is_finished() {
        if start.elapsed() >= PROCESS_READER_JOIN_TIMEOUT {
            tracing::warn!(
                target: "ftui.process",
                sub_id,
                stream,
                timeout_ms = PROCESS_READER_JOIN_TIMEOUT.as_millis() as u64,
                "process reader thread did not exit within timeout; detaching"
            );
            detach_reader_join(handle, stream);
            return;
        }
        std::thread::sleep(PROCESS_READER_JOIN_POLL);
    }
    let _ = handle.join();
}

fn detach_reader_join(handle: std::thread::JoinHandle<()>, stream: &'static str) {
    let _ = std::thread::Builder::new()
        .name(format!("ftui-process-{stream}-detached-join"))
        .spawn(move || {
            let _ = handle.join();
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc as stdmpsc;
    use std::thread;

    #[derive(Debug, Clone, PartialEq)]
    enum TestMsg {
        Proc(ProcessEvent),
    }

    #[test]
    fn process_event_variants() {
        let stdout = ProcessEvent::Stdout("hello".into());
        let stderr = ProcessEvent::Stderr("warn".into());
        let exited = ProcessEvent::Exited(0);
        let signaled = ProcessEvent::Signaled(15);
        let killed = ProcessEvent::Killed;
        let error = ProcessEvent::Error("oops".into());

        assert_eq!(stdout, ProcessEvent::Stdout("hello".into()));
        assert_eq!(stderr, ProcessEvent::Stderr("warn".into()));
        assert_eq!(exited, ProcessEvent::Exited(0));
        assert_eq!(signaled, ProcessEvent::Signaled(15));
        assert_eq!(killed, ProcessEvent::Killed);
        assert_eq!(error, ProcessEvent::Error("oops".into()));
    }

    #[test]
    fn subscription_id_is_stable() {
        let s1: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).arg("hello");
        let s2: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).arg("hello");
        assert_eq!(s1.id(), s2.id());
    }

    #[test]
    fn different_args_produce_different_ids() {
        let s1: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).arg("hello");
        let s2: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).arg("world");
        assert_ne!(s1.id(), s2.id());
    }

    #[test]
    fn different_programs_produce_different_ids() {
        let s1: ProcessSubscription<TestMsg> = ProcessSubscription::new("echo", TestMsg::Proc);
        let s2: ProcessSubscription<TestMsg> = ProcessSubscription::new("cat", TestMsg::Proc);
        assert_ne!(s1.id(), s2.id());
    }

    #[test]
    fn custom_id_overrides_default() {
        let s: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).with_id(42);
        assert_eq!(s.id(), 42);
    }

    #[test]
    fn env_changes_affect_subscription_id() {
        let s1: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).env("FTUI_TEST_VAR", "a");
        let s2: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).env("FTUI_TEST_VAR", "b");
        assert_ne!(s1.id(), s2.id());
    }

    #[test]
    fn timeout_changes_affect_subscription_id() {
        let s1: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).timeout(Duration::from_millis(10));
        let s2: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).timeout(Duration::from_millis(20));
        assert_ne!(s1.id(), s2.id());
    }

    #[test]
    fn explicit_id_remains_stable_after_builder_changes() {
        let s: ProcessSubscription<TestMsg> = ProcessSubscription::new("echo", TestMsg::Proc)
            .with_id(42)
            .arg("hello")
            .env("FTUI_TEST_VAR", "value")
            .timeout(Duration::from_millis(10));
        assert_eq!(s.id(), 42);
    }

    #[test]
    fn echo_captures_stdout() {
        let sub = ProcessSubscription::new("echo", TestMsg::Proc).arg("hello world");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        // Wait for process to complete
        thread::sleep(Duration::from_millis(500));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        let has_stdout = msgs.iter().any(|m| match m {
            TestMsg::Proc(ProcessEvent::Stdout(s)) => s.contains("hello world"),
            _ => false,
        });
        assert!(
            has_stdout,
            "Expected stdout with 'hello world', got: {msgs:?}"
        );

        let has_exit = msgs
            .iter()
            .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Exited(0))));
        assert!(has_exit, "Expected Exited(0), got: {msgs:?}");
    }

    #[test]
    fn nonexistent_program_sends_error() {
        let sub =
            ProcessSubscription::new("/nonexistent/program/that/should/not/exist", TestMsg::Proc);
        let (tx, rx) = stdmpsc::channel();
        let (signal, _trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        handle.join().unwrap();
        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        let has_error = msgs
            .iter()
            .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Error(_))));
        assert!(has_error, "Expected Error event, got: {msgs:?}");
    }

    #[test]
    fn stop_signal_kills_long_running_process() {
        let sub = ProcessSubscription::new("sleep", TestMsg::Proc).arg("60");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();
        let start = web_time::Instant::now();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        // Give it a moment to start, then stop
        thread::sleep(Duration::from_millis(100));
        trigger.stop();
        handle.join().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "stop should kill a quiet process promptly"
        );

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        let has_killed = msgs
            .iter()
            .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Killed)));
        assert!(has_killed, "Expected Killed event, got: {msgs:?}");
    }

    #[test]
    fn timeout_kills_process() {
        let sub = ProcessSubscription::new("sleep", TestMsg::Proc)
            .arg("60")
            .timeout(Duration::from_millis(100));
        let (tx, rx) = stdmpsc::channel();
        let (signal, _trigger) = StopSignal::new();
        let start = web_time::Instant::now();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        handle.join().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timeout should kill a quiet process promptly"
        );
        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        let has_killed = msgs
            .iter()
            .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Killed)));
        assert!(has_killed, "Expected Killed on timeout, got: {msgs:?}");
    }

    #[test]
    fn env_vars_are_passed() {
        let sub =
            ProcessSubscription::new("env", TestMsg::Proc).env("FTUI_TEST_VAR", "test_value_42");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(500));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        let has_var = msgs.iter().any(|m| match m {
            TestMsg::Proc(ProcessEvent::Stdout(s)) => s.contains("FTUI_TEST_VAR=test_value_42"),
            _ => false,
        });
        assert!(has_var, "Expected env var in output, got: {msgs:?}");
    }

    #[test]
    fn multiple_args_via_args_method() {
        let sub = ProcessSubscription::new("echo", TestMsg::Proc).args(["hello", "world"]);
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(500));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        let has_output = msgs.iter().any(|m| match m {
            TestMsg::Proc(ProcessEvent::Stdout(s)) => s.contains("hello world"),
            _ => false,
        });
        assert!(has_output, "Expected combined output, got: {msgs:?}");
    }

    #[test]
    fn stderr_captured() {
        // Use sh -c to write to stderr
        let sub = ProcessSubscription::new("sh", TestMsg::Proc)
            .arg("-c")
            .arg("echo error_msg >&2");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(500));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        let has_stderr = msgs.iter().any(|m| match m {
            TestMsg::Proc(ProcessEvent::Stderr(s)) => s.contains("error_msg"),
            _ => false,
        });
        assert!(has_stderr, "Expected stderr output, got: {msgs:?}");
    }

    #[test]
    fn exit_code_captured() {
        let sub = ProcessSubscription::new("sh", TestMsg::Proc)
            .arg("-c")
            .arg("exit 42");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(500));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        let has_exit = msgs
            .iter()
            .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Exited(42))));
        assert!(has_exit, "Expected Exited(42), got: {msgs:?}");
    }

    #[cfg(unix)]
    #[test]
    fn signal_exit_is_preserved() {
        let sub = ProcessSubscription::new("sh", TestMsg::Proc)
            .arg("-c")
            .arg("kill -TERM $$");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(500));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        let has_signal = msgs
            .iter()
            .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Signaled(15))));
        assert!(has_signal, "Expected Signaled(15), got: {msgs:?}");
    }

    // =========================================================================
    // PROCESS LIFECYCLE CONTRACT TESTS (bd-3s3yw)
    //
    // These tests capture the observable process supervision contract that
    // the Asupersync migration must preserve.
    // =========================================================================

    /// CONTRACT: Process subscription uses CancellationToken internally for
    /// stop coordination (via StopSignal::cancellation_token()).
    #[test]
    fn contract_uses_cancellation_token_for_stop() {
        let sub = ProcessSubscription::new("sleep", TestMsg::Proc).arg("60");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();

        // Verify the cancellation token is accessible
        let token = signal.cancellation_token().clone();
        assert!(!token.is_cancelled());

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(100));

        // Stopping via trigger should cancel the token
        trigger.stop();
        assert!(token.is_cancelled());

        handle.join().unwrap();

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        assert!(
            msgs.iter()
                .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Killed))),
            "process must be killed on cancellation, got: {msgs:?}"
        );
    }

    /// CONTRACT: Final event is always sent, even on error paths.
    /// The subscription must always emit exactly one terminal event
    /// (Exited, Signaled, Killed, or Error).
    #[test]
    fn contract_always_emits_terminal_event() {
        // Happy path: process exits normally
        {
            let sub = ProcessSubscription::new("true", TestMsg::Proc);
            let (tx, rx) = stdmpsc::channel();
            let (signal, trigger) = StopSignal::new();

            let handle = thread::spawn(move || {
                sub.run(tx, signal);
            });

            thread::sleep(Duration::from_millis(500));
            trigger.stop();
            handle.join().unwrap();

            let msgs: Vec<TestMsg> = rx.try_iter().collect();
            let terminal_events: Vec<_> = msgs
                .iter()
                .filter(|m| {
                    matches!(
                        m,
                        TestMsg::Proc(
                            ProcessEvent::Exited(_)
                                | ProcessEvent::Signaled(_)
                                | ProcessEvent::Killed
                                | ProcessEvent::Error(_)
                        )
                    )
                })
                .collect();
            assert_eq!(
                terminal_events.len(),
                1,
                "must emit exactly one terminal event, got: {terminal_events:?}"
            );
        }

        // Error path: nonexistent program
        {
            let sub = ProcessSubscription::new(
                "/nonexistent/program/that/should/not/exist",
                TestMsg::Proc,
            );
            let (tx, rx) = stdmpsc::channel();
            let (signal, _trigger) = StopSignal::new();

            let handle = thread::spawn(move || {
                sub.run(tx, signal);
            });

            handle.join().unwrap();

            let msgs: Vec<TestMsg> = rx.try_iter().collect();
            let terminal_events: Vec<_> = msgs
                .iter()
                .filter(|m| {
                    matches!(
                        m,
                        TestMsg::Proc(
                            ProcessEvent::Exited(_)
                                | ProcessEvent::Signaled(_)
                                | ProcessEvent::Killed
                                | ProcessEvent::Error(_)
                        )
                    )
                })
                .collect();
            assert_eq!(
                terminal_events.len(),
                1,
                "must emit exactly one terminal event on error, got: {terminal_events:?}"
            );
        }
    }

    /// CONTRACT: stdout and stderr lines arrive before the terminal event.
    /// The output forwarding threads must join before the final event is sent.
    #[test]
    fn contract_output_precedes_terminal_event() {
        let sub = ProcessSubscription::new("sh", TestMsg::Proc)
            .arg("-c")
            .arg("echo FIRST && echo SECOND >&2 && exit 0");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(500));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<TestMsg> = rx.try_iter().collect();

        // Find the position of the terminal event
        let terminal_pos = msgs.iter().position(|m| {
            matches!(
                m,
                TestMsg::Proc(
                    ProcessEvent::Exited(_)
                        | ProcessEvent::Signaled(_)
                        | ProcessEvent::Killed
                        | ProcessEvent::Error(_)
                )
            )
        });

        // Find positions of stdout/stderr events
        let output_positions: Vec<usize> = msgs
            .iter()
            .enumerate()
            .filter_map(|(i, m)| match m {
                TestMsg::Proc(ProcessEvent::Stdout(_) | ProcessEvent::Stderr(_)) => Some(i),
                _ => None,
            })
            .collect();

        if let Some(term_pos) = terminal_pos {
            for &out_pos in &output_positions {
                assert!(
                    out_pos < term_pos,
                    "output event at position {out_pos} must precede terminal event at {term_pos}"
                );
            }
        }
    }

    /// CONTRACT: ProcessSubscription ID includes timeout in the hash.
    /// Changing timeout creates a different subscription identity.
    #[test]
    fn contract_id_includes_timeout() {
        let s1: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).timeout(Duration::from_secs(5));
        let s2: ProcessSubscription<TestMsg> =
            ProcessSubscription::new("echo", TestMsg::Proc).timeout(Duration::from_secs(10));
        let s3: ProcessSubscription<TestMsg> = ProcessSubscription::new("echo", TestMsg::Proc);

        assert_ne!(
            s1.id(),
            s2.id(),
            "different timeouts must produce different IDs"
        );
        assert_ne!(
            s1.id(),
            s3.id(),
            "timeout vs no-timeout must produce different IDs"
        );
    }

    /// CONTRACT: Kill is prompt — process is killed within poll_interval (50ms)
    /// of the stop signal, not blocked waiting for process output.
    #[test]
    fn contract_kill_is_prompt() {
        let sub = ProcessSubscription::new("sleep", TestMsg::Proc).arg("60");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(100));

        let kill_start = web_time::Instant::now();
        trigger.stop();
        handle.join().unwrap();
        let kill_elapsed = kill_start.elapsed();

        // The child sleeps 60 s; a prompt kill returns in about one poll
        // interval (50 ms). The 5 s bound is a hang guard with room for a
        // loaded runner, and it still fails when the kill waited for the
        // child (G04.2).
        assert!(
            kill_elapsed < Duration::from_secs(5),
            "kill must complete promptly after the stop signal, took {kill_elapsed:?}"
        );

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        assert!(
            msgs.iter()
                .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Killed))),
            "must emit Killed event"
        );
    }

    #[test]
    fn stop_signal_does_not_block_when_background_descendant_keeps_pipes_open() {
        let sub = ProcessSubscription::new("sh", TestMsg::Proc)
            .arg("-c")
            .arg("sleep 60 & sleep 60");
        let (tx, rx) = stdmpsc::channel();
        let (signal, trigger) = StopSignal::new();
        let start = web_time::Instant::now();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        thread::sleep(Duration::from_millis(100));
        trigger.stop();
        handle.join().unwrap();

        assert!(
            start.elapsed() < Duration::from_secs(2),
            "stop should not block behind inherited stdout/stderr pipes"
        );

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        assert!(
            msgs.iter()
                .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Killed))),
            "expected Killed event, got: {msgs:?}"
        );
    }

    #[test]
    fn timeout_does_not_block_when_background_descendant_keeps_pipes_open() {
        let sub = ProcessSubscription::new("sh", TestMsg::Proc)
            .arg("-c")
            .arg("sleep 60 & sleep 60")
            .timeout(Duration::from_millis(100));
        let (tx, rx) = stdmpsc::channel();
        let (signal, _trigger) = StopSignal::new();
        let start = web_time::Instant::now();

        let handle = thread::spawn(move || {
            sub.run(tx, signal);
        });

        handle.join().unwrap();

        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timeout should not block behind inherited stdout/stderr pipes"
        );

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        assert!(
            msgs.iter()
                .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Killed))),
            "expected Killed event, got: {msgs:?}"
        );
    }
}
