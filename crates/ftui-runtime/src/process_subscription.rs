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

use crate::subscription::{StopSignal, StopTrigger, SubId, Subscription, SubscriptionSender};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use web_time::{Duration, Instant};

/// Events emitted by a [`ProcessSubscription`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEvent {
    /// A UTF-8 stdout line, without LF or CRLF, up to [`MAX_PROCESS_LINE_BYTES`].
    Stdout(String),
    /// A UTF-8 stderr line, without LF or CRLF, up to [`MAX_PROCESS_LINE_BYTES`].
    Stderr(String),
    /// The process exited with a status code.
    Exited(i32),
    /// The process was terminated by a Unix signal.
    Signaled(i32),
    /// The process was killed by the subscription (stop signal or timeout).
    Killed,
    /// Spawning, monitoring, or reading process output failed.
    Error(String),
}

/// A subscription that spawns and monitors an external process.
///
/// Captures stdout/stderr line-by-line and sends [`ProcessEvent`] messages.
/// The process is killed when the subscription's [`StopSignal`] fires or
/// when the optional timeout expires.
///
/// Normal exit waits for stdout/stderr forwarding to finish, including bounded
/// channel backpressure. The timeout remains active during that drain. If the
/// drain is canceled after the child exits, an error describes the exit and
/// incomplete output. Cancellation can reject an unsent line or final status
/// when the model queue is full; final-status delivery is not guaranteed then.
/// Each UTF-8 line is limited to [`MAX_PROCESS_LINE_BYTES`] payload bytes.
/// LF and its immediately preceding CR are excluded from the limit. An
/// unterminated final line is delivered at EOF (preserving any trailing CR).
/// Oversized lines, invalid UTF-8, and read failures stop forwarding, terminate
/// and reap the immediate child, and produce one error instead of a normal
/// exit. That error reports incomplete output and any child cleanup failure.
/// Inherited descendant pipes cannot be interrupted by the reader stop signal.
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

/// Maximum UTF-8 payload bytes in a process stdout/stderr line (64 KiB).
///
/// The LF or CRLF delimiter is excluded. Each reader's assembly buffer holds
/// at most this many bytes plus one possible CR delimiter, alongside an 8 KiB
/// input buffer and one in-flight line of at most this many bytes. This does
/// not bound allocations in the message conversion callback or model.
pub const MAX_PROCESS_LINE_BYTES: usize = 64 * 1024;

#[derive(Debug)]
enum ProcessReadError {
    LineTooLong,
    InvalidUtf8(std::str::Utf8Error),
    Io(io::Error),
}

impl std::fmt::Display for ProcessReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LineTooLong => write!(f, "line exceeds {MAX_PROCESS_LINE_BYTES} payload bytes"),
            Self::InvalidUtf8(error) => write!(f, "invalid UTF-8: {error}"),
            Self::Io(error) => write!(f, "read failed: {error}"),
        }
    }
}

type ProcessReaderHandle = std::thread::JoinHandle<Result<(), ProcessReadError>>;

fn read_process_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    stop: &StopSignal,
) -> Result<Option<String>, ProcessReadError> {
    line.clear();
    loop {
        if stop.is_stopped() {
            return Ok(None);
        }
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ProcessReadError::Io(error)),
        };
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.unwrap_or(available.len());
        let payload = &available[..count];
        let total = line.len().saturating_add(count);
        // Retain one extra CR only while it could still be part of CRLF.
        // Check before appending so allocation never follows untrusted length.
        if total > MAX_PROCESS_LINE_BYTES
            && (total != MAX_PROCESS_LINE_BYTES + 1
                || payload.last().or_else(|| line.last()) != Some(&b'\r'))
        {
            return Err(ProcessReadError::LineTooLong);
        }
        line.extend_from_slice(payload);
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            break;
        }
    }
    if line.len() > MAX_PROCESS_LINE_BYTES {
        return Err(ProcessReadError::LineTooLong);
    }
    let text = std::str::from_utf8(line).map_err(ProcessReadError::InvalidUtf8)?;
    // Copy only the payload; the reusable maximum-sized assembly buffer stays
    // with the reader instead of inflating every short queued message.
    Ok(Some(text.to_owned()))
}

fn forward_lines<R: Read, M: Send + 'static>(
    reader: R,
    sender: SubscriptionSender<M>,
    stop: StopSignal,
    make_msg: impl Fn(String) -> M,
) -> Result<(), ProcessReadError> {
    let mut reader = io::BufReader::with_capacity(8 * 1024, reader);
    let mut line = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
    while let Some(line) = read_process_line(&mut reader, &mut line, &stop)? {
        if stop.is_stopped() {
            break;
        }
        let message = make_msg(line);
        if stop.is_stopped() || sender.send(message).is_err() {
            break;
        }
    }
    Ok(())
}

fn take_reader_error(handle: &mut Option<ProcessReaderHandle>, stream: &str) -> Option<String> {
    if !handle
        .as_ref()
        .is_some_and(std::thread::JoinHandle::is_finished)
    {
        return None;
    }
    match handle.take()?.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("{stream} {error}")),
        Err(_) => Some(format!("{stream} reader panicked")),
    }
}

fn output_failure_event(error: &str, child: &mut std::process::Child) -> ProcessEvent {
    let disposition = if let Ok(Some(status)) = child.try_wait() {
        format!("child reaped: {:?}", process_exit_event(status))
    } else {
        match child.kill() {
            Ok(()) => match child.wait() {
                Ok(status) => format!(
                    "child terminated and reaped: {:?}",
                    process_exit_event(status)
                ),
                Err(error) => format!("child kill succeeded but reaping failed: {error}"),
            },
            Err(error) => match child.try_wait() {
                Ok(Some(status)) => format!("child reaped: {:?}", process_exit_event(status)),
                _ => format!("child kill failed: {error}; child may still be running"),
            },
        }
    };
    ProcessEvent::Error(format!(
        "{error}; {disposition}; stdout/stderr output is incomplete"
    ))
}

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

    fn run(&self, sender: SubscriptionSender<M>, stop: StopSignal) {
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
                    target: crate::telemetry_schema::TARGET_PROCESS,
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
                    target: crate::telemetry_schema::TARGET_PROCESS,
                    sub_id,
                    program = %self.program,
                    error = %e,
                    "process spawn failed"
                );
                let message = (self.make_msg.as_ref())(ProcessEvent::Error(format!(
                    "Failed to spawn '{}': {}",
                    self.program, e
                )));
                let _ = send_terminal_message(
                    &sender,
                    &stop,
                    self.timeout.map(|timeout| spawn_start + timeout),
                    message,
                );
                return;
            }
        };

        let deadline = self.timeout.map(|t| web_time::Instant::now() + t);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let make_msg_ref = std::sync::Arc::clone(&self.make_msg);
        // Use the cancellation token for cooperative stop coordination.
        let token = stop.cancellation_token().clone();
        let (reader_stop, reader_trigger) = StopSignal::new();
        let reader_sender = sender.with_stop_signal(reader_stop.clone());
        let poll_interval = Duration::from_millis(50);
        let mut stdout_handle = stdout.map(|stdout| {
            let sender_out = reader_sender.clone();
            let stop_out = reader_stop.clone();
            let make_msg_out = std::sync::Arc::clone(&make_msg_ref);
            std::thread::spawn(move || {
                forward_lines(stdout, sender_out, stop_out, |line| {
                    (make_msg_out.as_ref())(ProcessEvent::Stdout(line))
                })
            })
        });
        let mut stderr_handle = stderr.map(|stderr| {
            let sender_err = reader_sender.clone();
            let stop_err = reader_stop.clone();
            let make_msg_err = std::sync::Arc::clone(&make_msg_ref);
            std::thread::spawn(move || {
                forward_lines(stderr, sender_err, stop_err, |line| {
                    (make_msg_err.as_ref())(ProcessEvent::Stderr(line))
                })
            })
        });

        let mut final_event = loop {
            // Reader results bypass the model queue: a full queue must never
            // prevent supervising a child whose other stream has failed.
            if let Some(error) = take_reader_error(&mut stdout_handle, "stdout")
                .or_else(|| take_reader_error(&mut stderr_handle, "stderr"))
            {
                reader_trigger.stop();
                break output_failure_event(&error, &mut child);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let event = process_exit_event(status);
                    if let ProcessEvent::Exited(code) = &event {
                        tracing::debug!(
                            target: crate::telemetry_schema::TARGET_PROCESS,
                            sub_id,
                            exit_code = *code,
                            elapsed_ms = spawn_start.elapsed().as_millis() as u64,
                            "process exited"
                        );
                    } else if let ProcessEvent::Signaled(signal) = &event {
                        tracing::debug!(
                            target: crate::telemetry_schema::TARGET_PROCESS,
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
                        target: crate::telemetry_schema::TARGET_PROCESS,
                        sub_id,
                        error = %e,
                        "process wait error"
                    );
                    reader_trigger.stop();
                    break ProcessEvent::Error(format!("wait error: {e}"));
                }
            }

            if let Some(dl) = deadline
                && web_time::Instant::now() >= dl
            {
                tracing::debug!(
                    target: crate::telemetry_schema::TARGET_PROCESS,
                    sub_id,
                    elapsed_ms = spawn_start.elapsed().as_millis() as u64,
                    reason = "timeout",
                    "killing process"
                );
                reader_trigger.stop();
                let _ = child.kill();
                let _ = child.wait();
                break ProcessEvent::Killed;
            }

            if token.wait_timeout(poll_interval) {
                tracing::debug!(
                    target: crate::telemetry_schema::TARGET_PROCESS,
                    sub_id,
                    elapsed_ms = spawn_start.elapsed().as_millis() as u64,
                    reason = "cancellation",
                    "killing process"
                );
                reader_trigger.stop();
                let _ = child.kill();
                let _ = child.wait();
                break ProcessEvent::Killed;
            }
        };

        if matches!(
            final_event,
            ProcessEvent::Exited(_) | ProcessEvent::Signaled(_)
        ) {
            // A child can exit while its reader is waiting for model queue
            // capacity. Preserve every line on normal completion, however
            // slowly the model drains, while still honoring stop and timeout.
            loop {
                if let Some(error) = take_reader_error(&mut stdout_handle, "stdout")
                    .or_else(|| take_reader_error(&mut stderr_handle, "stderr"))
                {
                    reader_trigger.stop();
                    final_event = output_failure_event(&error, &mut child);
                    break;
                }
                if stdout_handle.is_none() && stderr_handle.is_none() {
                    break;
                }
                let interruption = if stop.is_stopped() {
                    Some("canceled")
                } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    Some("timed out")
                } else {
                    None
                };
                if let Some(reason) = interruption {
                    reader_trigger.stop();
                    final_event = ProcessEvent::Error(format!(
                        "output drain {reason} after {final_event:?}; stdout/stderr output is incomplete"
                    ));
                    break;
                }
                token.wait_timeout(PROCESS_READER_JOIN_POLL);
            }
        }

        if let Some(handle) = stdout_handle {
            join_reader_thread_bounded(handle, "stdout", sub_id, &reader_trigger);
        }
        if let Some(handle) = stderr_handle {
            join_reader_thread_bounded(handle, "stderr", sub_id, &reader_trigger);
        }

        if send_terminal_message(
            &sender,
            &stop,
            deadline,
            (make_msg_ref.as_ref())(final_event),
        )
        .is_err()
        {
            tracing::warn!(
                target: crate::telemetry_schema::TARGET_PROCESS,
                sub_id,
                "process terminal event was not delivered: queue disconnected or stop/timeout interrupted backpressure"
            );
        }
    }
}

fn send_terminal_message<M>(
    sender: &SubscriptionSender<M>,
    stop: &StopSignal,
    deadline: Option<Instant>,
    mut message: M,
) -> Result<(), mpsc::SendError<M>> {
    loop {
        // Preserve the sender's one immediate attempt after cancellation.
        match sender.try_send(message) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Disconnected(message)) => {
                return Err(mpsc::SendError(message));
            }
            Err(mpsc::TrySendError::Full(unsent)) => message = unsent,
        }
        if stop.is_stopped() || deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(mpsc::SendError(message));
        }
        stop.wait_timeout(PROCESS_READER_JOIN_POLL);
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
    handle: ProcessReaderHandle,
    stream: &'static str,
    sub_id: SubId,
    reader_trigger: &StopTrigger,
) {
    let start = Instant::now();
    while !handle.is_finished() {
        if start.elapsed() >= PROCESS_READER_JOIN_TIMEOUT {
            reader_trigger.stop();
            tracing::warn!(
                target: crate::telemetry_schema::TARGET_PROCESS,
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

fn detach_reader_join(handle: ProcessReaderHandle, stream: &'static str) {
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

    fn collect_lines(bytes: &[u8], read_capacity: usize) -> Result<Vec<String>, ProcessReadError> {
        let mut reader = io::BufReader::with_capacity(read_capacity, bytes);
        let mut pending = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
        let (stop, _trigger) = StopSignal::new();
        let mut lines = Vec::new();
        while let Some(line) = read_process_line(&mut reader, &mut pending, &stop)? {
            lines.push(line);
        }
        Ok(lines)
    }

    #[test]
    fn bounded_lines_preserve_delimiters_unicode_and_eof_across_reads() {
        let input = "\nalpha\r\n\r\n🦀é\ninside\rcarriage\nlast\r";
        let expected = ["", "alpha", "", "🦀é", "inside\rcarriage", "last\r"];
        for capacity in 1..=input.len() {
            assert_eq!(collect_lines(input.as_bytes(), capacity).unwrap(), expected);
            assert!(collect_lines(b"", capacity).unwrap().is_empty());
            assert_eq!(collect_lines(b"tail\n", capacity).unwrap(), ["tail"]);
        }
    }

    #[test]
    fn bounded_lines_enforce_payload_limit_for_lf_crlf_and_eof() {
        for size in [MAX_PROCESS_LINE_BYTES - 1, MAX_PROCESS_LINE_BYTES] {
            let payload = "x".repeat(size);
            for delimiter in ["", "\n", "\r\n"] {
                let input = format!("{payload}{delimiter}");
                for capacity in [1, 8192, MAX_PROCESS_LINE_BYTES + 1] {
                    assert_eq!(
                        collect_lines(input.as_bytes(), capacity).unwrap(),
                        [payload.as_str()],
                        "size={size}, delimiter={delimiter:?}, read capacity={capacity}"
                    );
                }
            }
        }
        let unicode = "🦀".repeat(MAX_PROCESS_LINE_BYTES / 4);
        assert_eq!(collect_lines(unicode.as_bytes(), 3).unwrap(), [unicode]);
        for tail in ["x", "x\n", "x\r\n", "\r"] {
            let input = format!("{}{tail}", "x".repeat(MAX_PROCESS_LINE_BYTES));
            assert!(matches!(
                collect_lines(input.as_bytes(), 8192),
                Err(ProcessReadError::LineTooLong)
            ));
        }
    }

    #[test]
    fn bounded_lines_reject_invalid_utf8_without_lossy_substitution() {
        for input in [b"secret\xff\n".as_slice(), b"\xf0\x9f\xa6", b"\xc0\x80\r\n"] {
            for capacity in 1..=input.len() {
                assert!(matches!(
                    collect_lines(input, capacity),
                    Err(ProcessReadError::InvalidUtf8(_))
                ));
            }
        }
    }

    #[test]
    fn bounded_lines_stop_reading_before_oversized_input_allocates() {
        let input = vec![b'x'; MAX_PROCESS_LINE_BYTES * 16];
        let cursor = io::Cursor::new(input);
        let mut reader = io::BufReader::with_capacity(8192, cursor);
        let mut pending = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
        let capacity = pending.capacity();
        let (stop, trigger) = StopSignal::new();
        assert!(matches!(
            read_process_line(&mut reader, &mut pending, &stop),
            Err(ProcessReadError::LineTooLong)
        ));
        assert!(pending.len() <= MAX_PROCESS_LINE_BYTES + 1);
        assert_eq!(pending.capacity(), capacity);
        let consumed = reader.get_ref().position();
        assert!(consumed <= (MAX_PROCESS_LINE_BYTES + 8192) as u64);
        trigger.stop();
        assert_eq!(
            read_process_line(&mut reader, &mut pending, &stop).unwrap(),
            None
        );
        assert_eq!(reader.get_ref().position(), consumed);
    }

    #[test]
    fn bounded_lines_retry_interrupted_reads_and_report_io_failures() {
        struct FaultingReader {
            calls: usize,
        }
        impl Read for FaultingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.calls += 1;
                match self.calls {
                    1 => Err(io::ErrorKind::Interrupted.into()),
                    2 => {
                        let bytes = b"good\npartial";
                        buffer[..bytes.len()].copy_from_slice(bytes);
                        Ok(bytes.len())
                    }
                    _ => Err(io::Error::other("injected read failure")),
                }
            }
        }
        let mut reader = io::BufReader::new(FaultingReader { calls: 0 });
        let mut pending = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
        let (stop, _trigger) = StopSignal::new();
        assert_eq!(
            read_process_line(&mut reader, &mut pending, &stop).unwrap(),
            Some("good".to_owned())
        );
        assert!(matches!(
            read_process_line(&mut reader, &mut pending, &stop),
            Err(ProcessReadError::Io(error)) if error.kind() == io::ErrorKind::Other
        ));
    }

    fn collect_real_process(script: &str) -> Vec<ProcessEvent> {
        let sub = ProcessSubscription::new("sh", |event| event).args(["-c", script]);
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();
        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });
        let mut events = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(event) => events.push(event),
                Err(stdmpsc::RecvTimeoutError::Disconnected) => break,
                Err(stdmpsc::RecvTimeoutError::Timeout) => {
                    trigger.stop();
                    handle.join().expect("stop timed-out child");
                    panic!("child supervision stalled after {} events", events.len());
                }
            }
        }
        handle.join().expect("supervisor finished");
        events
    }

    #[test]
    fn real_process_preserves_complete_lines_and_maximum_payload_on_both_streams() {
        let events = collect_real_process(
            "printf '\\nalpha\\r\\n🦀é\\nlast\\r'; printf '%065536d\\r\\n' 0 >&2; printf 'tail' >&2",
        );
        let stdout: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ProcessEvent::Stdout(line) => Some(line.as_str()),
                _ => None,
            })
            .collect();
        let stderr: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ProcessEvent::Stderr(line) => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(stdout, ["", "alpha", "🦀é", "last\r"]);
        assert_eq!(
            stderr,
            ["0".repeat(MAX_PROCESS_LINE_BYTES), "tail".to_owned()]
        );
        assert_eq!(events.len(), 7);
        assert_eq!(events.last(), Some(&ProcessEvent::Exited(0)));

        let events = collect_real_process("printf '%065536d\\n' 0");
        assert_eq!(
            events,
            [
                ProcessEvent::Stdout("0".repeat(MAX_PROCESS_LINE_BYTES)),
                ProcessEvent::Exited(0)
            ]
        );
    }

    #[test]
    fn real_process_invalid_or_oversized_output_reports_one_error() {
        for (output, reason) in [
            ("printf '\\377\\n'", "invalid UTF-8"),
            ("printf '%065537d' 0", "line exceeds 65536 payload bytes"),
        ] {
            for stream in ["stdout", "stderr"] {
                let redirect = if stream == "stderr" { " >&2" } else { "" };
                for ending in ["exit 0", "exec sleep 60"] {
                    let events = collect_real_process(&format!("{output}{redirect}; {ending}"));
                    assert_eq!(events.len(), 1, "one terminal error: {events:?}");
                    let ProcessEvent::Error(error) = &events[0] else {
                        panic!("reader failure misreported: {events:?}");
                    };
                    assert!(error.starts_with(&format!("{stream} {reason}")), "{error}");
                    assert!(error.contains("reaped:"), "{error}");
                    assert!(
                        error.ends_with("stdout/stderr output is incomplete"),
                        "{error}"
                    );
                }
            }
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn real_process_reader_failure_reaps_child_while_model_queue_is_full() {
        for (output, reason) in [
            ("printf '\\377\\n'", "stderr invalid UTF-8:"),
            (
                "printf '%065537d' 0",
                "stderr line exceeds 65536 payload bytes",
            ),
        ] {
            assert_reader_failure_with_full_queue(output, reason);
        }
    }

    #[cfg(target_os = "linux")]
    fn assert_reader_failure_with_full_queue(output: &str, reason: &str) {
        let (observed_tx, observed_rx) = stdmpsc::channel();
        let sub = ProcessSubscription::new("sh", move |event| {
            observed_tx
                .send(event.clone())
                .expect("observe child and supervision");
            event
        })
        .args([
            "-c",
            &format!("printf '%s\\n' \"$$\"; kill -STOP \"$$\"; {output} >&2; exec sleep 60"),
        ]);
        let (tx, rx) = stdmpsc::sync_channel(256);
        let prefix: Vec<_> = (0..256)
            .map(|index| ProcessEvent::Stdout(format!("accepted-{index}")))
            .collect();
        for event in &prefix {
            tx.send(event.clone()).expect("fill application queue");
        }
        let (signal, trigger) = StopSignal::new();
        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });
        // This observer is before the blocked application send. Hold the real
        // child stopped until its PID is observed so reader scheduling cannot
        // race the PID callback against the other stream's failure.
        let first = observed_rx.recv_timeout(Duration::from_secs(5));
        let pid = match &first {
            Ok(ProcessEvent::Stdout(line)) => line.parse::<u32>().ok(),
            _ => None,
        };
        let Some(pid) = pid else {
            trigger.stop();
            handle.join().expect("stop child without PID");
            panic!("expected actual child PID, got {first:?}");
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
            if status.lines().any(|line| line.starts_with("State:\tT")) {
                break;
            }
            if Instant::now() >= deadline {
                trigger.stop();
                handle.join().expect("stop child that did not pause");
                panic!("child never stopped: {status}");
            }
            thread::sleep(Duration::from_millis(5));
        }
        let resumed = Command::new("kill")
            .args(["-CONT", &pid.to_string()])
            .status();
        if !resumed
            .as_ref()
            .is_ok_and(std::process::ExitStatus::success)
        {
            trigger.stop();
            handle.join().expect("stop child that did not resume");
            panic!("child resume failed: {resumed:?}");
        }
        // The other stream must fail and be supervised without any drain.
        let mut observed = vec![first.unwrap()];
        loop {
            match observed_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(event) => {
                    let terminal = matches!(event, ProcessEvent::Error(_));
                    observed.push(event);
                    if terminal {
                        break;
                    }
                }
                Err(error) => {
                    trigger.stop();
                    handle.join().expect("stop stalled supervision");
                    panic!("full queue hid reader failure: {error:?}");
                }
            }
        }
        let terminal = observed.last().unwrap().clone();
        // Retain the observations before releasing queue backpressure. This
        // detects a live child or unreaped zombie, not just a reported status.
        let child_reaped = std::fs::metadata(format!("/proc/{pid}"))
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
        let mut received: Vec<_> = (0..256)
            .map(|_| rx.recv().expect("accepted prefix"))
            .collect();
        received.push(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("terminal error after drain"),
        );
        trigger.stop();
        handle.join().expect("supervisor finished");
        assert!(
            child_reaped,
            "child must be reaped before any model queue drain: {observed:?}"
        );
        assert!(
            matches!(&terminal, ProcessEvent::Error(error)
            if error.starts_with(reason)
                && error.contains("reaped:")
                && error.ends_with("stdout/stderr output is incomplete")),
            "{terminal:?}"
        );
        let mut expected = prefix;
        expected.push(terminal);
        assert_eq!(received, expected);
        assert!(
            rx.try_iter().next().is_none(),
            "no late reader output or duplicate status"
        );
        assert!(
            observed_rx.try_iter().next().is_none(),
            "readers finished before final event"
        );
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, _trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();
        let start = web_time::Instant::now();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, _trigger) = StopSignal::new();
        let start = web_time::Instant::now();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        // Verify the cancellation token is accessible
        let token = signal.cancellation_token().clone();
        assert!(!token.is_cancelled());

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
            let (tx, rx) = stdmpsc::sync_channel(256);
            let (signal, trigger) = StopSignal::new();

            let handle = thread::spawn(move || {
                sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
            let (tx, rx) = stdmpsc::sync_channel(256);
            let (signal, _trigger) = StopSignal::new();

            let handle = thread::spawn(move || {
                sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

        thread::sleep(Duration::from_millis(500));
        trigger.stop();
        handle.join().unwrap();

        let msgs: Vec<TestMsg> = rx.try_iter().collect();

        assert_eq!(
            msgs.len(),
            3,
            "both output lines and one terminal event: {msgs:?}"
        );
        assert!(msgs.contains(&TestMsg::Proc(ProcessEvent::Stdout("FIRST".to_owned()))));
        assert!(msgs.contains(&TestMsg::Proc(ProcessEvent::Stderr("SECOND".to_owned()))));
        assert_eq!(msgs.last(), Some(&TestMsg::Proc(ProcessEvent::Exited(0))));

        // Find the position of the terminal event
        let terminal_pos = msgs
            .iter()
            .position(|m| {
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
            .expect("process must report its terminal event");

        // Find positions of stdout/stderr events
        let output_positions: Vec<usize> = msgs
            .iter()
            .enumerate()
            .filter_map(|(i, m)| match m {
                TestMsg::Proc(ProcessEvent::Stdout(_) | ProcessEvent::Stderr(_)) => Some(i),
                _ => None,
            })
            .collect();

        for &out_pos in &output_positions {
            assert!(
                out_pos < terminal_pos,
                "output event at position {out_pos} must precede terminal event at {terminal_pos}"
            );
        }
    }

    #[test]
    fn real_process_flood_backpressures_and_drains_in_order() {
        use crate::subscription::SubscriptionManager;

        let (progress_tx, progress_rx) = stdmpsc::channel();
        let sub = ProcessSubscription::new("sh", move |event| {
            if let ProcessEvent::Stdout(line) = &event
                && matches!(line.as_str(), "256" | "257")
            {
                progress_tx
                    .send(line.clone())
                    .expect("observe actual child output");
            }
            TestMsg::Proc(event)
        })
        .args([
            "-c",
            "i=0; while [ \"$i\" -lt 1024 ]; do printf '%s\\n' \"$i\"; i=$((i+1)); done",
        ]);
        let mut manager = SubscriptionManager::new();
        manager.reconcile(vec![Box::new(sub)]);

        // The conversion callback observes the real 257th line before its
        // send. With no manager drain, that send must wait on the full queue.
        assert_eq!(
            progress_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("child reached capacity"),
            "256"
        );
        // Hold backpressure longer than the old reader join timeout.
        assert_eq!(
            progress_rx.recv_timeout(PROCESS_READER_JOIN_TIMEOUT + Duration::from_millis(100)),
            Err(stdmpsc::RecvTimeoutError::Timeout),
            "the reader must not reach line 257 while 256 messages remain undrained"
        );

        let mut received = manager.drain_messages();
        assert_eq!(
            received.len(),
            64,
            "one runtime turn has a fixed batch budget"
        );
        assert_eq!(
            progress_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("drain released backpressure"),
            "257"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(
            received.last(),
            Some(TestMsg::Proc(ProcessEvent::Exited(0)))
        ) {
            assert!(
                Instant::now() < deadline,
                "actual child output did not finish: {} messages",
                received.len()
            );
            let batch = manager.drain_messages();
            assert!(
                batch.len() <= 64,
                "every drain must preserve the runtime batch budget"
            );
            if batch.is_empty() {
                thread::sleep(Duration::from_millis(1));
            }
            received.extend(batch);
        }
        manager.stop_all();
        received.extend(manager.drain_messages());
        let mut expected: Vec<_> = (0..1024)
            .map(|index| TestMsg::Proc(ProcessEvent::Stdout(index.to_string())))
            .collect();
        expected.push(TestMsg::Proc(ProcessEvent::Exited(0)));
        assert_eq!(
            received, expected,
            "all real child lines precede exactly one exit"
        );
        assert!(manager.drain_messages().is_empty());
    }

    #[test]
    fn real_process_full_queue_stop_is_prompt() {
        use crate::subscription::SubscriptionManager;

        let (capacity_tx, capacity_rx) = stdmpsc::channel();
        let (terminal_tx, terminal_rx) = stdmpsc::channel();
        let sub = ProcessSubscription::new("sh", move |event| {
            if matches!(&event, ProcessEvent::Stdout(line) if line == "256") {
                capacity_tx
                    .send(())
                    .expect("observe actual full-queue output");
            }
            if matches!(
                &event,
                ProcessEvent::Killed
                    | ProcessEvent::Exited(_)
                    | ProcessEvent::Signaled(_)
                    | ProcessEvent::Error(_)
            ) {
                terminal_tx
                    .send(event.clone())
                    .expect("observe actual child termination");
            }
            TestMsg::Proc(event)
        })
        .args([
            "-c",
            "i=0; while [ \"$i\" -lt 1024 ]; do printf '%s\\n' \"$i\"; i=$((i+1)); done; exec sleep 60",
        ]);
        let mut manager = SubscriptionManager::new();
        manager.reconcile(vec![Box::new(sub)]);
        capacity_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("child reached capacity");

        let stop_start = Instant::now();
        manager.stop_all();
        assert!(
            stop_start.elapsed() < Duration::from_secs(2),
            "full output queue must not block subscription stop"
        );
        assert_eq!(
            terminal_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("supervisor reported termination"),
            ProcessEvent::Killed
        );
        assert_eq!(
            terminal_rx.recv_timeout(Duration::from_secs(2)),
            Err(stdmpsc::RecvTimeoutError::Disconnected),
            "the supervisor and reader callbacks must finish before draining"
        );

        // Cancellation may reject the final status on a full data queue. The
        // callback above observes supervision; only these 256 lines were sent.
        let mut received = Vec::new();
        loop {
            let batch = manager.drain_messages();
            assert!(batch.len() <= 64);
            if batch.is_empty() {
                break;
            }
            received.extend(batch);
        }
        let expected: Vec<_> = (0..256)
            .map(|index| TestMsg::Proc(ProcessEvent::Stdout(index.to_string())))
            .collect();
        assert_eq!(
            received, expected,
            "already accepted output remains in FIFO order"
        );
    }

    #[test]
    fn real_process_exit_drain_timeout_cannot_block_on_final_status() {
        let (tx, rx) = stdmpsc::sync_channel(256);
        let expected: Vec<_> = (0..256)
            .map(|index| TestMsg::Proc(ProcessEvent::Stdout(format!("queued-{index}"))))
            .collect();
        // Control the queue state independently of the real child's one-line
        // output so this exercises timeout after exit, not a sleeping child.
        for message in &expected {
            tx.send(message.clone()).expect("fill model queue");
        }
        let (observed_tx, observed_rx) = stdmpsc::channel();
        let (done_tx, done_rx) = stdmpsc::channel();
        let sub = ProcessSubscription::new("sh", move |event| {
            observed_tx
                .send(event.clone())
                .expect("observe actual process callbacks");
            TestMsg::Proc(event)
        })
        .args(["-c", "printf 'tail\\n'; exit 42"])
        .timeout(Duration::from_millis(500));
        let (signal, _trigger) = StopSignal::new();
        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
            done_tx.send(()).expect("report completed supervision");
        });

        assert_eq!(
            observed_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("real child output"),
            ProcessEvent::Stdout("tail".to_owned())
        );
        assert_eq!(
            observed_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("interrupted output drain"),
            ProcessEvent::Error(
                "output drain timed out after Exited(42); stdout/stderr output is incomplete"
                    .to_owned()
            )
        );
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("a full final-status send must honor the timeout");
        handle.join().expect("supervisor thread");
        assert_eq!(
            observed_rx.recv_timeout(Duration::from_secs(2)),
            Err(stdmpsc::RecvTimeoutError::Disconnected)
        );
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            expected,
            "timeout preserves accepted messages and rejects the unsent tail/status"
        );
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();
        let start = web_time::Instant::now();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, _trigger) = StopSignal::new();
        let start = web_time::Instant::now();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
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
