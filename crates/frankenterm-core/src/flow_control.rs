#![forbid(unsafe_code)]

//! Flow control policy engine for the WebSocket bridge.

#[derive(Debug, Clone, Copy)]
pub struct FlowControlConfig {
    pub output_hard_cap_bytes: u32,
    pub input_hard_cap_bytes: u32,
}

impl Default for FlowControlConfig {
    fn default() -> Self {
        Self {
            output_hard_cap_bytes: 10 * 1024 * 1024, // 10MB
            input_hard_cap_bytes: 1024 * 1024,       // 1MB
        }
    }
}

pub struct FlowControlPolicy {
    pub config: FlowControlConfig,
}

impl FlowControlPolicy {
    pub fn new(config: FlowControlConfig) -> Self {
        Self { config }
    }

    pub fn evaluate(&mut self, _snapshot: FlowControlSnapshot) -> FlowControlDecision {
        FlowControlDecision {
            should_pause_pty_reads: false,
            output_batch_budget_bytes: u32::MAX,
            chosen_action: None,
            reason: None,
            fairness_index: 1.0,
        }
    }

    pub fn should_drop_input_event(&self, pending_bytes: u32, class: InputEventClass) -> bool {
        match class {
            InputEventClass::Interactive => false,
            InputEventClass::NonInteractive => pending_bytes > self.config.input_hard_cap_bytes,
        }
    }

    pub fn should_replenish(&self, consumed: u32, window: u32, _elapsed_ms: u64) -> bool {
        consumed >= window / 2
    }
}

pub struct FlowControlSnapshot {
    pub queues: QueueDepthBytes,
    pub rates: RateWindowBps,
    pub latency: LatencyWindowMs,
    pub serviced_input_bytes: u64,
    pub serviced_output_bytes: u64,
    pub output_hard_cap_duration_ms: u64,
}

pub struct QueueDepthBytes {
    pub input: u32,
    pub output: u32,
    pub render_frames: u32,
}

pub struct RateWindowBps {
    pub lambda_in: u32,
    pub lambda_out: u32,
    pub mu_in: u32,
    pub mu_out: u32,
}

pub struct LatencyWindowMs {
    pub key_p50_ms: f64,
    pub key_p95_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEventClass {
    Interactive,
    NonInteractive,
}

#[derive(Debug, Clone)]
pub struct FlowControlDecision {
    pub should_pause_pty_reads: bool,
    pub output_batch_budget_bytes: u32,
    pub chosen_action: Option<String>,
    pub reason: Option<String>,
    pub fairness_index: f64,
}
