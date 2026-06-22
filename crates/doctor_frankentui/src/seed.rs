use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use clap::Args;
use ftui_runtime::cancellation::CancellationToken;
use reqwest::blocking::Client;
use serde_json::{Value, json};

use crate::error::{DoctorError, Result};
use crate::supervised_orchestration::{
    AttemptEvidence, AttemptOutcomeKind, Backoff, CancelReason, StepOutcome, SupervisionBudget,
    SupervisionRecord,
};
use crate::util::{OutputIntegration, append_line, normalize_http_path, now_utc_iso, output_for};

const RPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const RPC_RETRY_MAX_ATTEMPTS: u32 = 3;
const RPC_RETRY_BASE_BACKOFF_MS: u64 = 100;
const SERVER_READY_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Args)]
pub struct SeedDemoArgs {
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long, default_value = "8879")]
    pub port: String,

    #[arg(long = "path", default_value = "/mcp/")]
    pub http_path: String,

    #[arg(long = "auth-token", default_value = "")]
    pub auth_bearer: String,

    #[arg(long, default_value = "/tmp/tui_inspector_demo_project")]
    pub project_key: String,

    #[arg(long = "agent-a", default_value = "CrimsonHarbor")]
    pub agent_a: String,

    #[arg(long = "agent-b", default_value = "AzureMeadow")]
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
    pub auth_bearer: String,
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
            auth_bearer: args.auth_bearer,
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
    auth_bearer: String,
    counter: u64,
    log_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RetryPolicy {
    max_attempts: u32,
    base_backoff_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: RPC_RETRY_MAX_ATTEMPTS,
            base_backoff_ms: RPC_RETRY_BASE_BACKOFF_MS,
        }
    }
}

impl RetryPolicy {
    fn should_retry(self, attempt: u32, error: &DoctorError) -> bool {
        attempt < self.max_attempts && RpcClient::should_retry(error)
    }

    fn backoff_for_attempt(self, attempt: u32) -> Duration {
        Duration::from_millis(
            self.base_backoff_ms
                .saturating_mul(1_u64 << attempt.saturating_sub(1)),
        )
    }
}

/// Deadline tracks timeout and optional structured cancellation.
///
/// When a `CancellationToken` is attached, sleeps become cancellation-aware
/// and the `is_cancelled` method can be used to check for early termination.
#[derive(Clone)]
struct Deadline {
    started_at: Instant,
    timeout: Duration,
    cancel_token: Option<CancellationToken>,
}

impl Deadline {
    fn after(timeout: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
            cancel_token: None,
        }
    }

    fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    fn remaining(&self) -> Duration {
        self.timeout.saturating_sub(self.elapsed())
    }

    fn is_expired(&self) -> bool {
        self.elapsed() >= self.timeout
    }

    /// Check if the associated cancellation token has been triggered.
    fn is_cancelled(&self) -> bool {
        self.cancel_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }

    /// Sleep for at most `poll_interval`, returning early if cancelled.
    ///
    /// Returns `None` if deadline expired or cancelled, `Some(slept)` otherwise.
    fn cancellation_aware_sleep(&self, poll_interval: Duration) -> Option<Duration> {
        let remaining = self.remaining();
        if remaining.is_zero() {
            return None;
        }
        let sleep_for = remaining.min(poll_interval);

        if let Some(token) = &self.cancel_token {
            // Use token.wait_timeout which returns early on cancellation
            let was_cancelled = token.wait_timeout(sleep_for);
            if was_cancelled {
                return None;
            }
        } else {
            thread::sleep(sleep_for);
        }

        Some(sleep_for)
    }

    /// Legacy method for backwards compatibility - use cancellation_aware_sleep instead.
    fn next_sleep(&self, poll_interval: Duration) -> Option<Duration> {
        let remaining = self.remaining();
        if remaining.is_zero() {
            None
        } else {
            Some(remaining.min(poll_interval))
        }
    }
}

fn deadline_exceeded_error(stage: &str) -> DoctorError {
    DoctorError::invalid(format!("Seed deadline exceeded during {stage}"))
}

fn log_stage_started(client: &RpcClient, stage: &str, deadline: &Deadline) {
    let _ = client.log_line(&format!(
        "event=seed_stage_started stage={stage} elapsed_ms={} remaining_ms={}",
        deadline.elapsed().as_millis(),
        deadline.remaining().as_millis()
    ));
}

fn log_stage_completed(client: &RpcClient, stage: &str, deadline: &Deadline) {
    let _ = client.log_line(&format!(
        "event=seed_stage_completed stage={stage} elapsed_ms={} remaining_ms={}",
        deadline.elapsed().as_millis(),
        deadline.remaining().as_millis()
    ));
}

fn log_stage_failed(client: &RpcClient, stage: &str, deadline: &Deadline, error: &DoctorError) {
    let _ = client.log_line(&format!(
        "event=seed_stage_failed stage={stage} elapsed_ms={} remaining_ms={} reason={error}",
        deadline.elapsed().as_millis(),
        deadline.remaining().as_millis()
    ));
}

fn log_message_stage_started(
    client: &RpcClient,
    deadline: &Deadline,
    iteration: u32,
    from_agent: &str,
    to_agent: &str,
) {
    let _ = client.log_line(&format!(
        "event=seed_stage_started stage=send_message iteration={iteration} from_agent={from_agent} to_agent={to_agent} elapsed_ms={} remaining_ms={}",
        deadline.elapsed().as_millis(),
        deadline.remaining().as_millis()
    ));
}

fn log_message_stage_completed(
    client: &RpcClient,
    deadline: &Deadline,
    iteration: u32,
    from_agent: &str,
    to_agent: &str,
) {
    let _ = client.log_line(&format!(
        "event=seed_stage_completed stage=send_message iteration={iteration} from_agent={from_agent} to_agent={to_agent} elapsed_ms={} remaining_ms={}",
        deadline.elapsed().as_millis(),
        deadline.remaining().as_millis()
    ));
}

fn log_message_stage_failed(
    client: &RpcClient,
    deadline: &Deadline,
    iteration: u32,
    from_agent: &str,
    to_agent: &str,
    error: &DoctorError,
) {
    let _ = client.log_line(&format!(
        "event=seed_stage_failed stage=send_message iteration={iteration} from_agent={from_agent} to_agent={to_agent} elapsed_ms={} remaining_ms={} reason={error}",
        deadline.elapsed().as_millis(),
        deadline.remaining().as_millis()
    ));
}

/// Classify a stage failure into the supervision outcome lattice, using the
/// deadline's cancellation/expiry state to distinguish timeout and cancellation
/// from an ordinary failure.
fn classify_stage_failure(deadline: &Deadline) -> StepOutcome {
    if deadline.is_cancelled() {
        StepOutcome::Cancelled {
            reason: CancelReason::Caller,
        }
    } else if deadline.is_expired() {
        StepOutcome::TimedOut
    } else {
        StepOutcome::Failed {
            reason: "stage_failed".to_string(),
        }
    }
}

/// Emit a deterministic, stage-level [`SupervisionRecord`] for the seed lane.
///
/// This is additive structured evidence layered over the existing per-stage text
/// logs: the RPC-level retry/backoff detail stays in the `rpc_*`/`seed_stage_*`
/// lines, while this one machine-parseable `event=seed_supervision` line carries
/// the stage's outcome in the shared supervision vocabulary so it can feed the
/// evidence ledger and operator triage uniformly across lanes.
fn emit_stage_supervision(
    client: &RpcClient,
    stage: &str,
    deadline: &Deadline,
    outcome: StepOutcome,
) {
    let attempt_kind = match &outcome {
        StepOutcome::Succeeded => AttemptOutcomeKind::Ok,
        StepOutcome::Failed { .. } => AttemptOutcomeKind::Fatal,
        StepOutcome::TimedOut => AttemptOutcomeKind::TimedOut,
        StepOutcome::Cancelled { .. } => AttemptOutcomeKind::Cancelled,
    };
    let cancel_reason = match &outcome {
        StepOutcome::Cancelled { reason } => Some(*reason),
        StepOutcome::TimedOut => Some(CancelReason::Deadline),
        _ => None,
    };
    let record = SupervisionRecord {
        lane: "seed".to_string(),
        step: stage.to_string(),
        budget: SupervisionBudget::new(deadline.timeout, 1),
        backoff: Backoff::exponential(
            RPC_RETRY_BASE_BACKOFF_MS,
            RPC_RETRY_BASE_BACKOFF_MS << RPC_RETRY_MAX_ATTEMPTS.saturating_sub(1),
        ),
        attempts: vec![AttemptEvidence {
            index: 1,
            outcome: attempt_kind,
            detail: None,
        }],
        attempts_used: 1,
        final_outcome: outcome,
        cancel_reason,
    };
    let _ = client.log_line(&format!(
        "event=seed_supervision {}",
        record.evidence_terms().join(" ")
    ));
}

fn run_seed_stage(
    client: &mut RpcClient,
    stage: &str,
    arguments: Value,
    deadline: &Deadline,
) -> Result<Value> {
    log_stage_started(client, stage, deadline);
    match client.call_tool(stage, arguments, deadline) {
        Ok(value) => {
            log_stage_completed(client, stage, deadline);
            emit_stage_supervision(client, stage, deadline, StepOutcome::Succeeded);
            Ok(value)
        }
        Err(error) => {
            log_stage_failed(client, stage, deadline, &error);
            emit_stage_supervision(client, stage, deadline, classify_stage_failure(deadline));
            Err(error)
        }
    }
}

impl RpcClient {
    fn new(config: &SeedDemoConfig) -> Result<Self> {
        let http_path = normalize_http_path(&config.http_path);
        let endpoint = format!("http://{}:{}{}", config.host, config.port, http_path);
        let client = Client::builder()
            .connect_timeout(RPC_CONNECT_TIMEOUT)
            .timeout(RPC_REQUEST_TIMEOUT)
            .build()?;

        Ok(Self {
            client,
            endpoint,
            auth_bearer: config.auth_bearer.clone(),
            counter: 0,
            log_file: config.log_file.clone(),
        })
    }

    fn log_response(&self, method: &str, payload: &str) -> Result<()> {
        self.log_line(&format!("{method} {payload}"))
    }

    fn log_line(&self, line: &str) -> Result<()> {
        if let Some(path) = &self.log_file {
            append_line(path, &format!("[{}] {line}", now_utc_iso()))?;
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

    fn parsed_tool_error(parsed: &Value) -> bool {
        parsed
            .get("result")
            .and_then(|result| result.get("isError"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn call_tool_once(
        &mut self,
        method: &str,
        arguments: Value,
        deadline: &Deadline,
    ) -> Result<Value> {
        self.counter = self.counter.saturating_add(1);

        // Check cancellation first
        if deadline.is_cancelled() {
            let _ = self.log_line(&format!(
                "event=seed_cancelled stage={method} elapsed_ms={}",
                deadline.elapsed().as_millis()
            ));
            return Err(DoctorError::invalid(format!(
                "Seed cancelled during {method}"
            )));
        }

        if deadline.is_expired() {
            let _ = self.log_line(&format!(
                "event=seed_deadline_exceeded stage={method} elapsed_ms={}",
                deadline.elapsed().as_millis()
            ));
            return Err(deadline_exceeded_error(method));
        }

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
            .timeout(deadline.remaining().min(RPC_REQUEST_TIMEOUT))
            .json(&request_payload);

        if !self.auth_bearer.is_empty() {
            request = request.bearer_auth(&self.auth_bearer);
        }

        let response_text = request.send()?.text()?;
        if deadline.is_expired() {
            let _ = self.log_line(&format!(
                "event=seed_deadline_exceeded stage={method} elapsed_ms={}",
                deadline.elapsed().as_millis()
            ));
            return Err(deadline_exceeded_error(method));
        }
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
        if Self::parsed_tool_error(&parsed) {
            return Err(DoctorError::invalid(format!(
                "MCP tool error for {method}: {response_text}"
            )));
        }

        Ok(parsed)
    }

    fn call_tool(&mut self, method: &str, arguments: Value, deadline: &Deadline) -> Result<Value> {
        let policy = RetryPolicy::default();
        let mut attempt = 0_u32;
        loop {
            // Check cancellation at loop start
            if deadline.is_cancelled() {
                let _ = self.log_line(&format!(
                    "event=rpc_cancelled method={method} attempt={attempt} elapsed_ms={}",
                    deadline.elapsed().as_millis()
                ));
                return Err(DoctorError::invalid(format!(
                    "RPC cancelled during {method}"
                )));
            }

            attempt = attempt.saturating_add(1);
            match self.call_tool_once(method, arguments.clone(), deadline) {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if !policy.should_retry(attempt, &error) {
                        let _ = self.log_line(&format!(
                            "event=rpc_retry_exhausted method={method} attempt={attempt} reason={error}"
                        ));
                        return Err(error);
                    }

                    let backoff = policy.backoff_for_attempt(attempt);
                    let Some(clamped_backoff) = deadline.next_sleep(backoff) else {
                        let _ = self.log_line(&format!(
                            "event=seed_deadline_exceeded stage={method} elapsed_ms={}",
                            deadline.elapsed().as_millis()
                        ));
                        let _ = self.log_line(&format!(
                            "event=rpc_retry_exhausted method={method} attempt={attempt} reason={error}"
                        ));
                        return Err(deadline_exceeded_error(method));
                    };
                    let _ = self.log_line(&format!(
                        "event=rpc_retry_scheduled method={method} attempt={attempt} backoff_ms={} reason={error}",
                        clamped_backoff.as_millis()
                    ));

                    // Use cancellation-aware sleep for backoff
                    if deadline.cancellation_aware_sleep(clamped_backoff).is_none()
                        && deadline.is_cancelled()
                    {
                        let _ = self.log_line(&format!(
                            "event=rpc_cancelled method={method} attempt={attempt} elapsed_ms={}",
                            deadline.elapsed().as_millis()
                        ));
                        return Err(DoctorError::invalid(format!(
                            "RPC cancelled during {method} backoff"
                        )));
                    }
                }
            }
        }
    }
}

fn wait_for_server(client: &mut RpcClient, deadline: &Deadline) -> Result<()> {
    let mut attempt = 0_u32;

    loop {
        // Check cancellation before each attempt
        if deadline.is_cancelled() {
            let _ = client.log_line(&format!(
                "event=server_wait_cancelled attempt={attempt} elapsed_ms={}",
                deadline.elapsed().as_millis()
            ));
            return Err(DoctorError::invalid(format!(
                "Cancelled waiting for server at {}",
                client.endpoint
            )));
        }

        attempt = attempt.saturating_add(1);
        match client.call_tool_once("health_check", json!({}), deadline) {
            Ok(response) if response.get("result").is_some() => {
                let _ = client.log_line(&format!(
                    "event=server_ready attempt={attempt} elapsed_ms={}",
                    deadline.elapsed().as_millis()
                ));
                return Ok(());
            }
            Ok(_) => {
                let _ = client.log_line(&format!(
                    "event=server_probe_nonresult attempt={attempt} remaining_ms={}",
                    deadline.remaining().as_millis()
                ));
            }
            Err(error) => {
                let _ = client.log_line(&format!(
                    "event=server_probe_retry attempt={attempt} remaining_ms={} reason={error}",
                    deadline.remaining().as_millis()
                ));
            }
        }

        if deadline.is_expired() {
            return Err(DoctorError::invalid(format!(
                "Timed out waiting for server at {}",
                client.endpoint
            )));
        }

        // Use cancellation-aware sleep instead of thread::sleep
        if deadline
            .cancellation_aware_sleep(SERVER_READY_POLL_INTERVAL)
            .is_none()
        {
            // Sleep returned early due to cancellation or expiry
            if deadline.is_cancelled() {
                let _ = client.log_line(&format!(
                    "event=server_wait_cancelled attempt={attempt} elapsed_ms={}",
                    deadline.elapsed().as_millis()
                ));
                return Err(DoctorError::invalid(format!(
                    "Cancelled waiting for server at {}",
                    client.endpoint
                )));
            }
        }
    }
}

pub fn run_seed_demo(args: SeedDemoArgs) -> Result<()> {
    run_seed_with_config(args.into())
}

fn seed_summary_payload(
    config: &SeedDemoConfig,
    endpoint: &str,
    integration: &OutputIntegration,
) -> Value {
    json!({
        "command": "seed-demo",
        "status": "ok",
        "project_key": config.project_key,
        "agent_a": config.agent_a,
        "agent_b": config.agent_b,
        "messages": config.messages,
        "endpoint": endpoint,
        "integration": integration,
    })
}

pub fn run_seed_with_config(config: SeedDemoConfig) -> Result<()> {
    let integration = OutputIntegration::detect();
    let ui = output_for(&integration);
    let mut client = RpcClient::new(&config)?;
    let deadline = Deadline::after(Duration::from_secs(config.timeout_seconds));

    let _ = client.log_line(&format!(
        "event=seed_start endpoint={} project_key={} agent_a={} agent_b={} messages={} timeout_seconds={}",
        client.endpoint,
        config.project_key,
        config.agent_a,
        config.agent_b,
        config.messages,
        config.timeout_seconds
    ));
    ui.info(&format!("waiting for MCP server at {}", client.endpoint));
    wait_for_server(&mut client, &deadline)?;
    ui.info("seeding demo data");

    let project_key = config.project_key.clone();
    let agent_a = config.agent_a.clone();
    let agent_b = config.agent_b.clone();

    run_seed_stage(
        &mut client,
        "ensure_project",
        json!({ "human_key": project_key }),
        &deadline,
    )?;
    run_seed_stage(
        &mut client,
        "register_agent",
        json!({
            "project_key": config.project_key,
            "program": "doctor_frankentui",
            "model": "gpt-5-codex",
            "name": agent_a,
            "task_description": "demo sender",
        }),
        &deadline,
    )?;
    run_seed_stage(
        &mut client,
        "register_agent",
        json!({
            "project_key": config.project_key,
            "program": "doctor_frankentui",
            "model": "gpt-5-codex",
            "name": agent_b,
            "task_description": "demo receiver",
        }),
        &deadline,
    )?;

    for i in 1..=config.messages {
        let (from_agent, to_agent) = if i % 2 == 1 {
            (&config.agent_a, &config.agent_b)
        } else {
            (&config.agent_b, &config.agent_a)
        };

        log_message_stage_started(&client, &deadline, i, from_agent, to_agent);
        client
            .call_tool(
                "send_message",
                json!({
                    "project_key": config.project_key,
                    "sender_name": from_agent,
                    "to": [to_agent],
                    "subject": format!("Inspector demo message {i}"),
                    "body_md": format!("Seeded by doctor_frankentui run. Iteration {i}."),
                }),
                &deadline,
            )
            .inspect_err(|error| {
                log_message_stage_failed(&client, &deadline, i, from_agent, to_agent, error)
            })?;
        log_message_stage_completed(&client, &deadline, i, from_agent, to_agent);
        let _ = client.log_line(&format!(
            "event=seed_message_sent iteration={i} from_agent={from_agent} to_agent={to_agent}"
        ));
    }

    run_seed_stage(
        &mut client,
        "fetch_inbox",
        json!({
            "project_key": config.project_key,
            "agent_name": config.agent_b,
            "limit": 20,
        }),
        &deadline,
    )?;

    run_seed_stage(
        &mut client,
        "search_messages",
        json!({
            "project_key": config.project_key,
            "query": "Inspector",
            "limit": 20,
        }),
        &deadline,
    )?;

    log_stage_started(&client, "file_reservation_paths", &deadline);
    if let Err(error) = client.call_tool(
        "file_reservation_paths",
        json!({
            "project_key": config.project_key,
            "agent_name": config.agent_a,
            "paths": ["crates/mcp-agent-mail-server/src/tui_screens/analytics.rs"],
            "ttl_seconds": 3600,
            "exclusive": false,
            "reason": "doctor-frankentui-demo",
        }),
        &deadline,
    ) {
        let _ = client.log_line(&format!(
            "event=seed_reservation_warning agent_name={} reason={error}",
            config.agent_a
        ));
        log_stage_failed(&client, "file_reservation_paths", &deadline, &error);
        ui.warning(&format!("file_reservation_paths failed: {error}"));
    } else {
        log_stage_completed(&client, "file_reservation_paths", &deadline);
    }

    let _ = client.log_line(&format!(
        "event=seed_complete endpoint={} project_key={} agent_a={} agent_b={} messages={}",
        client.endpoint, config.project_key, config.agent_a, config.agent_b, config.messages
    ));
    ui.success("seed complete");
    ui.info(&format!("project_key: {}", config.project_key));
    ui.info(&format!("agents: {}, {}", config.agent_a, config.agent_b));
    ui.info(&format!("messages: {}", config.messages));

    if integration.should_emit_json() {
        println!(
            "{}",
            seed_summary_payload(&config, &client.endpoint, &integration)
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Deadline, RPC_RETRY_BASE_BACKOFF_MS, RetryPolicy, RpcClient, SeedDemoConfig,
        classify_stage_failure, wait_for_server,
    };
    use crate::error::DoctorError;
    use crate::supervised_orchestration::{CancelReason, StepOutcome};
    use crate::util::OutputIntegration;
    use ftui_runtime::cancellation::CancellationSource;
    use serde_json::json;
    use std::time::{Duration, Instant};

    #[test]
    fn classify_stage_failure_maps_deadline_state_to_outcome_lattice() {
        // Healthy deadline (not expired, not cancelled) -> ordinary failure.
        let live = Deadline::after(Duration::from_secs(30));
        assert!(matches!(
            classify_stage_failure(&live),
            StepOutcome::Failed { .. }
        ));

        // Expired deadline -> timed out.
        let expired = Deadline::after(Duration::ZERO);
        assert_eq!(classify_stage_failure(&expired), StepOutcome::TimedOut);

        // Cancelled token -> cancelled by caller.
        let source = CancellationSource::new();
        source.cancel();
        let cancelled = Deadline {
            started_at: Instant::now(),
            timeout: Duration::from_secs(30),
            cancel_token: Some(source.token()),
        };
        assert_eq!(
            classify_stage_failure(&cancelled),
            StepOutcome::Cancelled {
                reason: CancelReason::Caller
            }
        );
    }

    #[test]
    fn should_retry_matches_retryable_invalid_argument_messages() {
        let empty_response = DoctorError::invalid("RPC empty response for health_check");
        let non_json = DoctorError::invalid("RPC non-JSON-RPC response for health_check: nope");
        let rpc_error = DoctorError::invalid("RPC error for send_message: {\"error\":true}");
        let tool_error = DoctorError::invalid(
            "MCP tool error for register_agent: {\"result\":{\"isError\":true}}",
        );
        let other_invalid = DoctorError::invalid("some other validation error");

        assert!(RpcClient::should_retry(&empty_response));
        assert!(RpcClient::should_retry(&non_json));
        assert!(RpcClient::should_retry(&rpc_error));
        assert!(!RpcClient::should_retry(&tool_error));
        assert!(!RpcClient::should_retry(&other_invalid));
        assert!(!RpcClient::should_retry(&DoctorError::MissingCommand {
            command: "vhs".to_string(),
        }));
    }

    #[test]
    fn parsed_tool_error_detects_mcp_tool_failures() {
        assert!(RpcClient::parsed_tool_error(&json!({
            "result": {
                "isError": true,
                "content": [{"type": "text", "text": "bad"}]
            }
        })));
        assert!(!RpcClient::parsed_tool_error(&json!({
            "result": {
                "content": [{"type": "text", "text": "ok"}]
            }
        })));
    }

    #[test]
    fn should_retry_returns_true_for_http_errors() {
        let config = SeedDemoConfig {
            host: "127.0.0.1".to_string(),
            port: "not-a-port".to_string(),
            http_path: "/mcp/".to_string(),
            auth_bearer: String::new(),
            project_key: "/tmp/project".to_string(),
            agent_a: "A".to_string(),
            agent_b: "B".to_string(),
            messages: 1,
            timeout_seconds: 1,
            log_file: None,
        };

        let mut client = RpcClient::new(&config).expect("rpc client");
        let error = client
            .call_tool_once(
                "health_check",
                json!({}),
                &Deadline::after(Duration::from_secs(1)),
            )
            .expect_err("invalid URL should surface HTTP error");
        assert!(matches!(error, DoctorError::Http(_)));
        assert!(RpcClient::should_retry(&error));
    }

    #[test]
    fn rpc_client_new_normalizes_http_path_in_endpoint() {
        let config = SeedDemoConfig {
            host: "127.0.0.1".to_string(),
            port: "8879".to_string(),
            http_path: "mcp".to_string(),
            auth_bearer: String::new(),
            project_key: "/tmp/project".to_string(),
            agent_a: "A".to_string(),
            agent_b: "B".to_string(),
            messages: 1,
            timeout_seconds: 2,
            log_file: None,
        };

        let client = RpcClient::new(&config).expect("rpc client");
        assert_eq!(client.endpoint, "http://127.0.0.1:8879/mcp/");
    }

    #[test]
    fn wait_for_server_times_out_for_unreachable_endpoint() {
        let config = SeedDemoConfig {
            host: "127.0.0.1".to_string(),
            port: "1".to_string(),
            http_path: "/mcp/".to_string(),
            auth_bearer: String::new(),
            project_key: "/tmp/project".to_string(),
            agent_a: "A".to_string(),
            agent_b: "B".to_string(),
            messages: 1,
            timeout_seconds: 1,
            log_file: None,
        };

        let mut client = RpcClient::new(&config).expect("rpc client");
        let error = wait_for_server(&mut client, &Deadline::after(Duration::from_secs(1)))
            .expect_err("server should time out");
        assert!(error.to_string().contains("Timed out waiting for server"));
    }

    #[test]
    fn retry_policy_uses_deterministic_exponential_backoff() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.backoff_for_attempt(1),
            Duration::from_millis(RPC_RETRY_BASE_BACKOFF_MS)
        );
        assert_eq!(
            policy.backoff_for_attempt(2),
            Duration::from_millis(RPC_RETRY_BASE_BACKOFF_MS * 2)
        );
        assert_eq!(
            policy.backoff_for_attempt(3),
            Duration::from_millis(RPC_RETRY_BASE_BACKOFF_MS * 4)
        );
    }

    #[test]
    fn deadline_sleep_is_clamped_to_remaining_budget() {
        let deadline = Deadline::after(Duration::from_millis(5));
        assert!(
            deadline
                .next_sleep(Duration::from_secs(1))
                .expect("sleep step should exist")
                <= Duration::from_millis(5)
        );
    }

    #[test]
    fn seed_summary_payload_contains_expected_machine_fields() {
        let config = SeedDemoConfig {
            host: "127.0.0.1".to_string(),
            port: "8879".to_string(),
            http_path: "/mcp/".to_string(),
            auth_bearer: String::new(),
            project_key: "/tmp/project".to_string(),
            agent_a: "Alpha".to_string(),
            agent_b: "Beta".to_string(),
            messages: 3,
            timeout_seconds: 5,
            log_file: None,
        };
        let integration = OutputIntegration {
            fastapi_mode: "plain".to_string(),
            fastapi_agent: true,
            fastapi_ci: false,
            fastapi_tty: false,
            sqlmodel_mode: "json".to_string(),
            sqlmodel_agent: true,
        };

        let payload =
            super::seed_summary_payload(&config, "http://127.0.0.1:8879/mcp/", &integration);
        assert_eq!(payload["command"], "seed-demo");
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["project_key"], "/tmp/project");
        assert_eq!(payload["agent_a"], "Alpha");
        assert_eq!(payload["agent_b"], "Beta");
        assert_eq!(payload["messages"], 3);
        assert_eq!(payload["endpoint"], "http://127.0.0.1:8879/mcp/");
        assert_eq!(payload["integration"]["sqlmodel_mode"], "json");
    }
}
