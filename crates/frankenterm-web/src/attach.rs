#![forbid(unsafe_code)]

use serde::Serialize;
use serde_json::json;
use std::collections::VecDeque;

/// Integration script that exercises the remote attach path end-to-end.
pub const ATTACH_E2E_SCRIPT_PATH: &str = "tests/e2e/scripts/test_remote_resize_storm.sh";
/// Full remote attach E2E suite runner.
pub const ATTACH_E2E_SUITE_SCRIPT_PATH: &str = "tests/e2e/scripts/test_remote_all.sh";

const TRANSITION_LOG_CAPACITY: usize = 512;

/// Attach lifecycle states for the browser client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachState {
    Detached,
    ConnectingTransport,
    AwaitingHandshakeAck,
    Active,
    BackingOff,
    Closing,
    Closed,
    Failed,
}

impl AttachState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Detached => "detached",
            Self::ConnectingTransport => "connecting_transport",
            Self::AwaitingHandshakeAck => "awaiting_handshake_ack",
            Self::Active => "active",
            Self::BackingOff => "backing_off",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }
}

/// Event classes accepted by the deterministic state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachEventKind {
    ConnectRequested,
    TransportOpened,
    HandshakeAck,
    TransportClosed,
    ProtocolError,
    SessionEnded,
    CloseRequested,
    RetryTimerElapsed,
    HandshakeTimeoutElapsed,
    Tick,
    ResetRequested,
}

impl AttachEventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConnectRequested => "connect_requested",
            Self::TransportOpened => "transport_opened",
            Self::HandshakeAck => "handshake_ack",
            Self::TransportClosed => "transport_closed",
            Self::ProtocolError => "protocol_error",
            Self::SessionEnded => "session_ended",
            Self::CloseRequested => "close_requested",
            Self::RetryTimerElapsed => "retry_timer_elapsed",
            Self::HandshakeTimeoutElapsed => "handshake_timeout_elapsed",
            Self::Tick => "tick",
            Self::ResetRequested => "reset_requested",
        }
    }
}

/// Side effects that the host should execute in reaction to transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachActionKind {
    OpenTransport,
    SendHandshake,
    StartHandshakeTimeout,
    ScheduleRetry,
    SendSessionEnd,
    CloseTransport,
}

impl AttachActionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenTransport => "open_transport",
            Self::SendHandshake => "send_handshake",
            Self::StartHandshakeTimeout => "start_handshake_timeout",
            Self::ScheduleRetry => "schedule_retry",
            Self::SendSessionEnd => "send_session_end",
            Self::CloseTransport => "close_transport",
        }
    }
}

/// Structured host action with optional timing metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachAction {
    pub kind: AttachActionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
}

impl AttachAction {
    #[must_use]
    const fn open_transport() -> Self {
        Self {
            kind: AttachActionKind::OpenTransport,
            deadline_ms: None,
            attempt: None,
        }
    }

    #[must_use]
    const fn send_handshake() -> Self {
        Self {
            kind: AttachActionKind::SendHandshake,
            deadline_ms: None,
            attempt: None,
        }
    }

    #[must_use]
    const fn start_handshake_timeout(deadline_ms: u64) -> Self {
        Self {
            kind: AttachActionKind::StartHandshakeTimeout,
            deadline_ms: Some(deadline_ms),
            attempt: None,
        }
    }

    #[must_use]
    const fn schedule_retry(deadline_ms: u64, attempt: u32) -> Self {
        Self {
            kind: AttachActionKind::ScheduleRetry,
            deadline_ms: Some(deadline_ms),
            attempt: Some(attempt),
        }
    }

    #[must_use]
    const fn send_session_end() -> Self {
        Self {
            kind: AttachActionKind::SendSessionEnd,
            deadline_ms: None,
            attempt: None,
        }
    }

    #[must_use]
    const fn close_transport() -> Self {
        Self {
            kind: AttachActionKind::CloseTransport,
            deadline_ms: None,
            attempt: None,
        }
    }
}

/// Deterministic attach/reconnect policy knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachConfig {
    /// Number of retries after the first failed attempt.
    pub max_retries: u32,
    /// Base delay (ms) for the first retry.
    pub backoff_base_ms: u64,
    /// Upper cap (ms) for retry delay.
    pub backoff_max_ms: u64,
    /// Handshake acknowledgement timeout (ms).
    pub handshake_timeout_ms: u64,
    /// Whether to retry on clean close codes (1000/1001).
    pub retry_on_clean_close: bool,
}

impl Default for AttachConfig {
    fn default() -> Self {
        Self {
            max_retries: 4,
            backoff_base_ms: 250,
            backoff_max_ms: 8_000,
            handshake_timeout_ms: 5_000,
            retry_on_clean_close: false,
        }
    }
}

/// Snapshot returned to host callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachSnapshot {
    pub state: AttachState,
    pub attempt: u32,
    pub max_retries: u32,
    pub handshake_deadline_ms: Option<u64>,
    pub retry_deadline_ms: Option<u64>,
    pub session_id: Option<String>,
    pub close_reason: Option<String>,
    pub failure_code: Option<String>,
    pub close_code: Option<u16>,
    pub clean_close: Option<bool>,
    pub can_retry: bool,
}

/// Transition record and deterministic log payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachTransition {
    pub seq: u64,
    pub at_ms: u64,
    pub event: AttachEventKind,
    pub from_state: AttachState,
    pub to_state: AttachState,
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handshake_deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clean_close: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    pub actions: Vec<AttachAction>,
}

impl AttachTransition {
    /// Serialize one JSONL transition line for deterministic diagnostics.
    #[must_use]
    pub fn to_jsonl_line(&self, run_id: &str) -> String {
        let record = AttachTransitionJsonl {
            schema_version: "e2e-jsonl-v1",
            event: "attach_state_transition",
            run_id,
            ts_ms: self.at_ms,
            transition_seq: self.seq,
            attach_event: self.event.as_str(),
            from_state: self.from_state.as_str(),
            to_state: self.to_state.as_str(),
            attempt: self.attempt,
            handshake_deadline_ms: self.handshake_deadline_ms,
            retry_deadline_ms: self.retry_deadline_ms,
            session_id: self.session_id.as_deref(),
            close_code: self.close_code,
            clean_close: self.clean_close,
            reason: self.reason.as_deref(),
            failure_code: self.failure_code.as_deref(),
            actions: &self.actions,
        };
        match serde_json::to_string(&record) {
            Ok(line) => line,
            Err(error) => serde_json::to_string(&json!({
                "schema_version": "e2e-jsonl-v1",
                "event": "attach_state_transition_encode_error",
                "run_id": run_id,
                "ts_ms": self.at_ms,
                "transition_seq": self.seq,
                "error": error.to_string(),
            }))
            .unwrap_or_else(|_| {
                "{\"schema_version\":\"e2e-jsonl-v1\",\"event\":\"attach_state_transition_encode_error\"}".to_owned()
            }),
        }
    }
}

#[derive(Serialize)]
struct AttachTransitionJsonl<'a> {
    schema_version: &'static str,
    event: &'static str,
    run_id: &'a str,
    ts_ms: u64,
    transition_seq: u64,
    attach_event: &'static str,
    from_state: &'static str,
    to_state: &'static str,
    attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    handshake_deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clean_close: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<&'a str>,
    actions: &'a [AttachAction],
}

/// Input events accepted by the state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachEvent {
    ConnectRequested,
    TransportOpened,
    HandshakeAck {
        session_id: String,
    },
    TransportClosed {
        code: u16,
        clean: bool,
        reason: String,
    },
    ProtocolError {
        code: String,
        fatal: bool,
    },
    SessionEnded {
        reason: String,
    },
    CloseRequested {
        reason: String,
    },
    Tick,
    Reset,
}

/// Deterministic websocket attach lifecycle state machine.
#[derive(Debug, Clone)]
pub struct AttachClientStateMachine {
    config: AttachConfig,
    state: AttachState,
    attempt: u32,
    handshake_deadline_ms: Option<u64>,
    retry_deadline_ms: Option<u64>,
    session_id: Option<String>,
    close_reason: Option<String>,
    failure_code: Option<String>,
    close_code: Option<u16>,
    clean_close: Option<bool>,
    transition_seq: u64,
    transitions: VecDeque<AttachTransition>,
}

impl Default for AttachClientStateMachine {
    fn default() -> Self {
        Self::new(AttachConfig::default())
    }
}

impl AttachClientStateMachine {
    #[must_use]
    pub fn new(config: AttachConfig) -> Self {
        Self {
            config,
            state: AttachState::Detached,
            attempt: 0,
            handshake_deadline_ms: None,
            retry_deadline_ms: None,
            session_id: None,
            close_reason: None,
            failure_code: None,
            close_code: None,
            clean_close: None,
            transition_seq: 0,
            transitions: VecDeque::new(),
        }
    }

    #[must_use]
    pub const fn state(&self) -> AttachState {
        self.state
    }

    #[must_use]
    pub fn snapshot(&self) -> AttachSnapshot {
        AttachSnapshot {
            state: self.state,
            attempt: self.attempt,
            max_retries: self.config.max_retries,
            handshake_deadline_ms: self.handshake_deadline_ms,
            retry_deadline_ms: self.retry_deadline_ms,
            session_id: self.session_id.clone(),
            close_reason: self.close_reason.clone(),
            failure_code: self.failure_code.clone(),
            close_code: self.close_code,
            clean_close: self.clean_close,
            can_retry: self.can_retry_from_current_attempt(),
        }
    }

    pub fn handle_event(&mut self, now_ms: u64, event: AttachEvent) -> AttachTransition {
        match event {
            AttachEvent::ConnectRequested => self.on_connect_requested(now_ms),
            AttachEvent::TransportOpened => self.on_transport_opened(now_ms),
            AttachEvent::HandshakeAck { session_id } => self.on_handshake_ack(now_ms, session_id),
            AttachEvent::TransportClosed {
                code,
                clean,
                reason,
            } => self.on_transport_closed(now_ms, code, clean, reason),
            AttachEvent::ProtocolError { code, fatal } => {
                self.on_protocol_error(now_ms, code, fatal)
            }
            AttachEvent::SessionEnded { reason } => self.on_session_ended(now_ms, reason),
            AttachEvent::CloseRequested { reason } => self.on_close_requested(now_ms, reason),
            AttachEvent::Tick => self.on_tick(now_ms),
            AttachEvent::Reset => self.on_reset(now_ms),
        }
    }

    #[must_use]
    pub fn drain_transitions(&mut self) -> Vec<AttachTransition> {
        self.transitions.drain(..).collect()
    }

    #[must_use]
    pub fn drain_transition_jsonl(&mut self, run_id: &str) -> Vec<String> {
        self.drain_transitions()
            .into_iter()
            .map(|transition| transition.to_jsonl_line(run_id))
            .collect()
    }

    fn on_connect_requested(&mut self, now_ms: u64) -> AttachTransition {
        let from_state = self.state;
        let mut reason = None;
        let mut actions = Vec::new();

        match self.state {
            AttachState::Detached | AttachState::Closed | AttachState::Failed => {
                self.state = AttachState::ConnectingTransport;
                self.attempt = 1;
                self.handshake_deadline_ms = None;
                self.retry_deadline_ms = None;
                self.session_id = None;
                self.close_reason = None;
                self.failure_code = None;
                self.close_code = None;
                self.clean_close = None;
                actions.push(AttachAction::open_transport());
            }
            AttachState::BackingOff => {
                self.state = AttachState::ConnectingTransport;
                if self.attempt == 0 {
                    self.attempt = 1;
                }
                self.retry_deadline_ms = None;
                self.handshake_deadline_ms = None;
                actions.push(AttachAction::open_transport());
            }
            AttachState::ConnectingTransport
            | AttachState::AwaitingHandshakeAck
            | AttachState::Active
            | AttachState::Closing => {
                reason = Some("ignored_in_current_state".to_owned());
            }
        }

        self.record_transition(
            now_ms,
            AttachEventKind::ConnectRequested,
            from_state,
            reason,
            actions,
        )
    }

    fn on_transport_opened(&mut self, now_ms: u64) -> AttachTransition {
        let from_state = self.state;
        let mut reason = None;
        let mut actions = Vec::new();

        if self.state == AttachState::ConnectingTransport {
            let deadline = now_ms.saturating_add(self.config.handshake_timeout_ms);
            self.state = AttachState::AwaitingHandshakeAck;
            self.handshake_deadline_ms = Some(deadline);
            actions.push(AttachAction::send_handshake());
            actions.push(AttachAction::start_handshake_timeout(deadline));
        } else {
            reason = Some("ignored_in_current_state".to_owned());
        }

        self.record_transition(
            now_ms,
            AttachEventKind::TransportOpened,
            from_state,
            reason,
            actions,
        )
    }

    fn on_handshake_ack(&mut self, now_ms: u64, session_id: String) -> AttachTransition {
        let from_state = self.state;
        let mut reason = None;

        if self.state == AttachState::AwaitingHandshakeAck {
            let normalized_session_id = if session_id.trim().is_empty() {
                "unknown_session".to_owned()
            } else {
                session_id
            };
            self.state = AttachState::Active;
            self.session_id = Some(normalized_session_id);
            self.handshake_deadline_ms = None;
            self.retry_deadline_ms = None;
            self.close_reason = None;
            self.failure_code = None;
            self.close_code = None;
            self.clean_close = None;
        } else {
            reason = Some("ignored_in_current_state".to_owned());
        }

        self.record_transition(
            now_ms,
            AttachEventKind::HandshakeAck,
            from_state,
            reason,
            Vec::new(),
        )
    }

    fn on_protocol_error(&mut self, now_ms: u64, code: String, fatal: bool) -> AttachTransition {
        let from_state = self.state;
        let mut actions = Vec::new();

        if matches!(
            self.state,
            AttachState::Detached | AttachState::Closed | AttachState::Failed
        ) {
            return self.record_transition(
                now_ms,
                AttachEventKind::ProtocolError,
                from_state,
                Some("ignored_in_current_state".to_owned()),
                actions,
            );
        }

        let normalized_code = normalize_reason(&code, "protocol_error");
        self.failure_code = Some(normalized_code.clone());
        self.close_reason = Some(normalized_code.clone());
        self.close_code = None;
        self.clean_close = None;
        self.handshake_deadline_ms = None;
        actions.push(AttachAction::close_transport());

        if fatal || !self.can_retry_from_current_attempt() {
            self.state = AttachState::Failed;
            self.retry_deadline_ms = None;
        } else {
            let next_attempt = self.next_retry_attempt();
            let retry_deadline = now_ms.saturating_add(self.retry_backoff_ms(next_attempt));
            self.state = AttachState::BackingOff;
            self.attempt = next_attempt;
            self.retry_deadline_ms = Some(retry_deadline);
            actions.push(AttachAction::schedule_retry(retry_deadline, next_attempt));
        }

        self.record_transition(
            now_ms,
            AttachEventKind::ProtocolError,
            from_state,
            Some(normalized_code),
            actions,
        )
    }

    fn on_transport_closed(
        &mut self,
        now_ms: u64,
        code: u16,
        clean: bool,
        reason_text: String,
    ) -> AttachTransition {
        let from_state = self.state;
        let mut actions = Vec::new();

        self.close_code = Some(code);
        self.clean_close = Some(clean);
        self.handshake_deadline_ms = None;

        if matches!(self.state, AttachState::Detached | AttachState::Closed) {
            return self.record_transition(
                now_ms,
                AttachEventKind::TransportClosed,
                from_state,
                Some("ignored_in_current_state".to_owned()),
                actions,
            );
        }

        let normalized_reason = normalize_reason(&reason_text, "transport_closed");
        self.close_reason = Some(normalized_reason.clone());

        if self.state == AttachState::Closing {
            self.state = AttachState::Closed;
            self.retry_deadline_ms = None;
            self.failure_code = None;
            return self.record_transition(
                now_ms,
                AttachEventKind::TransportClosed,
                from_state,
                Some(normalized_reason),
                actions,
            );
        }

        let fatal_close_code = matches!(code, 1002 | 1003 | 1007 | 1008);
        let orderly_close = clean && matches!(code, 1000 | 1001);

        if fatal_close_code {
            self.state = AttachState::Failed;
            self.retry_deadline_ms = None;
            self.failure_code = Some(format!("close_{code}"));
        } else if orderly_close && !self.config.retry_on_clean_close {
            self.state = AttachState::Closed;
            self.retry_deadline_ms = None;
            self.failure_code = None;
        } else if self.can_retry_from_current_attempt() {
            let next_attempt = self.next_retry_attempt();
            let retry_deadline = now_ms.saturating_add(self.retry_backoff_ms(next_attempt));
            self.state = AttachState::BackingOff;
            self.attempt = next_attempt;
            self.retry_deadline_ms = Some(retry_deadline);
            actions.push(AttachAction::schedule_retry(retry_deadline, next_attempt));
        } else if orderly_close {
            self.state = AttachState::Closed;
            self.retry_deadline_ms = None;
            self.failure_code = None;
        } else {
            self.state = AttachState::Failed;
            self.retry_deadline_ms = None;
            self.failure_code = Some(format!("close_{code}"));
        }

        self.record_transition(
            now_ms,
            AttachEventKind::TransportClosed,
            from_state,
            Some(normalized_reason),
            actions,
        )
    }

    fn on_session_ended(&mut self, now_ms: u64, reason_text: String) -> AttachTransition {
        let from_state = self.state;
        let mut actions = Vec::new();

        let reason = if matches!(
            self.state,
            AttachState::ConnectingTransport
                | AttachState::AwaitingHandshakeAck
                | AttachState::Active
                | AttachState::BackingOff
                | AttachState::Failed
        ) {
            self.state = AttachState::Closing;
            self.handshake_deadline_ms = None;
            self.retry_deadline_ms = None;
            let normalized_reason = normalize_reason(&reason_text, "session_ended");
            self.close_reason = Some(normalized_reason.clone());
            actions.push(AttachAction::close_transport());
            Some(normalized_reason)
        } else {
            Some("ignored_in_current_state".to_owned())
        };

        self.record_transition(
            now_ms,
            AttachEventKind::SessionEnded,
            from_state,
            reason,
            actions,
        )
    }

    fn on_close_requested(&mut self, now_ms: u64, reason_text: String) -> AttachTransition {
        let from_state = self.state;
        let mut actions = Vec::new();

        let reason = if matches!(
            self.state,
            AttachState::ConnectingTransport
                | AttachState::AwaitingHandshakeAck
                | AttachState::Active
                | AttachState::BackingOff
                | AttachState::Failed
        ) {
            self.state = AttachState::Closing;
            self.handshake_deadline_ms = None;
            self.retry_deadline_ms = None;
            let normalized_reason = normalize_reason(&reason_text, "client_close");
            self.close_reason = Some(normalized_reason.clone());
            actions.push(AttachAction::send_session_end());
            actions.push(AttachAction::close_transport());
            Some(normalized_reason)
        } else {
            Some("ignored_in_current_state".to_owned())
        };

        self.record_transition(
            now_ms,
            AttachEventKind::CloseRequested,
            from_state,
            reason,
            actions,
        )
    }

    fn on_tick(&mut self, now_ms: u64) -> AttachTransition {
        let from_state = self.state;
        let mut reason = None;
        let mut actions = Vec::new();
        let event_kind;

        if self.state == AttachState::AwaitingHandshakeAck
            && self
                .handshake_deadline_ms
                .is_some_and(|deadline| now_ms >= deadline)
        {
            event_kind = AttachEventKind::HandshakeTimeoutElapsed;
            self.handshake_deadline_ms = None;
            self.failure_code = Some("handshake_timeout".to_owned());
            self.close_reason = Some("handshake_timeout".to_owned());
            reason = Some("handshake_timeout".to_owned());
            actions.push(AttachAction::close_transport());

            if self.can_retry_from_current_attempt() {
                let next_attempt = self.next_retry_attempt();
                let retry_deadline = now_ms.saturating_add(self.retry_backoff_ms(next_attempt));
                self.state = AttachState::BackingOff;
                self.attempt = next_attempt;
                self.retry_deadline_ms = Some(retry_deadline);
                actions.push(AttachAction::schedule_retry(retry_deadline, next_attempt));
            } else {
                self.state = AttachState::Failed;
                self.retry_deadline_ms = None;
            }
        } else if self.state == AttachState::BackingOff
            && self
                .retry_deadline_ms
                .is_some_and(|deadline| now_ms >= deadline)
        {
            event_kind = AttachEventKind::RetryTimerElapsed;
            self.state = AttachState::ConnectingTransport;
            self.retry_deadline_ms = None;
            self.handshake_deadline_ms = None;
            actions.push(AttachAction::open_transport());
        } else {
            event_kind = AttachEventKind::Tick;
            reason = Some("no_timer_elapsed".to_owned());
        }

        self.record_transition(now_ms, event_kind, from_state, reason, actions)
    }

    fn on_reset(&mut self, now_ms: u64) -> AttachTransition {
        let from_state = self.state;
        let mut actions = Vec::new();
        if !matches!(self.state, AttachState::Detached | AttachState::Closed) {
            actions.push(AttachAction::close_transport());
        }

        self.state = AttachState::Detached;
        self.attempt = 0;
        self.handshake_deadline_ms = None;
        self.retry_deadline_ms = None;
        self.session_id = None;
        self.close_reason = None;
        self.failure_code = None;
        self.close_code = None;
        self.clean_close = None;

        self.record_transition(
            now_ms,
            AttachEventKind::ResetRequested,
            from_state,
            None,
            actions,
        )
    }

    #[must_use]
    const fn can_retry_from_current_attempt(&self) -> bool {
        self.attempt <= self.config.max_retries
    }

    #[must_use]
    const fn next_retry_attempt(&self) -> u32 {
        self.attempt.saturating_add(1)
    }

    #[must_use]
    fn retry_backoff_ms(&self, next_attempt: u32) -> u64 {
        let exp = next_attempt.saturating_sub(2).min(62);
        let factor = 1_u64.checked_shl(exp).unwrap_or(u64::MAX);
        self.config
            .backoff_base_ms
            .saturating_mul(factor)
            .min(self.config.backoff_max_ms)
    }

    fn record_transition(
        &mut self,
        now_ms: u64,
        event: AttachEventKind,
        from_state: AttachState,
        reason: Option<String>,
        actions: Vec<AttachAction>,
    ) -> AttachTransition {
        self.transition_seq = self.transition_seq.saturating_add(1);
        let transition = AttachTransition {
            seq: self.transition_seq,
            at_ms: now_ms,
            event,
            from_state,
            to_state: self.state,
            attempt: self.attempt,
            handshake_deadline_ms: self.handshake_deadline_ms,
            retry_deadline_ms: self.retry_deadline_ms,
            session_id: self.session_id.clone(),
            close_code: self.close_code,
            clean_close: self.clean_close,
            reason,
            failure_code: self.failure_code.clone(),
            actions,
        };

        if self.transitions.len() >= TRANSITION_LOG_CAPACITY {
            let _ = self.transitions.pop_front();
        }
        self.transitions.push_back(transition.clone());
        transition
    }
}

fn normalize_reason(raw: &str, fallback: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn connect_open_handshake_ack_reaches_active_state() {
        let mut machine = AttachClientStateMachine::default();

        let connect = machine.handle_event(100, AttachEvent::ConnectRequested);
        assert_eq!(connect.from_state, AttachState::Detached);
        assert_eq!(connect.to_state, AttachState::ConnectingTransport);
        assert_eq!(connect.attempt, 1);
        assert_eq!(connect.actions, vec![AttachAction::open_transport()]);

        let opened = machine.handle_event(110, AttachEvent::TransportOpened);
        assert_eq!(opened.from_state, AttachState::ConnectingTransport);
        assert_eq!(opened.to_state, AttachState::AwaitingHandshakeAck);
        assert_eq!(opened.actions[0], AttachAction::send_handshake());
        assert_eq!(
            opened.actions[1],
            AttachAction::start_handshake_timeout(5_110)
        );

        let ack = machine.handle_event(
            125,
            AttachEvent::HandshakeAck {
                session_id: "sess-123".to_owned(),
            },
        );
        assert_eq!(ack.from_state, AttachState::AwaitingHandshakeAck);
        assert_eq!(ack.to_state, AttachState::Active);
        assert_eq!(ack.actions, Vec::<AttachAction>::new());
        assert_eq!(machine.snapshot().session_id.as_deref(), Some("sess-123"));
    }

    #[test]
    fn handshake_timeout_transitions_to_backoff_then_retry_timer_reopens_transport() {
        let mut machine = AttachClientStateMachine::default();
        machine.handle_event(0, AttachEvent::ConnectRequested);
        machine.handle_event(10, AttachEvent::TransportOpened);

        let waiting = machine.handle_event(5_009, AttachEvent::Tick);
        assert_eq!(waiting.event, AttachEventKind::Tick);
        assert_eq!(waiting.to_state, AttachState::AwaitingHandshakeAck);

        let timed_out = machine.handle_event(5_010, AttachEvent::Tick);
        assert_eq!(timed_out.event, AttachEventKind::HandshakeTimeoutElapsed);
        assert_eq!(timed_out.to_state, AttachState::BackingOff);
        assert_eq!(
            timed_out.actions,
            vec![
                AttachAction::close_transport(),
                AttachAction::schedule_retry(5_260, 2),
            ]
        );

        let not_ready = machine.handle_event(5_259, AttachEvent::Tick);
        assert_eq!(not_ready.event, AttachEventKind::Tick);
        assert_eq!(not_ready.to_state, AttachState::BackingOff);

        let retry = machine.handle_event(5_260, AttachEvent::Tick);
        assert_eq!(retry.event, AttachEventKind::RetryTimerElapsed);
        assert_eq!(retry.to_state, AttachState::ConnectingTransport);
        assert_eq!(retry.actions, vec![AttachAction::open_transport()]);
    }

    #[test]
    fn fatal_protocol_close_transitions_to_failed_without_retry() {
        let mut machine = AttachClientStateMachine::default();
        machine.handle_event(0, AttachEvent::ConnectRequested);
        machine.handle_event(1, AttachEvent::TransportOpened);
        machine.handle_event(
            2,
            AttachEvent::HandshakeAck {
                session_id: "s".to_owned(),
            },
        );

        let closed = machine.handle_event(
            50,
            AttachEvent::TransportClosed {
                code: 1008,
                clean: false,
                reason: "policy_violation".to_owned(),
            },
        );
        assert_eq!(closed.event, AttachEventKind::TransportClosed);
        assert_eq!(closed.to_state, AttachState::Failed);
        assert_eq!(closed.retry_deadline_ms, None);
        assert_eq!(closed.failure_code.as_deref(), Some("close_1008"));
    }

    #[test]
    fn close_request_path_is_explicit_and_orderly() {
        let mut machine = AttachClientStateMachine::default();
        machine.handle_event(0, AttachEvent::ConnectRequested);
        machine.handle_event(1, AttachEvent::TransportOpened);
        machine.handle_event(
            2,
            AttachEvent::HandshakeAck {
                session_id: "s".to_owned(),
            },
        );

        let close_request = machine.handle_event(
            10,
            AttachEvent::CloseRequested {
                reason: "user_requested".to_owned(),
            },
        );
        assert_eq!(close_request.to_state, AttachState::Closing);
        assert_eq!(
            close_request.actions,
            vec![
                AttachAction::send_session_end(),
                AttachAction::close_transport(),
            ]
        );

        let closed = machine.handle_event(
            11,
            AttachEvent::TransportClosed {
                code: 1000,
                clean: true,
                reason: "normal".to_owned(),
            },
        );
        assert_eq!(closed.from_state, AttachState::Closing);
        assert_eq!(closed.to_state, AttachState::Closed);
    }

    #[test]
    fn transition_log_is_bounded_to_capacity() {
        let mut machine = AttachClientStateMachine::default();
        for idx in 0..(TRANSITION_LOG_CAPACITY + 8) {
            let timestamp = u64::try_from(idx).expect("idx fits into u64");
            machine.handle_event(timestamp, AttachEvent::Tick);
        }
        let drained = machine.drain_transitions();
        assert_eq!(drained.len(), TRANSITION_LOG_CAPACITY);
        assert!(drained.first().is_some_and(|entry| entry.seq > 1));
    }

    #[test]
    fn transition_jsonl_contains_required_fields() {
        let mut machine = AttachClientStateMachine::default();
        let transition = machine.handle_event(42, AttachEvent::ConnectRequested);
        let line = transition.to_jsonl_line("run-1");
        let parsed: Value = serde_json::from_str(&line).expect("transition line should parse");

        assert_eq!(parsed["schema_version"], "e2e-jsonl-v1");
        assert_eq!(parsed["event"], "attach_state_transition");
        assert_eq!(parsed["run_id"], "run-1");
        assert_eq!(parsed["ts_ms"], 42);
        assert_eq!(parsed["from_state"], "detached");
        assert_eq!(parsed["to_state"], "connecting_transport");
        assert_eq!(parsed["attach_event"], "connect_requested");
        assert!(parsed.get("actions").is_some());
    }

    #[test]
    fn e2e_script_paths_target_remote_attach_flows() {
        assert!(ATTACH_E2E_SCRIPT_PATH.starts_with("tests/e2e/scripts/"));
        assert!(ATTACH_E2E_SCRIPT_PATH.ends_with(".sh"));
        assert!(ATTACH_E2E_SUITE_SCRIPT_PATH.starts_with("tests/e2e/scripts/"));
        assert!(ATTACH_E2E_SUITE_SCRIPT_PATH.ends_with(".sh"));
    }
}
