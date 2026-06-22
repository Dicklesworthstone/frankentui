use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use doctor_frankentui::seed::{SeedDemoArgs, SeedDemoConfig, run_seed_demo, run_seed_with_config};
use serde::Serialize;
use serde_json::{Value, json};
use tempfile::tempdir;

#[derive(Debug, Clone)]
enum ResponseBody {
    Empty,
    Text(String),
    Json(Value),
}

#[derive(Debug, Clone)]
struct ScriptedResponse {
    expected_tool: Option<String>,
    body: ResponseBody,
    delay_ms: u64,
}

impl ScriptedResponse {
    fn success(expected_tool: &str) -> Self {
        Self {
            expected_tool: Some(expected_tool.to_string()),
            body: ResponseBody::Json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "ok": true },
            })),
            delay_ms: 0,
        }
    }

    fn empty(expected_tool: &str) -> Self {
        Self {
            expected_tool: Some(expected_tool.to_string()),
            body: ResponseBody::Empty,
            delay_ms: 0,
        }
    }

    fn text(expected_tool: &str, text: &str) -> Self {
        Self {
            expected_tool: Some(expected_tool.to_string()),
            body: ResponseBody::Text(text.to_string()),
            delay_ms: 0,
        }
    }

    fn json_value(expected_tool: &str, body: Value) -> Self {
        Self {
            expected_tool: Some(expected_tool.to_string()),
            body: ResponseBody::Json(body),
            delay_ms: 0,
        }
    }

    fn with_delay(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
struct TranscriptEntry {
    request_index: usize,
    path: String,
    authorization: Option<String>,
    tool: Option<String>,
    expected_tool: Option<String>,
    expected_tool_matched: bool,
    request_body: String,
}

struct ServerHarness {
    endpoint: String,
    transcripts: Arc<Mutex<Vec<TranscriptEntry>>>,
    stop: Arc<AtomicBool>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl Drop for ServerHarness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(stream) =
            TcpStream::connect(self.endpoint.strip_prefix("http://").unwrap_or_default())
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_request(
    stream: &mut TcpStream,
) -> Option<(String, Option<String>, String, Option<String>)> {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 4096];
    let mut content_length = 0_usize;
    let mut header_end = None;

    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;

    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => {
                bytes.extend_from_slice(&buf[..read]);

                if header_end.is_none()
                    && let Some(pos) = find_header_end(&bytes)
                {
                    header_end = Some(pos + 4);
                    let header_text = String::from_utf8_lossy(&bytes[..pos]).to_string();
                    for line in header_text.lines() {
                        let lower = line.to_ascii_lowercase();
                        if lower.starts_with("content-length:")
                            && let Some(value) = line.split(':').nth(1)
                        {
                            content_length = value.trim().parse::<usize>().unwrap_or(0);
                        }
                    }
                }

                if let Some(end) = header_end
                    && bytes.len() >= end + content_length
                {
                    break;
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => return None,
        }
    }

    let end = header_end?;
    if bytes.len() < end + content_length {
        return None;
    }

    let header_text = String::from_utf8_lossy(&bytes[..end]).to_string();
    let mut lines = header_text.lines();
    let request_line = lines.next()?.to_string();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .map_or_else(|| "/".to_string(), ToOwned::to_owned);

    let mut authorization = None;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("authorization:") {
            authorization = line.split(':').nth(1).map(|value| value.trim().to_string());
        }
    }

    let body = String::from_utf8_lossy(&bytes[end..end + content_length]).to_string();
    let tool = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|parsed| {
            parsed
                .pointer("/params/name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });

    Some((path, authorization, body, tool))
}

fn write_response(stream: &mut TcpStream, response: &ResponseBody) {
    let (content_type, body) = match response {
        ResponseBody::Empty => ("text/plain", String::new()),
        ResponseBody::Text(body) => ("text/plain", body.clone()),
        ResponseBody::Json(body) => ("application/json", body.to_string()),
    };

    let payload = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
}

fn start_scripted_server(mut responses: Vec<ScriptedResponse>) -> ServerHarness {
    if responses.is_empty() {
        responses.push(ScriptedResponse {
            expected_tool: None,
            body: ResponseBody::Json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "ok": true },
            })),
            delay_ms: 0,
        });
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking listener");

    let endpoint = format!("http://{}/", listener.local_addr().expect("local addr"));
    let stop = Arc::new(AtomicBool::new(false));
    let transcripts = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(VecDeque::from(responses)));

    let stop_clone = Arc::clone(&stop);
    let transcripts_clone = Arc::clone(&transcripts);
    let queue_clone = Arc::clone(&queue);

    let join_handle = thread::spawn(move || {
        let mut request_index = 0_usize;

        loop {
            if stop_clone.load(Ordering::SeqCst) {
                break;
            }

            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    if let Some((path, authorization, request_body, tool)) =
                        parse_request(&mut stream)
                    {
                        request_index = request_index.saturating_add(1);

                        let response = {
                            let mut guard = queue_clone.lock().expect("queue lock");
                            if guard.len() > 1 {
                                guard.pop_front().expect("queued response")
                            } else {
                                guard.front().expect("fallback response").clone()
                            }
                        };

                        let expected_tool = response.expected_tool.clone();
                        let expected_tool_matched = expected_tool.as_ref() == tool.as_ref();
                        if response.delay_ms > 0 {
                            thread::sleep(Duration::from_millis(response.delay_ms));
                        }

                        transcripts_clone
                            .lock()
                            .expect("transcript lock")
                            .push(TranscriptEntry {
                                request_index,
                                path,
                                authorization,
                                tool,
                                expected_tool,
                                expected_tool_matched,
                                request_body,
                            });

                        write_response(&mut stream, &response.body);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    ServerHarness {
        endpoint,
        transcripts,
        stop,
        join_handle: Some(join_handle),
    }
}

fn configured_seed_run(
    endpoint: &str,
    http_path: &str,
    auth_bearer: &str,
    log_file: Option<std::path::PathBuf>,
    timeout_seconds: u64,
) -> SeedDemoConfig {
    let without_scheme = endpoint.trim_start_matches("http://").trim_end_matches('/');
    let mut parts = without_scheme.split(':');
    let host = parts.next().unwrap_or("127.0.0.1").to_string();
    let port = parts.next().unwrap_or("80").to_string();

    SeedDemoConfig {
        host,
        port,
        http_path: http_path.to_string(),
        auth_bearer: auth_bearer.to_string(),
        project_key: "/tmp/doctor-seed-demo-project".to_string(),
        agent_a: "SeedAlpha".to_string(),
        agent_b: "SeedBeta".to_string(),
        messages: 1,
        timeout_seconds,
        log_file,
    }
}

fn write_transcript_jsonl(path: &std::path::Path, entries: &[TranscriptEntry]) {
    let mut content = String::new();
    for entry in entries {
        content.push_str(&serde_json::to_string(entry).expect("serialize transcript"));
        content.push('\n');
    }
    std::fs::write(path, content).expect("write transcript jsonl");
}

#[test]
fn seed_demo_run_seed_demo_wrapper_converts_args_and_succeeds() {
    let temp = tempdir().expect("tempdir");
    let log_file = temp.path().join("seed_args.log");

    let server = start_scripted_server(vec![
        ScriptedResponse::success("health_check"),
        ScriptedResponse::success("ensure_project"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::success("send_message"),
        ScriptedResponse::success("send_message"),
        ScriptedResponse::success("fetch_inbox"),
        ScriptedResponse::success("search_messages"),
        ScriptedResponse::success("file_reservation_paths"),
    ]);

    let without_scheme = server
        .endpoint
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let mut parts = without_scheme.split(':');
    let host = parts.next().unwrap_or("127.0.0.1").to_string();
    let port = parts.next().unwrap_or("80").to_string();

    run_seed_demo(SeedDemoArgs {
        host,
        port,
        http_path: "/mcp/".to_string(),
        auth_bearer: "example-auth".to_string(),
        project_key: "/tmp/doctor-seed-demo-args-project".to_string(),
        agent_a: "SeedAlpha".to_string(),
        agent_b: "SeedBeta".to_string(),
        messages: 2,
        timeout_seconds: 3,
        log_file: Some(log_file.clone()),
    })
    .expect("run_seed_demo should succeed");

    let entries = server.transcripts.lock().expect("transcript lock").clone();
    assert!(entries.iter().all(|entry| entry.expected_tool_matched));
    assert!(entries.iter().all(|entry| entry.path == "/mcp/"));
    assert!(
        entries
            .iter()
            .all(|entry| entry.authorization.as_deref() == Some("Bearer example-auth"))
    );

    let send_messages: Vec<&TranscriptEntry> = entries
        .iter()
        .filter(|entry| entry.tool.as_deref() == Some("send_message"))
        .collect();
    assert_eq!(send_messages.len(), 2);

    let log = std::fs::read_to_string(&log_file).expect("read seed args log");
    assert!(log.contains("event=seed_start"));
    assert!(log.contains("event=seed_stage_started stage=ensure_project"));
    assert!(log.contains("event=seed_stage_completed stage=ensure_project"));
    assert!(log.contains("event=seed_stage_started stage=send_message iteration=1 from_agent=SeedAlpha to_agent=SeedBeta"));
    assert!(log.contains("event=seed_stage_completed stage=send_message iteration=1 from_agent=SeedAlpha to_agent=SeedBeta"));
    assert!(log.contains("event=seed_stage_started stage=send_message iteration=2 from_agent=SeedBeta to_agent=SeedAlpha"));
    assert!(log.contains("event=seed_stage_completed stage=send_message iteration=2 from_agent=SeedBeta to_agent=SeedAlpha"));
    assert!(log.contains("event=seed_stage_completed stage=fetch_inbox"));
    assert!(log.contains("event=seed_stage_completed stage=search_messages"));
    assert!(
        log.contains("event=seed_message_sent iteration=1 from_agent=SeedAlpha to_agent=SeedBeta")
    );
    assert!(
        log.contains("event=seed_message_sent iteration=2 from_agent=SeedBeta to_agent=SeedAlpha")
    );
    assert!(log.contains("event=seed_complete"));

    // Structured supervision evidence (bd-vf06l): each completed stage emits a
    // deterministic, machine-parseable supervision record alongside the existing
    // text logs.
    assert!(
        log.contains(
            "event=seed_supervision lane=seed step=ensure_project final_outcome=succeeded"
        ),
        "seed lane must emit structured supervision evidence per stage"
    );

    let first_send_body: Value =
        serde_json::from_str(&send_messages[0].request_body).expect("parse send_message payload");
    assert_eq!(
        first_send_body
            .pointer("/params/arguments/sender_name")
            .and_then(Value::as_str),
        Some("SeedAlpha")
    );
    assert_eq!(
        first_send_body
            .pointer("/params/arguments/to/0")
            .and_then(Value::as_str),
        Some("SeedBeta")
    );

    let second_send_body: Value =
        serde_json::from_str(&send_messages[1].request_body).expect("parse send_message payload");
    assert_eq!(
        second_send_body
            .pointer("/params/arguments/sender_name")
            .and_then(Value::as_str),
        Some("SeedBeta")
    );
    assert_eq!(
        second_send_body
            .pointer("/params/arguments/to/0")
            .and_then(Value::as_str),
        Some("SeedAlpha")
    );
}

#[test]
fn seed_demo_retries_transient_failures_and_preserves_auth_and_path() {
    let temp = tempdir().expect("tempdir");
    let log_file = temp.path().join("seed.log");
    let transcript_path = temp.path().join("seed_transcript.jsonl");

    let server = start_scripted_server(vec![
        ScriptedResponse::success("health_check"),
        ScriptedResponse::empty("ensure_project"),
        ScriptedResponse::success("ensure_project"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::json_value("send_message", json!({ "status": "non-rpc" })),
        ScriptedResponse::success("send_message"),
        ScriptedResponse::json_value(
            "fetch_inbox",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32000,
                    "message": "transient",
                }
            }),
        ),
        ScriptedResponse::success("fetch_inbox"),
        ScriptedResponse::success("search_messages"),
        ScriptedResponse::success("file_reservation_paths"),
    ]);

    let config = configured_seed_run(
        &server.endpoint,
        "mcp",
        "super-secret-token",
        Some(log_file.clone()),
        3,
    );

    run_seed_with_config(config).expect("seed run should succeed after retries");

    let entries = server.transcripts.lock().expect("transcript lock").clone();
    write_transcript_jsonl(&transcript_path, &entries);

    assert!(entries.iter().all(|entry| entry.expected_tool_matched));
    assert!(entries.iter().all(|entry| entry.path == "/mcp/"));
    assert!(
        entries
            .iter()
            .all(|entry| entry.authorization.as_deref() == Some("Bearer super-secret-token"))
    );

    let mut counts: HashMap<String, usize> = HashMap::new();
    for entry in &entries {
        if let Some(tool) = &entry.tool {
            *counts.entry(tool.clone()).or_insert(0) += 1;
        }
    }
    assert_eq!(counts.get("ensure_project"), Some(&2));
    assert_eq!(counts.get("send_message"), Some(&2));
    assert_eq!(counts.get("fetch_inbox"), Some(&2));

    let log = std::fs::read_to_string(&log_file).expect("read seed log");
    assert!(log.contains("event=seed_start"));
    assert!(log.contains("event=seed_stage_started stage=ensure_project"));
    assert!(log.contains("event=server_ready"));
    assert!(log.contains("event=rpc_retry_scheduled method=ensure_project"));
    assert!(log.contains("event=rpc_retry_scheduled method=send_message"));
    assert!(log.contains("event=rpc_retry_scheduled method=fetch_inbox"));
    assert!(log.contains("event=seed_stage_started stage=send_message iteration=1 from_agent=SeedAlpha to_agent=SeedBeta"));
    assert!(log.contains("event=seed_stage_completed stage=send_message iteration=1 from_agent=SeedAlpha to_agent=SeedBeta"));
    assert!(log.contains("event=seed_stage_completed stage=fetch_inbox"));
    assert!(
        log.contains("event=seed_message_sent iteration=1 from_agent=SeedAlpha to_agent=SeedBeta")
    );
    assert!(log.contains("event=seed_complete"));

    let transcript = std::fs::read_to_string(&transcript_path).expect("read transcript");
    assert!(transcript.contains("\"tool\":\"ensure_project\""));
    assert!(transcript.contains("\"path\":\"/mcp/\""));
}

#[test]
fn seed_demo_times_out_when_health_check_never_returns_result() {
    let temp = tempdir().expect("tempdir");
    let log_file = temp.path().join("seed_timeout.log");
    let mut responses = Vec::new();
    for _ in 0..16 {
        responses.push(ScriptedResponse::json_value(
            "health_check",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
            }),
        ));
    }

    let server = start_scripted_server(responses);
    let config = configured_seed_run(&server.endpoint, "/mcp/", "", Some(log_file.clone()), 1);

    let error =
        run_seed_with_config(config).expect_err("run should time out without health result");
    assert!(error.to_string().contains("Timed out waiting for server"));

    let entries = server.transcripts.lock().expect("transcript lock").clone();
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .all(|entry| entry.tool.as_deref() == Some("health_check"))
    );

    let log = std::fs::read_to_string(&log_file).expect("read timeout log");
    assert!(log.contains("event=server_probe_nonresult"));
    assert!(!log.contains("event=server_ready"));
}

#[test]
fn seed_demo_non_json_retries_exhaust_and_surface_clear_error() {
    let temp = tempdir().expect("tempdir");
    let log_file = temp.path().join("seed_retry_exhaust.log");
    let server = start_scripted_server(vec![
        ScriptedResponse::success("health_check"),
        ScriptedResponse::json_value("ensure_project", json!({ "status": "nope-1" })),
        ScriptedResponse::json_value("ensure_project", json!({ "status": "nope-2" })),
        ScriptedResponse::json_value("ensure_project", json!({ "status": "nope-3" })),
    ]);

    let config = configured_seed_run(&server.endpoint, "mcp", "", Some(log_file.clone()), 2);
    let error = run_seed_with_config(config).expect_err("run should fail after retry exhaustion");

    assert!(
        error
            .to_string()
            .contains("RPC non-JSON-RPC response for ensure_project")
    );

    let entries = server.transcripts.lock().expect("transcript lock").clone();
    let ensure_project_attempts = entries
        .iter()
        .filter(|entry| entry.tool.as_deref() == Some("ensure_project"))
        .count();
    assert_eq!(ensure_project_attempts, 3);

    let log = std::fs::read_to_string(&log_file).expect("read retry exhaust log");
    assert!(log.contains("event=seed_stage_failed stage=ensure_project"));
    assert!(log.contains("event=rpc_retry_scheduled method=ensure_project attempt=1"));
    assert!(log.contains("event=rpc_retry_scheduled method=ensure_project attempt=2"));
    assert!(log.contains("event=rpc_retry_exhausted method=ensure_project attempt=3"));
}

#[test]
fn seed_demo_send_message_failure_logs_stage_failure_and_stops_run() {
    let temp = tempdir().expect("tempdir");
    let log_file = temp.path().join("seed_send_failure.log");
    let server = start_scripted_server(vec![
        ScriptedResponse::success("health_check"),
        ScriptedResponse::success("ensure_project"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::json_value("send_message", json!({ "status": "nope-1" })),
        ScriptedResponse::json_value("send_message", json!({ "status": "nope-2" })),
        ScriptedResponse::json_value("send_message", json!({ "status": "nope-3" })),
    ]);

    let config = configured_seed_run(&server.endpoint, "mcp", "", Some(log_file.clone()), 2);
    let error =
        run_seed_with_config(config).expect_err("run should fail after send_message exhaustion");

    assert!(
        error
            .to_string()
            .contains("RPC non-JSON-RPC response for send_message")
    );

    let entries = server.transcripts.lock().expect("transcript lock").clone();
    let send_attempts = entries
        .iter()
        .filter(|entry| entry.tool.as_deref() == Some("send_message"))
        .count();
    assert_eq!(send_attempts, 3);
    assert!(
        !entries
            .iter()
            .any(|entry| entry.tool.as_deref() == Some("fetch_inbox"))
    );

    let log = std::fs::read_to_string(&log_file).expect("read send failure log");
    assert!(log.contains("event=seed_stage_started stage=send_message iteration=1 from_agent=SeedAlpha to_agent=SeedBeta"));
    assert!(log.contains("event=seed_stage_failed stage=send_message iteration=1 from_agent=SeedAlpha to_agent=SeedBeta"));
    assert!(log.contains("event=rpc_retry_scheduled method=send_message attempt=1"));
    assert!(log.contains("event=rpc_retry_scheduled method=send_message attempt=2"));
    assert!(log.contains("event=rpc_retry_exhausted method=send_message attempt=3"));
    assert!(!log.contains("event=seed_complete"));
}

#[test]
fn seed_demo_deadline_expires_mid_run_after_readiness() {
    let temp = tempdir().expect("tempdir");
    let log_file = temp.path().join("seed_midrun_timeout.log");
    let server = start_scripted_server(vec![
        ScriptedResponse::success("health_check"),
        ScriptedResponse::success("ensure_project").with_delay(1200),
    ]);

    let config = configured_seed_run(&server.endpoint, "mcp", "", Some(log_file.clone()), 1);
    let error = run_seed_with_config(config).expect_err("run should fail once seed budget expires");

    assert!(
        error
            .to_string()
            .contains("Seed deadline exceeded during ensure_project")
    );

    let log = std::fs::read_to_string(&log_file).expect("read midrun timeout log");
    assert!(log.contains("event=seed_start"));
    assert!(log.contains("event=server_ready"));
    assert!(log.contains("event=seed_stage_started stage=ensure_project"));
    assert!(log.contains("event=seed_stage_failed stage=ensure_project"));
    assert!(log.contains("event=seed_deadline_exceeded stage=ensure_project"));
    assert!(!log.contains("event=seed_complete"));
}

#[test]
fn seed_demo_plain_text_non_json_response_surfaces_parse_error() {
    let server = start_scripted_server(vec![
        ScriptedResponse::success("health_check"),
        ScriptedResponse::text("ensure_project", "plain-text-not-json"),
    ]);

    let config = configured_seed_run(&server.endpoint, "mcp", "", None, 2);
    let error = run_seed_with_config(config).expect_err("plain text response should fail");
    assert!(error.to_string().contains("JSON error"));
}

#[test]
fn seed_demo_wait_loop_recovers_when_health_becomes_ready() {
    let temp = tempdir().expect("tempdir");
    let log_file = temp.path().join("seed_wait.log");
    let server = start_scripted_server(vec![
        ScriptedResponse::json_value(
            "health_check",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
            }),
        ),
        ScriptedResponse::success("health_check"),
        ScriptedResponse::success("ensure_project"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::success("send_message"),
        ScriptedResponse::success("fetch_inbox"),
        ScriptedResponse::success("search_messages"),
        ScriptedResponse::success("file_reservation_paths"),
    ]);

    let config = configured_seed_run(&server.endpoint, "mcp", "", Some(log_file.clone()), 3);
    run_seed_with_config(config).expect("seed run should recover after delayed health success");

    let entries = server.transcripts.lock().expect("transcript lock").clone();
    let health_checks = entries
        .iter()
        .filter(|entry| entry.tool.as_deref() == Some("health_check"))
        .count();
    assert!(health_checks >= 2);

    let log = std::fs::read_to_string(&log_file).expect("read wait log");
    assert!(log.contains("event=server_probe_nonresult attempt=1"));
    assert!(log.contains("event=server_ready attempt=2"));
}

#[test]
fn seed_demo_reservation_failure_is_warning_only_and_command_still_succeeds() {
    let temp = tempdir().expect("tempdir");
    let log_file = temp.path().join("seed_reservation_warning.log");
    let server = start_scripted_server(vec![
        ScriptedResponse::success("health_check"),
        ScriptedResponse::success("ensure_project"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::success("register_agent"),
        ScriptedResponse::success("send_message"),
        ScriptedResponse::success("fetch_inbox"),
        ScriptedResponse::success("search_messages"),
        ScriptedResponse::json_value(
            "file_reservation_paths",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32000,
                    "message": "reservation transient failure",
                },
            }),
        ),
        ScriptedResponse::json_value(
            "file_reservation_paths",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32000,
                    "message": "reservation transient failure",
                },
            }),
        ),
        ScriptedResponse::json_value(
            "file_reservation_paths",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32000,
                    "message": "reservation transient failure",
                },
            }),
        ),
    ]);

    let config = configured_seed_run(&server.endpoint, "mcp", "", Some(log_file.clone()), 3);
    run_seed_with_config(config).expect("file_reservation_paths failure should be warning-only");

    let entries = server.transcripts.lock().expect("transcript lock").clone();
    let reservation_attempts = entries
        .iter()
        .filter(|entry| entry.tool.as_deref() == Some("file_reservation_paths"))
        .count();
    assert_eq!(reservation_attempts, 3);

    let log = std::fs::read_to_string(&log_file).expect("read reservation warning log");
    assert!(log.contains("event=seed_stage_started stage=file_reservation_paths"));
    assert!(log.contains("event=seed_stage_failed stage=file_reservation_paths"));
    assert!(log.contains("event=seed_reservation_warning agent_name=SeedAlpha"));
    assert!(log.contains("event=seed_complete"));
}
