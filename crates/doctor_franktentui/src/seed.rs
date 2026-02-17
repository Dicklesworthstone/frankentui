use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use clap::Args;
use reqwest::blocking::Client;
use serde_json::{Value, json};

use crate::error::{DoctorError, Result};
use crate::util::{OutputIntegration, append_line, normalize_http_path, now_utc_iso, output_for};

#[derive(Debug, Clone, Args)]
pub struct SeedDemoArgs {
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long, default_value = "8879")]
    pub port: String,

    #[arg(long = "path", default_value = "/mcp/")]
    pub http_path: String,

    #[arg(long, default_value = "")]
    pub auth_token: String,

    #[arg(long, default_value = "/tmp/tui_inspector_demo_project")]
    pub project_key: String,

    #[arg(long = "agent-a", default_value = "InspectorRed")]
    pub agent_a: String,

    #[arg(long = "agent-b", default_value = "InspectorBlue")]
    pub agent_b: String,

    #[arg(long = "messages", default_value_t = 6, value_parser = clap::value_parser!(u32).range(1..))]
    pub messages: u32,

    #[arg(long = "timeout", default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout_seconds: u64,

    #[arg(long = "log-file")]
    pub log_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SeedDemoConfig {
    pub host: String,
    pub port: String,
    pub http_path: String,
    pub auth_token: String,
    pub project_key: String,
    pub agent_a: String,
    pub agent_b: String,
    pub messages: u32,
    pub timeout_seconds: u64,
    pub log_file: Option<PathBuf>,
}

impl From<SeedDemoArgs> for SeedDemoConfig {
    fn from(args: SeedDemoArgs) -> Self {
        Self {
            host: args.host,
            port: args.port,
            http_path: args.http_path,
            auth_token: args.auth_token,
            project_key: args.project_key,
            agent_a: args.agent_a,
            agent_b: args.agent_b,
            messages: args.messages,
            timeout_seconds: args.timeout_seconds,
            log_file: args.log_file,
        }
    }
}

#[derive(Debug)]
struct RpcClient {
    client: Client,
    endpoint: String,
    auth_token: String,
    counter: u64,
    log_file: Option<PathBuf>,
}

impl RpcClient {
    fn new(config: &SeedDemoConfig) -> Result<Self> {
        let http_path = normalize_http_path(&config.http_path);
        let endpoint = format!("http://{}:{}{}", config.host, config.port, http_path);
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(10))
            .build()?;

        Ok(Self {
            client,
            endpoint,
            auth_token: config.auth_token.clone(),
            counter: 0,
            log_file: config.log_file.clone(),
        })
    }

    fn log_response(&self, method: &str, payload: &str) -> Result<()> {
        if let Some(path) = &self.log_file {
            append_line(path, &format!("[{}] {method} {payload}", now_utc_iso()))?;
        }
        Ok(())
    }

    fn should_retry(error: &DoctorError) -> bool {
        match error {
            DoctorError::Http(_) => true,
            DoctorError::InvalidArgument { message } => {
                message.contains("empty response")
                    || message.contains("non-JSON-RPC response")
                    || message.contains("RPC error")
            }
            _ => false,
        }
    }

    fn call_tool_once(&mut self, method: &str, arguments: Value) -> Result<Value> {
        self.counter = self.counter.saturating_add(1);

        let request_payload = json!({
            "jsonrpc": "2.0",
            "id": self.counter,
            "method": "tools/call",
            "params": {
                "name": method,
                "arguments": arguments,
            }
        });

        let mut request = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .json(&request_payload);

        if !self.auth_token.is_empty() {
            request = request.bearer_auth(&self.auth_token);
        }

        let response_text = request.send()?.text()?;
        self.log_response(method, &response_text)?;

        if response_text.trim().is_empty() {
            return Err(DoctorError::invalid(format!(
                "RPC empty response for {method}"
            )));
        }

        let parsed: Value = serde_json::from_str(&response_text)?;

        if parsed.get("jsonrpc").is_none() {
            return Err(DoctorError::invalid(format!(
                "RPC non-JSON-RPC response for {method}: {response_text}"
            )));
        }

        if parsed.get("error").is_some() {
            return Err(DoctorError::invalid(format!(
                "RPC error for {method}: {response_text}"
            )));
        }

        Ok(parsed)
    }

    fn call_tool(&mut self, method: &str, arguments: Value) -> Result<Value> {
        let mut attempt = 0_u32;
        loop {
            attempt = attempt.saturating_add(1);
            match self.call_tool_once(method, arguments.clone()) {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if attempt >= 3 || !Self::should_retry(&error) {
                        return Err(error);
                    }

                    let backoff_ms = 100_u64.saturating_mul(1_u64 << (attempt - 1));
                    if let Some(path) = &self.log_file {
                        let _ = append_line(
                            path,
                            &format!(
                                "[{}] retry method={} attempt={} backoff_ms={} reason={}",
                                now_utc_iso(),
                                method,
                                attempt,
                                backoff_ms,
                                error
                            ),
                        );
                    }
                    thread::sleep(Duration::from_millis(backoff_ms));
                }
            }
        }
    }
}

fn wait_for_server(client: &mut RpcClient, timeout_seconds: u64) -> Result<()> {
    let start = Instant::now();

    loop {
        if let Ok(response) = client.call_tool_once("health_check", json!({}))
            && response.get("result").is_some()
        {
            return Ok(());
        }

        if start.elapsed() >= Duration::from_secs(timeout_seconds) {
            return Err(DoctorError::invalid(format!(
                "Timed out waiting for server at {}",
                client.endpoint
            )));
        }

        thread::sleep(Duration::from_secs(1));
    }
}

pub fn run_seed_demo(args: SeedDemoArgs) -> Result<()> {
    run_seed_with_config(args.into())
}

pub fn run_seed_with_config(config: SeedDemoConfig) -> Result<()> {
    let integration = OutputIntegration::detect();
    let ui = output_for(&integration);
    let mut client = RpcClient::new(&config)?;

    ui.info(&format!("waiting for MCP server at {}", client.endpoint));
    wait_for_server(&mut client, config.timeout_seconds)?;
    ui.info("seeding demo data");

    let project_key = config.project_key.clone();
    let agent_a = config.agent_a.clone();
    let agent_b = config.agent_b.clone();

    client.call_tool("ensure_project", json!({ "human_key": project_key }))?;
    client.call_tool(
        "register_agent",
        json!({
            "project_key": config.project_key,
            "program": "doctor_franktentui",
            "model": "gpt-5-codex",
            "name": agent_a,
            "task_description": "demo sender",
        }),
    )?;
    client.call_tool(
        "register_agent",
        json!({
            "project_key": config.project_key,
            "program": "doctor_franktentui",
            "model": "gpt-5-codex",
            "name": agent_b,
            "task_description": "demo receiver",
        }),
    )?;

    for i in 1..=config.messages {
        let (from_agent, to_agent) = if i % 2 == 1 {
            (&config.agent_a, &config.agent_b)
        } else {
            (&config.agent_b, &config.agent_a)
        };

        client.call_tool(
            "send_message",
            json!({
                "project_key": config.project_key,
                "sender_name": from_agent,
                "to": [to_agent],
                "subject": format!("Inspector demo message {i}"),
                "body_md": format!("Seeded by doctor_franktentui run. Iteration {i}."),
            }),
        )?;
    }

    client.call_tool(
        "fetch_inbox",
        json!({
            "project_key": config.project_key,
            "agent_name": config.agent_b,
            "limit": 20,
        }),
    )?;

    client.call_tool(
        "search_messages",
        json!({
            "project_key": config.project_key,
            "query": "Inspector",
            "limit": 20,
        }),
    )?;

    if let Err(error) = client.call_tool(
        "file_reservation_paths",
        json!({
            "project_key": config.project_key,
            "agent_name": config.agent_a,
            "paths": ["crates/mcp-agent-mail-server/src/tui_screens/analytics.rs"],
            "ttl_seconds": 3600,
            "exclusive": false,
            "reason": "doctor-franktentui-demo",
        }),
    ) {
        ui.warning(&format!("file_reservation_paths failed: {error}"));
    }

    ui.success("seed complete");
    ui.info(&format!("project_key: {}", config.project_key));
    ui.info(&format!("agents: {}, {}", config.agent_a, config.agent_b));
    ui.info(&format!("messages: {}", config.messages));

    if integration.should_emit_json() {
        println!(
            "{}",
            json!({
                "command": "seed-demo",
                "status": "ok",
                "project_key": config.project_key,
                "agent_a": config.agent_a,
                "agent_b": config.agent_b,
                "messages": config.messages,
                "endpoint": client.endpoint,
                "integration": integration,
            })
        );
    }

    Ok(())
}
