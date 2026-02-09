//! WebSocket-to-PTY bridge for FrankenTerm remote sessions.
//!
//! This module provides a small, deterministic server that:
//! - accepts a websocket client,
//! - spawns a PTY child process,
//! - forwards websocket binary input to the PTY,
//! - forwards PTY output back to websocket binary frames,
//! - supports resize control messages over websocket text frames, and
//! - emits JSONL telemetry for session/debug analysis.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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

    loop {
        let mut progressed = false;

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
                    )? {
                        return Ok(BridgeSummary {
                            session_id: session_id.to_string(),
                            ws_in_bytes: counters.ws_in_bytes,
                            ws_out_bytes: counters.ws_out_bytes,
                            pty_in_bytes: counters.pty_in_bytes,
                            pty_out_bytes: counters.pty_out_bytes,
                            resize_events: counters.resize_events,
                            exit_code,
                            exit_signal,
                        });
                    }
                }
                Err(WsError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => {
                    return Ok(BridgeSummary {
                        session_id: session_id.to_string(),
                        ws_in_bytes: counters.ws_in_bytes,
                        ws_out_bytes: counters.ws_out_bytes,
                        pty_in_bytes: counters.pty_in_bytes,
                        pty_out_bytes: counters.pty_out_bytes,
                        resize_events: counters.resize_events,
                        exit_code,
                        exit_signal,
                    });
                }
                Err(error) => {
                    return Err(io::Error::other(format!("websocket read failed: {error}")));
                }
            }
        }

        let output = pty.drain_output_nonblocking()?;
        if !output.is_empty() {
            progressed = true;
            counters.pty_out_bytes = counters
                .pty_out_bytes
                .saturating_add(u64::try_from(output.len()).unwrap_or(u64::MAX));
            counters.ws_out_bytes = counters
                .ws_out_bytes
                .saturating_add(u64::try_from(output.len()).unwrap_or(u64::MAX));
            send_ws_message(&mut websocket, Message::binary(output))?;
        }

        if let Some(status) = pty.try_wait()? {
            exit_code = Some(status.exit_code());
            exit_signal = status.signal().map(ToOwned::to_owned);

            let trailing = pty.drain_output_nonblocking()?;
            if !trailing.is_empty() {
                counters.pty_out_bytes = counters
                    .pty_out_bytes
                    .saturating_add(u64::try_from(trailing.len()).unwrap_or(u64::MAX));
                counters.ws_out_bytes = counters
                    .ws_out_bytes
                    .saturating_add(u64::try_from(trailing.len()).unwrap_or(u64::MAX));
                send_ws_message(&mut websocket, Message::binary(trailing))?;
            }

            let end = json!({
                "type": "session_end",
                "exit_code": exit_code,
                "exit_signal": exit_signal,
            });
            send_ws_message(&mut websocket, Message::text(end.to_string()))?;
            let _ = websocket.close(None);
            return Ok(BridgeSummary {
                session_id: session_id.to_string(),
                ws_in_bytes: counters.ws_in_bytes,
                ws_out_bytes: counters.ws_out_bytes,
                pty_in_bytes: counters.pty_in_bytes,
                pty_out_bytes: counters.pty_out_bytes,
                resize_events: counters.resize_events,
                exit_code,
                exit_signal,
            });
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
) -> io::Result<bool> {
    match message {
        Message::Binary(bytes) => {
            counters.ws_in_bytes = counters
                .ws_in_bytes
                .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            pty.send_input(bytes.as_ref())?;
            counters.pty_in_bytes = counters
                .pty_in_bytes
                .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            telemetry.write("bridge_input", json!({ "bytes": bytes.len() }))?;
            Ok(false)
        }
        Message::Text(text) => match parse_control_message(text.as_ref())? {
            Some(ControlMessage::Resize { cols, rows }) => {
                pty.resize(cols, rows)?;
                counters.resize_events = counters.resize_events.saturating_add(1);
                telemetry.write("bridge_resize", json!({ "cols": cols, "rows": rows }))?;
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

#[derive(Debug, Default)]
struct Counters {
    ws_in_bytes: u64,
    ws_out_bytes: u64,
    pty_in_bytes: u64,
    pty_out_bytes: u64,
    resize_events: u64,
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
                Err(error) => panic!("websocket read failed: {error}"),
            }
        }

        assert!(
            observed
                .windows(b"hello-through-bridge".len())
                .any(|window| window == b"hello-through-bridge"),
            "expected PTY echo in websocket output"
        );

        client
            .send(Message::Text(r#"{"type":"close"}"#.to_string().into()))
            .expect("send close control");
        let result = handle.join().expect("bridge thread join");
        result.expect("bridge result");
    }
}
