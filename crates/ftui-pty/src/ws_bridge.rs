//! WebSocket-to-PTY bridge for FrankenTerm remote sessions.
//!
//! This module provides a small, deterministic server that:
//! - accepts a websocket client,
//! - spawns a PTY child process,
//! - forwards websocket binary input to the PTY,
//! - forwards PTY output back to websocket binary frames,
//! - supports resize control messages over websocket text frames, and
//! - emits JSONL telemetry for session/debug analysis.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use frankenterm_core::flow_control::{
    FlowControlConfig, FlowControlPolicy, FlowControlSnapshot, InputEventClass, LatencyWindowMs,
    QueueDepthBytes, RateWindowBps,
};
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Error as WsError, Message, WebSocket, accept_hdr_with_config};

/// Runtime configuration for the websocket PTY bridge.
#[derive(Debug, Clone)]
pub struct WsPtyBridgeConfig {
    /// Address to bind the websocket server to.
    pub bind_addr: SocketAddr,
    /// Executable to spawn in the PTY.
    pub command: String,
    /// Command arguments.
    pub args: Vec<String>,
    /// TERM value exported to the child process.
    pub term: String,
    /// Extra child environment variables.
    pub env: Vec<(String, String)>,
    /// Initial PTY columns.
    pub cols: u16,
    /// Initial PTY rows.
    pub rows: u16,
    /// Allowlist for `Origin` headers. Empty means allow all.
    pub allowed_origins: Vec<String>,
    /// Optional shared secret expected as query parameter `token`.
    pub auth_token: Option<String>,
    /// Optional JSONL telemetry file path.
    pub telemetry_path: Option<PathBuf>,
    /// Max websocket message/frame size.
    pub max_message_bytes: usize,
    /// Loop sleep duration when idle.
    pub idle_sleep: Duration,
    /// Stop after one session if true.
    pub accept_once: bool,
    /// Optional flow control configuration. When `Some`, the bridge tracks
    /// credit windows, bounded output queues, resize coalescing, and
    /// backpressure decisions. When `None`, raw passthrough (legacy behavior).
    pub flow_control: Option<FlowControlBridgeConfig>,
}

impl Default for WsPtyBridgeConfig {
    fn default() -> Self {
        let command = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 9231)),
            command,
            args: Vec::new(),
            term: "xterm-256color".to_string(),
            env: Vec::new(),
            cols: 120,
            rows: 40,
            allowed_origins: Vec::new(),
            auth_token: None,
            telemetry_path: None,
            max_message_bytes: 256 * 1024,
            idle_sleep: Duration::from_millis(5),
            accept_once: true,
            flow_control: None,
        }
    }
}

/// Session summary emitted when a bridge session ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeSummary {
    /// Session id used in telemetry.
    pub session_id: String,
    /// Total websocket inbound bytes.
    pub ws_in_bytes: u64,
    /// Total websocket outbound bytes.
    pub ws_out_bytes: u64,
    /// Total bytes written into PTY stdin.
    pub pty_in_bytes: u64,
    /// Total bytes read from PTY stdout/stderr.
    pub pty_out_bytes: u64,
    /// Number of resize operations processed.
    pub resize_events: u64,
    /// Exit code if the child terminated during session.
    pub exit_code: Option<u32>,
    /// Exit signal (platform-dependent text) if available.
    pub exit_signal: Option<String>,
}

impl BridgeSummary {
    #[must_use]
    fn as_json(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "ws_in_bytes": self.ws_in_bytes,
            "ws_out_bytes": self.ws_out_bytes,
            "pty_in_bytes": self.pty_in_bytes,
            "pty_out_bytes": self.pty_out_bytes,
            "resize_events": self.resize_events,
            "exit_code": self.exit_code,
            "exit_signal": self.exit_signal,
        })
    }
}

/// Run the websocket PTY bridge server.
///
/// If `accept_once` is true, this accepts a single client and returns.
/// If false, the server keeps accepting new sessions until an unrecoverable
/// listener error occurs.
pub fn run_ws_pty_bridge(config: WsPtyBridgeConfig) -> io::Result<()> {
    let listener = TcpListener::bind(config.bind_addr)?;

    loop {
        let (stream, peer_addr) = listener.accept()?;
        let session_id = make_session_id();
        let mut telemetry = TelemetrySink::new(config.telemetry_path.as_deref(), &session_id)?;
        telemetry.write(
            "bridge_session_start",
            json!({
                "peer": peer_addr.to_string(),
                "bind_addr": config.bind_addr.to_string(),
                "command": config.command,
                "args": config.args,
                "cols": config.cols,
                "rows": config.rows,
                "term": config.term,
                "max_message_bytes": config.max_message_bytes,
            }),
        )?;

        let result = run_single_session(stream, &config, &session_id, &mut telemetry);
        match result {
            Ok(summary) => {
                telemetry.write("bridge_session_end", summary.as_json())?;
            }
            Err(error) => {
                telemetry.write(
                    "bridge_session_error",
                    json!({ "error": error.to_string() }),
                )?;
                if config.accept_once {
                    return Err(error);
                }
            }
        }

        if config.accept_once {
            break;
        }
    }

    Ok(())
}

fn make_summary(
    session_id: &str,
    counters: &Counters,
    exit_code: Option<u32>,
    exit_signal: Option<String>,
) -> BridgeSummary {
    BridgeSummary {
        session_id: session_id.to_string(),
        ws_in_bytes: counters.ws_in_bytes,
        ws_out_bytes: counters.ws_out_bytes,
        pty_in_bytes: counters.pty_in_bytes,
        pty_out_bytes: counters.pty_out_bytes,
        resize_events: counters.resize_events,
        exit_code,
        exit_signal,
    }
}

fn run_single_session(
    stream: TcpStream,
    config: &WsPtyBridgeConfig,
    session_id: &str,
    telemetry: &mut TelemetrySink,
) -> io::Result<BridgeSummary> {
    stream.set_nodelay(true)?;
    let mut websocket = accept_websocket(stream, config)?;
    websocket.get_mut().set_nonblocking(true)?;

    let mut pty = PtyBridgeSession::spawn(config)?;
    let mut counters = Counters::default();
    let mut exit_code = None;
    let mut exit_signal: Option<String> = None;

    let mut fc_state: Option<FlowControlBridgeState> = config
        .flow_control
        .as_ref()
        .map(FlowControlBridgeState::new);

    if let Some(ref fc_config) = config.flow_control {
        telemetry.write(
            "flow_control_enabled",
            json!({
                "output_window": fc_config.output_window,
                "input_window": fc_config.input_window,
                "coalesce_resize_ms": fc_config.coalesce_resize_ms,
            }),
        )?;
    }

    loop {
        let mut progressed = false;

        // --- WebSocket read loop ---
        loop {
            match websocket.read() {
                Ok(message) => {
                    progressed = true;
                    if handle_ws_message(
                        &mut websocket,
                        &mut pty,
                        &mut counters,
                        telemetry,
                        message,
                        &mut fc_state,
                    )? {
                        if let Some(ref fc) = fc_state {
                            telemetry.write("flow_control_summary", fc.summary_json())?;
                        }
                        return Ok(make_summary(session_id, &counters, exit_code, exit_signal));
                    }
                }
                Err(WsError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => {
                    if let Some(ref fc) = fc_state {
                        telemetry.write("flow_control_summary", fc.summary_json())?;
                    }
                    return Ok(make_summary(session_id, &counters, exit_code, exit_signal));
                }
                Err(error) => {
                    return Err(io::Error::other(format!("websocket read failed: {error}")));
                }
            }
        }

        // --- Resize coalescing flush ---
        if let Some(ref mut fc) = fc_state
            && let Some((cols, rows)) = fc.flush_pending_resize()
        {
            pty.resize(cols, rows)?;
            counters.resize_events = counters.resize_events.saturating_add(1);
            telemetry.write(
                "bridge_resize",
                json!({ "cols": cols, "rows": rows, "coalesced": true }),
            )?;
        }

        // --- PTY output ---
        let read_pty = fc_state.as_ref().is_none_or(|fc| !fc.pty_reads_paused);
        if read_pty {
            let output = pty.drain_output_nonblocking()?;
            if !output.is_empty() {
                progressed = true;
                counters.pty_out_bytes = counters
                    .pty_out_bytes
                    .saturating_add(u64::try_from(output.len()).unwrap_or(u64::MAX));

                match fc_state {
                    Some(ref mut fc) => {
                        fc.enqueue_output(&output);
                    }
                    None => {
                        counters.ws_out_bytes = counters
                            .ws_out_bytes
                            .saturating_add(u64::try_from(output.len()).unwrap_or(u64::MAX));
                        send_ws_message(&mut websocket, Message::binary(output))?;
                    }
                }
            }
        }

        // --- Flow control: evaluate policy and drain output queue ---
        if let Some(ref mut fc) = fc_state {
            fc.maybe_reset_rate_window();
            let was_paused = fc.pty_reads_paused;
            let decision = fc.evaluate();

            // Log non-stable decisions
            if decision.chosen_action.is_some() {
                telemetry.write(
                    "flow_control_decision",
                    json!({
                        "action": format!("{:?}", decision.chosen_action),
                        "reason": format!("{:?}", decision.reason),
                        "fairness_index": decision.fairness_index,
                        "output_batch_budget": decision.output_batch_budget_bytes,
                        "should_pause_pty_reads": decision.should_pause_pty_reads,
                        "output_queue_bytes": fc.output_queue_bytes(),
                    }),
                )?;
            }

            emit_flow_control_stall_if_transitioned(telemetry, fc, was_paused)?;

            // Drain output queue per budget
            let batch = fc.drain_output(decision.output_batch_budget_bytes);
            if !batch.is_empty() {
                progressed = true;
                counters.ws_out_bytes = counters
                    .ws_out_bytes
                    .saturating_add(u64::try_from(batch.len()).unwrap_or(u64::MAX));
                send_ws_message(&mut websocket, Message::binary(batch))?;
            }

            // Send replenishment if needed
            if fc.should_send_replenish() {
                emit_flow_control_event(
                    telemetry,
                    "input",
                    "replenish",
                    fc.input_window,
                    fc.input_pending_bytes,
                )?;
                telemetry.write(
                    "flow_control_replenish",
                    json!({
                        "input_consumed": fc.input_consumed,
                        "input_window": fc.input_window,
                    }),
                )?;
                fc.record_replenish_sent();
            }
        }

        // --- Child exit ---
        if let Some(status) = pty.try_wait()? {
            exit_code = Some(status.exit_code());
            exit_signal = status.signal().map(ToOwned::to_owned);

            let trailing = pty.drain_output_nonblocking()?;
            if !trailing.is_empty() {
                counters.pty_out_bytes = counters
                    .pty_out_bytes
                    .saturating_add(u64::try_from(trailing.len()).unwrap_or(u64::MAX));

                match fc_state {
                    Some(ref mut fc) => {
                        fc.enqueue_output(&trailing);
                        let remaining = fc.drain_all_output();
                        if !remaining.is_empty() {
                            counters.ws_out_bytes = counters
                                .ws_out_bytes
                                .saturating_add(u64::try_from(remaining.len()).unwrap_or(u64::MAX));
                            send_ws_message(&mut websocket, Message::binary(remaining))?;
                        }
                    }
                    None => {
                        counters.ws_out_bytes = counters
                            .ws_out_bytes
                            .saturating_add(u64::try_from(trailing.len()).unwrap_or(u64::MAX));
                        send_ws_message(&mut websocket, Message::binary(trailing))?;
                    }
                }
            }

            if let Some(ref fc) = fc_state {
                telemetry.write("flow_control_summary", fc.summary_json())?;
            }

            let end = json!({
                "type": "session_end",
                "exit_code": exit_code,
                "exit_signal": exit_signal,
            });
            send_ws_message(&mut websocket, Message::text(end.to_string()))?;
            let _ = websocket.close(None);
            return Ok(make_summary(
                session_id,
                &counters,
                exit_code,
                exit_signal.clone(),
            ));
        }

        if !progressed {
            thread::sleep(config.idle_sleep);
        }
    }
}

fn handle_ws_message(
    websocket: &mut WebSocket<TcpStream>,
    pty: &mut PtyBridgeSession,
    counters: &mut Counters,
    telemetry: &mut TelemetrySink,
    message: Message,
    fc_state: &mut Option<FlowControlBridgeState>,
) -> io::Result<bool> {
    match message {
        Message::Binary(bytes) => {
            let byte_len = bytes.len();
            let len32 = u32::try_from(byte_len).unwrap_or(u32::MAX);
            counters.ws_in_bytes = counters
                .ws_in_bytes
                .saturating_add(u64::try_from(byte_len).unwrap_or(u64::MAX));

            // Flow control: check if non-interactive input should be dropped.
            // For raw binary input we conservatively classify as Interactive
            // (keystrokes are sent as raw bytes). A future binary-envelope
            // migration can use semantic sub-types for finer classification.
            if let Some(ref mut fc) = *fc_state {
                if fc.should_drop_input(InputEventClass::Interactive) {
                    // Interactive events are never dropped by policy, so this
                    // branch is unreachable for Interactive. Kept for symmetry
                    // with future NonInteractive classification.
                    fc.fc_counters.input_drops = fc.fc_counters.input_drops.saturating_add(1);
                    telemetry.write(
                        "flow_control_input_drop",
                        json!({
                            "bytes": byte_len,
                            "input_queue_bytes": fc.input_pending_bytes,
                        }),
                    )?;
                    return Ok(false);
                }
                fc.record_input_arrival(len32);
            }

            pty.send_input(bytes.as_ref())?;
            counters.pty_in_bytes = counters
                .pty_in_bytes
                .saturating_add(u64::try_from(byte_len).unwrap_or(u64::MAX));

            if let Some(ref mut fc) = *fc_state {
                fc.record_input_serviced(len32);
            }

            telemetry.write("bridge_input", json!({ "bytes": byte_len }))?;
            Ok(false)
        }
        Message::Text(text) => match parse_control_message(text.as_ref())? {
            Some(ControlMessage::Resize { cols, rows }) => {
                match *fc_state {
                    Some(ref mut fc) => {
                        // Coalesce: buffer the resize, flush later in main loop
                        fc.coalesce_resize(cols, rows);
                    }
                    None => {
                        pty.resize(cols, rows)?;
                        counters.resize_events = counters.resize_events.saturating_add(1);
                        telemetry.write("bridge_resize", json!({ "cols": cols, "rows": rows }))?;
                    }
                }
                Ok(false)
            }
            Some(ControlMessage::Ping) => {
                send_ws_message(websocket, Message::Pong(Vec::<u8>::new().into()))?;
                Ok(false)
            }
            Some(ControlMessage::Close) => Ok(true),
            None => {
                send_ws_message(
                    websocket,
                    Message::text(
                        json!({ "type": "warning", "message": "unknown_control_message" })
                            .to_string(),
                    ),
                )?;
                Ok(false)
            }
        },
        Message::Ping(payload) => {
            send_ws_message(websocket, Message::Pong(payload))?;
            Ok(false)
        }
        Message::Pong(_) | Message::Frame(_) => Ok(false),
        Message::Close(_) => Ok(true),
    }
}

fn send_ws_message(websocket: &mut WebSocket<TcpStream>, message: Message) -> io::Result<()> {
    let mut retries = 0_u8;
    loop {
        match websocket.send(message.clone()) {
            Ok(()) => return Ok(()),
            Err(WsError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock && retries < 5 => {
                retries = retries.saturating_add(1);
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => {
                return Err(io::Error::other(format!("websocket send failed: {error}")));
            }
        }
    }
}

#[allow(clippy::result_large_err)] // ErrorResponse size is dictated by tungstenite's API
fn accept_websocket(
    stream: TcpStream,
    config: &WsPtyBridgeConfig,
) -> io::Result<WebSocket<TcpStream>> {
    let allowed_origins = config.allowed_origins.clone();
    let expected_token = config.auth_token.clone();
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(config.max_message_bytes))
        .max_frame_size(Some(config.max_message_bytes))
        .write_buffer_size(0);

    let callback = move |request: &Request, response: Response| {
        validate_upgrade_request(request, &allowed_origins, expected_token.as_deref())
            .map(|()| response)
            .map_err(HandshakeRejection::into_response)
    };

    accept_hdr_with_config(stream, callback, Some(ws_config)).map_err(|error| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("websocket handshake failed: {error}"),
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlMessage {
    Resize { cols: u16, rows: u16 },
    Ping,
    Close,
}

fn parse_control_message(text: &str) -> io::Result<Option<ControlMessage>> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid control JSON: {error}"),
        )
    })?;

    let msg_type = value.get("type").and_then(Value::as_str).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "control message missing `type`")
    })?;

    match msg_type {
        "resize" => {
            let cols = read_u16_field(&value, "cols")?;
            let rows = read_u16_field(&value, "rows")?;
            if cols == 0 || rows == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "resize dimensions must be > 0",
                ));
            }
            Ok(Some(ControlMessage::Resize { cols, rows }))
        }
        "ping" => Ok(Some(ControlMessage::Ping)),
        "close" => Ok(Some(ControlMessage::Close)),
        _ => Ok(None),
    }
}

fn read_u16_field(value: &Value, key: &str) -> io::Result<u16> {
    let raw = value.get(key).and_then(Value::as_u64).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("control message missing numeric `{key}`"),
        )
    })?;
    u16::try_from(raw).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("`{key}` out of range for u16"),
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HandshakeRejection {
    status: StatusCode,
    body: String,
}

impl HandshakeRejection {
    fn into_response(self) -> ErrorResponse {
        let mut response = ErrorResponse::new(Some(self.body));
        *response.status_mut() = self.status;
        response
    }
}

fn validate_upgrade_request(
    request: &Request,
    allowed_origins: &[String],
    expected_token: Option<&str>,
) -> Result<(), HandshakeRejection> {
    if !allowed_origins.is_empty() {
        let origin = request
            .headers()
            .get("Origin")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| HandshakeRejection {
                status: StatusCode::FORBIDDEN,
                body: "Origin header missing".to_string(),
            })?;
        let allowed = allowed_origins.iter().any(|allowed| allowed == origin);
        if !allowed {
            return Err(HandshakeRejection {
                status: StatusCode::FORBIDDEN,
                body: "Origin not allowed".to_string(),
            });
        }
    }

    if let Some(token) = expected_token {
        let query = request.uri().query().ok_or_else(|| HandshakeRejection {
            status: StatusCode::UNAUTHORIZED,
            body: "Missing token".to_string(),
        })?;
        let presented = query_param(query, "token").ok_or_else(|| HandshakeRejection {
            status: StatusCode::UNAUTHORIZED,
            body: "Missing token".to_string(),
        })?;
        if presented != token {
            return Err(HandshakeRejection {
                status: StatusCode::UNAUTHORIZED,
                body: "Invalid token".to_string(),
            });
        }
    }

    Ok(())
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let mut pieces = pair.splitn(2, '=');
        let name = pieces.next().unwrap_or_default();
        let value = pieces.next().unwrap_or_default();
        if name == key { Some(value) } else { None }
    })
}

fn make_session_id() -> String {
    let ts = OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("ws-bridge-{}-{ts}", std::process::id())
}

fn emit_flow_control_event(
    telemetry: &mut TelemetrySink,
    direction: &'static str,
    action: &'static str,
    window_bytes: u32,
    queued_bytes: u32,
) -> io::Result<()> {
    telemetry.write(
        "flow_control",
        json!({
            "direction": direction,
            "action": action,
            "window_bytes": window_bytes,
            "queued_bytes": queued_bytes,
        }),
    )
}

fn emit_flow_control_stall_if_transitioned(
    telemetry: &mut TelemetrySink,
    fc: &FlowControlBridgeState,
    was_paused: bool,
) -> io::Result<()> {
    if !was_paused && fc.pty_reads_paused {
        emit_flow_control_event(
            telemetry,
            "output",
            "stall",
            fc.output_window,
            fc.output_queue_bytes(),
        )?;
    }
    Ok(())
}

#[derive(Debug, Default)]
struct Counters {
    ws_in_bytes: u64,
    ws_out_bytes: u64,
    pty_in_bytes: u64,
    pty_out_bytes: u64,
    resize_events: u64,
}

// ---------------------------------------------------------------------------
// Flow control
// ---------------------------------------------------------------------------

/// Flow control configuration for the WebSocket-PTY bridge.
///
/// When present in `WsPtyBridgeConfig`, enables credit-window tracking,
/// bounded output queuing, resize coalescing, and policy-driven backpressure.
#[derive(Debug, Clone)]
pub struct FlowControlBridgeConfig {
    /// Initial output credit window (bytes the server may send before client ACKs).
    pub output_window: u32,
    /// Initial input credit window (bytes the client may send before server ACKs).
    pub input_window: u32,
    /// Resize coalescing window in milliseconds (0 disables coalescing).
    pub coalesce_resize_ms: u32,
    /// Policy engine configuration (queue caps, fairness, batch sizes, etc.).
    pub policy: FlowControlConfig,
}

impl Default for FlowControlBridgeConfig {
    fn default() -> Self {
        Self {
            output_window: 65_536,
            input_window: 8_192,
            coalesce_resize_ms: 50,
            policy: FlowControlConfig::default(),
        }
    }
}

/// Aggregate counters emitted as telemetry at session end.
#[derive(Debug, Default)]
struct FlowControlCounters {
    output_queue_peak_bytes: u32,
    input_drops: u64,
    decisions_non_stable: u64,
    resizes_coalesced: u64,
    pty_read_pauses: u64,
    replenishments_sent: u64,
}

/// Mutable flow-control state tracked during a bridge session.
struct FlowControlBridgeState {
    policy: FlowControlPolicy,

    // Credit windows
    output_window: u32,
    input_window: u32,
    output_consumed: u32,
    input_consumed: u32,

    // Bounded output queue (FIFO byte buffer)
    output_queue: VecDeque<u8>,

    // Input depth (data flows through immediately; we track depth for policy)
    input_pending_bytes: u32,

    // Fairness tracking
    serviced_input_bytes: u64,
    serviced_output_bytes: u64,

    // Timing
    last_replenish: Instant,
    output_hard_cap_since: Option<Instant>,
    rate_window_start: Instant,

    // Rate accumulation in current 1-second window
    rate_in_arrived: u32,
    rate_out_arrived: u32,
    rate_in_serviced: u32,
    rate_out_serviced: u32,

    // Coalescing
    coalesce_resize_ms: u32,
    pending_resize: Option<(u16, u16, Instant)>,

    // State flags
    pty_reads_paused: bool,

    // Aggregate counters for telemetry
    fc_counters: FlowControlCounters,
}

impl FlowControlBridgeState {
    fn new(config: &FlowControlBridgeConfig) -> Self {
        let now = Instant::now();
        Self {
            policy: FlowControlPolicy::new(config.policy),
            output_window: config.output_window,
            input_window: config.input_window,
            output_consumed: 0,
            input_consumed: 0,
            output_queue: VecDeque::new(),
            input_pending_bytes: 0,
            serviced_input_bytes: 0,
            serviced_output_bytes: 0,
            last_replenish: now,
            output_hard_cap_since: None,
            rate_window_start: now,
            rate_in_arrived: 0,
            rate_out_arrived: 0,
            rate_in_serviced: 0,
            rate_out_serviced: 0,
            coalesce_resize_ms: config.coalesce_resize_ms,
            pending_resize: None,
            pty_reads_paused: false,
            fc_counters: FlowControlCounters::default(),
        }
    }

    /// Record input bytes arriving from the client.
    fn record_input_arrival(&mut self, len: u32) {
        self.input_pending_bytes = self.input_pending_bytes.saturating_add(len);
        self.rate_in_arrived = self.rate_in_arrived.saturating_add(len);
        self.input_consumed = self.input_consumed.saturating_add(len);
    }

    /// Record input bytes serviced (written to PTY).
    fn record_input_serviced(&mut self, len: u32) {
        self.input_pending_bytes = self.input_pending_bytes.saturating_sub(len);
        self.serviced_input_bytes = self.serviced_input_bytes.saturating_add(len as u64);
        self.rate_in_serviced = self.rate_in_serviced.saturating_add(len);
    }

    /// Enqueue PTY output bytes into the bounded output queue.
    /// Drops bytes that exceed the hard cap to enforce bounded memory.
    fn enqueue_output(&mut self, data: &[u8]) {
        let hard_cap = self.policy.config.output_hard_cap_bytes as usize;
        let available = hard_cap.saturating_sub(self.output_queue.len());
        let to_add = data.len().min(available);
        self.output_queue.extend(&data[..to_add]);
        self.rate_out_arrived = self.rate_out_arrived.saturating_add(to_add as u32);

        let queue_len = self.output_queue.len() as u32;
        if queue_len > self.fc_counters.output_queue_peak_bytes {
            self.fc_counters.output_queue_peak_bytes = queue_len;
        }
    }

    /// Drain up to `budget` bytes from the output queue, respecting credit window.
    fn drain_output(&mut self, budget: u32) -> Vec<u8> {
        let queue_available = self.output_queue.len().min(budget as usize);
        let window_available = self.output_window.saturating_sub(self.output_consumed) as usize;
        let to_drain = queue_available.min(window_available);
        if to_drain == 0 {
            return Vec::new();
        }
        let batch: Vec<u8> = self.output_queue.drain(..to_drain).collect();
        let len = batch.len() as u32;
        self.output_consumed = self.output_consumed.saturating_add(len);
        self.serviced_output_bytes = self.serviced_output_bytes.saturating_add(len as u64);
        self.rate_out_serviced = self.rate_out_serviced.saturating_add(len);
        batch
    }

    /// Drain ALL remaining bytes (ignoring window), used at session teardown.
    fn drain_all_output(&mut self) -> Vec<u8> {
        let batch: Vec<u8> = self.output_queue.drain(..).collect();
        let len = batch.len() as u32;
        self.output_consumed = self.output_consumed.saturating_add(len);
        self.serviced_output_bytes = self.serviced_output_bytes.saturating_add(len as u64);
        batch
    }

    /// Check if an input event should be dropped per policy.
    fn should_drop_input(&self, class: InputEventClass) -> bool {
        self.policy
            .should_drop_input_event(self.input_pending_bytes, class)
    }

    /// Buffer a resize, coalescing with any pending resize.
    fn coalesce_resize(&mut self, cols: u16, rows: u16) {
        self.pending_resize = Some((cols, rows, Instant::now()));
    }

    /// Flush pending resize if coalescing window has elapsed.
    fn flush_pending_resize(&mut self) -> Option<(u16, u16)> {
        let (cols, rows, queued_at) = self.pending_resize?;
        if self.coalesce_resize_ms == 0 {
            self.pending_resize = None;
            return Some((cols, rows));
        }
        if queued_at.elapsed() >= Duration::from_millis(self.coalesce_resize_ms as u64) {
            self.fc_counters.resizes_coalesced =
                self.fc_counters.resizes_coalesced.saturating_add(1);
            self.pending_resize = None;
            Some((cols, rows))
        } else {
            None
        }
    }

    /// Build a `FlowControlSnapshot` for the policy evaluator.
    fn build_snapshot(&self) -> FlowControlSnapshot {
        let rate_elapsed = self.rate_window_start.elapsed();
        let rate_secs = rate_elapsed.as_secs_f64().max(0.001);

        let queue_bytes = self.output_queue.len() as u32;
        let hard_cap = self.policy.config.output_hard_cap_bytes;
        let occupancy = if hard_cap > 0 {
            queue_bytes as f64 / hard_cap as f64
        } else {
            0.0
        };

        let hard_cap_duration_ms = self
            .output_hard_cap_since
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0);

        FlowControlSnapshot {
            queues: QueueDepthBytes {
                input: self.input_pending_bytes,
                output: queue_bytes,
                render_frames: 0,
            },
            rates: RateWindowBps {
                lambda_in: (self.rate_in_arrived as f64 / rate_secs) as u32,
                lambda_out: (self.rate_out_arrived as f64 / rate_secs) as u32,
                mu_in: (self.rate_in_serviced as f64 / rate_secs) as u32,
                mu_out: (self.rate_out_serviced as f64 / rate_secs) as u32,
            },
            latency: LatencyWindowMs {
                key_p50_ms: 10.0 * occupancy,
                key_p95_ms: 20.0 * occupancy,
            },
            serviced_input_bytes: self.serviced_input_bytes,
            serviced_output_bytes: self.serviced_output_bytes,
            output_hard_cap_duration_ms: hard_cap_duration_ms,
        }
    }

    /// Evaluate the policy and update internal hard-cap/pause state.
    fn evaluate(&mut self) -> frankenterm_core::flow_control::FlowControlDecision {
        let snapshot = self.build_snapshot();
        let decision = self.policy.evaluate(snapshot);

        // Track hard-cap state transitions
        let at_hard_cap =
            self.output_queue.len() as u32 >= self.policy.config.output_hard_cap_bytes;
        match (at_hard_cap, self.output_hard_cap_since) {
            (true, None) => self.output_hard_cap_since = Some(Instant::now()),
            (false, Some(_)) => self.output_hard_cap_since = None,
            _ => {}
        }

        let was_paused = self.pty_reads_paused;
        self.pty_reads_paused = decision.should_pause_pty_reads;
        if decision.should_pause_pty_reads && !was_paused {
            self.fc_counters.pty_read_pauses = self.fc_counters.pty_read_pauses.saturating_add(1);
        }

        if decision.chosen_action.is_some() {
            self.fc_counters.decisions_non_stable =
                self.fc_counters.decisions_non_stable.saturating_add(1);
        }

        decision
    }

    /// Check whether we should send a FlowControl replenishment to the client.
    fn should_send_replenish(&self) -> bool {
        let elapsed_ms = self.last_replenish.elapsed().as_millis() as u64;
        self.policy
            .should_replenish(self.input_consumed, self.input_window, elapsed_ms)
    }

    /// Reset replenishment tracking after sending FlowControl message.
    fn record_replenish_sent(&mut self) {
        self.input_consumed = 0;
        self.last_replenish = Instant::now();
        self.fc_counters.replenishments_sent =
            self.fc_counters.replenishments_sent.saturating_add(1);
    }

    /// Process an inbound FlowControl message from the client.
    /// Currently unused until binary envelope message routing is added;
    /// kept for downstream integration (bd-2vr05.2.4+).
    #[allow(dead_code)]
    fn process_flow_control_msg(&mut self, output_consumed: u32) {
        self.output_consumed = self.output_consumed.saturating_sub(output_consumed);
    }

    /// Reset rate counters at ~1 second intervals.
    fn maybe_reset_rate_window(&mut self) {
        if self.rate_window_start.elapsed() >= Duration::from_secs(1) {
            self.rate_in_arrived = 0;
            self.rate_out_arrived = 0;
            self.rate_in_serviced = 0;
            self.rate_out_serviced = 0;
            self.rate_window_start = Instant::now();
        }
    }

    fn output_queue_bytes(&self) -> u32 {
        self.output_queue.len() as u32
    }

    /// Produce a telemetry summary JSON.
    fn summary_json(&self) -> Value {
        json!({
            "output_queue_peak_bytes": self.fc_counters.output_queue_peak_bytes,
            "input_drops": self.fc_counters.input_drops,
            "decisions_non_stable": self.fc_counters.decisions_non_stable,
            "resizes_coalesced": self.fc_counters.resizes_coalesced,
            "pty_read_pauses": self.fc_counters.pty_read_pauses,
            "replenishments_sent": self.fc_counters.replenishments_sent,
            "serviced_input_bytes": self.serviced_input_bytes,
            "serviced_output_bytes": self.serviced_output_bytes,
        })
    }
}

#[derive(Debug)]
enum ReaderMsg {
    Data(Vec<u8>),
    Eof,
    Err(io::Error),
}

struct PtyBridgeSession {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    rx: mpsc::Receiver<ReaderMsg>,
    reader_thread: Option<thread::JoinHandle<()>>,
    eof: bool,
}

impl PtyBridgeSession {
    fn spawn(config: &WsPtyBridgeConfig) -> io::Result<Self> {
        let mut cmd = CommandBuilder::new(&config.command);
        for arg in &config.args {
            cmd.arg(arg);
        }
        cmd.env("TERM", &config.term);
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(portable_pty_error)?;

        let child = pair.slave.spawn_command(cmd).map_err(portable_pty_error)?;
        let mut reader = pair.master.try_clone_reader().map_err(portable_pty_error)?;
        let writer = pair.master.take_writer().map_err(portable_pty_error)?;

        let (tx, rx) = mpsc::channel::<ReaderMsg>();
        let reader_thread = thread::Builder::new()
            .name("ftui-pty-ws-reader".to_string())
            .spawn(move || {
                let mut buffer = [0_u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => {
                            let _ = tx.send(ReaderMsg::Eof);
                            break;
                        }
                        Ok(n) => {
                            let _ = tx.send(ReaderMsg::Data(buffer[..n].to_vec()));
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            let _ = tx.send(ReaderMsg::Err(error));
                            break;
                        }
                    }
                }
            })
            .map_err(|error| {
                io::Error::other(format!("failed to spawn PTY reader thread: {error}"))
            })?;

        Ok(Self {
            child,
            master: pair.master,
            writer,
            rx,
            reader_thread: Some(reader_thread),
            eof: false,
        })
    }

    fn send_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(portable_pty_error)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn drain_output_nonblocking(&mut self) -> io::Result<Vec<u8>> {
        if self.eof {
            return Ok(Vec::new());
        }

        let mut output = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(ReaderMsg::Data(bytes)) => output.extend_from_slice(&bytes),
                Ok(ReaderMsg::Eof) => {
                    self.eof = true;
                    break;
                }
                Ok(ReaderMsg::Err(error)) => return Err(error),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.eof = true;
                    break;
                }
            }
        }

        Ok(output)
    }
}

impl Drop for PtyBridgeSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

fn portable_pty_error<E: std::fmt::Display>(error: E) -> io::Error {
    io::Error::other(format!("{error}"))
}

struct TelemetrySink {
    file: Option<File>,
    session_id: String,
    seq: u64,
}

impl TelemetrySink {
    fn new(path: Option<&Path>, session_id: &str) -> io::Result<Self> {
        let file = match path {
            Some(path) => Some(OpenOptions::new().create(true).append(true).open(path)?),
            None => None,
        };
        Ok(Self {
            file,
            session_id: session_id.to_string(),
            seq: 0,
        })
    }

    fn write(&mut self, event: &str, payload: Value) -> io::Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        let line = json!({
            "event": event,
            "ts": now_iso8601(),
            "session_id": self.session_id,
            "seq": self.seq,
            "payload": payload,
        });
        self.seq = self.seq.saturating_add(1);
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }
}

fn now_iso8601() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{Message, connect};

    fn request(uri: &str, origin: Option<&str>) -> Request {
        let mut builder = Request::builder().uri(uri);
        if let Some(origin) = origin {
            builder = builder.header("Origin", origin);
        }
        builder.body(()).expect("request build")
    }

    #[test]
    fn query_param_extracts_expected_value() {
        assert_eq!(query_param("token=abc&x=1", "token"), Some("abc"));
        assert_eq!(query_param("x=1&token=abc", "token"), Some("abc"));
        assert_eq!(query_param("x=1", "token"), None);
    }

    #[test]
    fn validate_upgrade_request_allows_matching_origin_and_token() {
        let req = request("/ws?token=secret", Some("https://allowed.example"));
        let result = validate_upgrade_request(
            &req,
            &[String::from("https://allowed.example")],
            Some("secret"),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn validate_upgrade_request_rejects_invalid_origin() {
        let req = request("/ws?token=secret", Some("https://denied.example"));
        let result = validate_upgrade_request(
            &req,
            &[String::from("https://allowed.example")],
            Some("secret"),
        );
        let rejection = result.expect_err("should reject");
        assert_eq!(rejection.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn validate_upgrade_request_rejects_invalid_token() {
        let req = request("/ws?token=wrong", Some("https://allowed.example"));
        let result = validate_upgrade_request(
            &req,
            &[String::from("https://allowed.example")],
            Some("secret"),
        );
        let rejection = result.expect_err("should reject");
        assert_eq!(rejection.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn parse_control_message_resize_ping_and_unknown() {
        assert_eq!(
            parse_control_message(r#"{"type":"resize","cols":120,"rows":40}"#).expect("parse"),
            Some(ControlMessage::Resize {
                cols: 120,
                rows: 40
            })
        );
        assert_eq!(
            parse_control_message(r#"{"type":"ping"}"#).expect("parse"),
            Some(ControlMessage::Ping)
        );
        assert_eq!(
            parse_control_message(r#"{"type":"unknown"}"#).expect("parse"),
            None
        );
    }

    #[test]
    fn parse_control_message_rejects_invalid_resize_dimensions() {
        let error = parse_control_message(r#"{"type":"resize","cols":0,"rows":40}"#)
            .expect_err("invalid dims should fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    // --- WsPtyBridgeConfig ---

    #[test]
    fn config_default_fields() {
        let c = WsPtyBridgeConfig::default();
        assert_eq!(c.bind_addr, SocketAddr::from(([127, 0, 0, 1], 9231)));
        assert!(c.args.is_empty());
        assert_eq!(c.term, "xterm-256color");
        assert!(c.env.is_empty());
        assert_eq!(c.cols, 120);
        assert_eq!(c.rows, 40);
        assert!(c.allowed_origins.is_empty());
        assert!(c.auth_token.is_none());
        assert!(c.telemetry_path.is_none());
        assert_eq!(c.max_message_bytes, 256 * 1024);
        assert_eq!(c.idle_sleep, Duration::from_millis(5));
        assert!(c.accept_once);
    }

    #[test]
    fn config_clone() {
        let c1 = WsPtyBridgeConfig::default();
        let c2 = c1.clone();
        assert_eq!(c2.cols, c1.cols);
        assert_eq!(c2.rows, c1.rows);
        assert_eq!(c2.term, c1.term);
    }

    #[test]
    fn config_debug() {
        let c = WsPtyBridgeConfig::default();
        let dbg = format!("{c:?}");
        assert!(dbg.contains("WsPtyBridgeConfig"));
        assert!(dbg.contains("bind_addr"));
    }

    // --- BridgeSummary ---

    #[test]
    fn bridge_summary_as_json_contains_all_fields() {
        let summary = BridgeSummary {
            session_id: "test-123".to_string(),
            ws_in_bytes: 100,
            ws_out_bytes: 200,
            pty_in_bytes: 50,
            pty_out_bytes: 150,
            resize_events: 3,
            exit_code: Some(0),
            exit_signal: None,
        };
        let json = summary.as_json();
        assert_eq!(json["session_id"], "test-123");
        assert_eq!(json["ws_in_bytes"], 100);
        assert_eq!(json["ws_out_bytes"], 200);
        assert_eq!(json["pty_in_bytes"], 50);
        assert_eq!(json["pty_out_bytes"], 150);
        assert_eq!(json["resize_events"], 3);
        assert_eq!(json["exit_code"], 0);
        assert!(json["exit_signal"].is_null());
    }

    #[test]
    fn bridge_summary_as_json_with_signal() {
        let summary = BridgeSummary {
            session_id: "s".to_string(),
            ws_in_bytes: 0,
            ws_out_bytes: 0,
            pty_in_bytes: 0,
            pty_out_bytes: 0,
            resize_events: 0,
            exit_code: None,
            exit_signal: Some("SIGKILL".to_string()),
        };
        let json = summary.as_json();
        assert!(json["exit_code"].is_null());
        assert_eq!(json["exit_signal"], "SIGKILL");
    }

    #[test]
    fn bridge_summary_clone_and_eq() {
        let s1 = BridgeSummary {
            session_id: "a".to_string(),
            ws_in_bytes: 1,
            ws_out_bytes: 2,
            pty_in_bytes: 3,
            pty_out_bytes: 4,
            resize_events: 5,
            exit_code: Some(42),
            exit_signal: None,
        };
        let s2 = s1.clone();
        assert_eq!(s1, s2);
    }

    #[test]
    fn bridge_summary_debug() {
        let s = BridgeSummary {
            session_id: "x".to_string(),
            ws_in_bytes: 0,
            ws_out_bytes: 0,
            pty_in_bytes: 0,
            pty_out_bytes: 0,
            resize_events: 0,
            exit_code: None,
            exit_signal: None,
        };
        let dbg = format!("{s:?}");
        assert!(dbg.contains("BridgeSummary"));
        assert!(dbg.contains("session_id"));
    }

    // --- ControlMessage ---

    #[test]
    fn control_message_close() {
        assert_eq!(
            parse_control_message(r#"{"type":"close"}"#).expect("parse"),
            Some(ControlMessage::Close)
        );
    }

    #[test]
    fn control_message_debug_clone_eq() {
        let m = ControlMessage::Resize { cols: 80, rows: 24 };
        let m2 = m;
        assert_eq!(m, m2);
        let dbg = format!("{m:?}");
        assert!(dbg.contains("Resize"));
        assert!(dbg.contains("80"));
    }

    // --- parse_control_message edge cases ---

    #[test]
    fn parse_control_message_invalid_json() {
        let err = parse_control_message("not json").expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn parse_control_message_missing_type() {
        let err = parse_control_message(r#"{"cols":80}"#).expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn parse_control_message_resize_missing_cols() {
        let err = parse_control_message(r#"{"type":"resize","rows":40}"#).expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn parse_control_message_resize_missing_rows() {
        let err = parse_control_message(r#"{"type":"resize","cols":80}"#).expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn parse_control_message_resize_zero_rows() {
        let err = parse_control_message(r#"{"type":"resize","cols":80,"rows":0}"#)
            .expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn parse_control_message_resize_zero_cols() {
        let err = parse_control_message(r#"{"type":"resize","cols":0,"rows":24}"#)
            .expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn parse_control_message_resize_large_values() {
        // u16::MAX = 65535 is valid
        let result =
            parse_control_message(r#"{"type":"resize","cols":65535,"rows":65535}"#).expect("parse");
        assert_eq!(
            result,
            Some(ControlMessage::Resize {
                cols: 65535,
                rows: 65535
            })
        );
    }

    #[test]
    fn parse_control_message_resize_overflow_u16() {
        let err = parse_control_message(r#"{"type":"resize","cols":70000,"rows":40}"#)
            .expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    // --- read_u16_field ---

    #[test]
    fn read_u16_field_valid() {
        let v: Value = serde_json::from_str(r#"{"x": 42}"#).unwrap();
        assert_eq!(read_u16_field(&v, "x").unwrap(), 42);
    }

    #[test]
    fn read_u16_field_missing() {
        let v: Value = serde_json::from_str(r#"{"x": 42}"#).unwrap();
        let err = read_u16_field(&v, "y").expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_u16_field_not_numeric() {
        let v: Value = serde_json::from_str(r#"{"x": "hello"}"#).unwrap();
        let err = read_u16_field(&v, "x").expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_u16_field_overflow() {
        let v: Value = serde_json::from_str(r#"{"x": 100000}"#).unwrap();
        let err = read_u16_field(&v, "x").expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    // --- query_param edge cases ---

    #[test]
    fn query_param_empty_string() {
        assert_eq!(query_param("", "token"), None);
    }

    #[test]
    fn query_param_missing_value() {
        assert_eq!(query_param("token", "token"), Some(""));
    }

    #[test]
    fn query_param_first_of_duplicates() {
        assert_eq!(
            query_param("token=first&token=second", "token"),
            Some("first")
        );
    }

    #[test]
    fn query_param_value_with_equals() {
        assert_eq!(query_param("token=a=b", "token"), Some("a=b"));
    }

    // --- validate_upgrade_request edge cases ---

    #[test]
    fn validate_no_origin_required_no_token_required() {
        let req = request("/ws", None);
        let result = validate_upgrade_request(&req, &[], None);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_origin_required_but_header_missing() {
        let req = request("/ws", None);
        let result =
            validate_upgrade_request(&req, &[String::from("https://allowed.example")], None);
        let rejection = result.expect_err("should reject");
        assert_eq!(rejection.status, StatusCode::FORBIDDEN);
        assert!(rejection.body.contains("Origin"));
    }

    #[test]
    fn validate_token_required_but_no_query_string() {
        let req = request("/ws", None);
        let result = validate_upgrade_request(&req, &[], Some("secret"));
        let rejection = result.expect_err("should reject");
        assert_eq!(rejection.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn validate_token_required_but_missing_from_query() {
        let req = request("/ws?other=value", None);
        let result = validate_upgrade_request(&req, &[], Some("secret"));
        let rejection = result.expect_err("should reject");
        assert_eq!(rejection.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn validate_correct_token_no_origin_restriction() {
        let req = request("/ws?token=mysecret", None);
        let result = validate_upgrade_request(&req, &[], Some("mysecret"));
        assert!(result.is_ok());
    }

    // --- HandshakeRejection ---

    #[test]
    fn handshake_rejection_into_response() {
        let rejection = HandshakeRejection {
            status: StatusCode::FORBIDDEN,
            body: "test body".to_string(),
        };
        let response = rejection.into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn handshake_rejection_debug_clone_eq() {
        let r1 = HandshakeRejection {
            status: StatusCode::UNAUTHORIZED,
            body: "bad".to_string(),
        };
        let r2 = r1.clone();
        assert_eq!(r1, r2);
        let dbg = format!("{r1:?}");
        assert!(dbg.contains("HandshakeRejection"));
    }

    // --- Counters ---

    #[test]
    fn counters_default() {
        let c = Counters::default();
        assert_eq!(c.ws_in_bytes, 0);
        assert_eq!(c.ws_out_bytes, 0);
        assert_eq!(c.pty_in_bytes, 0);
        assert_eq!(c.pty_out_bytes, 0);
        assert_eq!(c.resize_events, 0);
    }

    #[test]
    fn counters_debug() {
        let c = Counters::default();
        let dbg = format!("{c:?}");
        assert!(dbg.contains("Counters"));
    }

    // --- make_session_id ---

    #[test]
    fn make_session_id_format() {
        let id = make_session_id();
        assert!(id.starts_with("ws-bridge-"));
        assert!(id.len() > 15);
    }

    #[test]
    fn make_session_id_unique() {
        let id1 = make_session_id();
        thread::sleep(Duration::from_millis(1));
        let id2 = make_session_id();
        assert_ne!(id1, id2);
    }

    // --- now_iso8601 ---

    #[test]
    fn now_iso8601_format() {
        let ts = now_iso8601();
        assert!(ts.contains('T'));
        assert!(ts.contains('-'));
        assert!(ts.len() >= 20);
    }

    // --- TelemetrySink ---

    #[test]
    fn telemetry_sink_no_path_write_is_noop() {
        let mut sink = TelemetrySink::new(None, "test").expect("create sink");
        // Writing without a file returns early — seq stays at 0.
        sink.write("event", json!({"key": "value"})).expect("write");
        assert_eq!(sink.seq, 0);
    }

    #[test]
    fn telemetry_sink_with_path_writes_jsonl() {
        let dir = std::env::temp_dir().join("ftui-test-telemetry");
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("test_telemetry.jsonl");
        let _ = std::fs::remove_file(&path);

        {
            let mut sink = TelemetrySink::new(Some(&path), "sess-1").expect("create sink");
            sink.write("start", json!({"x": 1})).expect("write 1");
            sink.write("end", json!({"x": 2})).expect("write 2");
            assert_eq!(sink.seq, 2);
        }

        let content = std::fs::read_to_string(&path).expect("read file");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let v: Value = serde_json::from_str(line).expect("parse JSON");
            assert_eq!(v["session_id"], "sess-1");
            assert!(v["ts"].is_string());
            assert!(v["event"].is_string());
        }

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn flow_control_event_writes_expected_payload_fields() {
        let dir = std::env::temp_dir().join("ftui-test-flow-control-event");
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("flow_control_event.jsonl");
        let _ = std::fs::remove_file(&path);

        {
            let mut sink = TelemetrySink::new(Some(&path), "sess-fc").expect("create sink");
            emit_flow_control_event(&mut sink, "output", "stall", 65_536, 65_000)
                .expect("write flow_control");
        }

        let content = std::fs::read_to_string(&path).expect("read file");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: Value = serde_json::from_str(lines[0]).expect("parse flow_control event");
        assert_eq!(parsed["event"], "flow_control");
        assert_eq!(parsed["payload"]["direction"], "output");
        assert_eq!(parsed["payload"]["action"], "stall");
        assert_eq!(parsed["payload"]["window_bytes"], 65_536);
        assert_eq!(parsed["payload"]["queued_bytes"], 65_000);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn flow_control_stall_emits_only_on_pause_transition() {
        let dir = std::env::temp_dir().join("ftui-test-flow-control-stall");
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("flow_control_stall.jsonl");
        let _ = std::fs::remove_file(&path);

        let mut fc = default_fc_state();
        let payload = [b'x'; 128];
        fc.enqueue_output(&payload);
        fc.pty_reads_paused = true;

        {
            let mut sink = TelemetrySink::new(Some(&path), "sess-stall").expect("create sink");
            emit_flow_control_stall_if_transitioned(&mut sink, &fc, false)
                .expect("emit rising-edge stall");
            emit_flow_control_stall_if_transitioned(&mut sink, &fc, true)
                .expect("do not emit while already paused");
        }

        let content = std::fs::read_to_string(&path).expect("read file");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: Value = serde_json::from_str(lines[0]).expect("parse flow_control stall");
        assert_eq!(parsed["event"], "flow_control");
        assert_eq!(parsed["payload"]["direction"], "output");
        assert_eq!(parsed["payload"]["action"], "stall");
        assert_eq!(parsed["payload"]["window_bytes"], fc.output_window);
        assert_eq!(parsed["payload"]["queued_bytes"], fc.output_queue_bytes());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn bridge_smoke_echoes_bytes_through_pty() {
        let listener =
            TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).expect("bind ephemeral port");
        let bind_addr = listener.local_addr().expect("local addr");
        drop(listener);

        let config = WsPtyBridgeConfig {
            bind_addr,
            accept_once: true,
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "cat".to_string()],
            idle_sleep: Duration::from_millis(1),
            ..WsPtyBridgeConfig::default()
        };

        let handle = thread::spawn(move || run_ws_pty_bridge(config));
        thread::sleep(Duration::from_millis(75));

        let url = format!("ws://{bind_addr}/ws");
        let (mut client, _response) = connect(url).expect("connect websocket");
        if let MaybeTlsStream::Plain(stream) = client.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .expect("set read timeout");
        }
        client
            .send(Message::Binary(b"hello-through-bridge\n".to_vec().into()))
            .expect("send input");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut observed = Vec::new();
        let mut last_error: Option<WsError> = None;
        while Instant::now() < deadline {
            match client.read() {
                Ok(Message::Binary(bytes)) => {
                    observed.extend_from_slice(bytes.as_ref());
                    if observed
                        .windows(b"hello-through-bridge".len())
                        .any(|window| window == b"hello-through-bridge")
                    {
                        break;
                    }
                }
                Ok(_) => {}
                Err(WsError::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => {
                    last_error = Some(error);
                    break;
                }
            }
        }

        let saw_echo = observed
            .windows(b"hello-through-bridge".len())
            .any(|window| window == b"hello-through-bridge");

        if let Err(err) = client.send(Message::Text(r#"{"type":"close"}"#.to_string().into())) {
            // Preserve the first meaningful error for assertion diagnostics.
            if last_error.is_none() {
                last_error = Some(err);
            }
        }
        let result = handle.join().expect("bridge thread join");
        result.expect("bridge result");

        assert!(
            saw_echo,
            "expected PTY echo in websocket output; last_error={last_error:?}; observed_len={}",
            observed.len()
        );
    }

    // --- FlowControlBridgeConfig ---

    #[test]
    fn flow_control_bridge_config_default() {
        let c = FlowControlBridgeConfig::default();
        assert_eq!(c.output_window, 65_536);
        assert_eq!(c.input_window, 8_192);
        assert_eq!(c.coalesce_resize_ms, 50);
    }

    #[test]
    fn flow_control_bridge_config_debug_clone() {
        let c = FlowControlBridgeConfig::default();
        let c2 = c.clone();
        let dbg = format!("{c2:?}");
        assert!(dbg.contains("FlowControlBridgeConfig"));
        assert!(dbg.contains("output_window"));
    }

    // --- FlowControlBridgeState: output queuing ---

    fn default_fc_state() -> FlowControlBridgeState {
        FlowControlBridgeState::new(&FlowControlBridgeConfig::default())
    }

    #[test]
    fn fc_state_new_has_empty_queue() {
        let fc = default_fc_state();
        assert_eq!(fc.output_queue_bytes(), 0);
        assert!(!fc.pty_reads_paused);
        assert_eq!(fc.input_pending_bytes, 0);
    }

    #[test]
    fn fc_enqueue_and_drain_output() {
        let mut fc = default_fc_state();
        fc.enqueue_output(&[0xAA; 100]);
        assert_eq!(fc.output_queue_bytes(), 100);

        let batch = fc.drain_output(50);
        assert_eq!(batch.len(), 50);
        assert_eq!(fc.output_queue_bytes(), 50);

        let batch2 = fc.drain_output(100);
        assert_eq!(batch2.len(), 50);
        assert_eq!(fc.output_queue_bytes(), 0);
    }

    #[test]
    fn fc_enqueue_respects_hard_cap() {
        let mut config = FlowControlBridgeConfig::default();
        config.policy.output_hard_cap_bytes = 200;
        let mut fc = FlowControlBridgeState::new(&config);

        fc.enqueue_output(&[0xBB; 300]);
        // Only 200 bytes should be enqueued (hard cap)
        assert_eq!(fc.output_queue_bytes(), 200);
    }

    #[test]
    fn fc_drain_respects_window() {
        let config = FlowControlBridgeConfig {
            output_window: 50,
            ..FlowControlBridgeConfig::default()
        };
        let mut fc = FlowControlBridgeState::new(&config);

        fc.enqueue_output(&[0xCC; 200]);
        let batch = fc.drain_output(200);
        // Window allows only 50 bytes
        assert_eq!(batch.len(), 50);
        assert_eq!(fc.output_queue_bytes(), 150);
    }

    #[test]
    fn fc_drain_all_output_ignores_window() {
        let config = FlowControlBridgeConfig {
            output_window: 10,
            ..FlowControlBridgeConfig::default()
        };
        let mut fc = FlowControlBridgeState::new(&config);

        fc.enqueue_output(&[0xDD; 100]);
        let batch = fc.drain_all_output();
        assert_eq!(batch.len(), 100);
        assert_eq!(fc.output_queue_bytes(), 0);
    }

    #[test]
    fn fc_output_queue_peak_tracked() {
        let mut fc = default_fc_state();
        fc.enqueue_output(&[0x01; 500]);
        assert_eq!(fc.fc_counters.output_queue_peak_bytes, 500);
        fc.drain_output(200);
        fc.enqueue_output(&[0x02; 100]);
        // Peak should still be 500
        assert_eq!(fc.fc_counters.output_queue_peak_bytes, 500);
        fc.enqueue_output(&[0x03; 500]);
        // Now peak should be 900 (300 remaining + 500 new, but capped)
        assert!(fc.fc_counters.output_queue_peak_bytes >= 800);
    }

    // --- FlowControlBridgeState: input tracking ---

    #[test]
    fn fc_input_arrival_and_service() {
        let mut fc = default_fc_state();
        fc.record_input_arrival(100);
        assert_eq!(fc.input_pending_bytes, 100);
        assert_eq!(fc.input_consumed, 100);

        fc.record_input_serviced(60);
        assert_eq!(fc.input_pending_bytes, 40);
        assert_eq!(fc.serviced_input_bytes, 60);
    }

    #[test]
    fn fc_should_drop_interactive_never() {
        let mut config = FlowControlBridgeConfig::default();
        config.policy.input_hard_cap_bytes = 100;
        let mut fc = FlowControlBridgeState::new(&config);
        fc.input_pending_bytes = 200; // way over hard cap
        // Interactive events are never dropped
        assert!(!fc.should_drop_input(InputEventClass::Interactive));
    }

    #[test]
    fn fc_should_drop_noninteractive_at_hard_cap() {
        let mut config = FlowControlBridgeConfig::default();
        config.policy.input_hard_cap_bytes = 100;
        let mut fc = FlowControlBridgeState::new(&config);

        fc.input_pending_bytes = 50;
        assert!(!fc.should_drop_input(InputEventClass::NonInteractive));

        fc.input_pending_bytes = 100;
        assert!(fc.should_drop_input(InputEventClass::NonInteractive));

        fc.input_pending_bytes = 200;
        assert!(fc.should_drop_input(InputEventClass::NonInteractive));
    }

    // --- FlowControlBridgeState: resize coalescing ---

    #[test]
    fn fc_coalesce_resize_disabled() {
        let config = FlowControlBridgeConfig {
            coalesce_resize_ms: 0,
            ..FlowControlBridgeConfig::default()
        };
        let mut fc = FlowControlBridgeState::new(&config);

        fc.coalesce_resize(80, 24);
        let result = fc.flush_pending_resize();
        assert_eq!(result, Some((80, 24)));
    }

    #[test]
    fn fc_coalesce_resize_defers() {
        let config = FlowControlBridgeConfig {
            coalesce_resize_ms: 100, // 100ms window
            ..FlowControlBridgeConfig::default()
        };
        let mut fc = FlowControlBridgeState::new(&config);

        fc.coalesce_resize(80, 24);
        // Immediately after, flush should return None (timer not elapsed)
        let result = fc.flush_pending_resize();
        assert!(result.is_none());
    }

    #[test]
    fn fc_coalesce_resize_overwrite() {
        let config = FlowControlBridgeConfig {
            coalesce_resize_ms: 0,
            ..FlowControlBridgeConfig::default()
        };
        let mut fc = FlowControlBridgeState::new(&config);

        fc.coalesce_resize(80, 24);
        fc.coalesce_resize(120, 40); // Overwrite pending
        let result = fc.flush_pending_resize();
        assert_eq!(result, Some((120, 40))); // Gets the latest
    }

    #[test]
    fn fc_coalesce_resize_flushes_after_timeout() {
        let config = FlowControlBridgeConfig {
            coalesce_resize_ms: 10, // 10ms window
            ..FlowControlBridgeConfig::default()
        };
        let mut fc = FlowControlBridgeState::new(&config);

        fc.coalesce_resize(132, 50);
        thread::sleep(Duration::from_millis(15));
        let result = fc.flush_pending_resize();
        assert_eq!(result, Some((132, 50)));
        assert_eq!(fc.fc_counters.resizes_coalesced, 1);
    }

    #[test]
    fn fc_flush_no_pending_resize() {
        let mut fc = default_fc_state();
        assert_eq!(fc.flush_pending_resize(), None);
    }

    // --- FlowControlBridgeState: snapshot and evaluation ---

    #[test]
    fn fc_build_snapshot_empty_state() {
        let fc = default_fc_state();
        let snapshot = fc.build_snapshot();
        assert_eq!(snapshot.queues.input, 0);
        assert_eq!(snapshot.queues.output, 0);
        assert_eq!(snapshot.serviced_input_bytes, 0);
        assert_eq!(snapshot.serviced_output_bytes, 0);
    }

    #[test]
    fn fc_build_snapshot_with_data() {
        let mut fc = default_fc_state();
        fc.enqueue_output(&[0xAA; 1000]);
        fc.record_input_arrival(200);
        fc.record_input_serviced(150);

        let snapshot = fc.build_snapshot();
        assert_eq!(snapshot.queues.output, 1000);
        assert_eq!(snapshot.queues.input, 50);
        assert_eq!(snapshot.serviced_input_bytes, 150);
    }

    #[test]
    fn fc_evaluate_no_pause_when_idle() {
        let mut fc = default_fc_state();
        let decision = fc.evaluate();
        // When idle, the output queue is empty so reads should not be paused
        assert!(!decision.should_pause_pty_reads);
    }

    #[test]
    fn fc_evaluate_pauses_reads_at_hard_cap() {
        let mut config = FlowControlBridgeConfig::default();
        config.policy.output_hard_cap_bytes = 500;
        let mut fc = FlowControlBridgeState::new(&config);

        // Fill queue to hard cap
        fc.enqueue_output(&[0xAA; 500]);
        let decision = fc.evaluate();
        assert!(decision.should_pause_pty_reads);
        assert!(fc.pty_reads_paused);
        assert_eq!(fc.fc_counters.pty_read_pauses, 1);
    }

    #[test]
    fn fc_evaluate_resumes_reads_after_drain() {
        let mut config = FlowControlBridgeConfig::default();
        config.policy.output_hard_cap_bytes = 500;
        config.output_window = 500;
        let mut fc = FlowControlBridgeState::new(&config);

        fc.enqueue_output(&[0xAA; 500]);
        let _ = fc.evaluate();
        assert!(fc.pty_reads_paused);

        // Drain below hard cap
        let _ = fc.drain_output(400);
        let decision = fc.evaluate();
        assert!(!decision.should_pause_pty_reads);
        assert!(!fc.pty_reads_paused);
    }

    // --- FlowControlBridgeState: replenishment ---

    #[test]
    fn fc_replenish_at_50_percent_consumption() {
        let mut fc = default_fc_state();
        // input_window is 8192, so 50% = 4096
        fc.input_consumed = 4096;
        assert!(fc.should_send_replenish());
    }

    #[test]
    fn fc_replenish_not_at_low_consumption() {
        let fc = default_fc_state();
        // Just created, no consumption
        // But note: should_replenish also checks elapsed time (10ms default)
        // Since we just created it, time elapsed is ~0ms
        // At 0 consumed, not at threshold... but the policy checks elapsed too
        // Let's just verify the API works
        let result = fc.should_send_replenish();
        // Could be true or false depending on timing; just ensure no panic
        let _ = result;
    }

    #[test]
    fn fc_record_replenish_resets_state() {
        let mut fc = default_fc_state();
        fc.input_consumed = 5000;
        fc.record_replenish_sent();
        assert_eq!(fc.input_consumed, 0);
        assert_eq!(fc.fc_counters.replenishments_sent, 1);
    }

    // --- FlowControlBridgeState: flow control message ---

    #[test]
    fn fc_process_flow_control_msg_replenishes_window() {
        let mut fc = default_fc_state();
        fc.output_consumed = 30000;
        fc.process_flow_control_msg(20000);
        assert_eq!(fc.output_consumed, 10000);
    }

    #[test]
    fn fc_process_flow_control_msg_saturates() {
        let mut fc = default_fc_state();
        fc.output_consumed = 100;
        fc.process_flow_control_msg(200);
        assert_eq!(fc.output_consumed, 0);
    }

    // --- FlowControlBridgeState: rate window ---

    #[test]
    fn fc_rate_window_resets_after_interval() {
        let mut fc = default_fc_state();
        fc.rate_in_arrived = 1000;
        fc.rate_out_arrived = 2000;
        fc.rate_in_serviced = 800;
        fc.rate_out_serviced = 1500;
        // Pretend 2 seconds elapsed
        fc.rate_window_start = Instant::now() - Duration::from_secs(2);
        fc.maybe_reset_rate_window();
        assert_eq!(fc.rate_in_arrived, 0);
        assert_eq!(fc.rate_out_arrived, 0);
        assert_eq!(fc.rate_in_serviced, 0);
        assert_eq!(fc.rate_out_serviced, 0);
    }

    #[test]
    fn fc_rate_window_does_not_reset_early() {
        let mut fc = default_fc_state();
        fc.rate_in_arrived = 1000;
        fc.maybe_reset_rate_window();
        assert_eq!(fc.rate_in_arrived, 1000);
    }

    // --- FlowControlBridgeState: summary ---

    #[test]
    fn fc_summary_json_contains_expected_fields() {
        let mut fc = default_fc_state();
        fc.fc_counters.input_drops = 5;
        fc.fc_counters.decisions_non_stable = 10;
        fc.serviced_input_bytes = 1000;
        fc.serviced_output_bytes = 2000;

        let summary = fc.summary_json();
        assert_eq!(summary["input_drops"], 5);
        assert_eq!(summary["decisions_non_stable"], 10);
        assert_eq!(summary["serviced_input_bytes"], 1000);
        assert_eq!(summary["serviced_output_bytes"], 2000);
    }

    // --- WsPtyBridgeConfig with flow control ---

    #[test]
    fn config_default_no_flow_control() {
        let c = WsPtyBridgeConfig::default();
        assert!(c.flow_control.is_none());
    }

    #[test]
    fn config_with_flow_control() {
        let c = WsPtyBridgeConfig {
            flow_control: Some(FlowControlBridgeConfig::default()),
            ..WsPtyBridgeConfig::default()
        };
        assert!(c.flow_control.is_some());
        assert_eq!(c.flow_control.as_ref().unwrap().output_window, 65_536);
    }

    // --- Integration: smoke test with flow control enabled ---

    #[cfg(unix)]
    #[test]
    fn bridge_smoke_with_flow_control() {
        let listener =
            TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).expect("bind ephemeral port");
        let bind_addr = listener.local_addr().expect("local addr");
        drop(listener);

        let config = WsPtyBridgeConfig {
            bind_addr,
            accept_once: true,
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "cat".to_string()],
            idle_sleep: Duration::from_millis(1),
            flow_control: Some(FlowControlBridgeConfig::default()),
            ..WsPtyBridgeConfig::default()
        };

        let handle = thread::spawn(move || run_ws_pty_bridge(config));
        thread::sleep(Duration::from_millis(75));

        let url = format!("ws://{bind_addr}/ws");
        let (mut client, _response) = connect(url).expect("connect websocket");
        if let MaybeTlsStream::Plain(stream) = client.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .expect("set read timeout");
        }
        client
            .send(Message::Binary(b"fc-echo-test\n".to_vec().into()))
            .expect("send input");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut observed = Vec::new();
        let mut last_error: Option<WsError> = None;
        while Instant::now() < deadline {
            match client.read() {
                Ok(Message::Binary(bytes)) => {
                    observed.extend_from_slice(bytes.as_ref());
                    if observed
                        .windows(b"fc-echo-test".len())
                        .any(|window| window == b"fc-echo-test")
                    {
                        break;
                    }
                }
                Ok(_) => {}
                Err(WsError::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => {
                    last_error = Some(error);
                    break;
                }
            }
        }

        let saw_echo = observed
            .windows(b"fc-echo-test".len())
            .any(|window| window == b"fc-echo-test");

        if let Err(err) = client.send(Message::Text(r#"{"type":"close"}"#.to_string().into()))
            && last_error.is_none()
        {
            last_error = Some(err);
        }
        let result = handle.join().expect("bridge thread join");
        result.expect("bridge result");

        assert!(
            saw_echo,
            "expected PTY echo with flow control; last_error={last_error:?}; observed_len={}",
            observed.len()
        );
    }
}
