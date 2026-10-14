// SPDX-License-Identifier: Apache-2.0
//! Process subscription for spawning and monitoring external processes.
//!
//! [`ProcessSubscription`] wraps [`std::process::Command`] as a first-class
//! runtime [`Subscription`]. It spawns a child process, captures stdout
//! line-by-line, and sends messages to the model. When the subscription is
//! stopped (via [`StopSignal`]), termination of the immediate child is requested
//! and its cleanup outcome is checked.
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
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use web_time::{Duration, Instant};

/// Events emitted by a [`ProcessSubscription`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEvent {
    /// A UTF-8 stdout line, without LF or CRLF, up to [`MAX_PROCESS_LINE_BYTES`].
    Stdout(String),
    /// A UTF-8 stderr line, without LF or CRLF, up to [`MAX_PROCESS_LINE_BYTES`].
    Stderr(String),
    /// Cumulative valid UTF-8 prefix of the pending stdout line.
    ///
    /// Emitted only with [`ProcessSubscription::partial_output`]. Replace the
    /// previous preview; do not append it or count it as a completed line.
    /// The original controls are retained for whole-prefix sanitization.
    StdoutPartial(String),
    /// Cumulative pending stderr prefix, with the same contract as
    /// [`Self::StdoutPartial`].
    StderrPartial(String),
    /// The process exited with a status code.
    Exited(i32),
    /// The process was terminated by a Unix signal.
    Signaled(i32),
    /// Termination was requested and the immediate child was reaped.
    ///
    /// On Unix its observed status must be SIGKILL. Pending input/output may
    /// be incomplete. Failed or unconfirmed cleanup produces [`Self::Error`].
    Killed,
    /// Spawning, monitoring, or process pipe I/O failed.
    Error(String),
}

/// Result of the most recent interrupt request for one process run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessInterruptStatus {
    /// No interrupt has been requested.
    Idle,
    /// One interrupt is waiting for the supervisor.
    Pending,
    /// The operating system accepted SIGINT; the child may ignore it.
    Sent,
    /// Signal delivery failed; no successful delivery is claimed.
    Failed(String),
    /// The run closed before the pending request was delivered.
    Canceled,
}

/// A snapshot of one process run, independent of model queue capacity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessControlStatus {
    /// Immediate child PID while control is open. Never use it to send signals.
    pub pid: Option<u32>,
    /// The supervisor accepts no further requests for this run.
    pub closed: bool,
    /// No child was spawned, or its owner confirmed that it was reaped.
    ///
    /// This does not prove output drain completion. Wait for this run's terminal
    /// [`ProcessEvent`] as well before replacing its subscription. A retained
    /// background reaper can set this after an unconfirmed-cleanup error.
    pub can_restart: bool,
    /// Outcome of the most recent interrupt request.
    pub interrupt: ProcessInterruptStatus,
}

/// Why an interrupt could not be admitted. No child I/O occurs during admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessControlError {
    /// The supervisor has not spawned this run yet.
    NotRunning,
    /// This run no longer accepts requests.
    Closed,
    /// One request is already waiting; there is no unbounded request queue.
    AlreadyPending,
    /// SIGINT delivery is not supported on this platform.
    Unsupported,
}

#[derive(Debug)]
struct ProcessControlState {
    claimed: bool,
    status: ProcessControlStatus,
    /// Latest requested PTY size as (columns, rows); sizes coalesce, not queue.
    resize: Option<(u16, u16)>,
}

/// Cloneable interrupt control for exactly one [`ProcessSubscription`] run.
///
/// Keep the handle in the model, rebuilding subscriptions with clones. Tag
/// process messages with [`generation`](Self::generation) to reject delayed
/// messages after a restart. Use a fresh handle for each new run. Dropping a
/// descriptor or a handle clone does not close an active run.
///
/// Requests never wait for child I/O or model/stdin queue capacity. The owning
/// supervisor sends SIGINT to the immediate child on Unix, without a shell or
/// process-group signal. Linux requires a retained pidfd; if unavailable,
/// delivery fails instead of falling back to a potentially reused PID. Other
/// Unix platforms require the supervisor to remain the child's sole reaper.
/// A [`ProcessSubscription::pty`] run is the exception: SIGINT goes to the
/// child's whole process group, as Ctrl-C does in a terminal, and
/// [`request_resize`](Self::request_resize) resizes its PTY.
#[derive(Clone, Debug)]
pub struct ProcessControl {
    id: u64,
    state: Arc<Mutex<ProcessControlState>>,
}

impl Default for ProcessControl {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessControl {
    /// Create control for a future process run.
    #[must_use]
    pub fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_ID
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("process control identity space exhausted"),
            state: Arc::new(Mutex::new(ProcessControlState {
                claimed: false,
                status: ProcessControlStatus {
                    pid: None,
                    closed: false,
                    can_restart: true,
                    interrupt: ProcessInterruptStatus::Idle,
                },
                resize: None,
            })),
        }
    }

    /// Stable identity shared by clones; fresh handles have distinct identities.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.id
    }

    /// Read one consistent snapshot. A closed handle need not mean output drained.
    #[must_use]
    pub fn status(&self) -> ProcessControlStatus {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .status
            .clone()
    }

    /// Admit one interrupt for the supervisor to deliver.
    ///
    /// Admission is not delivery. Read [`status`](Self::status) for the outcome;
    /// even `Sent` does not imply that a child handler ran or the child exited.
    pub fn request_interrupt(&self) -> Result<(), ProcessControlError> {
        if !cfg!(unix) {
            return Err(ProcessControlError::Unsupported);
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let status = &mut state.status;
        if status.closed {
            return Err(ProcessControlError::Closed);
        }
        if status.pid.is_none() {
            return Err(ProcessControlError::NotRunning);
        }
        if status.interrupt == ProcessInterruptStatus::Pending {
            return Err(ProcessControlError::AlreadyPending);
        }
        status.interrupt = ProcessInterruptStatus::Pending;
        Ok(())
    }

    /// Ask the supervisor to resize this run's PTY to `cols` x `rows` cells.
    ///
    /// The latest request replaces a pending one. A request made before the
    /// child spawns sets its initial size. Otherwise the supervisor applies it
    /// within one 50 ms poll, then sends SIGWINCH to the child's process group.
    /// Zero dimensions become one. Runs without [`ProcessSubscription::pty`]
    /// ignore resizes, and admission does not report whether one was applied.
    pub fn request_resize(&self, cols: u16, rows: u16) -> Result<(), ProcessControlError> {
        if !cfg!(unix) {
            return Err(ProcessControlError::Unsupported);
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.status.closed {
            return Err(ProcessControlError::Closed);
        }
        state.resize = Some((cols.max(1), rows.max(1)));
        Ok(())
    }

    fn take_resize(&self) -> Option<(u16, u16)> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .resize
            .take()
    }

    fn claim(&self) -> Option<ProcessControlRun> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.claimed {
            return None;
        }
        state.claimed = true;
        Some(ProcessControlRun(self.clone()))
    }

    fn started(&self, pid: u32) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.status.pid = Some(pid);
        state.status.can_restart = false;
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.status.pid = None;
        state.status.closed = true;
        if state.status.interrupt == ProcessInterruptStatus::Pending {
            state.status.interrupt = ProcessInterruptStatus::Canceled;
        }
    }

    fn confirm_reaped(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .status
            .can_restart = true;
    }

    fn dispatch_interrupt(&self, child: &std::process::Child, group: Option<&ProcessGroup>) {
        let status = self.status();
        if status.closed || status.interrupt != ProcessInterruptStatus::Pending {
            return;
        }
        // Only the supervisor calls this, before reaping. No state lock spans
        // the OS call, and no message queue is needed to publish its result.
        let result = match group {
            Some(group) => group.interrupt(),
            None => interrupt_child(child),
        };
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.status.interrupt = match result {
            Ok(()) => ProcessInterruptStatus::Sent,
            Err(error) => ProcessInterruptStatus::Failed(error.to_string()),
        };
    }
}

struct ProcessControlRun(ProcessControl);

impl Drop for ProcessControlRun {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(unix)]
fn interrupt_child(child: &std::process::Child) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    std::os::linux::process::ChildExt::pidfd(child).map_err(|error| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("retained pidfd unavailable; interrupt not sent: {error}"),
        )
    })?;
    std::os::unix::process::ChildExt::send_signal(child, rustix::process::Signal::INT.as_raw())
}

#[cfg(not(unix))]
fn interrupt_child(_child: &std::process::Child) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "SIGINT is unsupported",
    ))
}

/// The session and process group of a [`ProcessSubscription::pty`] child,
/// owned by its run.
///
/// The child starts a new session, so its PID is also its process group ID.
/// Group signals are sent only while that leader is unreaped: an unreaped
/// child keeps its PID reserved, and with it the group ID, so a signal can
/// never reach a group that reused the ID. The group is swept with SIGKILL
/// before the leader is reaped, so nothing that stayed in it outlives the run.
/// A descendant that leaves the group (`setsid`, `setpgid`) is not tracked.
#[cfg(unix)]
struct ProcessGroup {
    leader: rustix::process::Pid,
    swept: bool,
}

#[cfg(unix)]
impl ProcessGroup {
    fn new(child: &std::process::Child) -> Self {
        Self {
            leader: rustix::process::Pid::from_child(child),
            swept: false,
        }
    }

    fn signal(&self, signal: rustix::process::Signal) -> io::Result<()> {
        if self.swept {
            return Err(io::Error::other("process group was already swept"));
        }
        rustix::process::kill_process_group(self.leader, signal).map_err(io::Error::from)
    }

    fn interrupt(&self) -> io::Result<()> {
        self.signal(rustix::process::Signal::INT)
    }

    const fn is_swept(&self) -> bool {
        self.swept
    }

    /// Send SIGKILL to every process still in the group, once.
    fn kill(&mut self) -> io::Result<()> {
        let result = self.signal(rustix::process::Signal::KILL);
        self.swept = true;
        result
    }

    /// Whether the leader has exited, leaving it unreaped.
    fn leader_exited(&self) -> io::Result<bool> {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        Ok(waitid(WaitId::Pid(self.leader), options)?.is_some())
    }
}

/// Never constructed: PTY runs are refused before spawning off Unix.
#[cfg(not(unix))]
enum ProcessGroup {}

#[cfg(not(unix))]
impl ProcessGroup {
    fn interrupt(&self) -> io::Result<()> {
        match *self {}
    }

    const fn is_swept(&self) -> bool {
        match *self {}
    }

    fn kill(&mut self) -> io::Result<()> {
        match *self {}
    }
}

/// Maximum queued input lines, excluding the one currently being written.
pub const PROCESS_INPUT_CAPACITY: usize = 16;

/// A rejected input line. The original text is returned without modification.
#[derive(Debug, PartialEq, Eq)]
pub enum ProcessInputError {
    /// The bounded queue has no room; retry after the child reads input.
    Full(String),
    /// Input was closed, or its process run ended.
    Closed(String),
    /// The line exceeds [`MAX_PROCESS_LINE_BYTES`] UTF-8 bytes.
    TooLong(String),
}

#[derive(Debug)]
struct ProcessInputState {
    sender: Option<mpsc::SyncSender<Box<str>>>,
    receiver: Option<mpsc::Receiver<Box<str>>>,
}

/// Bounded, cloneable input for one [`ProcessSubscription`] run.
///
/// Keep this handle in the model and pass a clone to [`ProcessSubscription::stdin`]
/// each time `subscriptions()` is rebuilt. Clones share one queue and identity.
/// A fresh handle is required to start another process run. Dropping a clone
/// does not close input; call [`close`](Self::close) to request EOF.
///
/// Sending never waits for pipe I/O or queue capacity. At most
/// [`PROCESS_INPUT_CAPACITY`] lines are queued, plus one being written, each
/// limited to [`MAX_PROCESS_LINE_BYTES`] bytes. Acceptance means queued, not
/// acknowledged by the child; stop, exit, or I/O failure can discard pending
/// input. A worker writes accepted lines in FIFO order with an appended LF.
#[derive(Clone, Debug)]
pub struct ProcessInput {
    id: u64,
    state: Arc<Mutex<ProcessInputState>>,
}

impl Default for ProcessInput {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessInput {
    /// Create input for a single future process run. Lines may be queued now.
    #[must_use]
    pub fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let (sender, receiver) = mpsc::sync_channel(PROCESS_INPUT_CAPACITY);
        Self {
            id: NEXT_ID
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("process input identity space exhausted"),
            state: Arc::new(Mutex::new(ProcessInputState {
                sender: Some(sender),
                receiver: Some(receiver),
            })),
        }
    }

    /// Queue UTF-8 text followed by LF, preserving any embedded newlines.
    ///
    /// Returns the unchanged text on rejection. Oversized input is checked
    /// before queue state. The short state lock never covers child I/O.
    pub fn try_send_line(&self, line: String) -> Result<(), ProcessInputError> {
        if line.len() > MAX_PROCESS_LINE_BYTES {
            return Err(ProcessInputError::TooLong(line));
        }
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(sender) = &state.sender else {
            return Err(ProcessInputError::Closed(line));
        };
        // Do not retain excess capacity supplied by the caller in the queue.
        match sender.try_send(line.into_boxed_str()) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(line)) => Err(ProcessInputError::Full(line.into())),
            Err(mpsc::TrySendError::Disconnected(line)) => {
                Err(ProcessInputError::Closed(line.into()))
            }
        }
    }

    /// Reject future sends, drain accepted lines, then close the child's stdin.
    ///
    /// This is idempotent across clones. Drain still depends on the child
    /// reading; stop/timeout can cancel it. Closing before spawn gives EOF
    /// after any lines already queued.
    pub fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sender
            .take();
    }

    fn claim(&self) -> Option<(ProcessInputRun, mpsc::Receiver<Box<str>>)> {
        let receiver = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .receiver
            .take()?;
        Some((ProcessInputRun(self.clone()), receiver))
    }
}

struct ProcessInputRun(ProcessInput);

impl Drop for ProcessInputRun {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// A parent-owned pipe or PTY endpoint. Unix makes it nonblocking and polls it.
#[cfg(unix)]
trait ProcessEndpoint: std::os::fd::AsFd + Send + 'static {}
#[cfg(unix)]
impl<T: std::os::fd::AsFd + Send + 'static> ProcessEndpoint for T {}
#[cfg(not(unix))]
trait ProcessEndpoint: Send + 'static {}
#[cfg(not(unix))]
impl<T: Send + 'static> ProcessEndpoint for T {}

/// Maximum bytes written to a canonical PTY before flushing with `VEOF` so
/// the kernel's 4095-byte canonical buffer advances `canon_head` and applies
/// normal flow control instead of discarding bytes.
const PTY_CANONICAL_CHUNK_BYTES: usize = 2048;

/// Write queued lines, then `end_of_input` once input is closed.
///
/// A pipe signals EOF by closing, so its `end_of_input` is empty. A PTY has no
/// write side to close: EOF is the VEOF byte (Ctrl-D) at the start of a line,
/// and intermediate `VEOF` flushes on a non-empty canonical buffer deliver
/// lines longer than the 4095-byte kernel canonical line limit without newline.
fn write_process_input<W: Write + ProcessEndpoint>(
    stdin: W,
    receiver: mpsc::Receiver<Box<str>>,
    stop: StopSignal,
    end_of_input: &[u8],
) -> io::Result<()> {
    #[cfg(unix)]
    let mut stdin = process_pipe(stdin, stop.clone())?;
    #[cfg(not(unix))]
    let mut stdin = stdin;
    loop {
        match receiver.recv_timeout(PROCESS_READER_JOIN_POLL) {
            Ok(line) => {
                if stop.is_stopped() {
                    return Err(io::Error::other("queued stdin was canceled before writing"));
                }
                if end_of_input.is_empty() {
                    stdin.write_all(line.as_bytes())?;
                    stdin.write_all(b"\n")?;
                } else {
                    for segment in line.as_bytes().split(|&byte| byte == b'\n') {
                        let mut rest = segment;
                        while rest.len() > PTY_CANONICAL_CHUNK_BYTES {
                            let (chunk, tail) = rest.split_at(PTY_CANONICAL_CHUNK_BYTES);
                            stdin.write_all(chunk)?;
                            stdin.write_all(end_of_input)?;
                            rest = tail;
                        }
                        stdin.write_all(rest)?;
                        stdin.write_all(b"\n")?;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) if stop.is_stopped() => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                match stdin.write_all(end_of_input) {
                    // A stopping run closes input before it stops this worker,
                    // and a child whose side is gone cannot read EOF anyway.
                    Err(error) if stop.is_stopped() || is_hangup(&error) => {}
                    result => result?,
                }
                break;
            }
        }
    }
    Ok(())
}

/// A write found no reader: a closed pipe, or a PTY whose slave side is
/// closed on systems that report EIO for it (Linux buffers the write instead).
fn is_hangup(error: &io::Error) -> bool {
    #[cfg(unix)]
    if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) {
        return true;
    }
    error.kind() == io::ErrorKind::BrokenPipe
}

/// The parent's read endpoint of a PTY.
///
/// Once no slave descriptor remains open, Linux reports EIO where a pipe
/// reports end-of-file. Both mean the output is complete.
#[cfg(unix)]
struct PtyOutput(std::fs::File);

#[cfg(unix)]
impl Read for PtyOutput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buffer) {
            Err(error) if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => {
                Ok(0)
            }
            result => result,
        }
    }
}

#[cfg(unix)]
impl std::os::fd::AsFd for PtyOutput {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// Parent-side endpoints of one run's PTY, made before the child spawns.
#[cfg(unix)]
struct PtyEndpoints {
    /// Kept by the supervisor to resize the PTY.
    master: std::os::fd::OwnedFd,
    output: PtyOutput,
    input: Option<std::fs::File>,
}

/// Open a PTY of `(cols, rows)` and attach its slave to `cmd`.
///
/// stdout and stderr share the slave, which is also stdin when `stdin` is set;
/// otherwise stdin stays as configured. Echo is off because the host shows its
/// own input, and LF stays LF so captured lines do not gain a CR. The child
/// starts a new session without a controlling terminal: it cannot open the
/// host terminal through `/dev/tty`, and its PID becomes its process group ID.
#[cfg(unix)]
fn prepare_pty(
    cmd: &mut Command,
    (cols, rows): (u16, u16),
    stdin: bool,
) -> io::Result<PtyEndpoints> {
    use rustix::fs::{Mode, OFlags};
    use rustix::pty::OpenptFlags;
    use rustix::termios::{InputModes, LocalModes, OptionalActions, OutputModes};

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd"
    ))]
    let master =
        rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd"
    )))]
    let master = {
        let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)?;
        master
    };
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    let name = rustix::pty::ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut termios = rustix::termios::tcgetattr(&slave)?;
    termios
        .local_modes
        .remove(LocalModes::ECHO | LocalModes::ECHONL | LocalModes::IEXTEN);
    termios.input_modes.remove(InputModes::IXON);
    termios.output_modes.remove(OutputModes::ONLCR);
    rustix::termios::tcsetattr(&slave, OptionalActions::Now, &termios)?;
    resize_pty(&master, cols, rows)?;

    let output = PtyOutput(std::fs::File::from(master.try_clone()?));
    let input = if stdin {
        cmd.stdin(Stdio::from(slave.try_clone()?));
        Some(std::fs::File::from(master.try_clone()?))
    } else {
        None
    };
    cmd.stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    std::os::unix::process::CommandExt::setsid(cmd, true);
    Ok(PtyEndpoints {
        master,
        output,
        input,
    })
}

#[cfg(unix)]
fn resize_pty(master: impl std::os::fd::AsFd, cols: u16, rows: u16) -> io::Result<()> {
    let size = rustix::termios::Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    rustix::termios::tcsetwinsize(master, size).map_err(io::Error::from)
}

/// A new PTY's size: the host terminal's, from stdout then stdin, else 80x24.
#[cfg(unix)]
fn host_terminal_size() -> (u16, u16) {
    [
        rustix::termios::tcgetwinsize(io::stdout()),
        rustix::termios::tcgetwinsize(io::stdin()),
    ]
    .into_iter()
    .flatten()
    .find(|size| size.ws_col > 0 && size.ws_row > 0)
    .map_or((80, 24), |size| (size.ws_col, size.ws_row))
}

// Only the parent-owned endpoint becomes nonblocking. The child's opposite
// endpoint remains blocking, including copies inherited by its descendants.
#[cfg(unix)]
fn process_pipe<P: std::os::fd::AsFd>(pipe: P, stop: StopSignal) -> io::Result<ProcessPipe<P>> {
    rustix::io::ioctl_fionbio(&pipe, true)?;
    Ok(ProcessPipe { pipe, stop })
}

#[cfg(unix)]
struct ProcessPipe<P> {
    pipe: P,
    stop: StopSignal,
}

#[cfg(unix)]
impl<P: std::os::fd::AsFd> ProcessPipe<P> {
    fn wait_ready(&self, interest: rustix::event::PollFlags) -> io::Result<()> {
        let mut fds = [rustix::event::PollFd::new(&self.pipe, interest)];
        // Readiness wakes immediately for data/space/EOF. The finite timeout
        // bounds cancellation latency even when a descendant holds the pipe.
        let timeout = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 50_000_000,
        };
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(unix)]
impl<P: Read + std::os::fd::AsFd> Read for ProcessPipe<P> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stop.is_stopped() {
                // read_process_line checks stop before retrying Interrupted.
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            match self.pipe.read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.wait_ready(rustix::event::PollFlags::IN)?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

#[cfg(unix)]
impl<P: Write + std::os::fd::AsFd> Write for ProcessPipe<P> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            if self.stop.is_stopped() {
                // write_all retries Interrupted; cancellation must terminate
                // that loop rather than spinning on a permanently stopped run.
                return Err(io::Error::other("stdin write canceled before completion"));
            }
            match self.pipe.write(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.wait_ready(rustix::event::PollFlags::OUT)?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.pipe.flush()
    }
}

/// A subscription that spawns and monitors an external process.
///
/// Captures stdout/stderr line-by-line and sends [`ProcessEvent`] messages.
/// Termination is requested when the subscription's [`StopSignal`] fires or
/// when the optional timeout expires. [`ProcessEvent::Killed`] requires a
/// successful kill request and confirmed reaping (with SIGKILL status on Unix).
/// Failed or unconfirmed cleanup produces [`ProcessEvent::Error`].
///
/// Normal exit waits for stdout/stderr forwarding to finish, including bounded
/// channel backpressure. The timeout remains active during that drain. If the
/// drain is canceled after the child exits, an error describes the exit and
/// incomplete output. Cancellation can reject an unsent line, preview, or final
/// status when the model queue is full; final-status delivery is not guaranteed.
/// Each UTF-8 line is limited to [`MAX_PROCESS_LINE_BYTES`] payload bytes.
/// LF and its immediately preceding CR are excluded from the limit. An
/// unterminated final line is delivered at EOF (preserving any trailing CR).
/// Opt-in [`partial_output`](Self::partial_output) also previews growing valid
/// prefixes before the next read blocks. It does not change completed lines.
/// Oversized lines, invalid UTF-8, and read failures stop forwarding, request
/// termination of the immediate child, and produce one error instead of a
/// normal exit. That error reports incomplete output and the cleanup outcome.
/// Monitoring and input failures use the same cleanup path. After an accepted
/// kill request, foreground reaping is limited to 500 ms; a rejected request
/// never leads to a blocking foreground wait. If ownership remains safe, an
/// unconfirmed child is handed to a background reaper. If that thread cannot
/// start, the error says so; a later wait failure is logged and leaves restart
/// disabled. Late reaping does not turn the earlier error into success. Linux
/// retains a pidfd when available; after a wait error, targets without a
/// retained process handle refuse further waits and signals for that PID.
/// On Unix, parent pipe endpoints use nonblocking I/O and readiness waits;
/// cancellation stops the workers even if descendants retain their endpoints.
/// Other targets still use blocking pipes and bounded joins, so an inherited
/// endpoint can keep a detached worker alive after cancellation.
/// Input is closed by default; [`stdin`](Self::stdin) enables a bounded input
/// worker. Over pipes, descendants are neither terminated nor reaped on any
/// target. [`pty`](Self::pty) runs the child on a pseudo-terminal in its own
/// session, and ends its whole process group with the run.
/// Cancellation is not an atomic write boundary: a prefix may already have
/// reached the child, and an I/O syscall can race with the stop request.
/// Optional [`control`](Self::control) admits interrupts independently of those
/// queues. Its generation belongs to one run. Restart only after that run's
/// terminal event and confirmed child cleanup, using a fresh control handle.
pub struct ProcessSubscription<M: Send + 'static> {
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    timeout: Option<Duration>,
    input: Option<ProcessInput>,
    control: Option<ProcessControl>,
    partial_output: bool,
    pty: bool,
    id: SubId,
    explicit_id: bool,
    make_msg: std::sync::Arc<dyn Fn(ProcessEvent) -> M + Send + Sync>,
}

const PROCESS_READER_JOIN_TIMEOUT: Duration = Duration::from_millis(250);
const PROCESS_READER_JOIN_POLL: Duration = Duration::from_millis(5);
const PROCESS_REAP_TIMEOUT: Duration = Duration::from_millis(500);

/// Maximum UTF-8 payload bytes in a process stdout/stderr line (64 KiB).
///
/// The LF or CRLF delimiter is excluded. Each reader's assembly buffer holds
/// at most this many bytes plus one possible CR delimiter, alongside an 8 KiB
/// input buffer and one in-flight line or preview of at most this many bytes.
/// This does not bound allocations in the message conversion callback or model.
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

#[cfg(test)]
fn read_process_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    stop: &StopSignal,
    preview: Option<&mut dyn FnMut(&str) -> bool>,
) -> Result<Option<String>, ProcessReadError> {
    read_process_line_mode(reader, line, stop, false, preview)
}

fn read_process_line_mode<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    stop: &StopSignal,
    cr_rewinds: bool,
    mut preview: Option<&mut dyn FnMut(&str) -> bool>,
) -> Result<Option<String>, ProcessReadError> {
    line.clear();
    let mut previewed_bytes = 0;
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
        if cr_rewinds && line.last() == Some(&b'\r') {
            match available[0] {
                b'\r' => {
                    let crs = available.iter().take_while(|&&byte| byte == b'\r').count();
                    reader.consume(crs);
                    continue;
                }
                b'\n' => {
                    reader.consume(1);
                    line.pop();
                    break;
                }
                _ => {
                    std::str::from_utf8(line).map_err(ProcessReadError::InvalidUtf8)?;
                    line.clear();
                    previewed_bytes = 0;
                }
            }
        }
        if cr_rewinds {
            let stop_pos = available
                .iter()
                .position(|&byte| byte == b'\n' || byte == b'\r');
            let count = stop_pos.unwrap_or(available.len());
            let payload = &available[..count];
            let total = line.len().saturating_add(count);
            if total > MAX_PROCESS_LINE_BYTES {
                return Err(ProcessReadError::LineTooLong);
            }
            line.extend_from_slice(payload);
            match available.get(count) {
                Some(&b'\n') => {
                    reader.consume(count + 1);
                    break;
                }
                Some(&b'\r') => {
                    let crs = available[count..]
                        .iter()
                        .take_while(|&&byte| byte == b'\r')
                        .count();
                    line.push(b'\r');
                    reader.consume(count + crs);
                }
                _ => {
                    reader.consume(count);
                }
            }
        } else {
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
        if let Some(preview) = preview.as_mut() {
            let prefix = match std::str::from_utf8(line) {
                Ok(text) => text,
                Err(error) if error.error_len().is_none() => {
                    // Preserve an incomplete scalar for the next physical read.
                    std::str::from_utf8(&line[..error.valid_up_to()])
                        .map_err(ProcessReadError::InvalidUtf8)?
                }
                Err(error) => return Err(ProcessReadError::InvalidUtf8(error)),
            };
            // A final CR might be the first half of CRLF. Keep it in assembly
            // but do not expose it until another payload byte or EOF resolves it.
            let prefix = if line.last() == Some(&b'\r') {
                prefix.strip_suffix('\r').unwrap_or(prefix)
            } else {
                prefix
            };
            if prefix.len() > previewed_bytes {
                if !preview(prefix) {
                    return Ok(None);
                }
                previewed_bytes = prefix.len();
            }
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
    partial_output: bool,
    cr_rewinds: bool,
    make_msg: impl Fn(String) -> M,
    make_partial: impl Fn(String) -> M,
) -> Result<(), ProcessReadError> {
    let mut reader = io::BufReader::with_capacity(8 * 1024, reader);
    let mut line = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
    let mut preview = |prefix: &str| {
        if stop.is_stopped() {
            return false;
        }
        let message = make_partial(prefix.to_owned());
        !stop.is_stopped() && sender.send(message).is_ok()
    };
    while let Some(line) = read_process_line_mode(
        &mut reader,
        &mut line,
        &stop,
        cr_rewinds,
        partial_output.then_some(&mut preview as &mut dyn FnMut(&str) -> bool),
    )? {
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

/// What every output reader of one run shares.
struct LineReaders<M: Send + 'static> {
    sender: SubscriptionSender<M>,
    stop: StopSignal,
    partial_output: bool,
    cr_rewinds: bool,
    make_msg: std::sync::Arc<dyn Fn(ProcessEvent) -> M + Send + Sync>,
}

impl<M: Send + 'static> LineReaders<M> {
    /// Forward `reader`'s lines and previews from a new thread.
    fn spawn<R: Read + ProcessEndpoint>(
        &self,
        reader: R,
        line_event: fn(String) -> ProcessEvent,
        partial_event: fn(String) -> ProcessEvent,
    ) -> std::thread::JoinHandle<Result<(), ProcessReadError>> {
        let sender = self.sender.clone();
        let stop = self.stop.clone();
        let partial_output = self.partial_output;
        let cr_rewinds = self.cr_rewinds;
        let make_msg = std::sync::Arc::clone(&self.make_msg);
        std::thread::spawn(move || {
            #[cfg(unix)]
            let reader = process_pipe(reader, stop.clone()).map_err(ProcessReadError::Io)?;
            forward_lines(
                reader,
                sender,
                stop,
                partial_output,
                cr_rewinds,
                |line| (make_msg.as_ref())(line_event(line)),
                |prefix| (make_msg.as_ref())(partial_event(prefix)),
            )
        })
    }
}

fn take_reader_error<E: std::fmt::Display>(
    handle: &mut Option<std::thread::JoinHandle<Result<(), E>>>,
    stream: &str,
) -> Option<String> {
    if !handle
        .as_ref()
        .is_some_and(std::thread::JoinHandle::is_finished)
    {
        return None;
    }
    match handle.take()?.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("{stream} {error}")),
        Err(_) => Some(format!("{stream} worker panicked")),
    }
}

#[derive(Debug)]
struct ChildCleanup {
    status: Option<std::process::ExitStatus>,
    kill_sent: bool,
    detail: String,
}

impl ChildCleanup {
    fn reaped(status: std::process::ExitStatus, kill_sent: bool, prior_error: &str) -> Self {
        Self {
            status: Some(status),
            kill_sent,
            detail: format!(
                "{prior_error}child reaped: {:?}",
                process_exit_event(status)
            ),
        }
    }

    fn unconfirmed(detail: String) -> Self {
        Self {
            status: None,
            kill_sent: false,
            detail: format!("{detail}; child may still be running or unreaped"),
        }
    }
}

fn reap_child_until(
    child: &mut std::process::Child,
    deadline: Instant,
    wait_error: &mut Option<String>,
) -> io::Result<Option<std::process::ExitStatus>> {
    loop {
        match try_wait_child(child, wait_error) {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        // A stopped cancellation token would return immediately and spin.
        std::thread::sleep(PROCESS_READER_JOIN_POLL.min(remaining));
    }
}

fn has_retained_process_handle(child: &std::process::Child) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::os::linux::process::ChildExt::pidfd(child).is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = child;
        // Windows retains a process handle; other Unix targets only retain a
        // PID, which must not be signaled after ownership becomes uncertain.
        cfg!(windows)
    }
}

fn try_wait_child(
    child: &mut std::process::Child,
    wait_error: &mut Option<String>,
) -> io::Result<Option<std::process::ExitStatus>> {
    if let Some(error) = wait_error.as_ref()
        && !has_retained_process_handle(child)
    {
        // Ownership uncertainty is sticky. A later numeric-PID wait could
        // observe a different child that reused this PID, even without a kill.
        return Err(io::Error::other(error.clone()));
    }
    let result = child.try_wait();
    if let Err(error) = &result {
        wait_error.get_or_insert_with(|| error.to_string());
    }
    result
}

/// [`try_wait_child`] for a run that may own its child's process group.
///
/// An exited leader of an unswept group is reaped only after the group is
/// swept, while its unreaped PID still reserves the group ID.
fn try_wait_leader(
    child: &mut std::process::Child,
    wait_error: &mut Option<String>,
    group: Option<&mut ProcessGroup>,
) -> io::Result<Option<std::process::ExitStatus>> {
    #[cfg(unix)]
    if let Some(group) = group
        && !group.is_swept()
    {
        match group.leader_exited() {
            Ok(false) => return Ok(None),
            Ok(true) => {
                if let Err(error) = group.kill()
                    && error.raw_os_error() != Some(rustix::io::Errno::SRCH.raw_os_error())
                {
                    tracing::warn!(%error, "process group sweep after leader exit failed");
                }
            }
            Err(error) => {
                wait_error.get_or_insert_with(|| error.to_string());
                return Err(error);
            }
        }
    }
    #[cfg(not(unix))]
    if let Some(group) = group {
        match *group {}
    }
    try_wait_child(child, wait_error)
}

fn cleanup_child(
    child: &mut std::process::Child,
    wait_error: &mut Option<String>,
    mut group: Option<&mut ProcessGroup>,
) -> ChildCleanup {
    let prior_error = match try_wait_leader(child, wait_error, group.as_deref_mut()) {
        Ok(Some(status)) => return ChildCleanup::reaped(status, false, ""),
        Ok(None) => String::new(),
        Err(error) => {
            let context = format!("child wait failed: {error}; ");
            if !has_retained_process_handle(child) {
                return ChildCleanup::unconfirmed(format!(
                    "{context}kill and further waits not attempted without a retained process handle"
                ));
            }
            context
        }
    };
    // An owned group is killed whole; the leader is one of its members.
    let kill = match group {
        Some(group) if !group.is_swept() => group.kill(),
        _ => child.kill(),
    };
    if let Err(error) = kill {
        let context = format!("{prior_error}child kill failed: {error}; ");
        // An exit can race with kill. Observe it once; never block waiting for
        // a still-running child after the termination request was rejected.
        return match try_wait_child(child, wait_error) {
            Ok(Some(status)) => ChildCleanup::reaped(status, false, &context),
            Ok(None) => ChildCleanup::unconfirmed(format!("{context}child has not exited")),
            Err(error) => ChildCleanup::unconfirmed(format!("{context}reaping failed: {error}")),
        };
    }
    // This budget is independent of an already-expired subscription timeout.
    match reap_child_until(child, Instant::now() + PROCESS_REAP_TIMEOUT, wait_error) {
        Ok(Some(status)) => ChildCleanup::reaped(status, true, &prior_error),
        Ok(None) => ChildCleanup::unconfirmed(format!(
            "{prior_error}child kill accepted but reaping exceeded {} ms",
            PROCESS_REAP_TIMEOUT.as_millis()
        )),
        Err(error) => ChildCleanup::unconfirmed(format!(
            "{prior_error}child kill accepted but reaping failed: {error}"
        )),
    }
}

fn cleanup_failure_event(error: &str, cleanup: &ChildCleanup) -> ProcessEvent {
    ProcessEvent::Error(format!(
        "{error}; {}; stdout/stderr output is incomplete",
        cleanup.detail
    ))
}

fn output_failure_event(
    error: &str,
    child: &mut std::process::Child,
    wait_error: &mut Option<String>,
    group: Option<&mut ProcessGroup>,
) -> ProcessEvent {
    cleanup_failure_event(error, &cleanup_child(child, wait_error, group))
}

fn stopped_process_event(
    reason: &str,
    child: &mut std::process::Child,
    wait_error: &mut Option<String>,
    group: Option<&mut ProcessGroup>,
) -> ProcessEvent {
    let cleanup = cleanup_child(child, wait_error, group);
    let terminated = cleanup.status.is_some_and(|status| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            status.signal() == Some(rustix::process::Signal::KILL.as_raw())
        }
        #[cfg(not(unix))]
        {
            let _ = status;
            true
        }
    });
    if cleanup.kill_sent && terminated {
        ProcessEvent::Killed
    } else {
        cleanup_failure_event(reason, &cleanup)
    }
}

fn reap_child_in_background(
    mut child: std::process::Child,
    control: Option<ProcessControl>,
    sub_id: SubId,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("ftui-process-reaper".to_owned())
        .spawn(move || match child.wait() {
            Ok(status) => {
                if let Some(control) = control {
                    control.confirm_reaped();
                }
                tracing::debug!(sub_id, ?status, "background child reaping completed");
            }
            Err(error) => {
                tracing::warn!(sub_id, %error, "background child reaping failed");
            }
        })
        .map(|_| ())
}

impl<M: Send + 'static> ProcessSubscription<M> {
    fn computed_id(&self) -> SubId {
        let mut h = DefaultHasher::new();
        "ProcessSubscription".hash(&mut h);
        self.program.hash(&mut h);
        self.args.hash(&mut h);
        self.env.hash(&mut h);
        self.timeout
            .map(|duration| duration.as_nanos())
            .hash(&mut h);
        self.input.as_ref().map(|input| input.id).hash(&mut h);
        self.control
            .as_ref()
            .map(ProcessControl::generation)
            .hash(&mut h);
        self.partial_output.hash(&mut h);
        self.pty.hash(&mut h);
        h.finish()
    }

    fn refresh_id(&mut self) {
        if !self.explicit_id {
            self.id = self.computed_id();
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
        let mut subscription = Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            timeout: None,
            input: None,
            control: None,
            partial_output: false,
            pty: false,
            id: 0,
            explicit_id: false,
            make_msg: std::sync::Arc::new(make_msg),
        };
        subscription.refresh_id();
        subscription
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

    /// Connect bounded input for this process run.
    ///
    /// Reuse clones of the same handle when rebuilding subscriptions. A new
    /// handle changes the automatic subscription ID. An already-used handle
    /// produces an error before spawning; use a new handle for a restart.
    #[must_use]
    pub fn stdin(mut self, input: ProcessInput) -> Self {
        self.input = Some(input);
        self.refresh_id();
        self
    }

    /// Attach supervisor-owned interrupt control for this run.
    ///
    /// Clones preserve the automatic subscription ID. A fresh handle changes
    /// it, including when stdin is disabled. A used handle cannot spawn again.
    #[must_use]
    pub fn control(mut self, control: ProcessControl) -> Self {
        self.control = Some(control);
        self.refresh_id();
        self
    }

    /// Preview pending stdout/stderr lines before LF or EOF (default: false).
    ///
    /// Each nonempty preview replaces the prior preview for that stream. It
    /// contains the original cumulative valid UTF-8 prefix, including controls;
    /// sanitize the whole prefix when rendering, rather than individual pieces.
    /// An incomplete UTF-8 scalar and one possible CRLF delimiter are retained
    /// until resolved. Completed [`ProcessEvent::Stdout`] and
    /// [`ProcessEvent::Stderr`] events remain unchanged, including trailing CR
    /// at EOF. Clear previews on completed lines and terminal events.
    ///
    /// Previews share the bounded, interruptible output queue and the 64 KiB
    /// line bound; they can be delayed by backpressure. Earlier prefixes may
    /// already be visible when a later read fails UTF-8 or size validation.
    /// This does not force an unflushed child to write or provide arbitrary
    /// binary streaming. Unix pipe cancellation is independent of this setting;
    /// other targets can still block in OS reads. Changing this setting changes
    /// the automatic subscription ID.
    #[must_use]
    pub fn partial_output(mut self, enabled: bool) -> Self {
        self.partial_output = enabled;
        self.refresh_id();
        self
    }

    /// Run the child on a pseudo-terminal instead of pipes (Unix; default: false).
    ///
    /// The child's stdout and stderr are the PTY, so `isatty` holds and
    /// programs keep their terminal behavior: color, line-buffered output,
    /// progress lines. Both streams arrive as [`ProcessEvent::Stdout`] and
    /// [`ProcessEvent::StdoutPartial`], and standalone carriage returns (`\r`)
    /// rewind the active line so TTY progress updates replace the current
    /// preview rather than accumulating across frames. With
    /// [`stdin`](Self::stdin), stdin is the PTY as well, each line is written
    /// as typed input (chunked with canonical `VEOF` flushes so lines up to
    /// [`MAX_PROCESS_LINE_BYTES`] are not truncated by the kernel's 4095-byte
    /// canonical buffer), and closing input sends one end-of-file (Ctrl-D).
    /// Without it, stdin is `/dev/null`. Echo is off and LF is not translated
    /// to CRLF; a child may change that.
    ///
    /// The child leads a new session with no controlling terminal, so it
    /// cannot reach the host terminal through `/dev/tty`. Its process group
    /// belongs to the run: [`ProcessControl::request_interrupt`] sends SIGINT
    /// to the whole group, as Ctrl-C would. When the leader exits, and before
    /// it is reaped, every process left in the group receives SIGKILL, which
    /// also lets output reach end-of-file. Cancellation and timeout kill the
    /// group the same way. A descendant that starts its own session or group
    /// is not followed.
    ///
    /// The PTY starts at the size given by
    /// [`ProcessControl::request_resize`] before spawn, else at the host
    /// terminal's size (stdout, then stdin), else 80x24. Off Unix the run ends
    /// with [`ProcessEvent::Error`] before spawning. Changing this setting
    /// changes the automatic subscription ID.
    #[must_use]
    pub fn pty(mut self, enabled: bool) -> Self {
        self.pty = enabled;
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
        let _control_guard = if let Some(control) = &self.control {
            let Some(guard) = control.claim() else {
                let _ = send_terminal_message(
                    &sender,
                    &stop,
                    self.timeout.map(|timeout| spawn_start + timeout),
                    (self.make_msg)(ProcessEvent::Error(
                        "control handle was already used; create fresh control for a new run"
                            .to_owned(),
                    )),
                );
                return;
            };
            Some(guard)
        } else {
            None
        };
        let input_run = if let Some(input) = &self.input {
            let Some(run) = input.claim() else {
                if let Some(control) = &self.control {
                    control.close();
                }
                let _ = send_terminal_message(
                    &sender,
                    &stop,
                    self.timeout.map(|timeout| spawn_start + timeout),
                    (self.make_msg)(ProcessEvent::Error(
                        "stdin handle was already used; create fresh input for a new run"
                            .to_owned(),
                    )),
                );
                return;
            };
            Some(run)
        } else {
            None
        };
        let (_input_guard, input_receiver) = input_run.unzip();

        let fail_before_spawn = |detail: String| {
            if let Some(control) = &self.control {
                control.close();
            }
            if let Some(input) = &self.input {
                input.close();
            }
            tracing::warn!(
                target: crate::telemetry_schema::TARGET_PROCESS,
                sub_id,
                program = %self.program,
                error = %detail,
                "process spawn failed"
            );
            let _ = send_terminal_message(
                &sender,
                &stop,
                self.timeout.map(|timeout| spawn_start + timeout),
                (self.make_msg.as_ref())(ProcessEvent::Error(detail)),
            );
        };

        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if input_receiver.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });

        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        #[cfg(target_os = "linux")]
        {
            std::os::linux::process::CommandExt::create_pidfd(&mut cmd, true);
        }

        #[cfg(unix)]
        let pty = if self.pty {
            let size = self
                .control
                .as_ref()
                .and_then(ProcessControl::take_resize)
                .unwrap_or_else(host_terminal_size);
            match prepare_pty(&mut cmd, size, input_receiver.is_some()) {
                Ok(pty) => Some(pty),
                Err(error) => {
                    fail_before_spawn(format!(
                        "Failed to open a PTY for '{}': {error}",
                        self.program
                    ));
                    return;
                }
            }
        } else {
            None
        };
        #[cfg(not(unix))]
        if self.pty {
            fail_before_spawn(format!(
                "Failed to spawn '{}': PTY mode requires Unix",
                self.program
            ));
            return;
        }

        let spawned = cmd.spawn();
        // The parent's copies of a PTY slave must close, or the PTY could never
        // report end-of-file once the child's side is gone.
        drop(cmd);
        let mut child = match spawned {
            Ok(c) => {
                tracing::debug!(
                    target: crate::telemetry_schema::TARGET_PROCESS,
                    sub_id,
                    program = %self.program,
                    args = ?self.args,
                    pty = self.pty,
                    spawn_us = spawn_start.elapsed().as_micros() as u64,
                    "process spawned"
                );
                c
            }
            Err(e) => {
                fail_before_spawn(format!("Failed to spawn '{}': {}", self.program, e));
                return;
            }
        };

        if let Some(control) = &self.control {
            control.started(child.id());
        }

        // A PTY run reads, writes, and resizes through the master, and owns
        // the child's process group. A pipe run uses the child's own pipes.
        #[cfg(unix)]
        let (pty_master, pty_output, pty_input, mut group) = match pty {
            Some(PtyEndpoints {
                master,
                output,
                input,
            }) => (
                Some(master),
                Some(output),
                input,
                Some(ProcessGroup::new(&child)),
            ),
            None => (None, None, None, None),
        };
        #[cfg(not(unix))]
        let mut group: Option<ProcessGroup> = None;

        let deadline = self.timeout.map(|t| web_time::Instant::now() + t);
        let make_msg_ref = std::sync::Arc::clone(&self.make_msg);
        // Use the cancellation token for cooperative stop coordination.
        let token = stop.cancellation_token().clone();
        let (reader_stop, reader_trigger) = StopSignal::new();
        let (input_stop, input_trigger) = StopSignal::new();
        let stop_input = || {
            if let Some(input) = &self.input {
                input.close();
            }
            input_trigger.stop();
        };
        let stop_process_io = || {
            if let Some(control) = &self.control {
                control.close();
            }
            reader_trigger.stop();
            stop_input();
        };
        let mut input_handle = input_receiver.and_then(|receiver| {
            #[cfg(unix)]
            if let Some(master) = pty_input {
                return Some(std::thread::spawn(move || {
                    write_process_input(master, receiver, input_stop, b"\x04")
                }));
            }
            child.stdin.take().map(|stdin| {
                std::thread::spawn(move || write_process_input(stdin, receiver, input_stop, b""))
            })
        });
        let readers = LineReaders {
            sender: sender.with_stop_signal(reader_stop.clone()),
            stop: reader_stop.clone(),
            partial_output: self.partial_output,
            cr_rewinds: self.pty,
            make_msg: std::sync::Arc::clone(&make_msg_ref),
        };
        let poll_interval = Duration::from_millis(50);
        #[cfg(unix)]
        let pty_stdout = pty_output
            .map(|output| readers.spawn(output, ProcessEvent::Stdout, ProcessEvent::StdoutPartial));
        #[cfg(not(unix))]
        let pty_stdout = None;
        let mut stdout_handle = pty_stdout.or_else(|| {
            child.stdout.take().map(|stdout| {
                readers.spawn(stdout, ProcessEvent::Stdout, ProcessEvent::StdoutPartial)
            })
        });
        let mut stderr_handle = child
            .stderr
            .take()
            .map(|stderr| readers.spawn(stderr, ProcessEvent::Stderr, ProcessEvent::StderrPartial));

        let mut child_wait_error = None;
        let mut final_event = loop {
            // Reader results bypass the model queue: a full queue must never
            // prevent supervising a child whose other stream has failed.
            if let Some(error) = take_reader_error(&mut stdout_handle, "stdout")
                .or_else(|| take_reader_error(&mut stderr_handle, "stderr"))
                .or_else(|| take_reader_error(&mut input_handle, "stdin write failed"))
            {
                stop_process_io();
                break output_failure_event(
                    &error,
                    &mut child,
                    &mut child_wait_error,
                    group.as_mut(),
                );
            }
            match try_wait_leader(&mut child, &mut child_wait_error, group.as_mut()) {
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
                    stop_process_io();
                    break output_failure_event(
                        &format!("wait error: {e}"),
                        &mut child,
                        &mut child_wait_error,
                        group.as_mut(),
                    );
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
                // Keep the child's pipes open until termination is observed.
                // Closing a reader first can turn our kill into SIGPIPE while
                // the child is blocked writing to a full model queue.
                let event = stopped_process_event(
                    "process timed out",
                    &mut child,
                    &mut child_wait_error,
                    group.as_mut(),
                );
                stop_process_io();
                break event;
            }

            if token.wait_timeout(poll_interval) {
                tracing::debug!(
                    target: crate::telemetry_schema::TARGET_PROCESS,
                    sub_id,
                    elapsed_ms = spawn_start.elapsed().as_millis() as u64,
                    reason = "cancellation",
                    "killing process"
                );
                let event = stopped_process_event(
                    "process canceled",
                    &mut child,
                    &mut child_wait_error,
                    group.as_mut(),
                );
                stop_process_io();
                break event;
            }
            if let Some(control) = &self.control {
                control.dispatch_interrupt(&child, group.as_ref());
            }
            #[cfg(unix)]
            if let (Some(master), Some(group), Some(control)) = (&pty_master, &group, &self.control)
                && let Some((cols, rows)) = control.take_resize()
                && let Err(error) = resize_pty(master, cols, rows)
                    .and_then(|()| group.signal(rustix::process::Signal::WINCH))
            {
                tracing::debug!(
                    target: crate::telemetry_schema::TARGET_PROCESS,
                    sub_id,
                    cols,
                    rows,
                    %error,
                    "PTY resize was not applied"
                );
            }
        };

        if let Some(control) = &self.control {
            control.close();
            // Closing admission is not proof of cleanup. Only an observed
            // reaped status permits a consumer to offer a safe restart.
            if matches!(
                try_wait_leader(&mut child, &mut child_wait_error, group.as_mut()),
                Ok(Some(_))
            ) {
                control.confirm_reaped();
            }
        }
        stop_input();

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
                    .or_else(|| take_reader_error(&mut input_handle, "stdin write failed"))
                {
                    reader_trigger.stop();
                    final_event = output_failure_event(
                        &error,
                        &mut child,
                        &mut child_wait_error,
                        group.as_mut(),
                    );
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
            let _ = join_reader_thread_bounded(handle, "stdout", sub_id, &reader_trigger);
        }
        if let Some(handle) = stderr_handle {
            let _ = join_reader_thread_bounded(handle, "stderr", sub_id, &reader_trigger);
        }
        if let Some(handle) = input_handle
            && let Some(error) = join_reader_thread_bounded(handle, "stdin", sub_id, &input_trigger)
            && matches!(
                final_event,
                ProcessEvent::Exited(_) | ProcessEvent::Signaled(_)
            )
        {
            final_event =
                output_failure_event(&error, &mut child, &mut child_wait_error, group.as_mut());
        }

        // Keep ownership when foreground cleanup could not confirm reaping.
        // A late wait can update restart eligibility, but the error already
        // observed by this run remains an error. Pipe runs do not manage
        // descendants; a PTY run sweeps its group before reaping the leader.
        match try_wait_leader(&mut child, &mut child_wait_error, group.as_mut()) {
            Ok(Some(_)) => {
                if let Some(control) = &self.control {
                    control.confirm_reaped();
                }
            }
            _ if child_wait_error.is_some() && !has_retained_process_handle(&child) => {
                tracing::warn!(
                    sub_id,
                    "further child waits refused: prior wait failed without a retained process handle"
                );
            }
            _ => {
                if let Err(error) = reap_child_in_background(child, self.control.clone(), sub_id) {
                    final_event = ProcessEvent::Error(format!(
                        "{final_event:?}; background reaper could not start: {error}; child may remain unreaped"
                    ));
                }
            }
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

fn join_reader_thread_bounded<E: std::fmt::Display + Send + 'static>(
    handle: std::thread::JoinHandle<Result<(), E>>,
    stream: &'static str,
    sub_id: SubId,
    reader_trigger: &StopTrigger,
) -> Option<String> {
    let start = Instant::now();
    while !handle.is_finished() {
        if start.elapsed() >= PROCESS_READER_JOIN_TIMEOUT {
            reader_trigger.stop();
            tracing::warn!(
                target: crate::telemetry_schema::TARGET_PROCESS,
                sub_id,
                stream,
                timeout_ms = PROCESS_READER_JOIN_TIMEOUT.as_millis() as u64,
                "process pipe worker did not exit within timeout; detaching"
            );
            detach_reader_join(handle, stream);
            return Some(format!(
                "{stream} worker did not stop; I/O may be incomplete"
            ));
        }
        std::thread::sleep(PROCESS_READER_JOIN_POLL);
    }
    match handle.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("{stream} I/O failed: {error}")),
        Err(_) => Some(format!("{stream} worker panicked")),
    }
}

fn detach_reader_join<E: Send + 'static>(
    handle: std::thread::JoinHandle<Result<(), E>>,
    stream: &'static str,
) {
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

    #[cfg(unix)]
    fn spawn_cleanup_probe() -> (
        std::process::Child,
        std::process::ChildStdin,
        io::BufReader<std::process::ChildStdout>,
    ) {
        let script = "import signal,sys\nsignal.alarm(5)\nprint('READY',flush=True)\nline=sys.stdin.readline()\nprint('RELEASED '+line.removesuffix('\\n'),flush=True)\nraise SystemExit(67)";
        let mut command = Command::new("python3");
        command
            .args(["-u", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        #[cfg(target_os = "linux")]
        std::os::linux::process::CommandExt::create_pidfd(&mut command, true);
        let mut child = command.spawn().expect("spawn actual cleanup probe");
        #[cfg(target_os = "linux")]
        std::os::linux::process::ChildExt::pidfd(&child)
            .expect("cleanup probe requires retained PID ownership");
        // Hold stdin outside Child so a background Child::wait cannot turn the
        // test's pending child into a completed child by implicitly closing it.
        let stdin = child.stdin.take().unwrap();
        let mut output = io::BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        assert_eq!(ready, "READY\n");
        (child, stdin, output)
    }

    #[test]
    #[cfg(unix)]
    fn cleanup_live_child_reports_killed_only_after_sigkill_reaping() {
        use std::os::unix::process::ExitStatusExt;

        for reason in ["process canceled", "process timed out"] {
            let (mut child, _stdin, mut output) = spawn_cleanup_probe();
            let mut wait_error = None;
            let start = Instant::now();
            let event = stopped_process_event(reason, &mut child, &mut wait_error, None);
            let elapsed = start.elapsed();
            let observed = child.try_wait().unwrap();
            let mut tail = String::new();
            output.read_to_string(&mut tail).unwrap();
            assert_eq!(event, ProcessEvent::Killed);
            assert!(elapsed < Duration::from_secs(2));
            assert_eq!(
                observed.unwrap().signal(),
                Some(rustix::process::Signal::KILL.as_raw())
            );
            assert!(tail.is_empty(), "the held input was never released");
        }
    }

    #[test]
    #[cfg(unix)]
    fn cleanup_already_exited_child_retains_status_instead_of_reporting_killed() {
        let (mut child, mut stdin, mut output) = spawn_cleanup_probe();
        let mut wait_error = None;
        stdin.write_all(b"release\n").unwrap();
        drop(stdin);
        let mut tail = String::new();
        output.read_to_string(&mut tail).unwrap();
        let status = reap_child_until(
            &mut child,
            Instant::now() + Duration::from_secs(2),
            &mut wait_error,
        )
        .unwrap()
        .expect("actual natural exit");
        assert_eq!(tail, "RELEASED release\n");
        assert_eq!(status.code(), Some(67));
        let cleanup = cleanup_child(&mut child, &mut wait_error, None);
        assert_eq!(cleanup.status.unwrap().code(), Some(67));
        assert!(!cleanup.kill_sent);
        assert_eq!(cleanup.detail, "child reaped: Exited(67)");
        assert_eq!(
            stopped_process_event("process canceled", &mut child, &mut wait_error, None),
            ProcessEvent::Error(
                "process canceled; child reaped: Exited(67); stdout/stderr output is incomplete"
                    .to_owned()
            )
        );
    }

    #[test]
    #[cfg(unix)]
    fn cleanup_expired_reap_deadline_leaves_live_child_unconfirmed() {
        let (mut child, _stdin, _output) = spawn_cleanup_probe();
        let mut wait_error = None;
        let control = ProcessControl::new();
        let _guard = control.claim().unwrap();
        control.started(child.id());
        control.close();
        let start = Instant::now();
        let result = reap_child_until(&mut child, start, &mut wait_error);
        let elapsed = start.elapsed();
        let still_running = child.try_wait();
        let status_before_cleanup = control.status();
        // This exercises an explicitly expired helper deadline on a healthy
        // child, not an unkillable kernel task. Capture the refusal first;
        // the following explicit cleanup must not count as foreground proof.
        let cleanup = cleanup_child(&mut child, &mut wait_error, None);
        assert!(matches!(result, Ok(None)));
        assert!(matches!(still_running, Ok(None)));
        assert!(elapsed < Duration::from_secs(1));
        assert!(status_before_cleanup.closed);
        assert!(!status_before_cleanup.can_restart);
        assert!(cleanup.kill_sent);
        assert!(cleanup.status.is_some());
        assert!(!control.status().can_restart);
    }

    #[test]
    #[cfg(unix)]
    fn cleanup_background_reaper_confirms_only_after_real_stdin_release() {
        let (child, mut stdin, mut output) = spawn_cleanup_probe();
        let pid = rustix::process::Pid::from_child(&child);
        let control = ProcessControl::new();
        let _guard = control.claim().unwrap();
        control.started(child.id());
        control.close();
        let start = Instant::now();
        reap_child_in_background(child, Some(control.clone()), 991).unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        // The real child's read cannot finish while this test retains stdin.
        thread::sleep(Duration::from_millis(30));
        assert!(control.status().closed);
        assert!(!control.status().can_restart);
        stdin.write_all(b"release\n").unwrap();
        drop(stdin);
        let mut tail = String::new();
        output.read_to_string(&mut tail).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !control.status().can_restart && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(tail, "RELEASED release\n");
        assert_eq!(
            control.status(),
            ProcessControlStatus {
                pid: None,
                closed: true,
                can_restart: true,
                interrupt: ProcessInterruptStatus::Idle,
            }
        );
        // Independently confirm that the reaper consumed the actual child's
        // exit status; an early can_restart update must not hide a zombie.
        let subsequent_wait =
            rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG);
        assert_eq!(subsequent_wait.unwrap_err(), rustix::io::Errno::CHILD);
        assert_eq!(
            control.request_interrupt(),
            Err(ProcessControlError::Closed)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cleanup_externally_reaped_child_uses_retained_handle_without_false_killed() {
        use rustix::process::{Pid, WaitOptions, waitpid};

        let (mut child, mut stdin, mut output) = spawn_cleanup_probe();
        let mut wait_error = None;
        let pid = Pid::from_child(&child);
        let control = ProcessControl::new();
        let _guard = control.claim().unwrap();
        control.started(child.id());
        control.close();
        stdin.write_all(b"release\n").unwrap();
        drop(stdin);
        let deadline = Instant::now() + Duration::from_secs(2);
        let externally_reaped = loop {
            match waitpid(Some(pid), WaitOptions::NOHANG) {
                Ok(Some(observed)) => break observed,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                other => {
                    // Retained pidfd keeps this failure cleanup safe even if
                    // the exact child was reaped outside std::Child.
                    let cleanup = cleanup_child(&mut child, &mut wait_error, None);
                    panic!("external exact-child wait failed: {other:?}; {cleanup:?}");
                }
            }
        };
        let mut tail = String::new();
        output.read_to_string(&mut tail).unwrap();
        assert_eq!(tail, "RELEASED release\n");
        assert_eq!(externally_reaped.0, pid);
        assert_eq!(externally_reaped.1.exit_status(), Some(67));
        assert!(has_retained_process_handle(&child));
        match try_wait_child(&mut child, &mut wait_error) {
            Ok(Some(status)) => {
                // Newer kernels let std recover the reaped child's status
                // through the retained pidfd. This is not ECHILD evidence.
                eprintln!("retained pidfd recovered exit67; ECHILD branch not exercised");
                assert_eq!(status.code(), Some(67));
                assert!(wait_error.is_none());
                let cleanup = cleanup_child(&mut child, &mut wait_error, None);
                assert_eq!(cleanup.status.unwrap().code(), Some(67));
                assert!(!cleanup.kill_sent);
                assert_eq!(cleanup.detail, "child reaped: Exited(67)");
                assert_eq!(
                    stopped_process_event("process canceled", &mut child, &mut wait_error, None),
                    ProcessEvent::Error(
                        "process canceled; child reaped: Exited(67); stdout/stderr output is incomplete"
                            .to_owned()
                    )
                );
            }
            Err(error) => {
                eprintln!("retained pidfd could not recover exit67; checking actual ECHILD");
                assert_eq!(
                    error.raw_os_error(),
                    Some(rustix::io::Errno::CHILD.raw_os_error())
                );
                let error_text = error.to_string();
                assert_eq!(wait_error.as_deref(), Some(error_text.as_str()));
                let cleanup = cleanup_child(&mut child, &mut wait_error, None);
                let kill_error =
                    io::Error::from_raw_os_error(rustix::io::Errno::SRCH.raw_os_error());
                let expected = format!(
                    "child wait failed: {error}; child kill failed: {kill_error}; reaping failed: {error}; child may still be running or unreaped"
                );
                assert!(cleanup.status.is_none());
                assert!(!cleanup.kill_sent);
                assert_eq!(cleanup.detail, expected);
                assert_eq!(
                    stopped_process_event("process canceled", &mut child, &mut wait_error, None),
                    ProcessEvent::Error(format!(
                        "process canceled; {expected}; stdout/stderr output is incomplete"
                    ))
                );
            }
            Ok(None) => panic!("the externally reaped child cannot still be running"),
        }
        assert!(!control.status().can_restart);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cleanup_sticky_wait_error_refuses_signal_and_repoll_without_retained_handle() {
        use rustix::process::{Pid, WaitOptions, waitpid};

        let script = "import signal,sys\nsignal.alarm(5)\nprint('READY',flush=True)\nline=sys.stdin.readline()\nprint('RELEASED '+line.removesuffix('\\n'),flush=True)\nraise SystemExit(67)";
        let mut command = Command::new("python3");
        command
            .args(["-u", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        std::os::linux::process::CommandExt::create_pidfd(&mut command, false);
        let mut child = command
            .spawn()
            .expect("spawn actual no-pidfd cleanup probe");
        assert!(!has_retained_process_handle(&child));
        let pid = Pid::from_child(&child);
        let child_id = child.id();
        let mut stdin = child.stdin.take().unwrap();
        let mut output = io::BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        assert_eq!(ready, "READY\n");
        let control = ProcessControl::new();
        let _guard = control.claim().unwrap();
        control.started(child_id);
        control.close();

        // Inject only the previously observed error state. The child remains
        // real and owned; this is not evidence of an actual OS wait failure.
        let saved_error = "injected prior wait failure";
        let mut wait_error = Some(saved_error.to_owned());
        let expected = format!(
            "child wait failed: {saved_error}; kill and further waits not attempted without a retained process handle; child may still be running or unreaped"
        );
        assert_eq!(
            try_wait_child(&mut child, &mut wait_error)
                .unwrap_err()
                .to_string(),
            saved_error
        );
        let cleanup = cleanup_child(&mut child, &mut wait_error, None);
        assert!(cleanup.status.is_none());
        assert!(!cleanup.kill_sent);
        assert_eq!(cleanup.detail, expected);
        assert_eq!(
            stopped_process_event("process canceled", &mut child, &mut wait_error, None),
            ProcessEvent::Error(format!(
                "process canceled; {expected}; stdout/stderr output is incomplete"
            ))
        );
        assert!(!control.status().can_restart);

        // Exact output after the refusal proves that cleanup did not signal
        // this live child. The test, not Child::wait, releases its stdin.
        stdin.write_all(b"release\n").unwrap();
        drop(stdin);
        let mut tail = String::new();
        output.read_to_string(&mut tail).unwrap();
        assert_eq!(tail, "RELEASED release\n");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = std::fs::read_to_string(format!("/proc/{child_id}/status")).unwrap();
            if status.lines().any(|line| line.starts_with("State:\tZ")) {
                break;
            }
            assert!(Instant::now() < deadline, "actual child did not exit");
            thread::sleep(Duration::from_millis(5));
        }
        // The now-waitable child makes a forbidden re-poll observable: only
        // the independent exact-child wait below may consume its exit status.
        assert_eq!(
            reap_child_until(&mut child, Instant::now(), &mut wait_error)
                .unwrap_err()
                .to_string(),
            saved_error
        );
        let cleanup = cleanup_child(&mut child, &mut wait_error, None);
        assert!(cleanup.status.is_none());
        assert!(!cleanup.kill_sent);
        assert_eq!(cleanup.detail, expected);
        assert_eq!(wait_error.as_deref(), Some(saved_error));
        let reaped = waitpid(Some(pid), WaitOptions::NOHANG)
            .unwrap()
            .expect("sticky helpers must leave the exact child waitable");
        assert_eq!(reaped.0, pid);
        assert_eq!(reaped.1.exit_status(), Some(67));
        assert!(!control.status().can_restart);
        assert_eq!(
            control.request_interrupt(),
            Err(ProcessControlError::Closed)
        );
    }

    #[test]
    fn control_generation_claim_and_descriptor_identity_are_single_run() {
        let control = ProcessControl::new();
        assert_eq!(control.generation(), control.clone().generation());
        assert_ne!(control.generation(), ProcessControl::new().generation());
        assert_eq!(
            control.status(),
            ProcessControlStatus {
                pid: None,
                closed: false,
                can_restart: true,
                interrupt: ProcessInterruptStatus::Idle,
            }
        );
        assert_eq!(
            control.request_interrupt(),
            Err(if cfg!(unix) {
                ProcessControlError::NotRunning
            } else {
                ProcessControlError::Unsupported
            })
        );
        let descriptor =
            || ProcessSubscription::new("child", |event| event).control(control.clone());
        let id = descriptor().id();
        assert_eq!(id, descriptor().id());
        assert_ne!(id, ProcessSubscription::new("child", |event| event).id());
        assert_ne!(
            id,
            ProcessSubscription::new("child", |event| event)
                .control(ProcessControl::new())
                .id()
        );
        assert_eq!(
            descriptor().with_id(71).control(ProcessControl::new()).id(),
            71
        );
        let guard = control.claim().expect("first claim");
        assert!(control.claim().is_none());
        drop(descriptor());
        assert!(
            !control.status().closed,
            "unused descriptors do not close the owner"
        );
        drop(guard);
        assert!(control.status().closed);
        assert!(control.status().can_restart, "no child was spawned");
        assert!(
            control.claim().is_none(),
            "closed generations cannot be reused"
        );
    }

    #[test]
    #[cfg(unix)]
    fn control_pending_close_preserves_uncertain_cleanup_and_completed_outcomes() {
        // These are state-machine assertions, not evidence of child execution.
        let control = ProcessControl::new();
        let guard = control.claim().unwrap();
        control.started(123);
        assert!(!control.status().can_restart);
        assert_eq!(control.request_interrupt(), Ok(()));
        assert_eq!(
            control.request_interrupt(),
            Err(ProcessControlError::AlreadyPending)
        );
        drop(guard);
        assert_eq!(
            control.status(),
            ProcessControlStatus {
                pid: None,
                closed: true,
                can_restart: false,
                interrupt: ProcessInterruptStatus::Canceled,
            }
        );
        assert_eq!(
            control.request_interrupt(),
            Err(ProcessControlError::Closed)
        );
        control.confirm_reaped();
        assert!(control.status().can_restart);
        assert_eq!(control.status().interrupt, ProcessInterruptStatus::Canceled);

        for outcome in [
            ProcessInterruptStatus::Sent,
            ProcessInterruptStatus::Failed("delivery failed".to_owned()),
        ] {
            let control = ProcessControl::new();
            let guard = control.claim().unwrap();
            control.started(123);
            control.state.lock().unwrap().status.interrupt = outcome.clone();
            drop(guard);
            assert_eq!(control.status().interrupt, outcome);
            assert!(!control.status().can_restart);
        }
    }

    #[cfg(unix)]
    fn controlled_python(
        script: &str,
        control: &ProcessControl,
    ) -> ProcessSubscription<ProcessEvent> {
        ProcessSubscription::new("python3", |event| event)
            .args(["-u", "-c", script])
            .control(control.clone())
            .timeout(Duration::from_secs(5))
    }

    #[test]
    #[cfg(unix)]
    fn real_control_interrupt_continues_stdin_and_duplicate_claim_keeps_owner() {
        let control = ProcessControl::new();
        let input = ProcessInput::new();
        let script = "import signal,sys\ncount=0\ndef interrupt(signum,frame):\n global count\n count+=1\n print('INT-ACK',flush=True)\n if count==2:\n  raise SystemExit(0)\nsignal.signal(signal.SIGINT,interrupt)\nprint('READY',flush=True)\nfor line in sys.stdin:\n print('reply='+line.removesuffix('\\n'),flush=True)\nprint('EOF',flush=True)\nwhile True:\n signal.pause()";
        let sub = controlled_python(script, &control).stdin(input.clone());
        let (sender, receiver) = mpsc::sync_channel(256);
        let (stop, trigger) = StopSignal::new();
        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ProcessEvent::Stdout("READY".to_owned())
        );
        let running = control.status();
        assert!(running.pid.is_some());
        assert!(!running.closed);
        assert!(!running.can_restart);

        let duplicate = controlled_python("raise SystemExit(99)", &control);
        let (duplicate_sender, duplicate_receiver) = mpsc::sync_channel(1);
        let (duplicate_stop, _duplicate_trigger) = StopSignal::new();
        duplicate.run(
            SubscriptionSender::new(duplicate_sender, duplicate_stop.clone()),
            duplicate_stop,
        );
        assert_eq!(
            duplicate_receiver.into_iter().collect::<Vec<_>>(),
            [ProcessEvent::Error(
                "control handle was already used; create fresh control for a new run".to_owned()
            )]
        );
        assert_eq!(
            control.status(),
            running,
            "duplicate claim must not close its owner"
        );

        control.request_interrupt().unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ProcessEvent::Stdout("INT-ACK".to_owned())
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.status().interrupt == ProcessInterruptStatus::Pending
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(control.status().interrupt, ProcessInterruptStatus::Sent);
        assert_eq!(control.status().pid, running.pid);
        input
            .try_send_line("still alive 🦀 $() \\".to_owned())
            .unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ProcessEvent::Stdout("reply=still alive 🦀 $() \\".to_owned())
        );
        input.close();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ProcessEvent::Stdout("EOF".to_owned())
        );
        assert_eq!(control.status().pid, running.pid, "EOF only closes stdin");
        assert!(!control.status().closed);
        control.request_interrupt().unwrap();
        let events: Vec<_> = (0..2)
            .map(|_| receiver.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        handle.join().unwrap();
        // All positive observations precede any test-requested stop.
        trigger.stop();
        assert_eq!(
            events,
            [
                ProcessEvent::Stdout("INT-ACK".to_owned()),
                ProcessEvent::Exited(0)
            ]
        );
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            control.status(),
            ProcessControlStatus {
                pid: None,
                closed: true,
                can_restart: true,
                interrupt: ProcessInterruptStatus::Sent,
            }
        );
        assert_eq!(
            control.request_interrupt(),
            Err(ProcessControlError::Closed)
        );
    }

    #[test]
    fn control_spawn_failure_closes_admission_without_claiming_a_child() {
        let control = ProcessControl::new();
        let sub =
            ProcessSubscription::new("/ftui-process-control-command-does-not-exist", |event| {
                event
            })
            .control(control.clone())
            .timeout(Duration::from_secs(2));
        let (sender, receiver) = mpsc::sync_channel(1);
        let (stop, _trigger) = StopSignal::new();
        sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
        let events: Vec<_> = receiver.into_iter().collect();
        assert!(
            matches!(events.as_slice(), [ProcessEvent::Error(error)] if error.starts_with("Failed to spawn"))
        );
        assert_eq!(
            control.status(),
            ProcessControlStatus {
                pid: None,
                closed: true,
                can_restart: true,
                interrupt: ProcessInterruptStatus::Idle,
            }
        );
        assert!(control.claim().is_none());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn control_interrupt_refuses_child_without_retained_pidfd() {
        let script = "import signal,sys\nsignal.alarm(5)\nprint('READY',flush=True)\nprint(sys.stdin.readline().removesuffix('\\n'),flush=True)";
        let mut command = Command::new("python3");
        command
            .args(["-u", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        std::os::linux::process::CommandExt::create_pidfd(&mut command, false);
        let mut child = command.spawn().unwrap();
        let mut output = io::BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        let result = interrupt_child(&child);
        let mut stdin = child.stdin.take().unwrap();
        let written = stdin.write_all(b"STILL-ALIVE\n");
        drop(stdin);
        let mut tail = String::new();
        let read = output.read_to_string(&mut tail);
        let status = child.wait().unwrap();
        // Refusal and the subsequent actual roundtrip are checked after natural
        // completion; no cleanup signal can manufacture successful survival.
        let error = result.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(
            error
                .to_string()
                .starts_with("retained pidfd unavailable; interrupt not sent:")
        );
        assert_eq!(ready, "READY\n");
        written.unwrap();
        read.unwrap();
        assert_eq!(tail, "STILL-ALIVE\n");
        assert!(status.success());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn real_control_interrupt_bypasses_full_output_and_blocked_input_queues() {
        let control = ProcessControl::new();
        let input = ProcessInput::new();
        let payload = "x".repeat(MAX_PROCESS_LINE_BYTES);
        for _ in 0..PROCESS_INPUT_CAPACITY {
            input.try_send_line(payload.clone()).unwrap();
        }
        let (observed_sender, observed_receiver) = mpsc::channel();
        let script = "import os,signal\ncount=0\ndef interrupt(signum,frame):\n global count\n count+=1\n if count==1:\n  os.write(2,b'INT-ACK\\n')\n else:\n  os._exit(73)\nsignal.signal(signal.SIGINT,interrupt)\nprint('READY '+str(os.getpid()),flush=True)\nfor index in range(257):\n print('out:'+str(index),flush=True)\nwhile True:\n signal.pause()";
        let sub = ProcessSubscription::new("python3", move |event| {
            // These observations precede model-channel admission. They prove
            // supervision while full, not delivery of the unaccepted suffix.
            if matches!(&event, ProcessEvent::Stdout(line) if line == "out:256")
                || matches!(&event, ProcessEvent::Stderr(line) if line == "INT-ACK")
                || matches!(&event, ProcessEvent::Error(_))
            {
                observed_sender.send(event.clone()).unwrap();
            }
            event
        })
        .args(["-u", "-c", script])
        .stdin(input.clone())
        .control(control.clone())
        .timeout(Duration::from_secs(5));
        let (sender, receiver) = mpsc::sync_channel(256);
        let (stop, trigger) = StopSignal::new();
        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
        });
        let ProcessEvent::Stdout(ready) = receiver.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("missing actual child readiness");
        };
        let pid: u32 = ready.strip_prefix("READY ").unwrap().parse().unwrap();
        assert_eq!(
            observed_receiver
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            ProcessEvent::Stdout("out:256".to_owned())
        );
        // The only stdout reader reached conversion of line 257 after putting
        // exactly 256 lines into the undrained channel. The child never reads
        // stdin; a maximum-sized write plus LF cannot finish in its pipe.
        let mut input_full = false;
        for _ in 0..PROCESS_INPUT_CAPACITY + 2 {
            match input.try_send_line(payload.clone()) {
                Ok(()) => {}
                Err(ProcessInputError::Full(line)) => {
                    assert_eq!(line, payload);
                    input_full = true;
                    break;
                }
                other => panic!("unexpected input admission: {other:?}"),
            }
        }
        assert!(input_full);
        let requested_at = Instant::now();
        control.request_interrupt().unwrap();
        assert_eq!(
            observed_receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            ProcessEvent::Stderr("INT-ACK".to_owned())
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.status().interrupt == ProcessInterruptStatus::Pending
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(control.status().interrupt, ProcessInterruptStatus::Sent);
        // This is a distinct request after the first handler acknowledgment.
        control.request_interrupt().unwrap();
        let terminal = observed_receiver.recv_timeout(Duration::from_secs(2));
        let status_before_drain = control.status();
        let elapsed_before_drain = requested_at.elapsed();
        let reaped_before_drain = std::fs::metadata(format!("/proc/{pid}"))
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
        // Release backpressure only after capturing the child and supervisor
        // observations. Cleanup cannot make those prior observations pass.
        let delivered: Vec<_> = receiver.into_iter().collect();
        handle.join().unwrap();
        trigger.stop();
        let terminal = terminal.unwrap();
        assert!(
            matches!(&terminal, ProcessEvent::Error(error)
                if error.starts_with("stdin write failed")
                    && error.contains("reaped:")
                    && error.contains("Exited(73)")),
            "pending stdin must report its actual SIGINT exit and incomplete I/O: {terminal:?}"
        );
        assert!(
            reaped_before_drain,
            "child still existed before model drain"
        );
        assert!(elapsed_before_drain < Duration::from_secs(2));
        assert_eq!(
            status_before_drain,
            ProcessControlStatus {
                pid: None,
                closed: true,
                can_restart: true,
                interrupt: ProcessInterruptStatus::Sent,
            }
        );
        let mut expected: Vec<_> = (0..256)
            .map(|index| ProcessEvent::Stdout(format!("out:{index}")))
            .collect();
        expected.push(terminal);
        assert_eq!(delivered, expected, "accepted FIFO prefix must survive");
        assert_eq!(
            control.request_interrupt(),
            Err(ProcessControlError::Closed)
        );
        assert_eq!(
            input.try_send_line("late".to_owned()),
            Err(ProcessInputError::Closed("late".to_owned()))
        );
    }

    #[test]
    #[cfg(unix)]
    fn real_control_fresh_generation_restarts_after_terminal_without_stale_interrupts() {
        use crate::subscription::SubscriptionManager;
        let mut manager = SubscriptionManager::new();
        let script = "import signal\ndef interrupt(signum,frame):\n print('INT-EXIT',flush=True)\n raise SystemExit(37)\nsignal.signal(signal.SIGINT,interrupt)\nprint('READY',flush=True)\nwhile True:\n signal.pause()";
        let receive = |manager: &SubscriptionManager<(u64, ProcessEvent)>, count: usize| {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut events = Vec::new();
            while events.len() < count && Instant::now() < deadline {
                events.extend(manager.drain_messages());
                thread::sleep(Duration::from_millis(5));
            }
            events
        };
        let mut previous: Option<(ProcessControl, SubId)> = None;
        for _ in 0..2 {
            let control = ProcessControl::new();
            let generation = control.generation();
            let descriptor = || {
                ProcessSubscription::new("python3", move |event| (generation, event))
                    .args(["-u", "-c", script])
                    .control(control.clone())
                    .timeout(Duration::from_secs(5))
            };
            let id = descriptor().id();
            if let Some((old, old_id)) = &previous {
                assert_ne!(old.generation(), generation);
                assert_ne!(*old_id, id);
            }
            // There is deliberately no intermediate reconcile([]): the fresh
            // generation must replace the finished, still-registered old ID.
            manager.reconcile(vec![Box::new(descriptor())]);
            assert_eq!(
                receive(&manager, 1),
                [(generation, ProcessEvent::Stdout("READY".to_owned()))]
            );
            let running = control.status();
            if let Some((old, _)) = &previous {
                assert_eq!(old.request_interrupt(), Err(ProcessControlError::Closed));
                assert_eq!(old.request_interrupt(), Err(ProcessControlError::Closed));
                assert_eq!(control.status(), running);
            }
            manager.reconcile(vec![Box::new(descriptor())]);
            assert_eq!(manager.active_count(), 1);
            assert_eq!(control.status(), running);
            control.request_interrupt().unwrap();
            assert_eq!(
                receive(&manager, 2),
                [
                    (generation, ProcessEvent::Stdout("INT-EXIT".to_owned())),
                    (generation, ProcessEvent::Exited(37)),
                ]
            );
            assert!(control.status().closed);
            assert!(control.status().can_restart);
            assert_eq!(control.status().interrupt, ProcessInterruptStatus::Sent);
            assert!(manager.drain_messages().is_empty());
            previous = Some((control, id));
        }
        manager.stop_all();
    }

    #[test]
    fn input_admission_bounds_payload_and_returns_rejected_text() {
        let input = ProcessInput::new();
        let mut overallocated = String::with_capacity(MAX_PROCESS_LINE_BYTES * 8);
        overallocated.push_str("tiny");
        input.try_send_line(overallocated).unwrap();
        for index in 1..PROCESS_INPUT_CAPACITY {
            input.try_send_line(index.to_string()).unwrap();
        }
        assert_eq!(
            input.try_send_line("retry 🦀".to_owned()),
            Err(ProcessInputError::Full("retry 🦀".to_owned()))
        );
        let oversized = "x".repeat(MAX_PROCESS_LINE_BYTES + 1);
        assert_eq!(
            input.try_send_line(oversized.clone()),
            Err(ProcessInputError::TooLong(oversized))
        );
        input.clone().close();
        assert_eq!(
            input.try_send_line("closed".to_owned()),
            Err(ProcessInputError::Closed("closed".to_owned()))
        );
        let (_guard, receiver) = input.claim().expect("close still permits first run");
        assert_eq!(receiver.recv().unwrap().into_string().capacity(), 4);
        for index in 1..PROCESS_INPUT_CAPACITY {
            assert_eq!(&*receiver.recv().unwrap(), index.to_string());
        }
        assert!(matches!(receiver.recv(), Err(mpsc::RecvError)));
        assert!(input.claim().is_none());
    }

    #[test]
    fn input_identity_and_claim_ownership_survive_descriptor_rebuilds() {
        let input = ProcessInput::new();
        let sub = || ProcessSubscription::new("sh", |event| event).stdin(input.clone());
        let id = sub().id();
        assert_eq!(
            sub().with_id(7).arg("one").stdin(ProcessInput::new()).id(),
            7
        );
        assert_eq!(id, sub().id());
        assert_ne!(id, ProcessSubscription::new("sh", |event| event).id());
        assert_ne!(
            id,
            ProcessSubscription::new("sh", |event| event)
                .stdin(ProcessInput::new())
                .id()
        );
        let (guard, receiver) = input.claim().unwrap();
        assert!(input.claim().is_none());
        drop(sub());
        input.try_send_line("still open".to_owned()).unwrap();
        assert_eq!(&*receiver.recv().unwrap(), "still open");
        drop(guard);
        assert_eq!(
            input.try_send_line("late".to_owned()),
            Err(ProcessInputError::Closed("late".to_owned()))
        );
    }

    #[cfg(unix)]
    fn input_process(script: &str, input: &ProcessInput) -> ProcessSubscription<ProcessEvent> {
        ProcessSubscription::new("sh", |event| event)
            .args(["-c", script])
            .stdin(input.clone())
            .timeout(Duration::from_secs(5))
    }

    #[test]
    #[cfg(unix)]
    fn real_input_roundtrip_eof_and_fresh_run_survive_reconciliation() {
        use crate::subscription::SubscriptionManager;
        let mut manager = SubscriptionManager::new();
        let script = "printf 'pid=%s\\n' \"$$\"; while IFS= read -r line; do printf '%s\\n' \"$line\"; done; printf 'EOF\\n'";
        let receive = |manager: &SubscriptionManager<ProcessEvent>, count: usize| {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut events = Vec::new();
            while events.len() < count && Instant::now() < deadline {
                events.extend(manager.drain_messages());
                thread::sleep(Duration::from_millis(5));
            }
            events
        };
        let mut previous_id = None;
        for _ in 0..2 {
            let input = ProcessInput::new();
            let id = input_process(script, &input).id();
            assert_ne!(previous_id, Some(id));
            previous_id = Some(id);
            manager.reconcile(vec![Box::new(input_process(script, &input))]);
            let started = receive(&manager, 1);
            assert!(
                matches!(started.as_slice(), [ProcessEvent::Stdout(line)] if line.starts_with("pid="))
            );
            for (text, expected) in [
                ("alpha", vec!["alpha"]),
                ("🦀 $() \\ literal", vec!["🦀 $() \\ literal"]),
                ("", vec![""]),
                ("embedded\nline", vec!["embedded", "line"]),
            ] {
                manager.reconcile(vec![Box::new(input_process(script, &input))]);
                input.try_send_line(text.to_owned()).unwrap();
                let expected: Vec<_> = expected
                    .into_iter()
                    .map(|line| ProcessEvent::Stdout(line.to_owned()))
                    .collect();
                assert_eq!(receive(&manager, expected.len()), expected);
            }
            input.close();
            assert_eq!(
                receive(&manager, 2),
                [
                    ProcessEvent::Stdout("EOF".to_owned()),
                    ProcessEvent::Exited(0)
                ]
            );
            assert_eq!(
                input.try_send_line("after EOF".to_owned()),
                Err(ProcessInputError::Closed("after EOF".to_owned()))
            );
            assert!(manager.drain_messages().is_empty());
        }
        manager.stop_all();
    }

    #[test]
    #[cfg(unix)]
    fn real_input_drains_full_queue_and_maximum_line_before_eof() {
        let input = ProcessInput::new();
        let mut expected = Vec::new();
        for index in 0..PROCESS_INPUT_CAPACITY {
            let line = if index == 0 {
                "x".repeat(MAX_PROCESS_LINE_BYTES)
            } else {
                index.to_string()
            };
            input.try_send_line(line.clone()).unwrap();
            expected.push(ProcessEvent::Stdout(line));
        }
        assert_eq!(
            input.try_send_line("overflow".to_owned()),
            Err(ProcessInputError::Full("overflow".to_owned()))
        );
        input.close();
        let sub = input_process("cat", &input);
        let (sender, receiver) = mpsc::sync_channel(256);
        let (stop, _trigger) = StopSignal::new();
        sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
        expected.push(ProcessEvent::Exited(0));
        assert_eq!(receiver.into_iter().collect::<Vec<_>>(), expected);
    }

    #[test]
    #[cfg(unix)]
    fn real_input_natural_exit_and_spawn_failure_close_open_ingress() {
        for program in ["sh", "/ftui-process-input-command-does-not-exist"] {
            let input = ProcessInput::new();
            let sub = ProcessSubscription::new(program, |event| event)
                .args(["-c", "exit 0"])
                .stdin(input.clone())
                .timeout(Duration::from_secs(5));
            let (sender, receiver) = mpsc::sync_channel(256);
            let (stop, _trigger) = StopSignal::new();
            sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
            let events: Vec<_> = receiver.into_iter().collect();
            if program == "sh" {
                assert_eq!(events, [ProcessEvent::Exited(0)]);
            } else {
                assert!(
                    matches!(events.as_slice(), [ProcessEvent::Error(error)] if error.starts_with("Failed to spawn"))
                );
            }
            assert_eq!(
                input.try_send_line("late".to_owned()),
                Err(ProcessInputError::Closed("late".to_owned()))
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn input_writer_reports_canceled_pending_line_instead_of_success() {
        let input = ProcessInput::new();
        input.try_send_line("pending".to_owned()).unwrap();
        let (_guard, receiver) = input.claim().unwrap();
        let mut child = Command::new("sh")
            .args(["-c", "exec sleep 60"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let (stop, trigger) = StopSignal::new();
        trigger.stop();
        let result = write_process_input(child.stdin.take().unwrap(), receiver, stop, b"");
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(
            result.unwrap_err().to_string(),
            "queued stdin was canceled before writing"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn real_input_blocked_pipe_does_not_block_admission_or_stop() {
        let input = ProcessInput::new();
        let sub = input_process("printf '%s\\n' \"$$\"; exec sleep 60", &input);
        let (sender, receiver) = mpsc::sync_channel(256);
        let (stop, trigger) = StopSignal::new();
        let handle =
            thread::spawn(move || sub.run(SubscriptionSender::new(sender, stop.clone()), stop));
        let ProcessEvent::Stdout(pid) = receiver.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("missing live child PID")
        };
        let pid: u32 = pid.parse().unwrap();
        let payload = "x".repeat(MAX_PROCESS_LINE_BYTES);
        let started = Instant::now();
        let mut full = false;
        for _ in 0..PROCESS_INPUT_CAPACITY + 2 {
            match input.try_send_line(payload.clone()) {
                Ok(()) => {}
                Err(ProcessInputError::Full(unsent)) => {
                    assert_eq!(unsent, payload);
                    full = true;
                    break;
                }
                other => panic!("unexpected admission: {other:?}"),
            }
        }
        let admission_elapsed = started.elapsed();
        // Give the worker time to enter a write exceeding the pipe capacity.
        thread::sleep(Duration::from_millis(100));
        let retry = input.try_send_line(payload);
        let before_stop = Instant::now();
        trigger.stop();
        handle.join().unwrap();
        let stopped_in = before_stop.elapsed();
        let reaped = std::fs::metadata(format!("/proc/{pid}"))
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
        assert!(full);
        assert!(admission_elapsed < Duration::from_secs(1));
        // One line can move from the queue to the blocked worker after Full.
        assert!(matches!(retry, Ok(()) | Err(ProcessInputError::Full(_))));
        assert!(stopped_in < Duration::from_secs(2));
        assert!(reaped, "child must be reaped before test cleanup");
        assert_eq!(
            input.try_send_line("late".to_owned()),
            Err(ProcessInputError::Closed("late".to_owned()))
        );
        assert_eq!(
            receiver.into_iter().collect::<Vec<_>>(),
            [ProcessEvent::Killed]
        );
    }

    #[cfg(target_os = "linux")]
    fn await_pipe_wait(task: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let state = std::fs::read_to_string(task.join("wchan")).unwrap();
            // Observe an actual kernel pipe/readiness wait before cancellation.
            // The blocking implementation reaches pipe_read/pipe_write; the
            // interruptible implementation reaches poll_schedule_timeout.
            if state.contains("poll") || state.contains("pipe_read") || state.contains("pipe_write")
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "worker never waited on its pipe: {state}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn real_output_workers_cancel_while_child_keeps_both_pipes_open() {
        // Seed the original blocking-pipe defect as a live negative control;
        // the same observer must reject completion before fixture cleanup.
        assert_output_worker_lifetime(false);
        assert_output_worker_lifetime(true);
    }

    #[cfg(target_os = "linux")]
    fn assert_output_worker_lifetime(nonblocking: bool) {
        let script = "import os,signal\nsignal.alarm(8)\nassert os.read(0,1)==b'R'\nassert all(os.get_blocking(fd) for fd in (0,1,2))\nos.write(1,b'out-prefix')\nos.write(2,b'err-prefix')\nsignal.pause()";
        let mut child = Command::new("python3")
            .args(["-u", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (stop, trigger) = StopSignal::new();
        let (sender, receiver) = mpsc::sync_channel(8);
        let (task_sender, task_receiver) = mpsc::channel();
        let start_worker = |pipe: Box<dyn ReadPipe>, stream: &'static str| {
            let stop = stop.clone();
            let sender = sender.clone();
            let task_sender = task_sender.clone();
            thread::spawn(move || {
                let pipe = process_pipe(pipe, stop.clone()).map_err(ProcessReadError::Io)?;
                if !nonblocking {
                    rustix::io::ioctl_fionbio(&pipe.pipe, false)
                        .map_err(|error| ProcessReadError::Io(error.into()))?;
                }
                task_sender
                    .send(std::fs::read_link("/proc/thread-self").unwrap())
                    .unwrap();
                forward_lines(
                    pipe,
                    SubscriptionSender::new(sender, stop.clone()),
                    stop,
                    true,
                    false,
                    |line| format!("{stream}:line:{line}"),
                    |prefix| format!("{stream}:partial:{prefix}"),
                )
            })
        };
        let stdout_worker = start_worker(Box::new(stdout), "out");
        let stderr_worker = start_worker(Box::new(stderr), "err");
        let tasks = [task_receiver.recv().unwrap(), task_receiver.recv().unwrap()];
        child.stdin.as_mut().unwrap().write_all(b"R").unwrap();
        let mut previews = [
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
        ];
        previews.sort();
        assert_eq!(
            previews,
            ["err:partial:err-prefix", "out:partial:out-prefix"]
        );
        for task in tasks {
            await_pipe_wait(&std::path::Path::new("/proc").join(task));
        }
        trigger.stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !(stdout_worker.is_finished() && stderr_worker.is_finished())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        let workers_finished_before_cleanup =
            stdout_worker.is_finished() && stderr_worker.is_finished();
        let child_alive_before_cleanup = child.try_wait().unwrap().is_none();
        child.kill().unwrap();
        child.wait().unwrap();
        stdout_worker.join().unwrap().unwrap();
        stderr_worker.join().unwrap().unwrap();
        assert_eq!(
            workers_finished_before_cleanup, nonblocking,
            "only nonblocking workers should finish before peer cleanup"
        );
        assert!(child_alive_before_cleanup);
        assert!(
            receiver.try_iter().next().is_none(),
            "cancellation must not complete the pending prefixes as EOF lines"
        );
    }

    #[cfg(target_os = "linux")]
    trait ReadPipe: Read + std::os::fd::AsFd + Send {}

    #[cfg(target_os = "linux")]
    impl<P: Read + std::os::fd::AsFd + Send> ReadPipe for P {}

    #[test]
    #[cfg(target_os = "linux")]
    fn real_input_worker_cancels_full_pipe_and_closes_it_before_child_exits() {
        use std::os::unix::process::ChildExt;

        let script = "import fcntl,os,signal,sys,time\nsignal.alarm(8)\nfcntl.fcntl(0,fcntl.F_SETPIPE_SZ,4096)\nready=False\ndef release(*args):\n global ready\n ready=True\nsignal.signal(signal.SIGUSR1,release)\nprint('READY',flush=True)\nwhile not ready:\n time.sleep(0.005)\ndata=sys.stdin.buffer.read()\nprint(str(len(data))+':'+str(data==b'x'*len(data))+':'+str(os.get_blocking(0)),flush=True)";
        let mut child = Command::new("python3")
            .args(["-u", "-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = io::BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        stdout.read_line(&mut ready).unwrap();
        assert_eq!(ready, "READY\n");
        let input = ProcessInput::new();
        input
            .try_send_line("x".repeat(MAX_PROCESS_LINE_BYTES))
            .unwrap();
        let (_guard, receiver) = input.claim().unwrap();
        let stdin = child.stdin.take().unwrap();
        let (stop, trigger) = StopSignal::new();
        let (task_sender, task_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            task_sender
                .send(std::fs::read_link("/proc/thread-self").unwrap())
                .unwrap();
            write_process_input(stdin, receiver, stop, b"")
        });
        await_pipe_wait(&std::path::Path::new("/proc").join(task_receiver.recv().unwrap()));
        trigger.stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let worker_finished_before_release = worker.is_finished();
        let child_alive_before_release = child.try_wait().unwrap().is_none();
        child
            .send_signal(rustix::process::Signal::USR1.as_raw())
            .unwrap();
        let result = worker.join().unwrap();
        let mut output = String::new();
        stdout.read_to_string(&mut output).unwrap();
        let status = child.wait().unwrap();
        assert!(
            worker_finished_before_release,
            "stdin worker depended on child reading or exiting"
        );
        assert!(child_alive_before_release);
        assert_eq!(
            result.unwrap_err().to_string(),
            "stdin write canceled before completion"
        );
        assert!(status.success());
        assert_eq!(
            output, "4096:True:True\n",
            "only the exact written prefix should arrive, without LF, duplication or changed child blocking mode"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn real_input_write_failure_reaps_child_despite_full_model_queue() {
        let input = ProcessInput::new();
        let (observed_tx, observed_rx) = mpsc::channel();
        let sub = ProcessSubscription::new("sh", move |event| {
            observed_tx.send(event.clone()).unwrap();
            event
        })
        .args(["-c", "exec 0<&-; printf '%s\\n' \"$$\"; exec sleep 60"])
        .stdin(input.clone())
        .timeout(Duration::from_secs(5));
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(ProcessEvent::Stdout("prefix".to_owned()))
            .unwrap();
        let (stop, trigger) = StopSignal::new();
        let handle =
            thread::spawn(move || sub.run(SubscriptionSender::new(sender, stop.clone()), stop));
        let ProcessEvent::Stdout(pid) = observed_rx.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("missing live PID")
        };
        let pid: u32 = pid.parse().unwrap();
        input
            .try_send_line("cannot reach closed pipe".to_owned())
            .unwrap();
        let terminal = observed_rx.recv_timeout(Duration::from_secs(5));
        let reaped = std::fs::metadata(format!("/proc/{pid}"))
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
        // Observe supervision/reaping before releasing model backpressure.
        assert_eq!(
            receiver.recv().unwrap(),
            ProcessEvent::Stdout("prefix".to_owned())
        );
        let delivered = receiver.recv_timeout(Duration::from_secs(5));
        trigger.stop();
        handle.join().unwrap();
        let terminal = terminal.unwrap();
        assert!(
            matches!(&terminal, ProcessEvent::Error(error) if error.starts_with("stdin write failed") && error.contains("reaped:")),
            "{terminal:?}"
        );
        assert_eq!(delivered.unwrap(), terminal);
        assert!(reaped, "writer error must reap child before model drains");
        assert_eq!(
            input.try_send_line("late".to_owned()),
            Err(ProcessInputError::Closed("late".to_owned()))
        );
        assert!(receiver.try_recv().is_err());
    }

    fn collect_lines(bytes: &[u8], read_capacity: usize) -> Result<Vec<String>, ProcessReadError> {
        let mut reader = io::BufReader::with_capacity(read_capacity, bytes);
        let mut pending = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
        let (stop, _trigger) = StopSignal::new();
        let mut lines = Vec::new();
        while let Some(line) = read_process_line(&mut reader, &mut pending, &stop, None)? {
            lines.push(line);
        }
        Ok(lines)
    }

    #[test]
    fn partial_output_identity_is_stable_and_distinct_from_line_only() {
        let plain = ProcessSubscription::new("cat", |event| event);
        let enabled = ProcessSubscription::new("cat", |event| event).partial_output(true);
        assert_ne!(plain.id(), enabled.id());
        assert_eq!(
            enabled.id(),
            ProcessSubscription::new("cat", |event| event)
                .partial_output(true)
                .id()
        );
        assert_eq!(plain.id(), enabled.partial_output(false).id());
        assert_eq!(
            ProcessSubscription::new("cat", |event| event)
                .with_id(42)
                .partial_output(true)
                .id(),
            42
        );
    }

    #[test]
    fn partial_prefixes_preserve_utf8_controls_and_crlf_across_read_boundaries() {
        let input = "\n🦀é\x1b]2;hidden\x07prompt>\r\ninside\rcarriage\nlast\r";
        for capacity in 1..=input.len() {
            let mut reader = io::BufReader::with_capacity(capacity, input.as_bytes());
            let mut pending = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
            let (stop, _trigger) = StopSignal::new();
            let mut complete = Vec::new();
            loop {
                let mut previews = Vec::new();
                let mut preview = |text: &str| {
                    previews.push(text.to_owned());
                    true
                };
                let line = read_process_line(&mut reader, &mut pending, &stop, Some(&mut preview))
                    .unwrap();
                let Some(line) = line else {
                    assert!(previews.is_empty());
                    break;
                };
                let mut length = 0;
                for prefix in previews {
                    assert!(!prefix.is_empty());
                    assert!(line.starts_with(&prefix));
                    assert!(prefix.len() > length);
                    length = prefix.len();
                    assert!(!prefix.contains('\u{fffd}'));
                }
                complete.push(line);
            }
            assert_eq!(
                complete,
                [
                    "",
                    "🦀é\x1b]2;hidden\x07prompt>",
                    "inside\rcarriage",
                    "last\r"
                ]
            );
        }
    }

    #[test]
    fn partial_prefix_validation_keeps_line_bounds_and_stop_before_another_read() {
        for delimiter in ["", "\n", "\r\n"] {
            let input = format!("{}{delimiter}", "x".repeat(MAX_PROCESS_LINE_BYTES));
            let mut reader = io::BufReader::with_capacity(8192, input.as_bytes());
            let mut pending = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
            let capacity = pending.capacity();
            let (stop, _trigger) = StopSignal::new();
            let mut largest = 0;
            let mut preview = |prefix: &str| {
                assert!(prefix.len() <= MAX_PROCESS_LINE_BYTES);
                largest = largest.max(prefix.len());
                true
            };
            let line = read_process_line(&mut reader, &mut pending, &stop, Some(&mut preview))
                .unwrap()
                .unwrap();
            assert_eq!(line.len(), MAX_PROCESS_LINE_BYTES);
            assert!(largest > 0);
            assert_eq!(pending.capacity(), capacity);
        }

        for input in [
            vec![b'x'; MAX_PROCESS_LINE_BYTES + 1],
            b"safe\xff".to_vec(),
            b"safe\xf0\x9f".to_vec(),
        ] {
            let mut reader = io::BufReader::with_capacity(8192, input.as_slice());
            let mut pending = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
            let (stop, _trigger) = StopSignal::new();
            let mut preview = |prefix: &str| {
                assert!(prefix.len() <= MAX_PROCESS_LINE_BYTES);
                assert!(!prefix.contains('\u{fffd}'));
                true
            };
            assert!(matches!(
                read_process_line(&mut reader, &mut pending, &stop, Some(&mut preview)),
                Err(ProcessReadError::LineTooLong | ProcessReadError::InvalidUtf8(_))
            ));
            assert!(pending.len() <= MAX_PROCESS_LINE_BYTES + 1);
        }

        let mut reader = io::BufReader::with_capacity(3, io::Cursor::new(b"pending forever"));
        let mut pending = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
        let (stop, trigger) = StopSignal::new();
        let mut preview = |prefix: &str| {
            assert_eq!(prefix, "pen");
            trigger.stop();
            false
        };
        assert_eq!(
            read_process_line(&mut reader, &mut pending, &stop, Some(&mut preview)).unwrap(),
            None
        );
        assert_eq!(reader.get_ref().position(), 3);
    }

    #[test]
    #[cfg(unix)]
    fn real_partial_prompt_precedes_reply_and_preserves_split_utf8_controls_and_crlf() {
        let input = ProcessInput::new();
        let control = ProcessControl::new();
        let script = r#"import os,signal,sys
signal.alarm(5)
os.write(1,b'prompt \xf0\x9f')
assert sys.stdin.readline() == 'utf8\n'
os.write(1,b'\xa6\x80 \x1b]')
assert sys.stdin.readline() == 'osc\n'
os.write(1,b'2;hidden\x07 ready>')
assert sys.stdin.readline() == 'answer\n'
os.write(1,b'\r')
os.write(2,b'CR-WRITTEN\n')
assert sys.stdin.readline() == 'lf\n'
os.write(1,b'\nnormal\r\n')
os.write(2,b'err>')
assert sys.stdin.readline() == 'err\n'
os.write(2,b'\r\n')
os.write(1,b'tail\r')
"#;
        let sub = controlled_python(script, &control)
            .stdin(input.clone())
            .partial_output(true);
        let (sender, receiver) = mpsc::sync_channel(256);
        let (stop, _trigger) = StopSignal::new();
        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
        });
        let mut previous_length = 0;
        for (expected, reply) in [
            ("prompt ", "utf8"),
            ("prompt 🦀 \x1b]", "osc"),
            ("prompt 🦀 \x1b]2;hidden\x07 ready>", "answer"),
        ] {
            loop {
                let ProcessEvent::StdoutPartial(prefix) =
                    receiver.recv_timeout(Duration::from_secs(3)).unwrap()
                else {
                    panic!("expected a pending stdout prefix before the reply");
                };
                assert!(expected.starts_with(&prefix));
                assert!(prefix.len() > previous_length);
                previous_length = prefix.len();
                if prefix == expected {
                    break;
                }
            }
            assert!(!control.status().closed);
            // The real child is waiting for this reply: no LF/EOF can unlock
            // the expected preview. Disabled previews fail at the first wait.
            input.try_send_line(reply.to_owned()).unwrap();
        }
        loop {
            match receiver.recv_timeout(Duration::from_secs(3)).unwrap() {
                ProcessEvent::StderrPartial(prefix) => assert!("CR-WRITTEN".starts_with(&prefix)),
                ProcessEvent::Stderr(line) => {
                    assert_eq!(line, "CR-WRITTEN");
                    break;
                }
                event => panic!("unexpected event while CR is held: {event:?}"),
            }
        }
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        input.try_send_line("lf".to_owned()).unwrap();
        let expected_stdout = ["prompt 🦀 \x1b]2;hidden\x07 ready>", "normal", "tail\r"];
        let mut stdout = Vec::new();
        loop {
            match receiver.recv_timeout(Duration::from_secs(3)).unwrap() {
                ProcessEvent::Stdout(line) => stdout.push(line),
                ProcessEvent::StdoutPartial(prefix) => {
                    assert!(expected_stdout[stdout.len()].starts_with(&prefix));
                }
                ProcessEvent::StderrPartial(prefix) => {
                    assert!("err>".starts_with(&prefix));
                    if prefix == "err>" {
                        break;
                    }
                }
                event => panic!("unexpected event before stderr reply: {event:?}"),
            }
        }
        input.try_send_line("err".to_owned()).unwrap();
        let mut stderr = Vec::new();
        loop {
            match receiver.recv_timeout(Duration::from_secs(3)).unwrap() {
                ProcessEvent::Stdout(line) => stdout.push(line),
                ProcessEvent::Stderr(line) => stderr.push(line),
                ProcessEvent::StdoutPartial(prefix) => {
                    assert!(expected_stdout[stdout.len()].starts_with(&prefix));
                    assert!(!prefix.ends_with('\r'));
                }
                ProcessEvent::Exited(code) => {
                    assert_eq!(code, 0);
                    break;
                }
                event => panic!("unexpected event after reply: {event:?}"),
            }
        }
        handle.join().unwrap();
        assert_eq!(stdout, expected_stdout);
        assert_eq!(stderr, ["err>"]);
        assert!(receiver.try_recv().is_err());
        assert!(control.status().can_restart);
    }

    #[test]
    #[cfg(unix)]
    fn real_partial_output_is_disabled_by_default_without_changing_completed_lines() {
        let input = ProcessInput::new();
        let control = ProcessControl::new();
        let script = "import os,signal,sys\nsignal.alarm(5)\nos.write(1,b'prompt>')\nos.write(2,b'READY\\n')\nassert sys.stdin.readline()=='reply\\n'\nos.write(1,b'\\r\\nnormal\\n')";
        let sub = controlled_python(script, &control).stdin(input.clone());
        let (sender, receiver) = mpsc::sync_channel(256);
        let (stop, _trigger) = StopSignal::new();
        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
            ProcessEvent::Stderr("READY".to_owned())
        );
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        input.try_send_line("reply".to_owned()).unwrap();
        let events: Vec<_> = (0..3)
            .map(|_| receiver.recv_timeout(Duration::from_secs(3)).unwrap())
            .collect();
        handle.join().unwrap();
        assert_eq!(
            events,
            [
                ProcessEvent::Stdout("prompt>".to_owned()),
                ProcessEvent::Stdout("normal".to_owned()),
                ProcessEvent::Exited(0)
            ]
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    #[cfg(unix)]
    fn real_partial_queue_backpressure_remains_interruptible() {
        let input = ProcessInput::new();
        let control = ProcessControl::new();
        let (observed_sender, observed_receiver) = mpsc::channel();
        let sub = ProcessSubscription::new("python3", move |event| {
            if let ProcessEvent::StdoutPartial(prefix) = &event {
                observed_sender.send(prefix.clone()).unwrap();
            }
            event
        })
        .args(["-u", "-c", "import os,signal,sys\nsignal.alarm(5)\nos.write(1,b'a')\nassert sys.stdin.readline()=='next\\n'\nos.write(1,b'b')\nsignal.pause()"])
        .stdin(input.clone())
        .control(control.clone())
        .partial_output(true)
        .timeout(Duration::from_secs(5));
        let (sender, receiver) = mpsc::sync_channel(1);
        let (stop, trigger) = StopSignal::new();
        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
        });
        // Do not drain the first admitted prefix. The second conversion is
        // observed before bounded queue admission, not as delivered output.
        assert_eq!(
            observed_receiver
                .recv_timeout(Duration::from_secs(3))
                .unwrap(),
            "a"
        );
        input.try_send_line("next".to_owned()).unwrap();
        assert_eq!(
            observed_receiver
                .recv_timeout(Duration::from_secs(3))
                .unwrap(),
            "ab"
        );
        let stopped = Instant::now();
        trigger.stop();
        handle.join().unwrap();
        assert!(stopped.elapsed() < Duration::from_secs(2));
        assert_eq!(
            receiver.into_iter().collect::<Vec<_>>(),
            [ProcessEvent::StdoutPartial("a".to_owned())]
        );
        assert!(control.status().can_restart);
    }

    #[test]
    #[cfg(unix)]
    fn real_partial_output_errors_preserve_bounds_and_never_report_success() {
        for (payload, reason) in [
            ("b'x'*65537", "line exceeds"),
            ("b'valid\\xff'", "invalid UTF-8"),
            ("b'valid\\xf0\\x9f'", "invalid UTF-8"),
        ] {
            let control = ProcessControl::new();
            let script = format!("import os,signal\nsignal.alarm(5)\nos.write(1,{payload})");
            let sub = controlled_python(&script, &control).partial_output(true);
            let (sender, receiver) = mpsc::sync_channel(256);
            let (stop, _trigger) = StopSignal::new();
            let handle = thread::spawn(move || {
                sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
            });
            loop {
                match receiver.recv_timeout(Duration::from_secs(3)).unwrap() {
                    ProcessEvent::StdoutPartial(prefix) => {
                        assert!(prefix.len() <= MAX_PROCESS_LINE_BYTES);
                        assert!(!prefix.contains('\u{fffd}'));
                    }
                    ProcessEvent::Error(error) => {
                        assert!(error.contains(reason), "{error}");
                        break;
                    }
                    event => panic!("invalid output must not succeed: {event:?}"),
                }
            }
            handle.join().unwrap();
            assert!(receiver.try_recv().is_err());
            assert!(control.status().can_restart);
        }
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
            read_process_line(&mut reader, &mut pending, &stop, None),
            Err(ProcessReadError::LineTooLong)
        ));
        assert!(pending.len() <= MAX_PROCESS_LINE_BYTES + 1);
        assert_eq!(pending.capacity(), capacity);
        let consumed = reader.get_ref().position();
        assert!(consumed <= (MAX_PROCESS_LINE_BYTES + 8192) as u64);
        trigger.stop();
        assert_eq!(
            read_process_line(&mut reader, &mut pending, &stop, None).unwrap(),
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
            read_process_line(&mut reader, &mut pending, &stop, None).unwrap(),
            Some("good".to_owned())
        );
        assert!(matches!(
            read_process_line(&mut reader, &mut pending, &stop, None),
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
    fn child_has_sigstop(pid: rustix::process::Pid) -> rustix::io::Result<bool> {
        use rustix::process::{Signal, WaitId, WaitIdOptions, waitid};

        // A ptraced child can report /proc state `t` for an unrelated syscall
        // stop. Require the actual SIGSTOP event, leaving it waitable so this
        // observer never takes child status away from the supervisor.
        Ok(waitid(
            WaitId::Pid(pid),
            WaitIdOptions::STOPPED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )?
        .is_some_and(|status| status.stopping_signal() == Some(Signal::STOP.as_raw())))
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn real_process_stop_oracle_rejects_running_child_without_reaping() {
        let (mut child, _stdin, _output) = spawn_cleanup_probe();
        let observed = child_has_sigstop(rustix::process::Pid::from_child(&child));
        let still_running = child.try_wait();
        // Retain observations before explicit cleanup; cleanup is not the
        // evidence that the stop oracle left a live child under its owner.
        let cleanup = cleanup_child(&mut child, &mut None, None);
        assert_eq!(observed, Ok(false), "a running child has no SIGSTOP event");
        assert!(matches!(still_running, Ok(None)));
        assert!(cleanup.kill_sent);
        assert_eq!(
            cleanup.status.map(process_exit_event),
            Some(ProcessEvent::Signaled(
                rustix::process::Signal::KILL.as_raw()
            ))
        );
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
        let wait_pid =
            rustix::process::Pid::from_raw(i32::try_from(pid).expect("Linux child PID fits i32"))
                .expect("child PID is nonzero");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let stopped = child_has_sigstop(wait_pid);
            if matches!(stopped, Ok(true)) {
                break;
            }
            if stopped.is_err() || Instant::now() >= deadline {
                trigger.stop();
                handle.join().expect("stop child that did not pause");
                panic!("child never reported SIGSTOP: {stopped:?}");
            }
            thread::sleep(Duration::from_millis(5));
        }
        let retained_stop = child_has_sigstop(wait_pid);
        if !matches!(retained_stop, Ok(true)) {
            trigger.stop();
            handle.join().expect("stop child whose status was consumed");
            panic!("SIGSTOP observation must leave status waitable: {retained_stop:?}");
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
        let (signal, _trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

        // The child exits by itself. Stopping after a fixed sleep raced that
        // exit and reported Killed under load.
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
            start.elapsed() < Duration::from_secs(10),
            "timeout should kill a quiet 60s process promptly without waiting for sleep 60"
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
        let (signal, _trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

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
        let (signal, _trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

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
        let (signal, _trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

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
        let (signal, _trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

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
        let (signal, _trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

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
        let (signal, _trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

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
    #[cfg(target_os = "linux")]
    fn real_process_cancel_kills_blocked_writer_before_closing_pipe() {
        assert_blocked_writer_is_killed(false);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn real_process_timeout_kills_blocked_writer_before_closing_pipe() {
        assert_blocked_writer_is_killed(true);
    }

    #[cfg(target_os = "linux")]
    fn assert_blocked_writer_is_killed(timeout: bool) {
        let (tx, rx) = stdmpsc::sync_channel(256);
        let expected: Vec<_> = (0..256)
            .map(|index| ProcessEvent::Stdout(format!("accepted-{index}")))
            .collect();
        for event in &expected {
            tx.send(event.clone()).expect("fill application queue");
        }
        let (observed_tx, observed_rx) = stdmpsc::channel();
        let (done_tx, done_rx) = stdmpsc::channel();
        // Restore SIGPIPE's default action so premature reader closure has an
        // observable terminal status. The child stays inside an actual blocking
        // pipe write while the reader waits for model queue capacity.
        let script = "import os,signal\nsignal.alarm(15)\nsignal.signal(signal.SIGPIPE,signal.SIG_DFL)\nprint(os.getpid(),flush=True)\nwhile True: os.write(1,b'x'*65536)";
        let mut sub = ProcessSubscription::new("python3", move |event| {
            observed_tx
                .send(event.clone())
                .expect("observe process callback before model backpressure");
            event
        })
        .args(["-u", "-c", script]);
        if timeout {
            sub = sub.timeout(Duration::from_secs(3));
        }
        let (signal, trigger) = StopSignal::new();
        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
            done_tx.send(()).expect("observe supervisor completion");
        });
        let first = observed_rx.recv_timeout(Duration::from_secs(5));
        let pid = match &first {
            Ok(ProcessEvent::Stdout(line)) => line.parse::<u32>().ok(),
            _ => None,
        };
        let Some(pid) = pid else {
            trigger.stop();
            handle.join().expect("stop child without PID");
            panic!("expected actual blocked writer PID, got {first:?}");
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let wait = std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
            if wait.contains("pipe_write") {
                break;
            }
            if Instant::now() >= deadline {
                trigger.stop();
                handle
                    .join()
                    .expect("stop child that did not block writing");
                panic!("child never entered a blocking pipe write: {wait:?}");
            }
            thread::sleep(Duration::from_millis(5));
        }
        if !timeout {
            trigger.stop();
        }
        let done = done_rx.recv_timeout(Duration::from_secs(5));
        // Cleanup precedes assertions even when termination failed. No model
        // queue slot is released until both the child and workers have stopped.
        trigger.stop();
        handle.join().expect("join blocked writer supervisor");
        assert!(done.is_ok(), "full queue blocked termination: {done:?}");
        assert_eq!(
            observed_rx.try_iter().collect::<Vec<_>>(),
            [ProcessEvent::Killed],
            "cancel and timeout must reap SIGKILL, not induce SIGPIPE or EOF"
        );
        assert!(
            std::fs::metadata(format!("/proc/{pid}"))
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound),
            "the immediate child must already be reaped"
        );
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            expected,
            "the entire previously accepted prefix survives without a drain"
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
    #[cfg(target_os = "linux")]
    fn real_descendant_output_pipes_close_after_leader_exit_before_fixture_release() {
        use rustix::process::{Pid, WaitOptions, set_child_subreaper, waitpid};
        use std::net::TcpListener;
        use std::path::PathBuf;

        const CHILD_ENV: &str = "FTUI_DESCENDANT_PIPE_ISOLATED_TEST";
        const TEST_NAME: &str = concat!(
            "process_subscription::tests::",
            "real_descendant_output_pipes_close_after_leader_exit_before_fixture_release"
        );
        let isolated_parent = std::env::var(CHILD_ENV)
            .ok()
            .and_then(|value| value.parse::<i32>().ok());
        let isolated_run = isolated_parent.is_some()
            && isolated_parent == rustix::process::getppid().map(|pid| pid.as_raw_nonzero().get())
            && std::env::args().skip(1).eq([
                "--exact",
                TEST_NAME,
                "--nocapture",
                "--test-threads=1",
            ]);
        if !isolated_run {
            // Subreaper state belongs only to this exact-test subprocess. The
            // ordinary test runner must not adopt or reap unrelated children.
            let mut isolated = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
                .env(CHILD_ENV, std::process::id().to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("spawn isolated descendant test");
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if let Some(status) = isolated.try_wait().unwrap() {
                    let mut report = String::new();
                    isolated
                        .stdout
                        .take()
                        .unwrap()
                        .read_to_string(&mut report)
                        .unwrap();
                    print!("{report}");
                    assert!(
                        status.success(),
                        "isolated descendant test failed: {status}"
                    );
                    assert!(
                        report.contains(&format!("test {TEST_NAME} ... ok"))
                            && report.contains("1 passed; 0 failed"),
                        "the exact isolated test must execute, not pass with zero selected tests"
                    );
                    return;
                }
                if Instant::now() >= deadline {
                    isolated.kill().expect("stop timed-out isolated test");
                    isolated.wait().expect("reap isolated test");
                    panic!("isolated descendant test exceeded its safety deadline");
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        // rustix 1.1.3 represents this boolean prctl argument as Option<Pid>.
        set_child_subreaper(Pid::from_raw(1)).expect("enable isolated fixture adoption");
        let baseline_tasks: Vec<_> = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        let await_output_workers = || {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let mut workers = Vec::new();
                for entry in std::fs::read_dir("/proc/self/task").unwrap() {
                    let task = entry.unwrap().path();
                    if baseline_tasks.contains(&task) {
                        continue;
                    }
                    let state = match std::fs::read_to_string(task.join("wchan")) {
                        Ok(state) => state,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => panic!("cannot inspect actual worker wait: {error}"),
                    };
                    if state.contains("poll") || state.contains("pipe_read") {
                        workers.push(task);
                    }
                }
                if workers.len() == 2 {
                    return workers;
                }
                assert!(
                    Instant::now() < deadline,
                    "expected two actual output pipe waits, found {workers:?}"
                );
                thread::sleep(Duration::from_millis(5));
            }
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let script = r#"import os,signal,socket,sys
signal.alarm(15)
gate_read,gate_write=os.pipe()
leader=os.getpid()
descendant=os.fork()
if descendant:
 os.close(gate_write)
 assert os.read(gate_read,1)==b'X'
 os._exit(42)
os.close(gate_read)
signal.alarm(15)
sock=socket.create_connection(('127.0.0.1',int(sys.argv[1])),timeout=5)
sock.sendall((str(leader)+' '+str(os.getpid())+' '+os.readlink('/proc/self/fd/1')+' '+os.readlink('/proc/self/fd/2')+'\n').encode())
assert sock.recv(1)==b'B'
sock.sendall(('BLOCKING '+str(os.get_blocking(1))+' '+str(os.get_blocking(2))+'\n').encode())
os.write(1,b'accepted-out\nheld-out')
os.write(2,b'accepted-err\nheld-err')
assert sock.recv(1)==b'X'
os.write(gate_write,b'X')
os.close(gate_write)
assert sock.recv(1)==b'R'
closed=[]
for fd in (1,2):
 try:
  os.write(fd,b'probe')
  closed.append(False)
 except BrokenPipeError:
  closed.append(True)
sock.sendall(('CLOSED '+str(closed[0])+' '+str(closed[1])+'\n').encode())
assert sock.recv(1)==b'F'
os._exit(7)
"#;
        let control = ProcessControl::new();
        let sub = ProcessSubscription::new("python3", TestMsg::Proc)
            .args(["-u", "-c", script])
            .arg(listener.local_addr().unwrap().port().to_string())
            .control(control.clone());
        let (sender, receiver) = stdmpsc::sync_channel(256);
        let (stop, trigger) = StopSignal::new();
        let supervisor = thread::spawn(move || {
            sub.run(SubscriptionSender::new(sender, stop.clone()), stop);
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "descendant did not connect");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("descendant connection failed: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut socket = io::BufReader::new(socket);
        let mut hello = String::new();
        socket.read_line(&mut hello).unwrap();
        let fields: Vec<_> = hello.split_whitespace().collect();
        assert_eq!(fields.len(), 4, "actual descendant identities and pipe IDs");
        let leader: u32 = fields[0].parse().unwrap();
        let descendant = Pid::from_raw(fields[1].parse().unwrap()).unwrap();
        let pipe_ids = [PathBuf::from(fields[2]), PathBuf::from(fields[3])];
        assert_ne!(pipe_ids[0], pipe_ids[1]);
        assert!(fields[2..].iter().all(|pipe| pipe.starts_with("pipe:[")));
        // A real read/readiness wait means both adapters have been configured.
        // Only then ask the descendant to inspect its own endpoint flags.
        await_output_workers();
        socket.get_mut().write_all(b"B").unwrap();
        let mut flags = String::new();
        socket.read_line(&mut flags).unwrap();
        assert_eq!(flags, "BLOCKING True True\n");
        let mut accepted = [
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
        ];
        accepted.sort_by_key(|event| matches!(event, TestMsg::Proc(ProcessEvent::Stderr(_))));
        assert_eq!(
            accepted,
            [
                TestMsg::Proc(ProcessEvent::Stdout("accepted-out".to_owned())),
                TestMsg::Proc(ProcessEvent::Stderr("accepted-err".to_owned())),
            ]
        );
        let workers = await_output_workers();
        socket.get_mut().write_all(b"X").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !control.status().can_restart {
            assert!(Instant::now() < deadline, "leader was not reaped");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            std::fs::metadata(format!("/proc/{leader}"))
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        );
        assert!(
            waitpid(Some(descendant), WaitOptions::NOHANG)
                .unwrap()
                .is_none()
        );
        assert!(workers.iter().all(|task| task.exists()));
        trigger.stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !supervisor.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let supervisor_finished = supervisor.is_finished();
        let workers_gone = workers.iter().all(|task| {
            std::fs::metadata(task).is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        });
        let mut retained_pipes = Vec::new();
        for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
            match std::fs::read_link(entry.unwrap().path()) {
                Ok(target) if pipe_ids.contains(&target) => retained_pipes.push(target),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("cannot inspect retained pipe endpoint: {error}"),
            }
        }
        let descendant_alive = waitpid(Some(descendant), WaitOptions::NOHANG)
            .unwrap()
            .is_none();
        // Capture worker/FD state before any fixture release or child cleanup.
        // R only probes the still-live descendant's endpoints; F permits exit.
        socket.get_mut().write_all(b"R").unwrap();
        let mut closed = String::new();
        socket.read_line(&mut closed).unwrap();
        let alive_after_probe = waitpid(Some(descendant), WaitOptions::NOHANG)
            .unwrap()
            .is_none();
        socket.get_mut().write_all(b"F").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some((pid, status)) = waitpid(Some(descendant), WaitOptions::NOHANG).unwrap() {
                assert_eq!(pid, descendant);
                break status;
            }
            assert!(Instant::now() < deadline, "adopted descendant did not exit");
            thread::sleep(Duration::from_millis(5));
        };
        supervisor.join().unwrap();
        assert!(
            supervisor_finished,
            "supervision depended on fixture release"
        );
        assert!(workers_gone, "output workers survived canceled pipe reads");
        assert!(
            retained_pipes.is_empty(),
            "retained endpoints: {retained_pipes:?}"
        );
        assert!(descendant_alive && alive_after_probe);
        assert_eq!(closed, "CLOSED True True\n");
        assert_eq!(status.exit_status(), Some(7));
        assert_eq!(
            waitpid(Some(descendant), WaitOptions::NOHANG).unwrap_err(),
            rustix::io::Errno::CHILD
        );
        assert!(
            std::fs::metadata(format!("/proc/{}", descendant.as_raw_nonzero()))
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        );
        assert_eq!(
            receiver.try_iter().collect::<Vec<_>>(),
            [TestMsg::Proc(ProcessEvent::Error(
                "output drain canceled after Exited(42); stdout/stderr output is incomplete"
                    .to_owned()
            ))],
            "accepted lines stay exact; pending prefixes must not become EOF lines"
        );
    }

    #[test]
    fn stop_signal_does_not_block_when_background_descendant_keeps_pipes_open() {
        let sub = ProcessSubscription::new("sh", TestMsg::Proc)
            .arg("-c")
            .arg("sleep 60 & echo ready; sleep 60");
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, trigger) = StopSignal::new();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

        let mut msgs = Vec::new();
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < ready_deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(msg) => {
                    let is_ready =
                        matches!(&msg, TestMsg::Proc(ProcessEvent::Stdout(s)) if s == "ready");
                    msgs.push(msg);
                    if is_ready {
                        break;
                    }
                }
                Err(stdmpsc::RecvTimeoutError::Timeout) => {}
                Err(stdmpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        let stop_start = web_time::Instant::now();
        trigger.stop();
        handle.join().unwrap();

        assert!(
            stop_start.elapsed() < Duration::from_secs(10),
            "stop should not block behind inherited stdout/stderr pipes"
        );

        msgs.extend(rx.try_iter());
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
            .timeout(Duration::from_millis(200));
        let (tx, rx) = stdmpsc::sync_channel(256);
        let (signal, _trigger) = StopSignal::new();
        let start = web_time::Instant::now();

        let handle = thread::spawn(move || {
            sub.run(SubscriptionSender::new(tx, signal.clone()), signal);
        });

        handle.join().unwrap();

        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timeout should not block behind inherited stdout/stderr pipes"
        );

        let msgs: Vec<TestMsg> = rx.try_iter().collect();
        assert!(
            msgs.iter()
                .any(|m| matches!(m, TestMsg::Proc(ProcessEvent::Killed))),
            "expected Killed event, got: {msgs:?}"
        );
    }

    #[cfg(unix)]
    fn shell(script: &str) -> ProcessSubscription<ProcessEvent> {
        ProcessSubscription::new("sh", |event| event)
            .args(["-c", script])
            .timeout(Duration::from_secs(10))
    }

    #[cfg(unix)]
    fn pty_shell(script: &str) -> ProcessSubscription<ProcessEvent> {
        shell(script).pty(true)
    }

    #[cfg(unix)]
    fn start_process(
        sub: ProcessSubscription<ProcessEvent>,
    ) -> (
        stdmpsc::Receiver<ProcessEvent>,
        StopTrigger,
        thread::JoinHandle<()>,
    ) {
        let (sender, events) = stdmpsc::sync_channel(16 * 1024);
        let (stop, trigger) = StopSignal::new();
        let handle =
            thread::spawn(move || sub.run(SubscriptionSender::new(sender, stop.clone()), stop));
        (events, trigger, handle)
    }

    #[cfg(unix)]
    fn is_terminal_event(event: &ProcessEvent) -> bool {
        matches!(
            event,
            ProcessEvent::Exited(_)
                | ProcessEvent::Signaled(_)
                | ProcessEvent::Killed
                | ProcessEvent::Error(_)
        )
    }

    /// Events up to and including the first that satisfies `done`.
    #[cfg(unix)]
    fn events_until(
        events: &stdmpsc::Receiver<ProcessEvent>,
        done: impl Fn(&ProcessEvent) -> bool,
    ) -> Vec<ProcessEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(event) => {
                    let finished = done(&event);
                    seen.push(event);
                    if finished {
                        return seen;
                    }
                }
                Err(error) => panic!("expected event did not arrive ({error:?}); saw {seen:?}"),
            }
        }
    }

    #[cfg(unix)]
    fn run_to_end(sub: ProcessSubscription<ProcessEvent>) -> Vec<ProcessEvent> {
        let (events, _trigger, handle) = start_process(sub);
        let seen = events_until(&events, is_terminal_event);
        handle.join().expect("supervisor finished");
        seen
    }

    #[cfg(unix)]
    fn stdout(lines: &[&str]) -> Vec<ProcessEvent> {
        lines
            .iter()
            .map(|line| ProcessEvent::Stdout((*line).to_owned()))
            .collect()
    }

    /// The PID printed as `<prefix><pid>` in `event`.
    #[cfg(target_os = "linux")]
    fn printed_pid(event: &ProcessEvent, prefix: &str) -> i32 {
        match event {
            ProcessEvent::Stdout(line) => line
                .strip_prefix(prefix)
                .and_then(|pid| pid.parse().ok())
                .unwrap_or_else(|| panic!("expected {prefix}<pid>, got {line:?}")),
            other => panic!("expected {prefix}<pid>, got {other:?}"),
        }
    }

    /// A process that exited, or was killed and awaits reaping, is gone.
    #[cfg(target_os = "linux")]
    fn process_is_gone(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
    }

    #[cfg(target_os = "linux")]
    fn wait_until_gone(pid: i32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if process_is_gone(pid) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    #[cfg(unix)]
    fn pty_child_sees_a_terminal_on_stdout_and_stderr_and_lf_stays_lf() {
        let script = "for fd in 0 1 2; do if [ -t $fd ]; then echo \"fd$fd=tty\"; \
                      else echo \"fd$fd=other\"; fi; done; echo to-stderr >&2; printf 'crlf\\r\\n'";
        assert_eq!(
            run_to_end(pty_shell(script)),
            [
                stdout(&["fd0=other", "fd1=tty", "fd2=tty", "to-stderr", "crlf"]),
                vec![ProcessEvent::Exited(0)]
            ]
            .concat(),
            "stdin is /dev/null without input; stderr shares the PTY; ONLCR would leave \"crlf\\r\""
        );

        // Planted negative: the same child over pipes sees no terminal at all.
        let piped = run_to_end(shell(script));
        for expected in [
            ProcessEvent::Stdout("fd1=other".to_owned()),
            ProcessEvent::Stdout("fd2=other".to_owned()),
            ProcessEvent::Stderr("to-stderr".to_owned()),
            ProcessEvent::Exited(0),
        ] {
            assert!(
                piped.contains(&expected),
                "{expected:?} missing from {piped:?}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn pty_input_is_typed_without_echo_and_close_sends_one_eof() {
        let input = ProcessInput::new();
        let script = "[ -t 0 ] && echo stdin=tty; \
                      while IFS= read -r line; do echo \"got:$line\"; done; echo eof";
        let (events, _trigger, handle) = start_process(pty_shell(script).stdin(input.clone()));
        assert_eq!(
            events_until(&events, |event| event
                == &ProcessEvent::Stdout("stdin=tty".to_owned())),
            stdout(&["stdin=tty"])
        );
        input.try_send_line("alpha".to_owned()).unwrap();
        input.try_send_line("🦀 beta $HOME".to_owned()).unwrap();
        input.close();
        assert_eq!(
            events_until(&events, is_terminal_event),
            [
                stdout(&["got:alpha", "got:🦀 beta $HOME", "eof"]),
                vec![ProcessEvent::Exited(0)]
            ]
            .concat(),
            "an echoing PTY would repeat each typed line before its reply"
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn pty_input_closed_after_the_child_exits_is_not_an_error() {
        let input = ProcessInput::new();
        input.try_send_line("never read".to_owned()).unwrap();
        // The child exits without reading. Closing input then finds no slave
        // to deliver end-of-file to, which must not turn the exit into an error.
        assert_eq!(
            run_to_end(pty_shell("echo bye").stdin(input)),
            [stdout(&["bye"]), vec![ProcessEvent::Exited(0)]].concat()
        );
    }

    #[test]
    #[cfg(unix)]
    fn pty_size_follows_resize_requests_before_and_during_the_run() {
        let control = ProcessControl::new();
        control.request_resize(100, 30).unwrap();
        let script = "stty size <&1; trap 'stty size <&1; exit 7' WINCH; echo ready; \
                      while :; do sleep 0.05; done";
        let (events, _trigger, handle) = start_process(pty_shell(script).control(control.clone()));
        assert_eq!(
            events_until(&events, |event| event
                == &ProcessEvent::Stdout("ready".to_owned())),
            stdout(&["30 100", "ready"])
        );
        control.request_resize(120, 40).unwrap();
        assert_eq!(
            events_until(&events, is_terminal_event),
            [stdout(&["40 120"]), vec![ProcessEvent::Exited(7)]].concat(),
            "the new size must be set before SIGWINCH reaches the child"
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn resize_requests_coalesce_clamp_and_close_with_the_run() {
        let control = ProcessControl::new();
        control.request_resize(0, 0).unwrap();
        assert_eq!(control.take_resize(), Some((1, 1)));
        control.request_resize(80, 24).unwrap();
        control.request_resize(132, 50).unwrap();
        assert_eq!(control.take_resize(), Some((132, 50)));
        assert_eq!(control.take_resize(), None);
        control.close();
        assert_eq!(
            control.request_resize(80, 24),
            Err(ProcessControlError::Closed)
        );
    }

    #[test]
    #[cfg(unix)]
    fn pty_interrupt_reaches_every_process_in_the_group() {
        // The inner shell is a separate process in the outer one's group. A
        // trapped SIGINT ends its `wait` at once; unsignaled, it waits out one
        // bounded sleep, so the pipe-mode run neither leaks it nor depends on
        // how fast a loaded host spawns processes.
        let script = "trap 'echo parent-int; exit 4' INT; \
                      sh -c 'trap \"echo child-int; exit 5\" INT; echo ready; \
                      sleep 2 & wait $!'; echo unreachable";
        let interrupted = |sub: ProcessSubscription<ProcessEvent>| {
            let control = ProcessControl::new();
            let (events, _trigger, handle) = start_process(sub.control(control.clone()));
            events_until(&events, |event| {
                event == &ProcessEvent::Stdout("ready".to_owned())
            });
            control.request_interrupt().unwrap();
            let rest = events_until(&events, is_terminal_event);
            handle.join().unwrap();
            assert_eq!(control.status().interrupt, ProcessInterruptStatus::Sent);
            rest
        };
        assert_eq!(
            interrupted(pty_shell(script)),
            [
                stdout(&["child-int", "parent-int"]),
                vec![ProcessEvent::Exited(4)]
            ]
            .concat()
        );
        // Planted negative: over pipes only the immediate child is signaled,
        // so the inner shell finishes its loop without ever seeing SIGINT.
        assert_eq!(
            interrupted(shell(script)),
            [stdout(&["parent-int"]), vec![ProcessEvent::Exited(4)]].concat()
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pty_cancel_kills_the_whole_group_and_pipe_cancel_does_not() {
        let script = "sleep 30 & echo \"bg=$!\"; while :; do sleep 0.05; done";
        let canceled = |sub: ProcessSubscription<ProcessEvent>| {
            let (events, trigger, handle) = start_process(sub);
            let started = events_until(&events, |event| matches!(event, ProcessEvent::Stdout(_)));
            let background = printed_pid(&started[0], "bg=");
            trigger.stop();
            handle.join().unwrap();
            (background, events.try_iter().collect::<Vec<_>>())
        };

        let (background, rest) = canceled(pty_shell(script));
        assert_eq!(rest, [ProcessEvent::Killed]);
        assert!(
            wait_until_gone(background),
            "background member {background} survived its PTY run"
        );

        // Planted negative: a pipe run kills only its immediate child.
        let (background, rest) = canceled(shell(script));
        assert_eq!(rest, [ProcessEvent::Killed]);
        let survived = !process_is_gone(background);
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(background).expect("positive PID"),
            rustix::process::Signal::KILL,
        );
        assert!(survived, "the pipe-mode contrast no longer holds");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pty_leader_exit_ends_lingering_group_members_and_output() {
        let start = Instant::now();
        let events = run_to_end(pty_shell("sleep 30 & echo \"bg=$!\"; echo done"));
        // A member holding the PTY open would otherwise delay end-of-file by 30 s.
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "run took {:?}",
            start.elapsed()
        );
        assert_eq!(
            events[1..],
            [stdout(&["done"]), vec![ProcessEvent::Exited(0)]].concat()
        );
        let background = printed_pid(&events[0], "bg=");
        assert!(
            wait_until_gone(background),
            "background member {background} outlived its leader"
        );
    }

    #[test]
    #[cfg(unix)]
    fn pty_delivers_ten_thousand_lines_in_order_like_pipes() {
        let expected: Vec<_> = (1..=10_000)
            .map(|line| ProcessEvent::Stdout(line.to_string()))
            .chain([ProcessEvent::Exited(0)])
            .collect();
        for (mode, sub) in [
            ("pty", pty_shell("seq 1 10000")),
            ("pipe", shell("seq 1 10000")),
        ] {
            let start = Instant::now();
            let events = run_to_end(sub);
            eprintln!("{mode}: 10000 lines in {:?}", start.elapsed());
            assert!(
                events == expected,
                "{mode} run delivered {} events",
                events.len()
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn pty_progress_carriage_returns_over_64k_rewind_without_line_too_long() {
        // 1. Reader-level check across arbitrary chunk sizes, including leading
        // CR, repeated CRs, CRLF, and >100 KiB of CR-delimited progress frames.
        let mut stream = Vec::new();
        for step in 1..=5_000 {
            stream.extend_from_slice(format!("\rprogress {step:04} of 5000").as_bytes());
        }
        stream.extend_from_slice(b"\r\ndone\r\n");
        assert!(stream.len() > 100 * 1024);

        for capacity in [1, 7, 64, 8192] {
            let mut reader = io::BufReader::with_capacity(capacity, stream.as_slice());
            let mut pending = Vec::with_capacity(MAX_PROCESS_LINE_BYTES + 1);
            let (stop, _trigger) = StopSignal::new();
            let mut last_preview = String::new();
            let mut preview = |prefix: &str| {
                last_preview.clear();
                last_preview.push_str(prefix);
                true
            };
            let first =
                read_process_line_mode(&mut reader, &mut pending, &stop, true, Some(&mut preview))
                    .unwrap();
            assert_eq!(first.as_deref(), Some("progress 5000 of 5000"));
            assert_eq!(last_preview, "progress 5000 of 5000");
            let second =
                read_process_line_mode(&mut reader, &mut pending, &stop, true, None).unwrap();
            assert_eq!(second.as_deref(), Some("done"));
            assert_eq!(
                read_process_line_mode(&mut reader, &mut pending, &stop, true, None).unwrap(),
                None
            );
        }

        // 2. Real PTY child emitting >100 KiB of CR-delimited progress updates
        // before LF completes normally without LineTooLong termination.
        let script = "python3 -u -c 'import sys; \
            [sys.stdout.write(f\"step {i:04d}-{\"x\"*20}\\r\") for i in range(1, 4001)]; \
            sys.stdout.write(\"step 4000-done\\r\\n\")'";
        let events = run_to_end(pty_shell(script).partial_output(true));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ProcessEvent::StdoutPartial(p) if p.starts_with("step "))),
            "expected partial progress previews, got {events:?}"
        );
        let completed: Vec<_> = events
            .iter()
            .filter(|e| !matches!(e, ProcessEvent::StdoutPartial(_)))
            .cloned()
            .collect();
        assert_eq!(
            completed,
            vec![
                ProcessEvent::Stdout("step 4000-done".to_owned()),
                ProcessEvent::Exited(0),
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn pty_input_delivers_lines_over_4095_bytes_without_canonical_truncation() {
        let input = ProcessInput::new();
        let long_line = format!("{}{}", "a".repeat(6_000), "b".repeat(6_000));
        let script = "python3 -u -c 'import sys; \
            line = sys.stdin.readline().rstrip(\"\\n\"); \
            print(f\"len={len(line)} head={line[:4]} mid={line[5998:6002]} tail={line[-4:]}\"); \
            print(f\"eof={sys.stdin.read() == \"\"}\")'";
        let (events, _trigger, handle) = start_process(pty_shell(script).stdin(input.clone()));
        input.try_send_line(long_line).unwrap();
        input.close();
        assert_eq!(
            events_until(&events, is_terminal_event),
            [
                stdout(&["len=12000 head=aaaa mid=aabb tail=bbbb", "eof=True"]),
                vec![ProcessEvent::Exited(0)]
            ]
            .concat()
        );
        handle.join().unwrap();
    }
}
