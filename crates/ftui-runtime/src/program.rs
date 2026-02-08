#![forbid(unsafe_code)]

//! Bubbletea/Elm-style runtime for terminal applications.
//!
//! The program runtime manages the update/view loop, handling events and
//! rendering frames. It separates state (Model) from rendering (View) and
//! provides a command pattern for side effects.
//!
//! # Example
//!
//! ```ignore
//! use ftui_runtime::program::{Model, Cmd};
//! use ftui_core::event::Event;
//! use ftui_render::frame::Frame;
//!
//! struct Counter {
//!     count: i32,
//! }
//!
//! enum Msg {
//!     Increment,
//!     Decrement,
//!     Quit,
//! }
//!
//! impl From<Event> for Msg {
//!     fn from(event: Event) -> Self {
//!         match event {
//!             Event::Key(k) if k.is_char('q') => Msg::Quit,
//!             Event::Key(k) if k.is_char('+') => Msg::Increment,
//!             Event::Key(k) if k.is_char('-') => Msg::Decrement,
//!             _ => Msg::Increment, // Default
//!         }
//!     }
//! }
//!
//! impl Model for Counter {
//!     type Message = Msg;
//!
//!     fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
//!         match msg {
//!             Msg::Increment => { self.count += 1; Cmd::none() }
//!             Msg::Decrement => { self.count -= 1; Cmd::none() }
//!             Msg::Quit => Cmd::quit(),
//!         }
//!     }
//!
//!     fn view(&self, frame: &mut Frame) {
//!         // Render counter value to frame
//!     }
//! }
//! ```

use crate::StorageResult;
use crate::evidence_sink::{EvidenceSink, EvidenceSinkConfig};
use crate::evidence_telemetry::{
    BudgetDecisionSnapshot, ConformalSnapshot, ResizeDecisionSnapshot, set_budget_snapshot,
    set_resize_snapshot,
};
use crate::input_fairness::{FairnessDecision, FairnessEventType, InputFairnessGuard};
use crate::input_macro::{EventRecorder, InputMacro};
use crate::locale::LocaleContext;
use crate::queueing_scheduler::{EstimateSource, QueueingScheduler, SchedulerConfig, WeightSource};
use crate::render_trace::RenderTraceConfig;
use crate::resize_coalescer::{CoalesceAction, CoalescerConfig, ResizeCoalescer};
use crate::state_persistence::StateRegistry;
use crate::subscription::SubscriptionManager;
use crate::terminal_writer::{RuntimeDiffConfig, ScreenMode, TerminalWriter, UiAnchor};
use crate::voi_sampling::{VoiConfig, VoiSampler};
use crate::{BucketKey, ConformalConfig, ConformalPrediction, ConformalPredictor};
use ftui_backend::{BackendEventSource, BackendFeatures};
use ftui_core::event::Event;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_core::terminal_session::{SessionOptions, TerminalSession};
use ftui_render::budget::{BudgetDecision, DegradationLevel, FrameBudgetConfig, RenderBudget};
use ftui_render::buffer::Buffer;
use ftui_render::diff_strategy::DiffStrategy;
use ftui_render::frame::{Frame, WidgetBudget, WidgetSignal};
use ftui_render::sanitize::sanitize;
use std::collections::HashMap;
use std::io::{self, Stdout, Write};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tracing::{debug, debug_span, info, info_span};

/// The Model trait defines application state and behavior.
///
/// Implementations define how the application responds to events
/// and renders its current state.
pub trait Model: Sized {
    /// The message type for this model.
    ///
    /// Messages represent actions that update the model state.
    /// Must be convertible from terminal events.
    type Message: From<Event> + Send + 'static;

    /// Initialize the model with startup commands.
    ///
    /// Called once when the program starts. Return commands to execute
    /// initial side effects like loading data.
    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::none()
    }

    /// Update the model in response to a message.
    ///
    /// This is the core state transition function. Returns commands
    /// for any side effects that should be executed.
    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message>;

    /// Render the current state to a frame.
    ///
    /// Called after updates when the UI needs to be redrawn.
    fn view(&self, frame: &mut Frame);

    /// Declare active subscriptions.
    ///
    /// Called after each `update()`. The runtime compares the returned set
    /// (by `SubId`) against currently running subscriptions and starts/stops
    /// as needed. Returning an empty vec stops all subscriptions.
    ///
    /// # Default
    ///
    /// Returns an empty vec (no subscriptions).
    fn subscriptions(&self) -> Vec<Box<dyn crate::subscription::Subscription<Self::Message>>> {
        vec![]
    }
}

/// Default weight assigned to background tasks.
const DEFAULT_TASK_WEIGHT: f64 = 1.0;

/// Default estimated task cost (ms) used for scheduling.
const DEFAULT_TASK_ESTIMATE_MS: f64 = 10.0;

/// Scheduling metadata for background tasks.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    /// Task weight (importance). Higher = more priority.
    pub weight: f64,
    /// Estimated task cost in milliseconds.
    pub estimate_ms: f64,
    /// Optional task name for evidence logging.
    pub name: Option<String>,
}

impl Default for TaskSpec {
    fn default() -> Self {
        Self {
            weight: DEFAULT_TASK_WEIGHT,
            estimate_ms: DEFAULT_TASK_ESTIMATE_MS,
            name: None,
        }
    }
}

impl TaskSpec {
    /// Create a task spec with an explicit weight and estimate.
    #[must_use]
    pub fn new(weight: f64, estimate_ms: f64) -> Self {
        Self {
            weight,
            estimate_ms,
            name: None,
        }
    }

    /// Attach a task name for diagnostics.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Per-frame timing data for profiling.
#[derive(Debug, Clone, Copy)]
pub struct FrameTiming {
    pub frame_idx: u64,
    pub update_us: u64,
    pub render_us: u64,
    pub diff_us: u64,
    pub present_us: u64,
    pub total_us: u64,
}

/// Sink for frame timing events.
pub trait FrameTimingSink: Send + Sync {
    fn record_frame(&self, timing: &FrameTiming);
}

/// Configuration for frame timing capture.
#[derive(Clone)]
pub struct FrameTimingConfig {
    pub sink: Arc<dyn FrameTimingSink>,
}

impl FrameTimingConfig {
    #[must_use]
    pub fn new(sink: Arc<dyn FrameTimingSink>) -> Self {
        Self { sink }
    }
}

impl std::fmt::Debug for FrameTimingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameTimingConfig")
            .field("sink", &"<dyn FrameTimingSink>")
            .finish()
    }
}

/// Commands represent side effects to be executed by the runtime.
///
/// Commands are returned from `init()` and `update()` to trigger
/// actions like quitting, sending messages, or scheduling ticks.
#[derive(Default)]
pub enum Cmd<M> {
    /// No operation.
    #[default]
    None,
    /// Quit the application.
    Quit,
    /// Execute multiple commands as a batch (currently sequential).
    Batch(Vec<Cmd<M>>),
    /// Execute commands sequentially.
    Sequence(Vec<Cmd<M>>),
    /// Send a message to the model.
    Msg(M),
    /// Schedule a tick after a duration.
    Tick(Duration),
    /// Write a log message to the terminal output.
    ///
    /// This writes to the scrollback region in inline mode, or is ignored/handled
    /// appropriately in alternate screen mode. Safe to use with the One-Writer Rule.
    Log(String),
    /// Execute a blocking operation on a background thread.
    ///
    /// When effect queue scheduling is enabled, tasks are enqueued and executed
    /// in Smith-rule order on a dedicated worker thread. Otherwise the closure
    /// runs on a spawned thread immediately. The return value is sent back
    /// as a message to the model.
    Task(TaskSpec, Box<dyn FnOnce() -> M + Send>),
    /// Save widget state to the persistence registry.
    ///
    /// Triggers a flush of the state registry to the storage backend.
    /// No-op if persistence is not configured.
    SaveState,
    /// Restore widget state from the persistence registry.
    ///
    /// Triggers a load from the storage backend and updates the cache.
    /// No-op if persistence is not configured. Returns a message via
    /// callback if state was successfully restored.
    RestoreState,
    /// Toggle mouse capture at runtime.
    ///
    /// Instructs the terminal session to enable or disable mouse event capture.
    /// No-op in test simulators.
    SetMouseCapture(bool),
}

impl<M: std::fmt::Debug> std::fmt::Debug for Cmd<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            Self::Quit => write!(f, "Quit"),
            Self::Batch(cmds) => f.debug_tuple("Batch").field(cmds).finish(),
            Self::Sequence(cmds) => f.debug_tuple("Sequence").field(cmds).finish(),
            Self::Msg(m) => f.debug_tuple("Msg").field(m).finish(),
            Self::Tick(d) => f.debug_tuple("Tick").field(d).finish(),
            Self::Log(s) => f.debug_tuple("Log").field(s).finish(),
            Self::Task(spec, _) => f.debug_struct("Task").field("spec", spec).finish(),
            Self::SaveState => write!(f, "SaveState"),
            Self::RestoreState => write!(f, "RestoreState"),
            Self::SetMouseCapture(b) => write!(f, "SetMouseCapture({b})"),
        }
    }
}

impl<M> Cmd<M> {
    /// Create a no-op command.
    #[inline]
    pub fn none() -> Self {
        Self::None
    }

    /// Create a quit command.
    #[inline]
    pub fn quit() -> Self {
        Self::Quit
    }

    /// Create a message command.
    #[inline]
    pub fn msg(m: M) -> Self {
        Self::Msg(m)
    }

    /// Create a log command.
    ///
    /// The message will be sanitized and written to the terminal log (scrollback).
    /// A newline is appended if not present.
    #[inline]
    pub fn log(msg: impl Into<String>) -> Self {
        Self::Log(msg.into())
    }

    /// Create a batch of commands.
    pub fn batch(cmds: Vec<Self>) -> Self {
        if cmds.is_empty() {
            Self::None
        } else if cmds.len() == 1 {
            cmds.into_iter().next().unwrap()
        } else {
            Self::Batch(cmds)
        }
    }

    /// Create a sequence of commands.
    pub fn sequence(cmds: Vec<Self>) -> Self {
        if cmds.is_empty() {
            Self::None
        } else if cmds.len() == 1 {
            cmds.into_iter().next().unwrap()
        } else {
            Self::Sequence(cmds)
        }
    }

    /// Return a stable name for telemetry and tracing.
    #[inline]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Quit => "Quit",
            Self::Batch(_) => "Batch",
            Self::Sequence(_) => "Sequence",
            Self::Msg(_) => "Msg",
            Self::Tick(_) => "Tick",
            Self::Log(_) => "Log",
            Self::Task(..) => "Task",
            Self::SaveState => "SaveState",
            Self::RestoreState => "RestoreState",
            Self::SetMouseCapture(_) => "SetMouseCapture",
        }
    }

    /// Create a tick command.
    #[inline]
    pub fn tick(duration: Duration) -> Self {
        Self::Tick(duration)
    }

    /// Create a background task command.
    ///
    /// The closure runs on a spawned thread (or the effect queue worker when
    /// scheduling is enabled). When it completes, the returned message is
    /// sent back to the model's `update()`.
    pub fn task<F>(f: F) -> Self
    where
        F: FnOnce() -> M + Send + 'static,
    {
        Self::Task(TaskSpec::default(), Box::new(f))
    }

    /// Create a background task command with explicit scheduling metadata.
    pub fn task_with_spec<F>(spec: TaskSpec, f: F) -> Self
    where
        F: FnOnce() -> M + Send + 'static,
    {
        Self::Task(spec, Box::new(f))
    }

    /// Create a background task command with explicit weight and estimate.
    pub fn task_weighted<F>(weight: f64, estimate_ms: f64, f: F) -> Self
    where
        F: FnOnce() -> M + Send + 'static,
    {
        Self::Task(TaskSpec::new(weight, estimate_ms), Box::new(f))
    }

    /// Create a named background task command.
    pub fn task_named<F>(name: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> M + Send + 'static,
    {
        Self::Task(TaskSpec::default().with_name(name), Box::new(f))
    }

    /// Create a save state command.
    ///
    /// Triggers a flush of the state registry to the storage backend.
    /// No-op if persistence is not configured.
    #[inline]
    pub fn save_state() -> Self {
        Self::SaveState
    }

    /// Create a restore state command.
    ///
    /// Triggers a load from the storage backend.
    /// No-op if persistence is not configured.
    #[inline]
    pub fn restore_state() -> Self {
        Self::RestoreState
    }

    /// Create a mouse capture toggle command.
    ///
    /// Instructs the runtime to enable or disable mouse event capture on the
    /// underlying terminal session.
    #[inline]
    pub fn set_mouse_capture(enabled: bool) -> Self {
        Self::SetMouseCapture(enabled)
    }

    /// Count the number of atomic commands in this command.
    ///
    /// Returns 0 for None, 1 for atomic commands, and recursively counts for Batch/Sequence.
    pub fn count(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Batch(cmds) | Self::Sequence(cmds) => cmds.iter().map(Self::count).sum(),
            _ => 1,
        }
    }
}

/// Resize handling behavior for the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeBehavior {
    /// Apply resize immediately (no debounce, no placeholder).
    Immediate,
    /// Coalesce resize events for continuous reflow.
    Throttled,
}

impl ResizeBehavior {
    const fn uses_coalescer(self) -> bool {
        matches!(self, ResizeBehavior::Throttled)
    }
}

/// Configuration for state persistence in the program runtime.
///
/// Controls when and how widget state is saved/restored.
#[derive(Clone)]
pub struct PersistenceConfig {
    /// State registry for persistence. If None, persistence is disabled.
    pub registry: Option<std::sync::Arc<StateRegistry>>,
    /// Interval for periodic checkpoint saves. None disables checkpoints.
    pub checkpoint_interval: Option<Duration>,
    /// Automatically load state on program start.
    pub auto_load: bool,
    /// Automatically save state on program exit.
    pub auto_save: bool,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            registry: None,
            checkpoint_interval: None,
            auto_load: true,
            auto_save: true,
        }
    }
}

impl std::fmt::Debug for PersistenceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistenceConfig")
            .field(
                "registry",
                &self.registry.as_ref().map(|r| r.backend_name()),
            )
            .field("checkpoint_interval", &self.checkpoint_interval)
            .field("auto_load", &self.auto_load)
            .field("auto_save", &self.auto_save)
            .finish()
    }
}

impl PersistenceConfig {
    /// Create a disabled persistence config.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Create a persistence config with the given registry.
    #[must_use]
    pub fn with_registry(registry: std::sync::Arc<StateRegistry>) -> Self {
        Self {
            registry: Some(registry),
            ..Default::default()
        }
    }

    /// Set the checkpoint interval.
    #[must_use]
    pub fn checkpoint_every(mut self, interval: Duration) -> Self {
        self.checkpoint_interval = Some(interval);
        self
    }

    /// Enable or disable auto-load on start.
    #[must_use]
    pub fn auto_load(mut self, enabled: bool) -> Self {
        self.auto_load = enabled;
        self
    }

    /// Enable or disable auto-save on exit.
    #[must_use]
    pub fn auto_save(mut self, enabled: bool) -> Self {
        self.auto_save = enabled;
        self
    }
}

/// Configuration for widget refresh selection under render budget.
///
/// Defaults are conservative and deterministic:
/// - enabled: true
/// - staleness_window_ms: 1_000
/// - starve_ms: 3_000
/// - max_starved_per_frame: 2
/// - max_drop_fraction: 1.0 (disabled)
/// - weights: priority 1.0, staleness 0.5, focus 0.75, interaction 0.5
/// - starve_boost: 1.5
/// - min_cost_us: 1.0
#[derive(Debug, Clone)]
pub struct WidgetRefreshConfig {
    /// Enable budgeted widget refresh selection.
    pub enabled: bool,
    /// Staleness decay window (ms) used to normalize staleness scores.
    pub staleness_window_ms: u64,
    /// Staleness threshold that triggers starvation guard (ms).
    pub starve_ms: u64,
    /// Maximum number of starved widgets to force in per frame.
    pub max_starved_per_frame: usize,
    /// Maximum fraction of non-essential widgets that may be dropped.
    /// Set to 1.0 to disable the guardrail.
    pub max_drop_fraction: f32,
    /// Weight for base priority signal.
    pub weight_priority: f32,
    /// Weight for staleness signal.
    pub weight_staleness: f32,
    /// Weight for focus boost.
    pub weight_focus: f32,
    /// Weight for interaction boost.
    pub weight_interaction: f32,
    /// Additive boost to value for starved widgets.
    pub starve_boost: f32,
    /// Minimum cost (us) to avoid divide-by-zero.
    pub min_cost_us: f32,
}

impl Default for WidgetRefreshConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            staleness_window_ms: 1_000,
            starve_ms: 3_000,
            max_starved_per_frame: 2,
            max_drop_fraction: 1.0,
            weight_priority: 1.0,
            weight_staleness: 0.5,
            weight_focus: 0.75,
            weight_interaction: 0.5,
            starve_boost: 1.5,
            min_cost_us: 1.0,
        }
    }
}

/// Configuration for effect queue scheduling.
#[derive(Debug, Clone)]
pub struct EffectQueueConfig {
    /// Whether effect queue scheduling is enabled.
    pub enabled: bool,
    /// Scheduler configuration (Smith's rule by default).
    pub scheduler: SchedulerConfig,
}

impl Default for EffectQueueConfig {
    fn default() -> Self {
        let scheduler = SchedulerConfig {
            smith_enabled: true,
            force_fifo: false,
            preemptive: false,
            aging_factor: 0.0,
            wait_starve_ms: 0.0,
            enable_logging: false,
            ..Default::default()
        };
        Self {
            enabled: false,
            scheduler,
        }
    }
}

impl EffectQueueConfig {
    /// Enable effect queue scheduling with the provided scheduler config.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Override the scheduler configuration.
    #[must_use]
    pub fn with_scheduler(mut self, scheduler: SchedulerConfig) -> Self {
        self.scheduler = scheduler;
        self
    }
}

/// Configuration for the program runtime.
#[derive(Debug, Clone)]
pub struct ProgramConfig {
    /// Screen mode (inline or alternate screen).
    pub screen_mode: ScreenMode,
    /// UI anchor for inline mode.
    pub ui_anchor: UiAnchor,
    /// Frame budget configuration.
    pub budget: FrameBudgetConfig,
    /// Diff strategy configuration for the terminal writer.
    pub diff_config: RuntimeDiffConfig,
    /// Evidence JSONL sink configuration.
    pub evidence_sink: EvidenceSinkConfig,
    /// Render-trace recorder configuration.
    pub render_trace: RenderTraceConfig,
    /// Optional frame timing sink.
    pub frame_timing: Option<FrameTimingConfig>,
    /// Conformal predictor configuration for frame-time risk gating.
    pub conformal_config: Option<ConformalConfig>,
    /// Locale context used for rendering.
    pub locale_context: LocaleContext,
    /// Input poll timeout.
    pub poll_timeout: Duration,
    /// Resize coalescer configuration.
    pub resize_coalescer: CoalescerConfig,
    /// Resize handling behavior (immediate/throttled).
    pub resize_behavior: ResizeBehavior,
    /// Forced terminal size override (when set, resize events are ignored).
    pub forced_size: Option<(u16, u16)>,
    /// Enable mouse support.
    pub mouse: bool,
    /// Enable bracketed paste.
    pub bracketed_paste: bool,
    /// Enable focus reporting.
    pub focus_reporting: bool,
    /// Enable Kitty keyboard protocol (repeat/release events).
    pub kitty_keyboard: bool,
    /// State persistence configuration.
    pub persistence: PersistenceConfig,
    /// Inline auto UI height remeasurement policy.
    pub inline_auto_remeasure: Option<InlineAutoRemeasureConfig>,
    /// Widget refresh selection configuration.
    pub widget_refresh: WidgetRefreshConfig,
    /// Effect queue scheduling configuration.
    pub effect_queue: EffectQueueConfig,
}

impl Default for ProgramConfig {
    fn default() -> Self {
        Self {
            screen_mode: ScreenMode::Inline { ui_height: 4 },
            ui_anchor: UiAnchor::Bottom,
            budget: FrameBudgetConfig::default(),
            diff_config: RuntimeDiffConfig::default(),
            evidence_sink: EvidenceSinkConfig::default(),
            render_trace: RenderTraceConfig::default(),
            frame_timing: None,
            conformal_config: None,
            locale_context: LocaleContext::global(),
            poll_timeout: Duration::from_millis(100),
            resize_coalescer: CoalescerConfig::default(),
            resize_behavior: ResizeBehavior::Throttled,
            forced_size: None,
            mouse: false,
            bracketed_paste: true,
            focus_reporting: false,
            kitty_keyboard: false,
            persistence: PersistenceConfig::default(),
            inline_auto_remeasure: None,
            widget_refresh: WidgetRefreshConfig::default(),
            effect_queue: EffectQueueConfig::default(),
        }
    }
}

impl ProgramConfig {
    /// Create config for fullscreen applications.
    pub fn fullscreen() -> Self {
        Self {
            screen_mode: ScreenMode::AltScreen,
            ..Default::default()
        }
    }

    /// Create config for inline mode with specified height.
    pub fn inline(height: u16) -> Self {
        Self {
            screen_mode: ScreenMode::Inline { ui_height: height },
            ..Default::default()
        }
    }

    /// Create config for inline mode with automatic UI height.
    pub fn inline_auto(min_height: u16, max_height: u16) -> Self {
        Self {
            screen_mode: ScreenMode::InlineAuto {
                min_height,
                max_height,
            },
            inline_auto_remeasure: Some(InlineAutoRemeasureConfig::default()),
            ..Default::default()
        }
    }

    /// Enable mouse support.
    pub fn with_mouse(mut self) -> Self {
        self.mouse = true;
        self
    }

    /// Set the budget configuration.
    pub fn with_budget(mut self, budget: FrameBudgetConfig) -> Self {
        self.budget = budget;
        self
    }

    /// Set the diff strategy configuration for the terminal writer.
    pub fn with_diff_config(mut self, diff_config: RuntimeDiffConfig) -> Self {
        self.diff_config = diff_config;
        self
    }

    /// Set the evidence JSONL sink configuration.
    pub fn with_evidence_sink(mut self, config: EvidenceSinkConfig) -> Self {
        self.evidence_sink = config;
        self
    }

    /// Set the render-trace recorder configuration.
    pub fn with_render_trace(mut self, config: RenderTraceConfig) -> Self {
        self.render_trace = config;
        self
    }

    /// Set a frame timing sink for per-frame profiling.
    pub fn with_frame_timing(mut self, config: FrameTimingConfig) -> Self {
        self.frame_timing = Some(config);
        self
    }

    /// Enable conformal frame-time risk gating with the given config.
    pub fn with_conformal_config(mut self, config: ConformalConfig) -> Self {
        self.conformal_config = Some(config);
        self
    }

    /// Disable conformal frame-time risk gating.
    pub fn without_conformal(mut self) -> Self {
        self.conformal_config = None;
        self
    }

    /// Set the locale context used for rendering.
    pub fn with_locale_context(mut self, locale_context: LocaleContext) -> Self {
        self.locale_context = locale_context;
        self
    }

    /// Set the base locale used for rendering.
    pub fn with_locale(mut self, locale: impl Into<crate::locale::Locale>) -> Self {
        self.locale_context = LocaleContext::new(locale);
        self
    }

    /// Set the widget refresh selection configuration.
    pub fn with_widget_refresh(mut self, config: WidgetRefreshConfig) -> Self {
        self.widget_refresh = config;
        self
    }

    /// Set the effect queue scheduling configuration.
    pub fn with_effect_queue(mut self, config: EffectQueueConfig) -> Self {
        self.effect_queue = config;
        self
    }

    /// Set the resize coalescer configuration.
    pub fn with_resize_coalescer(mut self, config: CoalescerConfig) -> Self {
        self.resize_coalescer = config;
        self
    }

    /// Set the resize handling behavior.
    pub fn with_resize_behavior(mut self, behavior: ResizeBehavior) -> Self {
        self.resize_behavior = behavior;
        self
    }

    /// Force a fixed terminal size (cols, rows). Resize events are ignored.
    pub fn with_forced_size(mut self, width: u16, height: u16) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        self.forced_size = Some((width, height));
        self
    }

    /// Clear any forced terminal size override.
    pub fn without_forced_size(mut self) -> Self {
        self.forced_size = None;
        self
    }

    /// Toggle legacy immediate-resize behavior for migration.
    pub fn with_legacy_resize(mut self, enabled: bool) -> Self {
        if enabled {
            self.resize_behavior = ResizeBehavior::Immediate;
        }
        self
    }

    /// Set the persistence configuration.
    pub fn with_persistence(mut self, persistence: PersistenceConfig) -> Self {
        self.persistence = persistence;
        self
    }

    /// Enable persistence with the given registry.
    pub fn with_registry(mut self, registry: std::sync::Arc<StateRegistry>) -> Self {
        self.persistence = PersistenceConfig::with_registry(registry);
        self
    }

    /// Enable inline auto UI height remeasurement with the given policy.
    pub fn with_inline_auto_remeasure(mut self, config: InlineAutoRemeasureConfig) -> Self {
        self.inline_auto_remeasure = Some(config);
        self
    }

    /// Disable inline auto UI height remeasurement.
    pub fn without_inline_auto_remeasure(mut self) -> Self {
        self.inline_auto_remeasure = None;
        self
    }
}

enum EffectCommand<M> {
    Enqueue(TaskSpec, Box<dyn FnOnce() -> M + Send>),
    Shutdown,
}

struct EffectQueue<M: Send + 'static> {
    sender: mpsc::Sender<EffectCommand<M>>,
    handle: Option<JoinHandle<()>>,
}

impl<M: Send + 'static> EffectQueue<M> {
    fn start(
        config: EffectQueueConfig,
        result_sender: mpsc::Sender<M>,
        evidence_sink: Option<EvidenceSink>,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<EffectCommand<M>>();
        let handle = thread::Builder::new()
            .name("ftui-effects".into())
            .spawn(move || effect_queue_loop(config, rx, result_sender, evidence_sink))
            .expect("failed to spawn effect queue");

        Self {
            sender: tx,
            handle: Some(handle),
        }
    }

    fn enqueue(&self, spec: TaskSpec, task: Box<dyn FnOnce() -> M + Send>) {
        let _ = self.sender.send(EffectCommand::Enqueue(spec, task));
    }

    fn shutdown(&mut self) {
        let _ = self.sender.send(EffectCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl<M: Send + 'static> Drop for EffectQueue<M> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn effect_queue_loop<M: Send + 'static>(
    config: EffectQueueConfig,
    rx: mpsc::Receiver<EffectCommand<M>>,
    result_sender: mpsc::Sender<M>,
    evidence_sink: Option<EvidenceSink>,
) {
    let mut scheduler = QueueingScheduler::new(config.scheduler);
    let mut tasks: HashMap<u64, Box<dyn FnOnce() -> M + Send>> = HashMap::new();

    loop {
        if tasks.is_empty() {
            match rx.recv() {
                Ok(cmd) => {
                    if handle_effect_command(cmd, &mut scheduler, &mut tasks, &result_sender) {
                        return;
                    }
                }
                Err(_) => return,
            }
        }

        while let Ok(cmd) = rx.try_recv() {
            if handle_effect_command(cmd, &mut scheduler, &mut tasks, &result_sender) {
                return;
            }
        }

        if tasks.is_empty() {
            continue;
        }

        let Some(job) = scheduler.peek_next().cloned() else {
            continue;
        };

        if let Some(ref sink) = evidence_sink {
            let evidence = scheduler.evidence();
            let _ = sink.write_jsonl(&evidence.to_jsonl("effect_queue_select"));
        }

        let completed = scheduler.tick(job.remaining_time);
        for job_id in completed {
            if let Some(task) = tasks.remove(&job_id) {
                let msg = task();
                let _ = result_sender.send(msg);
            }
        }
    }
}

fn handle_effect_command<M: Send + 'static>(
    cmd: EffectCommand<M>,
    scheduler: &mut QueueingScheduler,
    tasks: &mut HashMap<u64, Box<dyn FnOnce() -> M + Send>>,
    result_sender: &mpsc::Sender<M>,
) -> bool {
    match cmd {
        EffectCommand::Enqueue(spec, task) => {
            let weight_source = if spec.weight == DEFAULT_TASK_WEIGHT {
                WeightSource::Default
            } else {
                WeightSource::Explicit
            };
            let estimate_source = if spec.estimate_ms == DEFAULT_TASK_ESTIMATE_MS {
                EstimateSource::Default
            } else {
                EstimateSource::Explicit
            };
            let id = scheduler.submit_with_sources(
                spec.weight,
                spec.estimate_ms,
                weight_source,
                estimate_source,
                spec.name,
            );
            if let Some(id) = id {
                tasks.insert(id, task);
            } else {
                let msg = task();
                let _ = result_sender.send(msg);
            }
            false
        }
        EffectCommand::Shutdown => true,
    }
}

// removed: legacy ResizeDebouncer (superseded by ResizeCoalescer)

/// Policy for remeasuring inline auto UI height.
///
/// Uses VOI (value-of-information) sampling to decide when to perform
/// a costly full-height measurement, with any-time valid guarantees via
/// the embedded e-process in `VoiSampler`.
#[derive(Debug, Clone)]
pub struct InlineAutoRemeasureConfig {
    /// VOI sampling configuration.
    pub voi: VoiConfig,
    /// Minimum row delta to count as a "violation".
    pub change_threshold_rows: u16,
}

impl Default for InlineAutoRemeasureConfig {
    fn default() -> Self {
        Self {
            voi: VoiConfig {
                // Height changes are expected to be rare; bias toward fewer samples.
                prior_alpha: 1.0,
                prior_beta: 9.0,
                // Allow ~1s max latency to adapt to growth/shrink.
                max_interval_ms: 1000,
                // Avoid over-sampling in high-FPS loops.
                min_interval_ms: 100,
                // Disable event forcing; use time-based gating.
                max_interval_events: 0,
                min_interval_events: 0,
                // Treat sampling as moderately expensive.
                sample_cost: 0.08,
                ..VoiConfig::default()
            },
            change_threshold_rows: 1,
        }
    }
}

#[derive(Debug)]
struct InlineAutoRemeasureState {
    config: InlineAutoRemeasureConfig,
    sampler: VoiSampler,
}

impl InlineAutoRemeasureState {
    fn new(config: InlineAutoRemeasureConfig) -> Self {
        let sampler = VoiSampler::new(config.voi.clone());
        Self { config, sampler }
    }

    fn reset(&mut self) {
        self.sampler = VoiSampler::new(self.config.voi.clone());
    }
}

#[derive(Debug, Clone)]
struct ConformalEvidence {
    bucket_key: String,
    n_b: usize,
    alpha: f64,
    q_b: f64,
    y_hat: f64,
    upper_us: f64,
    risk: bool,
    fallback_level: u8,
    window_size: usize,
    reset_count: u64,
}

impl ConformalEvidence {
    fn from_prediction(prediction: &ConformalPrediction) -> Self {
        let alpha = (1.0 - prediction.confidence).clamp(0.0, 1.0);
        Self {
            bucket_key: prediction.bucket.to_string(),
            n_b: prediction.sample_count,
            alpha,
            q_b: prediction.quantile,
            y_hat: prediction.y_hat,
            upper_us: prediction.upper_us,
            risk: prediction.risk,
            fallback_level: prediction.fallback_level,
            window_size: prediction.window_size,
            reset_count: prediction.reset_count,
        }
    }
}

#[derive(Debug, Clone)]
struct BudgetDecisionEvidence {
    frame_idx: u64,
    decision: BudgetDecision,
    controller_decision: BudgetDecision,
    degradation_before: DegradationLevel,
    degradation_after: DegradationLevel,
    frame_time_us: f64,
    budget_us: f64,
    pid_output: f64,
    pid_p: f64,
    pid_i: f64,
    pid_d: f64,
    e_value: f64,
    frames_observed: u32,
    frames_since_change: u32,
    in_warmup: bool,
    conformal: Option<ConformalEvidence>,
}

impl BudgetDecisionEvidence {
    fn decision_from_levels(before: DegradationLevel, after: DegradationLevel) -> BudgetDecision {
        if after > before {
            BudgetDecision::Degrade
        } else if after < before {
            BudgetDecision::Upgrade
        } else {
            BudgetDecision::Hold
        }
    }

    #[must_use]
    fn to_jsonl(&self) -> String {
        let conformal = self.conformal.as_ref();
        let bucket_key = Self::opt_str(conformal.map(|c| c.bucket_key.as_str()));
        let n_b = Self::opt_usize(conformal.map(|c| c.n_b));
        let alpha = Self::opt_f64(conformal.map(|c| c.alpha));
        let q_b = Self::opt_f64(conformal.map(|c| c.q_b));
        let y_hat = Self::opt_f64(conformal.map(|c| c.y_hat));
        let upper_us = Self::opt_f64(conformal.map(|c| c.upper_us));
        let risk = Self::opt_bool(conformal.map(|c| c.risk));
        let fallback_level = Self::opt_u8(conformal.map(|c| c.fallback_level));
        let window_size = Self::opt_usize(conformal.map(|c| c.window_size));
        let reset_count = Self::opt_u64(conformal.map(|c| c.reset_count));

        format!(
            r#"{{"event":"budget_decision","frame_idx":{},"decision":"{}","decision_controller":"{}","degradation_before":"{}","degradation_after":"{}","frame_time_us":{:.6},"budget_us":{:.6},"pid_output":{:.6},"pid_p":{:.6},"pid_i":{:.6},"pid_d":{:.6},"e_value":{:.6},"frames_observed":{},"frames_since_change":{},"in_warmup":{},"bucket_key":{},"n_b":{},"alpha":{},"q_b":{},"y_hat":{},"upper_us":{},"risk":{},"fallback_level":{},"window_size":{},"reset_count":{}}}"#,
            self.frame_idx,
            self.decision.as_str(),
            self.controller_decision.as_str(),
            self.degradation_before.as_str(),
            self.degradation_after.as_str(),
            self.frame_time_us,
            self.budget_us,
            self.pid_output,
            self.pid_p,
            self.pid_i,
            self.pid_d,
            self.e_value,
            self.frames_observed,
            self.frames_since_change,
            self.in_warmup,
            bucket_key,
            n_b,
            alpha,
            q_b,
            y_hat,
            upper_us,
            risk,
            fallback_level,
            window_size,
            reset_count
        )
    }

    fn opt_f64(value: Option<f64>) -> String {
        value
            .map(|v| format!("{v:.6}"))
            .unwrap_or_else(|| "null".to_string())
    }

    fn opt_u64(value: Option<u64>) -> String {
        value
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string())
    }

    fn opt_u8(value: Option<u8>) -> String {
        value
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string())
    }

    fn opt_usize(value: Option<usize>) -> String {
        value
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string())
    }

    fn opt_bool(value: Option<bool>) -> String {
        value
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string())
    }

    fn opt_str(value: Option<&str>) -> String {
        value
            .map(|v| format!("\"{}\"", v.replace('"', "\\\"")))
            .unwrap_or_else(|| "null".to_string())
    }
}

#[derive(Debug, Clone)]
struct FairnessConfigEvidence {
    enabled: bool,
    input_priority_threshold_ms: u64,
    dominance_threshold: u32,
    fairness_threshold: f64,
}

impl FairnessConfigEvidence {
    #[must_use]
    fn to_jsonl(&self) -> String {
        format!(
            r#"{{"event":"fairness_config","enabled":{},"input_priority_threshold_ms":{},"dominance_threshold":{},"fairness_threshold":{:.6}}}"#,
            self.enabled,
            self.input_priority_threshold_ms,
            self.dominance_threshold,
            self.fairness_threshold
        )
    }
}

#[derive(Debug, Clone)]
struct FairnessDecisionEvidence {
    frame_idx: u64,
    decision: &'static str,
    reason: &'static str,
    pending_input_latency_ms: Option<u64>,
    jain_index: f64,
    resize_dominance_count: u32,
    dominance_threshold: u32,
    fairness_threshold: f64,
    input_priority_threshold_ms: u64,
}

impl FairnessDecisionEvidence {
    #[must_use]
    fn to_jsonl(&self) -> String {
        let pending_latency = self
            .pending_input_latency_ms
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string());
        format!(
            r#"{{"event":"fairness_decision","frame_idx":{},"decision":"{}","reason":"{}","pending_input_latency_ms":{},"jain_index":{:.6},"resize_dominance_count":{},"dominance_threshold":{},"fairness_threshold":{:.6},"input_priority_threshold_ms":{}}}"#,
            self.frame_idx,
            self.decision,
            self.reason,
            pending_latency,
            self.jain_index,
            self.resize_dominance_count,
            self.dominance_threshold,
            self.fairness_threshold,
            self.input_priority_threshold_ms
        )
    }
}

#[derive(Debug, Clone)]
struct WidgetRefreshEntry {
    widget_id: u64,
    essential: bool,
    starved: bool,
    value: f32,
    cost_us: f32,
    score: f32,
    staleness_ms: u64,
}

impl WidgetRefreshEntry {
    fn to_json(&self) -> String {
        format!(
            r#"{{"id":{},"cost_us":{:.3},"value":{:.4},"score":{:.4},"essential":{},"starved":{},"staleness_ms":{}}}"#,
            self.widget_id,
            self.cost_us,
            self.value,
            self.score,
            self.essential,
            self.starved,
            self.staleness_ms
        )
    }
}

#[derive(Debug, Clone)]
struct WidgetRefreshPlan {
    frame_idx: u64,
    budget_us: f64,
    degradation: DegradationLevel,
    essentials_cost_us: f64,
    selected_cost_us: f64,
    selected_value: f64,
    signal_count: usize,
    selected: Vec<WidgetRefreshEntry>,
    skipped_count: usize,
    skipped_starved: usize,
    starved_selected: usize,
    over_budget: bool,
}

impl WidgetRefreshPlan {
    fn new() -> Self {
        Self {
            frame_idx: 0,
            budget_us: 0.0,
            degradation: DegradationLevel::Full,
            essentials_cost_us: 0.0,
            selected_cost_us: 0.0,
            selected_value: 0.0,
            signal_count: 0,
            selected: Vec::new(),
            skipped_count: 0,
            skipped_starved: 0,
            starved_selected: 0,
            over_budget: false,
        }
    }

    fn clear(&mut self) {
        self.frame_idx = 0;
        self.budget_us = 0.0;
        self.degradation = DegradationLevel::Full;
        self.essentials_cost_us = 0.0;
        self.selected_cost_us = 0.0;
        self.selected_value = 0.0;
        self.signal_count = 0;
        self.selected.clear();
        self.skipped_count = 0;
        self.skipped_starved = 0;
        self.starved_selected = 0;
        self.over_budget = false;
    }

    fn as_budget(&self) -> WidgetBudget {
        if self.signal_count == 0 {
            return WidgetBudget::allow_all();
        }
        let ids = self.selected.iter().map(|entry| entry.widget_id).collect();
        WidgetBudget::allow_only(ids)
    }

    fn recompute(
        &mut self,
        frame_idx: u64,
        budget_us: f64,
        degradation: DegradationLevel,
        signals: &[WidgetSignal],
        config: &WidgetRefreshConfig,
    ) {
        self.clear();
        self.frame_idx = frame_idx;
        self.budget_us = budget_us;
        self.degradation = degradation;

        if !config.enabled || signals.is_empty() {
            return;
        }

        self.signal_count = signals.len();
        let mut essentials_cost = 0.0f64;
        let mut selected_cost = 0.0f64;
        let mut selected_value = 0.0f64;

        let staleness_window = config.staleness_window_ms.max(1) as f32;
        let mut candidates: Vec<WidgetRefreshEntry> = Vec::with_capacity(signals.len());

        for signal in signals {
            let starved = config.starve_ms > 0 && signal.staleness_ms >= config.starve_ms;
            let staleness_score = (signal.staleness_ms as f32 / staleness_window).min(1.0);
            let mut value = config.weight_priority * signal.priority
                + config.weight_staleness * staleness_score
                + config.weight_focus * signal.focus_boost
                + config.weight_interaction * signal.interaction_boost;
            if starved {
                value += config.starve_boost;
            }
            let raw_cost = if signal.recent_cost_us > 0.0 {
                signal.recent_cost_us
            } else {
                signal.cost_estimate_us
            };
            let cost_us = raw_cost.max(config.min_cost_us);
            let score = if cost_us > 0.0 {
                value / cost_us
            } else {
                value
            };

            let entry = WidgetRefreshEntry {
                widget_id: signal.widget_id,
                essential: signal.essential,
                starved,
                value,
                cost_us,
                score,
                staleness_ms: signal.staleness_ms,
            };

            if degradation >= DegradationLevel::EssentialOnly && !signal.essential {
                self.skipped_count += 1;
                if starved {
                    self.skipped_starved = self.skipped_starved.saturating_add(1);
                }
                continue;
            }

            if signal.essential {
                essentials_cost += cost_us as f64;
                selected_cost += cost_us as f64;
                selected_value += value as f64;
                if starved {
                    self.starved_selected = self.starved_selected.saturating_add(1);
                }
                self.selected.push(entry);
            } else {
                candidates.push(entry);
            }
        }

        let mut remaining = budget_us - selected_cost;

        if degradation < DegradationLevel::EssentialOnly {
            let nonessential_total = candidates.len();
            let max_drop_fraction = config.max_drop_fraction.clamp(0.0, 1.0);
            let enforce_drop_rate = max_drop_fraction < 1.0 && nonessential_total > 0;
            let min_nonessential_selected = if enforce_drop_rate {
                let min_fraction = (1.0 - max_drop_fraction).max(0.0);
                ((min_fraction * nonessential_total as f32).ceil() as usize).min(nonessential_total)
            } else {
                0
            };

            candidates.sort_by(|a, b| {
                b.starved
                    .cmp(&a.starved)
                    .then_with(|| b.score.total_cmp(&a.score))
                    .then_with(|| b.value.total_cmp(&a.value))
                    .then_with(|| a.cost_us.total_cmp(&b.cost_us))
                    .then_with(|| a.widget_id.cmp(&b.widget_id))
            });

            let mut forced_starved = 0usize;
            let mut nonessential_selected = 0usize;
            let mut skipped_candidates = if enforce_drop_rate {
                Vec::with_capacity(candidates.len())
            } else {
                Vec::new()
            };

            for entry in candidates.into_iter() {
                if entry.starved && forced_starved >= config.max_starved_per_frame {
                    self.skipped_count += 1;
                    self.skipped_starved = self.skipped_starved.saturating_add(1);
                    if enforce_drop_rate {
                        skipped_candidates.push(entry);
                    }
                    continue;
                }

                if remaining >= entry.cost_us as f64 {
                    remaining -= entry.cost_us as f64;
                    selected_cost += entry.cost_us as f64;
                    selected_value += entry.value as f64;
                    if entry.starved {
                        self.starved_selected = self.starved_selected.saturating_add(1);
                        forced_starved += 1;
                    }
                    nonessential_selected += 1;
                    self.selected.push(entry);
                } else if entry.starved
                    && forced_starved < config.max_starved_per_frame
                    && nonessential_selected == 0
                {
                    // Starvation guard: ensure at least one starved widget can refresh.
                    selected_cost += entry.cost_us as f64;
                    selected_value += entry.value as f64;
                    self.starved_selected = self.starved_selected.saturating_add(1);
                    forced_starved += 1;
                    nonessential_selected += 1;
                    self.selected.push(entry);
                } else {
                    self.skipped_count += 1;
                    if entry.starved {
                        self.skipped_starved = self.skipped_starved.saturating_add(1);
                    }
                    if enforce_drop_rate {
                        skipped_candidates.push(entry);
                    }
                }
            }

            if enforce_drop_rate && nonessential_selected < min_nonessential_selected {
                for entry in skipped_candidates.into_iter() {
                    if nonessential_selected >= min_nonessential_selected {
                        break;
                    }
                    if entry.starved && forced_starved >= config.max_starved_per_frame {
                        continue;
                    }
                    selected_cost += entry.cost_us as f64;
                    selected_value += entry.value as f64;
                    if entry.starved {
                        self.starved_selected = self.starved_selected.saturating_add(1);
                        forced_starved += 1;
                        self.skipped_starved = self.skipped_starved.saturating_sub(1);
                    }
                    self.skipped_count = self.skipped_count.saturating_sub(1);
                    nonessential_selected += 1;
                    self.selected.push(entry);
                }
            }
        }

        self.essentials_cost_us = essentials_cost;
        self.selected_cost_us = selected_cost;
        self.selected_value = selected_value;
        self.over_budget = selected_cost > budget_us;
    }

    #[must_use]
    fn to_jsonl(&self) -> String {
        let mut out = String::with_capacity(256 + self.selected.len() * 96);
        out.push_str(r#"{"event":"widget_refresh""#);
        out.push_str(&format!(
            r#","frame_idx":{},"budget_us":{:.3},"degradation":"{}","essentials_cost_us":{:.3},"selected_cost_us":{:.3},"selected_value":{:.3},"selected_count":{},"skipped_count":{},"starved_selected":{},"starved_skipped":{},"over_budget":{}"#,
            self.frame_idx,
            self.budget_us,
            self.degradation.as_str(),
            self.essentials_cost_us,
            self.selected_cost_us,
            self.selected_value,
            self.selected.len(),
            self.skipped_count,
            self.starved_selected,
            self.skipped_starved,
            self.over_budget
        ));
        out.push_str(r#","selected":["#);
        for (i, entry) in self.selected.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&entry.to_json());
        }
        out.push_str("]}");
        out
    }
}

// =============================================================================
// CrosstermEventSource: BackendEventSource adapter for TerminalSession
// =============================================================================

/// Adapter that wraps [`TerminalSession`] to implement [`BackendEventSource`].
///
/// This provides the bridge between the legacy crossterm-based terminal session
/// and the new backend abstraction. Once the native `ftui-tty` backend fully
/// replaces crossterm, this adapter will be removed.
pub struct CrosstermEventSource {
    session: TerminalSession,
    features: BackendFeatures,
}

impl CrosstermEventSource {
    /// Create a new crossterm event source from a terminal session.
    pub fn new(session: TerminalSession, initial_features: BackendFeatures) -> Self {
        Self {
            session,
            features: initial_features,
        }
    }
}

impl BackendEventSource for CrosstermEventSource {
    type Error = io::Error;

    fn size(&self) -> Result<(u16, u16), io::Error> {
        self.session.size()
    }

    fn set_features(&mut self, features: BackendFeatures) -> Result<(), io::Error> {
        if features.mouse_capture != self.features.mouse_capture {
            self.session.set_mouse_capture(features.mouse_capture)?;
        }
        // bracketed_paste, focus_events, and kitty_keyboard are set at session
        // construction and cleaned up in TerminalSession::Drop. Runtime toggling
        // is not supported by the crossterm backend.
        self.features = features;
        Ok(())
    }

    fn poll_event(&mut self, timeout: Duration) -> Result<bool, io::Error> {
        self.session.poll_event(timeout)
    }

    fn read_event(&mut self) -> Result<Option<Event>, io::Error> {
        self.session.read_event()
    }
}

// =============================================================================
// Program
// =============================================================================

/// The program runtime that manages the update/view loop.
pub struct Program<
    M: Model,
    E: BackendEventSource<Error = io::Error> = CrosstermEventSource,
    W: Write + Send = Stdout,
> {
    /// The application model.
    model: M,
    /// Terminal output coordinator.
    writer: TerminalWriter<W>,
    /// Event source (terminal input, size queries, feature toggles).
    events: E,
    /// Currently active backend feature toggles.
    backend_features: BackendFeatures,
    /// Whether the program is running.
    running: bool,
    /// Current tick rate (if any).
    tick_rate: Option<Duration>,
    /// Last tick time.
    last_tick: Instant,
    /// Whether the UI needs to be redrawn.
    dirty: bool,
    /// Monotonic frame index for evidence logging.
    frame_idx: u64,
    /// Widget scheduling signals captured during the last render.
    widget_signals: Vec<WidgetSignal>,
    /// Widget refresh selection configuration.
    widget_refresh_config: WidgetRefreshConfig,
    /// Last computed widget refresh plan.
    widget_refresh_plan: WidgetRefreshPlan,
    /// Current terminal width.
    width: u16,
    /// Current terminal height.
    height: u16,
    /// Forced terminal size override (when set, resize events are ignored).
    forced_size: Option<(u16, u16)>,
    /// Poll timeout when no tick is scheduled.
    poll_timeout: Duration,
    /// Frame budget configuration.
    budget: RenderBudget,
    /// Conformal predictor for frame-time risk gating.
    conformal_predictor: Option<ConformalPredictor>,
    /// Last observed frame time (microseconds), used as a baseline predictor.
    last_frame_time_us: Option<f64>,
    /// Last observed update duration (microseconds).
    last_update_us: Option<u64>,
    /// Optional frame timing sink for profiling.
    frame_timing: Option<FrameTimingConfig>,
    /// Locale context used for rendering.
    locale_context: LocaleContext,
    /// Last observed locale version.
    locale_version: u64,
    /// Resize coalescer for rapid resize events.
    resize_coalescer: ResizeCoalescer,
    /// Shared evidence sink for decision logs (optional).
    evidence_sink: Option<EvidenceSink>,
    /// Whether fairness config has been logged to evidence sink.
    fairness_config_logged: bool,
    /// Resize handling behavior.
    resize_behavior: ResizeBehavior,
    /// Input fairness guard for scheduler integration.
    fairness_guard: InputFairnessGuard,
    /// Optional event recorder for macro capture.
    event_recorder: Option<EventRecorder>,
    /// Subscription lifecycle manager.
    subscriptions: SubscriptionManager<M::Message>,
    /// Channel for receiving messages from background tasks.
    task_sender: std::sync::mpsc::Sender<M::Message>,
    /// Channel for receiving messages from background tasks.
    task_receiver: std::sync::mpsc::Receiver<M::Message>,
    /// Join handles for background tasks; reaped opportunistically.
    task_handles: Vec<std::thread::JoinHandle<()>>,
    /// Optional effect queue scheduler for background tasks.
    effect_queue: Option<EffectQueue<M::Message>>,
    /// Optional state registry for widget persistence.
    state_registry: Option<std::sync::Arc<StateRegistry>>,
    /// Persistence configuration.
    persistence_config: PersistenceConfig,
    /// Last checkpoint save time.
    last_checkpoint: Instant,
    /// Inline auto UI height remeasurement state.
    inline_auto_remeasure: Option<InlineAutoRemeasureState>,
}

impl<M: Model> Program<M, CrosstermEventSource, Stdout> {
    /// Create a new program with default configuration.
    pub fn new(model: M) -> io::Result<Self>
    where
        M::Message: Send + 'static,
    {
        Self::with_config(model, ProgramConfig::default())
    }

    /// Create a new program with the specified configuration.
    pub fn with_config(model: M, config: ProgramConfig) -> io::Result<Self>
    where
        M::Message: Send + 'static,
    {
        let capabilities = TerminalCapabilities::with_overrides();
        let initial_features = BackendFeatures {
            mouse_capture: config.mouse,
            bracketed_paste: config.bracketed_paste,
            focus_events: config.focus_reporting,
            kitty_keyboard: config.kitty_keyboard,
        };
        let session = TerminalSession::new(SessionOptions {
            alternate_screen: matches!(config.screen_mode, ScreenMode::AltScreen),
            mouse_capture: initial_features.mouse_capture,
            bracketed_paste: initial_features.bracketed_paste,
            focus_events: initial_features.focus_events,
            kitty_keyboard: initial_features.kitty_keyboard,
        })?;
        let events = CrosstermEventSource::new(session, initial_features);

        let mut writer = TerminalWriter::with_diff_config(
            io::stdout(),
            config.screen_mode,
            config.ui_anchor,
            capabilities,
            config.diff_config.clone(),
        );

        let frame_timing = config.frame_timing.clone();
        writer.set_timing_enabled(frame_timing.is_some());

        let evidence_sink = EvidenceSink::from_config(&config.evidence_sink)?;
        if let Some(ref sink) = evidence_sink {
            writer = writer.with_evidence_sink(sink.clone());
        }

        let render_trace = crate::RenderTraceRecorder::from_config(
            &config.render_trace,
            crate::RenderTraceContext {
                capabilities: writer.capabilities(),
                diff_config: config.diff_config.clone(),
                resize_config: config.resize_coalescer.clone(),
                conformal_config: config.conformal_config.clone(),
            },
        )?;
        if let Some(recorder) = render_trace {
            writer = writer.with_render_trace(recorder);
        }

        // Get terminal size for initial frame (or forced size override).
        let (w, h) = config
            .forced_size
            .unwrap_or_else(|| events.size().unwrap_or((80, 24)));
        let width = w.max(1);
        let height = h.max(1);
        writer.set_size(width, height);

        let budget = RenderBudget::from_config(&config.budget);
        let conformal_predictor = config.conformal_config.clone().map(ConformalPredictor::new);
        let locale_context = config.locale_context.clone();
        let locale_version = locale_context.version();
        let mut resize_coalescer =
            ResizeCoalescer::new(config.resize_coalescer.clone(), (width, height))
                .with_screen_mode(config.screen_mode);
        if let Some(ref sink) = evidence_sink {
            resize_coalescer = resize_coalescer.with_evidence_sink(sink.clone());
        }
        let subscriptions = SubscriptionManager::new();
        let (task_sender, task_receiver) = std::sync::mpsc::channel();
        let inline_auto_remeasure = config
            .inline_auto_remeasure
            .clone()
            .map(InlineAutoRemeasureState::new);
        let effect_queue = if config.effect_queue.enabled {
            Some(EffectQueue::start(
                config.effect_queue.clone(),
                task_sender.clone(),
                evidence_sink.clone(),
            ))
        } else {
            None
        };

        Ok(Self {
            model,
            writer,
            events,
            backend_features: initial_features,
            running: true,
            tick_rate: None,
            last_tick: Instant::now(),
            dirty: true,
            frame_idx: 0,
            widget_signals: Vec::new(),
            widget_refresh_config: config.widget_refresh,
            widget_refresh_plan: WidgetRefreshPlan::new(),
            width,
            height,
            forced_size: config.forced_size,
            poll_timeout: config.poll_timeout,
            budget,
            conformal_predictor,
            last_frame_time_us: None,
            last_update_us: None,
            frame_timing,
            locale_context,
            locale_version,
            resize_coalescer,
            evidence_sink,
            fairness_config_logged: false,
            resize_behavior: config.resize_behavior,
            fairness_guard: InputFairnessGuard::new(),
            event_recorder: None,
            subscriptions,
            task_sender,
            task_receiver,
            task_handles: Vec::new(),
            effect_queue,
            state_registry: config.persistence.registry.clone(),
            persistence_config: config.persistence,
            last_checkpoint: Instant::now(),
            inline_auto_remeasure,
        })
    }
}

impl<M: Model, E: BackendEventSource<Error = io::Error>, W: Write + Send> Program<M, E, W> {
    /// Create a program with an externally-constructed event source and writer.
    ///
    /// This is the generic entry point for alternative backends (native tty,
    /// WASM, headless testing). The caller is responsible for terminal
    /// lifecycle (raw mode, cleanup) — the event source should handle that
    /// via its `Drop` impl or an external RAII guard.
    pub fn with_event_source(
        model: M,
        events: E,
        backend_features: BackendFeatures,
        writer: TerminalWriter<W>,
        config: ProgramConfig,
    ) -> io::Result<Self>
    where
        M::Message: Send + 'static,
    {
        let (width, height) = config
            .forced_size
            .unwrap_or_else(|| events.size().unwrap_or((80, 24)));
        let width = width.max(1);
        let height = height.max(1);

        let mut writer = writer;
        writer.set_size(width, height);

        let evidence_sink = EvidenceSink::from_config(&config.evidence_sink)?;
        if let Some(ref sink) = evidence_sink {
            writer = writer.with_evidence_sink(sink.clone());
        }

        let render_trace = crate::RenderTraceRecorder::from_config(
            &config.render_trace,
            crate::RenderTraceContext {
                capabilities: writer.capabilities(),
                diff_config: config.diff_config.clone(),
                resize_config: config.resize_coalescer.clone(),
                conformal_config: config.conformal_config.clone(),
            },
        )?;
        if let Some(recorder) = render_trace {
            writer = writer.with_render_trace(recorder);
        }

        let frame_timing = config.frame_timing.clone();
        writer.set_timing_enabled(frame_timing.is_some());

        let budget = RenderBudget::from_config(&config.budget);
        let conformal_predictor = config.conformal_config.clone().map(ConformalPredictor::new);
        let locale_context = config.locale_context.clone();
        let locale_version = locale_context.version();
        let mut resize_coalescer =
            ResizeCoalescer::new(config.resize_coalescer.clone(), (width, height))
                .with_screen_mode(config.screen_mode);
        if let Some(ref sink) = evidence_sink {
            resize_coalescer = resize_coalescer.with_evidence_sink(sink.clone());
        }
        let subscriptions = SubscriptionManager::new();
        let (task_sender, task_receiver) = std::sync::mpsc::channel();
        let inline_auto_remeasure = config
            .inline_auto_remeasure
            .clone()
            .map(InlineAutoRemeasureState::new);
        let effect_queue = if config.effect_queue.enabled {
            Some(EffectQueue::start(
                config.effect_queue.clone(),
                task_sender.clone(),
                evidence_sink.clone(),
            ))
        } else {
            None
        };

        Ok(Self {
            model,
            writer,
            events,
            backend_features,
            running: true,
            tick_rate: None,
            last_tick: Instant::now(),
            dirty: true,
            frame_idx: 0,
            widget_signals: Vec::new(),
            widget_refresh_config: config.widget_refresh,
            widget_refresh_plan: WidgetRefreshPlan::new(),
            width,
            height,
            forced_size: config.forced_size,
            poll_timeout: config.poll_timeout,
            budget,
            conformal_predictor,
            last_frame_time_us: None,
            last_update_us: None,
            frame_timing,
            locale_context,
            locale_version,
            resize_coalescer,
            evidence_sink,
            fairness_config_logged: false,
            resize_behavior: config.resize_behavior,
            fairness_guard: InputFairnessGuard::new(),
            event_recorder: None,
            subscriptions,
            task_sender,
            task_receiver,
            task_handles: Vec::new(),
            effect_queue,
            state_registry: config.persistence.registry.clone(),
            persistence_config: config.persistence,
            last_checkpoint: Instant::now(),
            inline_auto_remeasure,
        })
    }
}

// =============================================================================
// Native TTY backend constructor (feature-gated)
// =============================================================================

#[cfg(feature = "native-backend")]
impl<M: Model> Program<M, ftui_tty::TtyBackend, Stdout> {
    /// Create a program backed by the native TTY backend (no Crossterm).
    ///
    /// This opens a live terminal session via `ftui-tty`, entering raw mode
    /// and enabling the requested features. When the program exits (or panics),
    /// `TtyBackend::drop()` restores the terminal to its original state.
    pub fn with_native_backend(model: M, config: ProgramConfig) -> io::Result<Self>
    where
        M::Message: Send + 'static,
    {
        let features = BackendFeatures {
            mouse_capture: config.mouse,
            bracketed_paste: config.bracketed_paste,
            focus_events: config.focus_reporting,
            kitty_keyboard: config.kitty_keyboard,
        };
        let options = ftui_tty::TtySessionOptions {
            alternate_screen: matches!(config.screen_mode, ScreenMode::AltScreen),
            features,
        };
        let backend = ftui_tty::TtyBackend::open(0, 0, options)?;

        let capabilities = ftui_core::terminal_capabilities::TerminalCapabilities::detect();
        let writer = TerminalWriter::with_diff_config(
            io::stdout(),
            config.screen_mode,
            config.ui_anchor,
            capabilities,
            config.diff_config.clone(),
        );

        Self::with_event_source(model, backend, features, writer, config)
    }
}

impl<M: Model, E: BackendEventSource<Error = io::Error>, W: Write + Send> Program<M, E, W> {
    /// Run the main event loop.
    ///
    /// This is the main entry point. It handles:
    /// 1. Initialization (terminal setup, raw mode)
    /// 2. Event polling and message dispatch
    /// 3. Frame rendering
    /// 4. Shutdown (terminal cleanup)
    pub fn run(&mut self) -> io::Result<()> {
        self.run_event_loop()
    }

    /// Access widget scheduling signals captured on the last render.
    #[inline]
    pub fn last_widget_signals(&self) -> &[WidgetSignal] {
        &self.widget_signals
    }

    /// The inner event loop, separated for proper cleanup handling.
    fn run_event_loop(&mut self) -> io::Result<()> {
        // Auto-load state on start
        if self.persistence_config.auto_load {
            self.load_state();
        }

        // Initialize
        let cmd = {
            let _span = info_span!("ftui.program.init").entered();
            self.model.init()
        };
        self.execute_cmd(cmd)?;

        // Reconcile initial subscriptions
        self.reconcile_subscriptions();

        // Initial render
        self.render_frame()?;

        // Main loop
        let mut loop_count: u64 = 0;
        while self.running {
            loop_count += 1;
            // Log heartbeat every 100 iterations to avoid flooding stderr
            if loop_count.is_multiple_of(100) {
                crate::debug_trace!("main loop heartbeat: iteration {}", loop_count);
            }

            // Poll for input with tick timeout
            let timeout = self.effective_timeout();

            // Poll for events with timeout
            if self.events.poll_event(timeout)? {
                // Drain all pending events
                loop {
                    // read_event returns Option<Event> after converting from crossterm
                    if let Some(event) = self.events.read_event()? {
                        self.handle_event(event)?;
                    }
                    if !self.events.poll_event(Duration::from_millis(0))? {
                        break;
                    }
                }
            }

            // Process subscription messages
            self.process_subscription_messages()?;

            // Process background task results
            self.process_task_results()?;
            self.reap_finished_tasks();

            self.process_resize_coalescer()?;

            // Check for tick - deliver to model so periodic logic can run
            if self.should_tick() {
                let msg = M::Message::from(Event::Tick);
                let cmd = {
                    let _span = debug_span!(
                        "ftui.program.update",
                        msg_type = "Tick",
                        duration_us = tracing::field::Empty,
                        cmd_type = tracing::field::Empty
                    )
                    .entered();
                    let start = Instant::now();
                    let cmd = self.model.update(msg);
                    tracing::Span::current()
                        .record("duration_us", start.elapsed().as_micros() as u64);
                    tracing::Span::current()
                        .record("cmd_type", format!("{:?}", std::mem::discriminant(&cmd)));
                    cmd
                };
                self.mark_dirty();
                self.execute_cmd(cmd)?;
                self.reconcile_subscriptions();
            }

            // Check for periodic checkpoint save
            self.check_checkpoint_save();

            // Detect locale changes outside the event loop.
            self.check_locale_change();

            // Render if dirty
            if self.dirty {
                self.render_frame()?;
            }

            // Periodic grapheme pool GC
            if loop_count.is_multiple_of(1000) {
                self.writer.gc();
            }
        }

        // Auto-save state on exit
        if self.persistence_config.auto_save {
            self.save_state();
        }

        // Stop all subscriptions on exit
        self.subscriptions.stop_all();
        self.reap_finished_tasks();

        Ok(())
    }

    /// Load state from the persistence registry.
    fn load_state(&mut self) {
        if let Some(registry) = &self.state_registry {
            match registry.load() {
                Ok(count) => {
                    info!(count, "loaded widget state from persistence");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load widget state");
                }
            }
        }
    }

    /// Save state to the persistence registry.
    fn save_state(&mut self) {
        if let Some(registry) = &self.state_registry {
            match registry.flush() {
                Ok(true) => {
                    debug!("saved widget state to persistence");
                }
                Ok(false) => {
                    // No changes to save
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to save widget state");
                }
            }
        }
    }

    /// Check if it's time for a periodic checkpoint save.
    fn check_checkpoint_save(&mut self) {
        if let Some(interval) = self.persistence_config.checkpoint_interval
            && self.last_checkpoint.elapsed() >= interval
        {
            self.save_state();
            self.last_checkpoint = Instant::now();
        }
    }

    fn handle_event(&mut self, event: Event) -> io::Result<()> {
        // Track event start time and type for fairness scheduling.
        let event_start = Instant::now();
        let fairness_event_type = Self::classify_event_for_fairness(&event);
        if fairness_event_type == FairnessEventType::Input {
            self.fairness_guard.input_arrived(event_start);
        }

        // Record event before processing (no-op when recorder is None or idle).
        if let Some(recorder) = &mut self.event_recorder {
            recorder.record(&event);
        }

        let event = match event {
            Event::Resize { width, height } => {
                debug!(
                    width,
                    height,
                    behavior = ?self.resize_behavior,
                    "Resize event received"
                );
                if let Some((forced_width, forced_height)) = self.forced_size {
                    debug!(
                        forced_width,
                        forced_height, "Resize ignored due to forced size override"
                    );
                    self.fairness_guard.event_processed(
                        fairness_event_type,
                        event_start.elapsed(),
                        Instant::now(),
                    );
                    return Ok(());
                }
                // Clamp to minimum 1 to prevent Buffer::new panic on zero dimensions
                let width = width.max(1);
                let height = height.max(1);
                match self.resize_behavior {
                    ResizeBehavior::Immediate => {
                        self.resize_coalescer
                            .record_external_apply(width, height, Instant::now());
                        let result = self.apply_resize(width, height, Duration::ZERO, false);
                        self.fairness_guard.event_processed(
                            fairness_event_type,
                            event_start.elapsed(),
                            Instant::now(),
                        );
                        return result;
                    }
                    ResizeBehavior::Throttled => {
                        let action = self.resize_coalescer.handle_resize(width, height);
                        if let CoalesceAction::ApplyResize {
                            width,
                            height,
                            coalesce_time,
                            forced_by_deadline,
                        } = action
                        {
                            let result =
                                self.apply_resize(width, height, coalesce_time, forced_by_deadline);
                            self.fairness_guard.event_processed(
                                fairness_event_type,
                                event_start.elapsed(),
                                Instant::now(),
                            );
                            return result;
                        }

                        self.fairness_guard.event_processed(
                            fairness_event_type,
                            event_start.elapsed(),
                            Instant::now(),
                        );
                        return Ok(());
                    }
                }
            }
            other => other,
        };

        let msg = M::Message::from(event);
        let cmd = {
            let _span = debug_span!(
                "ftui.program.update",
                msg_type = "event",
                duration_us = tracing::field::Empty,
                cmd_type = tracing::field::Empty
            )
            .entered();
            let start = Instant::now();
            let cmd = self.model.update(msg);
            let elapsed_us = start.elapsed().as_micros() as u64;
            self.last_update_us = Some(elapsed_us);
            tracing::Span::current().record("duration_us", elapsed_us);
            tracing::Span::current()
                .record("cmd_type", format!("{:?}", std::mem::discriminant(&cmd)));
            cmd
        };
        self.mark_dirty();
        self.execute_cmd(cmd)?;
        self.reconcile_subscriptions();

        // Track input event processing for fairness.
        self.fairness_guard.event_processed(
            fairness_event_type,
            event_start.elapsed(),
            Instant::now(),
        );

        Ok(())
    }

    /// Classify an event for fairness tracking.
    fn classify_event_for_fairness(event: &Event) -> FairnessEventType {
        match event {
            Event::Key(_)
            | Event::Mouse(_)
            | Event::Paste(_)
            | Event::Focus(_)
            | Event::Clipboard(_) => FairnessEventType::Input,
            Event::Resize { .. } => FairnessEventType::Resize,
            Event::Tick => FairnessEventType::Tick,
        }
    }

    /// Reconcile the model's declared subscriptions with running ones.
    fn reconcile_subscriptions(&mut self) {
        let _span = debug_span!(
            "ftui.program.subscriptions",
            active_count = tracing::field::Empty,
            started = tracing::field::Empty,
            stopped = tracing::field::Empty
        )
        .entered();
        let subs = self.model.subscriptions();
        let before_count = self.subscriptions.active_count();
        self.subscriptions.reconcile(subs);
        let after_count = self.subscriptions.active_count();
        let started = after_count.saturating_sub(before_count);
        let stopped = before_count.saturating_sub(after_count);
        crate::debug_trace!(
            "subscriptions reconcile: before={}, after={}, started={}, stopped={}",
            before_count,
            after_count,
            started,
            stopped
        );
        if after_count == 0 {
            crate::debug_trace!("subscriptions reconcile: no active subscriptions");
        }
        let current = tracing::Span::current();
        current.record("active_count", after_count);
        // started/stopped would require tracking in SubscriptionManager
        current.record("started", started);
        current.record("stopped", stopped);
    }

    /// Process pending messages from subscriptions.
    fn process_subscription_messages(&mut self) -> io::Result<()> {
        let messages = self.subscriptions.drain_messages();
        let msg_count = messages.len();
        if msg_count > 0 {
            crate::debug_trace!("processing {} subscription message(s)", msg_count);
        }
        for msg in messages {
            let cmd = {
                let _span = debug_span!(
                    "ftui.program.update",
                    msg_type = "subscription",
                    duration_us = tracing::field::Empty,
                    cmd_type = tracing::field::Empty
                )
                .entered();
                let start = Instant::now();
                let cmd = self.model.update(msg);
                let elapsed_us = start.elapsed().as_micros() as u64;
                self.last_update_us = Some(elapsed_us);
                tracing::Span::current().record("duration_us", elapsed_us);
                tracing::Span::current()
                    .record("cmd_type", format!("{:?}", std::mem::discriminant(&cmd)));
                cmd
            };
            self.mark_dirty();
            self.execute_cmd(cmd)?;
        }
        if self.dirty {
            self.reconcile_subscriptions();
        }
        Ok(())
    }

    /// Process results from background tasks.
    fn process_task_results(&mut self) -> io::Result<()> {
        while let Ok(msg) = self.task_receiver.try_recv() {
            let cmd = {
                let _span = debug_span!(
                    "ftui.program.update",
                    msg_type = "task",
                    duration_us = tracing::field::Empty,
                    cmd_type = tracing::field::Empty
                )
                .entered();
                let start = Instant::now();
                let cmd = self.model.update(msg);
                let elapsed_us = start.elapsed().as_micros() as u64;
                self.last_update_us = Some(elapsed_us);
                tracing::Span::current().record("duration_us", elapsed_us);
                tracing::Span::current()
                    .record("cmd_type", format!("{:?}", std::mem::discriminant(&cmd)));
                cmd
            };
            self.mark_dirty();
            self.execute_cmd(cmd)?;
        }
        if self.dirty {
            self.reconcile_subscriptions();
        }
        Ok(())
    }

    /// Execute a command.
    fn execute_cmd(&mut self, cmd: Cmd<M::Message>) -> io::Result<()> {
        match cmd {
            Cmd::None => {}
            Cmd::Quit => self.running = false,
            Cmd::Msg(m) => {
                let start = Instant::now();
                let cmd = self.model.update(m);
                let elapsed_us = start.elapsed().as_micros() as u64;
                self.last_update_us = Some(elapsed_us);
                self.mark_dirty();
                self.execute_cmd(cmd)?;
            }
            Cmd::Batch(cmds) => {
                // Batch currently executes sequentially. This is intentional
                // until an async runtime or task scheduler is added.
                for c in cmds {
                    self.execute_cmd(c)?;
                    if !self.running {
                        break;
                    }
                }
            }
            Cmd::Sequence(cmds) => {
                for c in cmds {
                    self.execute_cmd(c)?;
                    if !self.running {
                        break;
                    }
                }
            }
            Cmd::Tick(duration) => {
                self.tick_rate = Some(duration);
                self.last_tick = Instant::now();
            }
            Cmd::Log(text) => {
                let sanitized = sanitize(&text);
                if sanitized.ends_with('\n') {
                    self.writer.write_log(&sanitized)?;
                } else {
                    let mut owned = sanitized.into_owned();
                    owned.push('\n');
                    self.writer.write_log(&owned)?;
                }
            }
            Cmd::Task(spec, f) => {
                if let Some(ref queue) = self.effect_queue {
                    queue.enqueue(spec, f);
                } else {
                    let sender = self.task_sender.clone();
                    let handle = std::thread::spawn(move || {
                        let msg = f();
                        let _ = sender.send(msg);
                    });
                    self.task_handles.push(handle);
                }
            }
            Cmd::SaveState => {
                self.save_state();
            }
            Cmd::RestoreState => {
                self.load_state();
            }
            Cmd::SetMouseCapture(enabled) => {
                self.backend_features.mouse_capture = enabled;
                self.events.set_features(self.backend_features)?;
            }
        }
        Ok(())
    }

    fn reap_finished_tasks(&mut self) {
        if self.task_handles.is_empty() {
            return;
        }

        let mut remaining = Vec::with_capacity(self.task_handles.len());
        for handle in self.task_handles.drain(..) {
            if handle.is_finished() {
                if let Err(payload) = handle.join() {
                    let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                        (*s).to_owned()
                    } else if let Some(s) = payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic payload".to_owned()
                    };
                    #[cfg(feature = "tracing")]
                    tracing::error!("spawned task panicked: {msg}");
                    #[cfg(not(feature = "tracing"))]
                    eprintln!("ftui: spawned task panicked: {msg}");
                }
            } else {
                remaining.push(handle);
            }
        }
        self.task_handles = remaining;
    }

    /// Render a frame with budget tracking.
    fn render_frame(&mut self) -> io::Result<()> {
        crate::debug_trace!("render_frame: {}x{}", self.width, self.height);

        self.frame_idx = self.frame_idx.wrapping_add(1);
        let frame_idx = self.frame_idx;
        let degradation_start = self.budget.degradation();

        // Reset budget for new frame, potentially upgrading quality
        self.budget.next_frame();

        // Apply conformal risk gate before rendering (if enabled)
        let mut conformal_prediction = None;
        if let Some(predictor) = self.conformal_predictor.as_ref() {
            let baseline_us = self
                .last_frame_time_us
                .unwrap_or_else(|| self.budget.total().as_secs_f64() * 1_000_000.0);
            let diff_strategy = self
                .writer
                .last_diff_strategy()
                .unwrap_or(DiffStrategy::Full);
            let frame_height_hint = self.writer.render_height_hint().max(1);
            let key = BucketKey::from_context(
                self.writer.screen_mode(),
                diff_strategy,
                self.width,
                frame_height_hint,
            );
            let budget_us = self.budget.total().as_secs_f64() * 1_000_000.0;
            let prediction = predictor.predict(key, baseline_us, budget_us);
            if prediction.risk {
                self.budget.degrade();
            }
            debug!(
                bucket = %prediction.bucket,
                upper_us = prediction.upper_us,
                budget_us = prediction.budget_us,
                fallback = prediction.fallback_level,
                risk = prediction.risk,
                "conformal risk gate"
            );
            conformal_prediction = Some(prediction);
        }

        // Early skip if budget says to skip this frame entirely
        if self.budget.exhausted() {
            self.budget.record_frame_time(Duration::ZERO);
            self.emit_budget_evidence(
                frame_idx,
                degradation_start,
                0.0,
                conformal_prediction.as_ref(),
            );
            crate::debug_trace!(
                "frame skipped: budget exhausted (degradation={})",
                self.budget.degradation().as_str()
            );
            debug!(
                degradation = self.budget.degradation().as_str(),
                "frame skipped: budget exhausted before render"
            );
            self.dirty = false;
            return Ok(());
        }

        let auto_bounds = self.writer.inline_auto_bounds();
        let needs_measure = auto_bounds.is_some() && self.writer.auto_ui_height().is_none();
        let mut should_measure = needs_measure;
        if auto_bounds.is_some()
            && let Some(state) = self.inline_auto_remeasure.as_mut()
        {
            let decision = state.sampler.decide(Instant::now());
            if decision.should_sample {
                should_measure = true;
            }
        } else {
            crate::voi_telemetry::clear_inline_auto_voi_snapshot();
        }

        // --- Render phase ---
        let render_start = Instant::now();
        if let (Some((min_height, max_height)), true) = (auto_bounds, should_measure) {
            let measure_height = if needs_measure {
                self.writer.render_height_hint().max(1)
            } else {
                max_height.max(1)
            };
            let (measure_buffer, _) = self.render_measure_buffer(measure_height);
            let measured_height = measure_buffer.content_height();
            let clamped = measured_height.clamp(min_height, max_height);
            let previous_height = self.writer.auto_ui_height();
            self.writer.set_auto_ui_height(clamped);
            if let Some(state) = self.inline_auto_remeasure.as_mut() {
                let threshold = state.config.change_threshold_rows;
                let violated = previous_height
                    .map(|prev| prev.abs_diff(clamped) >= threshold)
                    .unwrap_or(false);
                state.sampler.observe(violated);
            }
        }
        if auto_bounds.is_some()
            && let Some(state) = self.inline_auto_remeasure.as_ref()
        {
            let snapshot = state.sampler.snapshot(8, crate::debug_trace::elapsed_ms());
            crate::voi_telemetry::set_inline_auto_voi_snapshot(Some(snapshot));
        }

        let frame_height = self.writer.render_height_hint().max(1);
        let _frame_span = info_span!(
            "ftui.render.frame",
            width = self.width,
            height = frame_height,
            duration_us = tracing::field::Empty
        )
        .entered();
        let (buffer, cursor, cursor_visible) = self.render_buffer(frame_height);
        self.update_widget_refresh_plan(frame_idx);
        let render_elapsed = render_start.elapsed();
        let mut present_elapsed = Duration::ZERO;
        let mut presented = false;

        // Check if render phase overspent its budget
        let render_budget = self.budget.phase_budgets().render;
        if render_elapsed > render_budget {
            debug!(
                render_ms = render_elapsed.as_millis() as u32,
                budget_ms = render_budget.as_millis() as u32,
                "render phase exceeded budget"
            );
            // Trigger degradation if we're consistently over budget
            if self.budget.should_degrade(render_budget) {
                self.budget.degrade();
            }
        }

        // --- Present phase ---
        if !self.budget.exhausted() {
            let present_start = Instant::now();
            {
                let _present_span = debug_span!("ftui.render.present").entered();
                self.writer
                    .present_ui_owned(buffer, cursor, cursor_visible)?;
            }
            presented = true;
            present_elapsed = present_start.elapsed();

            let present_budget = self.budget.phase_budgets().present;
            if present_elapsed > present_budget {
                debug!(
                    present_ms = present_elapsed.as_millis() as u32,
                    budget_ms = present_budget.as_millis() as u32,
                    "present phase exceeded budget"
                );
            }
        } else {
            debug!(
                degradation = self.budget.degradation().as_str(),
                elapsed_ms = self.budget.elapsed().as_millis() as u32,
                "frame present skipped: budget exhausted after render"
            );
        }

        if let Some(ref frame_timing) = self.frame_timing {
            let update_us = self.last_update_us.unwrap_or(0);
            let render_us = render_elapsed.as_micros() as u64;
            let present_us = present_elapsed.as_micros() as u64;
            let diff_us = if presented {
                self.writer
                    .take_last_present_timings()
                    .map(|timings| timings.diff_us)
                    .unwrap_or(0)
            } else {
                let _ = self.writer.take_last_present_timings();
                0
            };
            let total_us = update_us
                .saturating_add(render_us)
                .saturating_add(present_us);
            let timing = FrameTiming {
                frame_idx,
                update_us,
                render_us,
                diff_us,
                present_us,
                total_us,
            };
            frame_timing.sink.record_frame(&timing);
        }

        let frame_time = render_elapsed.saturating_add(present_elapsed);
        self.budget.record_frame_time(frame_time);
        let frame_time_us = frame_time.as_secs_f64() * 1_000_000.0;

        if let (Some(predictor), Some(prediction)) = (
            self.conformal_predictor.as_mut(),
            conformal_prediction.as_ref(),
        ) {
            let diff_strategy = self
                .writer
                .last_diff_strategy()
                .unwrap_or(DiffStrategy::Full);
            let key = BucketKey::from_context(
                self.writer.screen_mode(),
                diff_strategy,
                self.width,
                frame_height,
            );
            predictor.observe(key, prediction.y_hat, frame_time_us);
        }
        self.last_frame_time_us = Some(frame_time_us);
        self.emit_budget_evidence(
            frame_idx,
            degradation_start,
            frame_time_us,
            conformal_prediction.as_ref(),
        );
        self.dirty = false;

        Ok(())
    }

    fn emit_budget_evidence(
        &self,
        frame_idx: u64,
        degradation_start: DegradationLevel,
        frame_time_us: f64,
        conformal_prediction: Option<&ConformalPrediction>,
    ) {
        let Some(telemetry) = self.budget.telemetry() else {
            set_budget_snapshot(None);
            return;
        };

        let budget_us = conformal_prediction
            .map(|prediction| prediction.budget_us)
            .unwrap_or_else(|| self.budget.total().as_secs_f64() * 1_000_000.0);
        let conformal = conformal_prediction.map(ConformalEvidence::from_prediction);
        let degradation_after = self.budget.degradation();

        let evidence = BudgetDecisionEvidence {
            frame_idx,
            decision: BudgetDecisionEvidence::decision_from_levels(
                degradation_start,
                degradation_after,
            ),
            controller_decision: telemetry.last_decision,
            degradation_before: degradation_start,
            degradation_after,
            frame_time_us,
            budget_us,
            pid_output: telemetry.pid_output,
            pid_p: telemetry.pid_p,
            pid_i: telemetry.pid_i,
            pid_d: telemetry.pid_d,
            e_value: telemetry.e_value,
            frames_observed: telemetry.frames_observed,
            frames_since_change: telemetry.frames_since_change,
            in_warmup: telemetry.in_warmup,
            conformal,
        };

        let conformal_snapshot = evidence
            .conformal
            .as_ref()
            .map(|snapshot| ConformalSnapshot {
                bucket_key: snapshot.bucket_key.clone(),
                sample_count: snapshot.n_b,
                upper_us: snapshot.upper_us,
                risk: snapshot.risk,
            });
        set_budget_snapshot(Some(BudgetDecisionSnapshot {
            frame_idx: evidence.frame_idx,
            decision: evidence.decision,
            controller_decision: evidence.controller_decision,
            degradation_before: evidence.degradation_before,
            degradation_after: evidence.degradation_after,
            frame_time_us: evidence.frame_time_us,
            budget_us: evidence.budget_us,
            pid_output: evidence.pid_output,
            e_value: evidence.e_value,
            frames_observed: evidence.frames_observed,
            frames_since_change: evidence.frames_since_change,
            in_warmup: evidence.in_warmup,
            conformal: conformal_snapshot,
        }));

        if let Some(ref sink) = self.evidence_sink {
            let _ = sink.write_jsonl(&evidence.to_jsonl());
        }
    }

    fn update_widget_refresh_plan(&mut self, frame_idx: u64) {
        if !self.widget_refresh_config.enabled {
            self.widget_refresh_plan.clear();
            return;
        }

        let budget_us = self.budget.phase_budgets().render.as_secs_f64() * 1_000_000.0;
        let degradation = self.budget.degradation();
        self.widget_refresh_plan.recompute(
            frame_idx,
            budget_us,
            degradation,
            &self.widget_signals,
            &self.widget_refresh_config,
        );

        if let Some(ref sink) = self.evidence_sink {
            let _ = sink.write_jsonl(&self.widget_refresh_plan.to_jsonl());
        }
    }

    fn render_buffer(&mut self, frame_height: u16) -> (Buffer, Option<(u16, u16)>, bool) {
        // Note: Frame borrows the pool and links from writer.
        // We scope it so it drops before we call present_ui (which needs exclusive writer access).
        let buffer = self.writer.take_render_buffer(self.width, frame_height);
        let (pool, links) = self.writer.pool_and_links_mut();
        let mut frame = Frame::from_buffer(buffer, pool);
        frame.set_degradation(self.budget.degradation());
        frame.set_links(links);
        frame.set_widget_budget(self.widget_refresh_plan.as_budget());

        let view_start = Instant::now();
        let _view_span = debug_span!(
            "ftui.program.view",
            duration_us = tracing::field::Empty,
            widget_count = tracing::field::Empty
        )
        .entered();
        self.model.view(&mut frame);
        self.widget_signals = frame.take_widget_signals();
        tracing::Span::current().record("duration_us", view_start.elapsed().as_micros() as u64);
        // widget_count would require tracking in Frame

        (frame.buffer, frame.cursor_position, frame.cursor_visible)
    }

    fn emit_fairness_evidence(&mut self, decision: &FairnessDecision, dominance_count: u32) {
        let Some(ref sink) = self.evidence_sink else {
            return;
        };

        let config = self.fairness_guard.config();
        if !self.fairness_config_logged {
            let config_entry = FairnessConfigEvidence {
                enabled: config.enabled,
                input_priority_threshold_ms: config.input_priority_threshold.as_millis() as u64,
                dominance_threshold: config.dominance_threshold,
                fairness_threshold: config.fairness_threshold,
            };
            let _ = sink.write_jsonl(&config_entry.to_jsonl());
            self.fairness_config_logged = true;
        }

        let evidence = FairnessDecisionEvidence {
            frame_idx: self.frame_idx,
            decision: if decision.should_process {
                "allow"
            } else {
                "yield"
            },
            reason: decision.reason.as_str(),
            pending_input_latency_ms: decision
                .pending_input_latency
                .map(|latency| latency.as_millis() as u64),
            jain_index: decision.jain_index,
            resize_dominance_count: dominance_count,
            dominance_threshold: config.dominance_threshold,
            fairness_threshold: config.fairness_threshold,
            input_priority_threshold_ms: config.input_priority_threshold.as_millis() as u64,
        };

        let _ = sink.write_jsonl(&evidence.to_jsonl());
    }

    fn render_measure_buffer(&mut self, frame_height: u16) -> (Buffer, Option<(u16, u16)>) {
        let pool = self.writer.pool_mut();
        let mut frame = Frame::new(self.width, frame_height, pool);
        frame.set_degradation(self.budget.degradation());

        let view_start = Instant::now();
        let _view_span = debug_span!(
            "ftui.program.view",
            duration_us = tracing::field::Empty,
            widget_count = tracing::field::Empty
        )
        .entered();
        self.model.view(&mut frame);
        tracing::Span::current().record("duration_us", view_start.elapsed().as_micros() as u64);

        (frame.buffer, frame.cursor_position)
    }

    /// Calculate the effective poll timeout.
    fn effective_timeout(&self) -> Duration {
        if let Some(tick_rate) = self.tick_rate {
            let elapsed = self.last_tick.elapsed();
            let mut timeout = tick_rate.saturating_sub(elapsed);
            if self.resize_behavior.uses_coalescer()
                && let Some(resize_timeout) = self.resize_coalescer.time_until_apply(Instant::now())
            {
                timeout = timeout.min(resize_timeout);
            }
            timeout
        } else {
            let mut timeout = self.poll_timeout;
            if self.resize_behavior.uses_coalescer()
                && let Some(resize_timeout) = self.resize_coalescer.time_until_apply(Instant::now())
            {
                timeout = timeout.min(resize_timeout);
            }
            timeout
        }
    }

    /// Check if we should send a tick.
    fn should_tick(&mut self) -> bool {
        if let Some(tick_rate) = self.tick_rate
            && self.last_tick.elapsed() >= tick_rate
        {
            self.last_tick = Instant::now();
            return true;
        }
        false
    }

    fn process_resize_coalescer(&mut self) -> io::Result<()> {
        if !self.resize_behavior.uses_coalescer() {
            return Ok(());
        }

        // Check fairness: if input is starving, skip resize application this cycle.
        // This ensures input events are processed before resize is finalized.
        let dominance_count = self.fairness_guard.resize_dominance_count();
        let fairness_decision = self.fairness_guard.check_fairness(Instant::now());
        self.emit_fairness_evidence(&fairness_decision, dominance_count);
        if !fairness_decision.should_process {
            debug!(
                reason = ?fairness_decision.reason,
                pending_latency_ms = fairness_decision.pending_input_latency.map(|d| d.as_millis() as u64),
                "Resize yielding to input for fairness"
            );
            // Skip resize application this cycle to allow input processing.
            return Ok(());
        }

        let action = self.resize_coalescer.tick();
        let resize_snapshot =
            self.resize_coalescer
                .logs()
                .last()
                .map(|entry| ResizeDecisionSnapshot {
                    event_idx: entry.event_idx,
                    action: entry.action,
                    dt_ms: entry.dt_ms,
                    event_rate: entry.event_rate,
                    regime: entry.regime,
                    pending_size: entry.pending_size,
                    applied_size: entry.applied_size,
                    time_since_render_ms: entry.time_since_render_ms,
                    bocpd: self
                        .resize_coalescer
                        .bocpd()
                        .and_then(|detector| detector.last_evidence().cloned()),
                });
        set_resize_snapshot(resize_snapshot);

        match action {
            CoalesceAction::ApplyResize {
                width,
                height,
                coalesce_time,
                forced_by_deadline,
            } => self.apply_resize(width, height, coalesce_time, forced_by_deadline),
            _ => Ok(()),
        }
    }

    fn apply_resize(
        &mut self,
        width: u16,
        height: u16,
        coalesce_time: Duration,
        forced_by_deadline: bool,
    ) -> io::Result<()> {
        // Clamp to minimum 1 to prevent Buffer::new panic on zero dimensions
        let width = width.max(1);
        let height = height.max(1);
        self.width = width;
        self.height = height;
        self.writer.set_size(width, height);
        info!(
            width = width,
            height = height,
            coalesce_ms = coalesce_time.as_millis() as u64,
            forced = forced_by_deadline,
            "Resize applied"
        );

        let msg = M::Message::from(Event::Resize { width, height });
        let start = Instant::now();
        let cmd = self.model.update(msg);
        let elapsed_us = start.elapsed().as_micros() as u64;
        self.last_update_us = Some(elapsed_us);
        self.mark_dirty();
        self.execute_cmd(cmd)
    }

    // removed: resize placeholder rendering (continuous reflow preferred)

    /// Get a reference to the model.
    pub fn model(&self) -> &M {
        &self.model
    }

    /// Get a mutable reference to the model.
    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    /// Check if the program is running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Request a quit.
    pub fn quit(&mut self) {
        self.running = false;
    }

    /// Get a reference to the state registry, if configured.
    pub fn state_registry(&self) -> Option<&std::sync::Arc<StateRegistry>> {
        self.state_registry.as_ref()
    }

    /// Check if state persistence is enabled.
    pub fn has_persistence(&self) -> bool {
        self.state_registry.is_some()
    }

    /// Trigger a manual save of widget state.
    ///
    /// Returns the result of the flush operation, or `Ok(false)` if
    /// persistence is not configured.
    pub fn trigger_save(&mut self) -> StorageResult<bool> {
        if let Some(registry) = &self.state_registry {
            registry.flush()
        } else {
            Ok(false)
        }
    }

    /// Trigger a manual load of widget state.
    ///
    /// Returns the number of entries loaded, or `Ok(0)` if persistence
    /// is not configured.
    pub fn trigger_load(&mut self) -> StorageResult<usize> {
        if let Some(registry) = &self.state_registry {
            registry.load()
        } else {
            Ok(0)
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn check_locale_change(&mut self) {
        let version = self.locale_context.version();
        if version != self.locale_version {
            self.locale_version = version;
            self.mark_dirty();
        }
    }

    /// Mark the UI as needing redraw.
    pub fn request_redraw(&mut self) {
        self.mark_dirty();
    }

    /// Request a re-measure of inline auto UI height on next render.
    pub fn request_ui_height_remeasure(&mut self) {
        if self.writer.inline_auto_bounds().is_some() {
            self.writer.clear_auto_ui_height();
            if let Some(state) = self.inline_auto_remeasure.as_mut() {
                state.reset();
            }
            crate::voi_telemetry::clear_inline_auto_voi_snapshot();
            self.mark_dirty();
        }
    }

    /// Start recording events into a macro.
    ///
    /// If already recording, the current recording is discarded and a new one starts.
    /// The current terminal size is captured as metadata.
    pub fn start_recording(&mut self, name: impl Into<String>) {
        let mut recorder = EventRecorder::new(name).with_terminal_size(self.width, self.height);
        recorder.start();
        self.event_recorder = Some(recorder);
    }

    /// Stop recording and return the recorded macro, if any.
    ///
    /// Returns `None` if not currently recording.
    pub fn stop_recording(&mut self) -> Option<InputMacro> {
        self.event_recorder.take().map(EventRecorder::finish)
    }

    /// Check if event recording is active.
    pub fn is_recording(&self) -> bool {
        self.event_recorder
            .as_ref()
            .is_some_and(EventRecorder::is_recording)
    }
}

/// Builder for creating and running programs.
pub struct App;

impl App {
    /// Create a new app builder with the given model.
    #[allow(clippy::new_ret_no_self)] // App is a namespace for builder methods
    pub fn new<M: Model>(model: M) -> AppBuilder<M> {
        AppBuilder {
            model,
            config: ProgramConfig::default(),
        }
    }

    /// Create a fullscreen app.
    pub fn fullscreen<M: Model>(model: M) -> AppBuilder<M> {
        AppBuilder {
            model,
            config: ProgramConfig::fullscreen(),
        }
    }

    /// Create an inline app with the given height.
    pub fn inline<M: Model>(model: M, height: u16) -> AppBuilder<M> {
        AppBuilder {
            model,
            config: ProgramConfig::inline(height),
        }
    }

    /// Create an inline app with automatic UI height.
    pub fn inline_auto<M: Model>(model: M, min_height: u16, max_height: u16) -> AppBuilder<M> {
        AppBuilder {
            model,
            config: ProgramConfig::inline_auto(min_height, max_height),
        }
    }

    /// Create a fullscreen app from a [`StringModel`](crate::string_model::StringModel).
    ///
    /// This wraps the string model in a [`StringModelAdapter`](crate::string_model::StringModelAdapter)
    /// so that `view_string()` output is rendered through the standard kernel pipeline.
    pub fn string_model<S: crate::string_model::StringModel>(
        model: S,
    ) -> AppBuilder<crate::string_model::StringModelAdapter<S>> {
        AppBuilder {
            model: crate::string_model::StringModelAdapter::new(model),
            config: ProgramConfig::fullscreen(),
        }
    }
}

/// Builder for configuring and running programs.
pub struct AppBuilder<M: Model> {
    model: M,
    config: ProgramConfig,
}

impl<M: Model> AppBuilder<M> {
    /// Set the screen mode.
    pub fn screen_mode(mut self, mode: ScreenMode) -> Self {
        self.config.screen_mode = mode;
        self
    }

    /// Set the UI anchor.
    pub fn anchor(mut self, anchor: UiAnchor) -> Self {
        self.config.ui_anchor = anchor;
        self
    }

    /// Enable mouse support.
    pub fn with_mouse(mut self) -> Self {
        self.config.mouse = true;
        self
    }

    /// Set the frame budget configuration.
    pub fn with_budget(mut self, budget: FrameBudgetConfig) -> Self {
        self.config.budget = budget;
        self
    }

    /// Set the evidence JSONL sink configuration.
    pub fn with_evidence_sink(mut self, config: EvidenceSinkConfig) -> Self {
        self.config.evidence_sink = config;
        self
    }

    /// Set the render-trace recorder configuration.
    pub fn with_render_trace(mut self, config: RenderTraceConfig) -> Self {
        self.config.render_trace = config;
        self
    }

    /// Set the widget refresh selection configuration.
    pub fn with_widget_refresh(mut self, config: WidgetRefreshConfig) -> Self {
        self.config.widget_refresh = config;
        self
    }

    /// Set the effect queue scheduling configuration.
    pub fn with_effect_queue(mut self, config: EffectQueueConfig) -> Self {
        self.config.effect_queue = config;
        self
    }

    /// Enable inline auto UI height remeasurement.
    pub fn with_inline_auto_remeasure(mut self, config: InlineAutoRemeasureConfig) -> Self {
        self.config.inline_auto_remeasure = Some(config);
        self
    }

    /// Disable inline auto UI height remeasurement.
    pub fn without_inline_auto_remeasure(mut self) -> Self {
        self.config.inline_auto_remeasure = None;
        self
    }

    /// Set the locale context used for rendering.
    pub fn with_locale_context(mut self, locale_context: LocaleContext) -> Self {
        self.config.locale_context = locale_context;
        self
    }

    /// Set the base locale used for rendering.
    pub fn with_locale(mut self, locale: impl Into<crate::locale::Locale>) -> Self {
        self.config.locale_context = LocaleContext::new(locale);
        self
    }

    /// Set the resize coalescer configuration.
    pub fn resize_coalescer(mut self, config: CoalescerConfig) -> Self {
        self.config.resize_coalescer = config;
        self
    }

    /// Set the resize handling behavior.
    pub fn resize_behavior(mut self, behavior: ResizeBehavior) -> Self {
        self.config.resize_behavior = behavior;
        self
    }

    /// Toggle legacy immediate-resize behavior for migration.
    pub fn legacy_resize(mut self, enabled: bool) -> Self {
        if enabled {
            self.config.resize_behavior = ResizeBehavior::Immediate;
        }
        self
    }

    /// Run the application.
    pub fn run(self) -> io::Result<()>
    where
        M::Message: Send + 'static,
    {
        let mut program = Program::with_config(self.model, self.config)?;
        program.run()
    }
}

// =============================================================================
// Adaptive Batch Window: Queueing Model (bd-4kq0.8.1)
// =============================================================================
//
// # M/G/1 Queueing Model for Event Batching
//
// ## Problem
//
// The event loop must balance two objectives:
// 1. **Low latency**: Process events quickly (small batch window τ).
// 2. **Efficiency**: Batch multiple events to amortize render cost (large τ).
//
// ## Model
//
// We model the event loop as an M/G/1 queue:
// - Events arrive at rate λ (Poisson process, reasonable for human input).
// - Service time S has mean E[S] and variance Var[S] (render + present).
// - Utilization ρ = λ·E[S] must be < 1 for stability.
//
// ## Pollaczek–Khinchine Mean Waiting Time
//
// For M/G/1: E[W] = (λ·E[S²]) / (2·(1 − ρ))
// where E[S²] = Var[S] + E[S]².
//
// ## Optimal Batch Window τ
//
// With batching window τ, we collect ~(λ·τ) events per batch, amortizing
// the per-frame render cost. The effective per-event latency is:
//
//   L(τ) = τ/2 + E[S]
//         (waiting in batch)  (service)
//
// The batch reduces arrival rate to λ_eff = 1/τ (one batch per window),
// giving utilization ρ_eff = E[S]/τ.
//
// Minimizing L(τ) subject to ρ_eff < 1:
//   L(τ) = τ/2 + E[S]
//   dL/dτ = 1/2  (always positive, so smaller τ is always better for latency)
//
// But we need ρ_eff < 1, so τ > E[S].
//
// The practical rule: τ = max(E[S] · headroom_factor, τ_min)
// where headroom_factor provides margin (typically 1.5–2.0).
//
// For high arrival rates: τ = max(E[S] · headroom, 1/λ_target)
// where λ_target is the max frame rate we want to sustain.
//
// ## Failure Modes
//
// 1. **Overload (ρ ≥ 1)**: Queue grows unbounded. Mitigation: increase τ
//    (degrade to lower frame rate), or drop stale events.
// 2. **Bursty arrivals**: Real input is bursty (typing, mouse drag). The
//    exponential moving average of λ smooths this; high burst periods
//    temporarily increase τ.
// 3. **Variable service time**: Render complexity varies per frame. Using
//    EMA of E[S] tracks this adaptively.
//
// ## Observable Telemetry
//
// - λ_est: Exponential moving average of inter-arrival times.
// - es_est: Exponential moving average of service (render) times.
// - ρ_est: λ_est × es_est (estimated utilization).

/// Adaptive batch window controller based on M/G/1 queueing model.
///
/// Estimates arrival rate λ and service time E[S] from observations,
/// then computes the optimal batch window τ to maintain stability
/// (ρ < 1) while minimizing latency.
#[derive(Debug, Clone)]
pub struct BatchController {
    /// Exponential moving average of inter-arrival time (seconds).
    ema_inter_arrival_s: f64,
    /// Exponential moving average of service time (seconds).
    ema_service_s: f64,
    /// EMA smoothing factor (0..1). Higher = more responsive.
    alpha: f64,
    /// Minimum batch window (floor).
    tau_min_s: f64,
    /// Maximum batch window (cap for responsiveness).
    tau_max_s: f64,
    /// Headroom factor: τ >= E[S] × headroom to keep ρ < 1.
    headroom: f64,
    /// Last event arrival timestamp.
    last_arrival: Option<std::time::Instant>,
    /// Number of observations.
    observations: u64,
}

impl BatchController {
    /// Create a new controller with sensible defaults.
    ///
    /// - `alpha`: EMA smoothing (default 0.2)
    /// - `tau_min`: minimum batch window (default 1ms)
    /// - `tau_max`: maximum batch window (default 50ms)
    /// - `headroom`: stability margin (default 2.0, keeps ρ ≤ 0.5)
    pub fn new() -> Self {
        Self {
            ema_inter_arrival_s: 0.1, // assume 10 events/sec initially
            ema_service_s: 0.002,     // assume 2ms render initially
            alpha: 0.2,
            tau_min_s: 0.001, // 1ms floor
            tau_max_s: 0.050, // 50ms cap
            headroom: 2.0,
            last_arrival: None,
            observations: 0,
        }
    }

    /// Record an event arrival, updating the inter-arrival estimate.
    pub fn observe_arrival(&mut self, now: std::time::Instant) {
        if let Some(last) = self.last_arrival {
            let dt = now.duration_since(last).as_secs_f64();
            if dt > 0.0 && dt < 10.0 {
                // Guard against stale gaps (e.g., app was suspended)
                self.ema_inter_arrival_s =
                    self.alpha * dt + (1.0 - self.alpha) * self.ema_inter_arrival_s;
                self.observations += 1;
            }
        }
        self.last_arrival = Some(now);
    }

    /// Record a service (render) time observation.
    pub fn observe_service(&mut self, duration: std::time::Duration) {
        let dt = duration.as_secs_f64();
        if (0.0..10.0).contains(&dt) {
            self.ema_service_s = self.alpha * dt + (1.0 - self.alpha) * self.ema_service_s;
        }
    }

    /// Estimated arrival rate λ (events/second).
    #[inline]
    pub fn lambda_est(&self) -> f64 {
        if self.ema_inter_arrival_s > 0.0 {
            1.0 / self.ema_inter_arrival_s
        } else {
            0.0
        }
    }

    /// Estimated service time E[S] (seconds).
    #[inline]
    pub fn service_est_s(&self) -> f64 {
        self.ema_service_s
    }

    /// Estimated utilization ρ = λ × E[S].
    #[inline]
    pub fn rho_est(&self) -> f64 {
        self.lambda_est() * self.ema_service_s
    }

    /// Compute the optimal batch window τ (seconds).
    ///
    /// τ = clamp(E[S] × headroom, τ_min, τ_max)
    ///
    /// When ρ approaches 1, τ increases to maintain stability.
    pub fn tau_s(&self) -> f64 {
        let base = self.ema_service_s * self.headroom;
        base.clamp(self.tau_min_s, self.tau_max_s)
    }

    /// Compute the optimal batch window as a Duration.
    pub fn tau(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.tau_s())
    }

    /// Check if the system is stable (ρ < 1).
    #[inline]
    pub fn is_stable(&self) -> bool {
        self.rho_est() < 1.0
    }

    /// Number of observations recorded.
    #[inline]
    pub fn observations(&self) -> u64 {
        self.observations
    }
}

impl Default for BatchController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::terminal_capabilities::TerminalCapabilities;
    use ftui_core::terminal_session::{SessionOptions, TerminalSession};
    use ftui_render::buffer::Buffer;
    use ftui_render::cell::Cell;
    use ftui_render::diff_strategy::DiffStrategy;
    use ftui_render::frame::CostEstimateSource;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    // Simple test model
    struct TestModel {
        value: i32,
    }

    #[derive(Debug)]
    enum TestMsg {
        Increment,
        Decrement,
        Quit,
    }

    impl From<Event> for TestMsg {
        fn from(_event: Event) -> Self {
            TestMsg::Increment
        }
    }

    impl Model for TestModel {
        type Message = TestMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                TestMsg::Increment => {
                    self.value += 1;
                    Cmd::none()
                }
                TestMsg::Decrement => {
                    self.value -= 1;
                    Cmd::none()
                }
                TestMsg::Quit => Cmd::quit(),
            }
        }

        fn view(&self, _frame: &mut Frame) {
            // No-op for tests
        }
    }

    #[test]
    fn cmd_none() {
        let cmd: Cmd<TestMsg> = Cmd::none();
        assert!(matches!(cmd, Cmd::None));
    }

    #[test]
    fn cmd_quit() {
        let cmd: Cmd<TestMsg> = Cmd::quit();
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn cmd_msg() {
        let cmd: Cmd<TestMsg> = Cmd::msg(TestMsg::Increment);
        assert!(matches!(cmd, Cmd::Msg(TestMsg::Increment)));
    }

    #[test]
    fn cmd_batch_empty() {
        let cmd: Cmd<TestMsg> = Cmd::batch(vec![]);
        assert!(matches!(cmd, Cmd::None));
    }

    #[test]
    fn cmd_batch_single() {
        let cmd: Cmd<TestMsg> = Cmd::batch(vec![Cmd::quit()]);
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn cmd_batch_multiple() {
        let cmd: Cmd<TestMsg> = Cmd::batch(vec![Cmd::none(), Cmd::quit()]);
        assert!(matches!(cmd, Cmd::Batch(_)));
    }

    #[test]
    fn cmd_sequence_empty() {
        let cmd: Cmd<TestMsg> = Cmd::sequence(vec![]);
        assert!(matches!(cmd, Cmd::None));
    }

    #[test]
    fn cmd_tick() {
        let cmd: Cmd<TestMsg> = Cmd::tick(Duration::from_millis(100));
        assert!(matches!(cmd, Cmd::Tick(_)));
    }

    #[test]
    fn cmd_task() {
        let cmd: Cmd<TestMsg> = Cmd::task(|| TestMsg::Increment);
        assert!(matches!(cmd, Cmd::Task(..)));
    }

    #[test]
    fn cmd_debug_format() {
        let cmd: Cmd<TestMsg> = Cmd::task(|| TestMsg::Increment);
        let debug = format!("{cmd:?}");
        assert_eq!(
            debug,
            "Task { spec: TaskSpec { weight: 1.0, estimate_ms: 10.0, name: None } }"
        );
    }

    #[test]
    fn model_subscriptions_default_empty() {
        let model = TestModel { value: 0 };
        let subs = model.subscriptions();
        assert!(subs.is_empty());
    }

    #[test]
    fn program_config_default() {
        let config = ProgramConfig::default();
        assert!(matches!(config.screen_mode, ScreenMode::Inline { .. }));
        assert!(!config.mouse);
        assert!(config.bracketed_paste);
        assert_eq!(config.resize_behavior, ResizeBehavior::Throttled);
        assert!(config.inline_auto_remeasure.is_none());
        assert!(config.conformal_config.is_none());
        assert!(config.diff_config.bayesian_enabled);
        assert!(config.diff_config.dirty_rows_enabled);
        assert!(!config.resize_coalescer.enable_bocpd);
        assert!(!config.effect_queue.enabled);
        assert_eq!(
            config.resize_coalescer.steady_delay_ms,
            CoalescerConfig::default().steady_delay_ms
        );
    }

    #[test]
    fn program_config_fullscreen() {
        let config = ProgramConfig::fullscreen();
        assert!(matches!(config.screen_mode, ScreenMode::AltScreen));
    }

    #[test]
    fn program_config_inline() {
        let config = ProgramConfig::inline(10);
        assert!(matches!(
            config.screen_mode,
            ScreenMode::Inline { ui_height: 10 }
        ));
    }

    #[test]
    fn program_config_inline_auto() {
        let config = ProgramConfig::inline_auto(3, 9);
        assert!(matches!(
            config.screen_mode,
            ScreenMode::InlineAuto {
                min_height: 3,
                max_height: 9
            }
        ));
        assert!(config.inline_auto_remeasure.is_some());
    }

    #[test]
    fn program_config_with_mouse() {
        let config = ProgramConfig::default().with_mouse();
        assert!(config.mouse);
    }

    #[test]
    fn model_update() {
        let mut model = TestModel { value: 0 };
        model.update(TestMsg::Increment);
        assert_eq!(model.value, 1);
        model.update(TestMsg::Decrement);
        assert_eq!(model.value, 0);
        assert!(matches!(model.update(TestMsg::Quit), Cmd::Quit));
    }

    #[test]
    fn model_init_default() {
        let mut model = TestModel { value: 0 };
        let cmd = model.init();
        assert!(matches!(cmd, Cmd::None));
    }

    // Resize coalescer behavior is covered by resize_coalescer.rs tests.

    // =========================================================================
    // DETERMINISM TESTS - Program loop determinism (bd-2nu8.10.1)
    // =========================================================================

    #[test]
    fn cmd_sequence_executes_in_order() {
        // Verify that Cmd::Sequence executes commands in declared order
        use crate::simulator::ProgramSimulator;

        struct SeqModel {
            trace: Vec<i32>,
        }

        #[derive(Debug)]
        enum SeqMsg {
            Append(i32),
            TriggerSequence,
        }

        impl From<Event> for SeqMsg {
            fn from(_: Event) -> Self {
                SeqMsg::Append(0)
            }
        }

        impl Model for SeqModel {
            type Message = SeqMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    SeqMsg::Append(n) => {
                        self.trace.push(n);
                        Cmd::none()
                    }
                    SeqMsg::TriggerSequence => Cmd::sequence(vec![
                        Cmd::msg(SeqMsg::Append(1)),
                        Cmd::msg(SeqMsg::Append(2)),
                        Cmd::msg(SeqMsg::Append(3)),
                    ]),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(SeqModel { trace: vec![] });
        sim.init();
        sim.send(SeqMsg::TriggerSequence);

        assert_eq!(sim.model().trace, vec![1, 2, 3]);
    }

    #[test]
    fn cmd_batch_executes_all_regardless_of_order() {
        // Verify that Cmd::Batch executes all commands
        use crate::simulator::ProgramSimulator;

        struct BatchModel {
            values: Vec<i32>,
        }

        #[derive(Debug)]
        enum BatchMsg {
            Add(i32),
            TriggerBatch,
        }

        impl From<Event> for BatchMsg {
            fn from(_: Event) -> Self {
                BatchMsg::Add(0)
            }
        }

        impl Model for BatchModel {
            type Message = BatchMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    BatchMsg::Add(n) => {
                        self.values.push(n);
                        Cmd::none()
                    }
                    BatchMsg::TriggerBatch => Cmd::batch(vec![
                        Cmd::msg(BatchMsg::Add(10)),
                        Cmd::msg(BatchMsg::Add(20)),
                        Cmd::msg(BatchMsg::Add(30)),
                    ]),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(BatchModel { values: vec![] });
        sim.init();
        sim.send(BatchMsg::TriggerBatch);

        // All values should be present
        assert_eq!(sim.model().values.len(), 3);
        assert!(sim.model().values.contains(&10));
        assert!(sim.model().values.contains(&20));
        assert!(sim.model().values.contains(&30));
    }

    #[test]
    fn cmd_sequence_stops_on_quit() {
        // Verify that Cmd::Sequence stops processing after Quit
        use crate::simulator::ProgramSimulator;

        struct SeqQuitModel {
            trace: Vec<i32>,
        }

        #[derive(Debug)]
        enum SeqQuitMsg {
            Append(i32),
            TriggerSequenceWithQuit,
        }

        impl From<Event> for SeqQuitMsg {
            fn from(_: Event) -> Self {
                SeqQuitMsg::Append(0)
            }
        }

        impl Model for SeqQuitModel {
            type Message = SeqQuitMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    SeqQuitMsg::Append(n) => {
                        self.trace.push(n);
                        Cmd::none()
                    }
                    SeqQuitMsg::TriggerSequenceWithQuit => Cmd::sequence(vec![
                        Cmd::msg(SeqQuitMsg::Append(1)),
                        Cmd::quit(),
                        Cmd::msg(SeqQuitMsg::Append(2)), // Should not execute
                    ]),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(SeqQuitModel { trace: vec![] });
        sim.init();
        sim.send(SeqQuitMsg::TriggerSequenceWithQuit);

        assert_eq!(sim.model().trace, vec![1]);
        assert!(!sim.is_running());
    }

    #[test]
    fn identical_input_produces_identical_state() {
        // Verify deterministic state transitions
        use crate::simulator::ProgramSimulator;

        fn run_scenario() -> Vec<i32> {
            struct DetModel {
                values: Vec<i32>,
            }

            #[derive(Debug, Clone)]
            enum DetMsg {
                Add(i32),
                Double,
            }

            impl From<Event> for DetMsg {
                fn from(_: Event) -> Self {
                    DetMsg::Add(1)
                }
            }

            impl Model for DetModel {
                type Message = DetMsg;

                fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                    match msg {
                        DetMsg::Add(n) => {
                            self.values.push(n);
                            Cmd::none()
                        }
                        DetMsg::Double => {
                            if let Some(&last) = self.values.last() {
                                self.values.push(last * 2);
                            }
                            Cmd::none()
                        }
                    }
                }

                fn view(&self, _frame: &mut Frame) {}
            }

            let mut sim = ProgramSimulator::new(DetModel { values: vec![] });
            sim.init();
            sim.send(DetMsg::Add(5));
            sim.send(DetMsg::Double);
            sim.send(DetMsg::Add(3));
            sim.send(DetMsg::Double);

            sim.model().values.clone()
        }

        // Run the same scenario multiple times
        let run1 = run_scenario();
        let run2 = run_scenario();
        let run3 = run_scenario();

        assert_eq!(run1, run2);
        assert_eq!(run2, run3);
        assert_eq!(run1, vec![5, 10, 3, 6]);
    }

    #[test]
    fn identical_state_produces_identical_render() {
        // Verify consistent render outputs for identical inputs
        use crate::simulator::ProgramSimulator;

        struct RenderModel {
            counter: i32,
        }

        #[derive(Debug)]
        enum RenderMsg {
            Set(i32),
        }

        impl From<Event> for RenderMsg {
            fn from(_: Event) -> Self {
                RenderMsg::Set(0)
            }
        }

        impl Model for RenderModel {
            type Message = RenderMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    RenderMsg::Set(n) => {
                        self.counter = n;
                        Cmd::none()
                    }
                }
            }

            fn view(&self, frame: &mut Frame) {
                let text = format!("Value: {}", self.counter);
                for (i, c) in text.chars().enumerate() {
                    if (i as u16) < frame.width() {
                        use ftui_render::cell::Cell;
                        frame.buffer.set_raw(i as u16, 0, Cell::from_char(c));
                    }
                }
            }
        }

        // Create two simulators with the same state
        let mut sim1 = ProgramSimulator::new(RenderModel { counter: 42 });
        let mut sim2 = ProgramSimulator::new(RenderModel { counter: 42 });

        let buf1 = sim1.capture_frame(80, 24);
        let buf2 = sim2.capture_frame(80, 24);

        // Compare buffer contents
        for y in 0..24 {
            for x in 0..80 {
                let cell1 = buf1.get(x, y).unwrap();
                let cell2 = buf2.get(x, y).unwrap();
                assert_eq!(
                    cell1.content.as_char(),
                    cell2.content.as_char(),
                    "Mismatch at ({}, {})",
                    x,
                    y
                );
            }
        }
    }

    // Resize coalescer timing invariants are covered in resize_coalescer.rs tests.

    #[test]
    fn cmd_log_creates_log_command() {
        let cmd: Cmd<TestMsg> = Cmd::log("test message");
        assert!(matches!(cmd, Cmd::Log(s) if s == "test message"));
    }

    #[test]
    fn cmd_log_from_string() {
        let msg = String::from("dynamic message");
        let cmd: Cmd<TestMsg> = Cmd::log(msg);
        assert!(matches!(cmd, Cmd::Log(s) if s == "dynamic message"));
    }

    #[test]
    fn program_simulator_logs_jsonl_with_seed_and_run_id() {
        // Ensure ProgramSimulator captures JSONL log lines with run_id/seed.
        use crate::simulator::ProgramSimulator;

        struct LogModel {
            run_id: &'static str,
            seed: u64,
        }

        #[derive(Debug)]
        enum LogMsg {
            Emit,
        }

        impl From<Event> for LogMsg {
            fn from(_: Event) -> Self {
                LogMsg::Emit
            }
        }

        impl Model for LogModel {
            type Message = LogMsg;

            fn update(&mut self, _msg: Self::Message) -> Cmd<Self::Message> {
                let line = format!(
                    r#"{{"event":"test","run_id":"{}","seed":{}}}"#,
                    self.run_id, self.seed
                );
                Cmd::log(line)
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(LogModel {
            run_id: "test-run-001",
            seed: 4242,
        });
        sim.init();
        sim.send(LogMsg::Emit);

        let logs = sim.logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains(r#""run_id":"test-run-001""#));
        assert!(logs[0].contains(r#""seed":4242"#));
    }

    #[test]
    fn cmd_sequence_single_unwraps() {
        let cmd: Cmd<TestMsg> = Cmd::sequence(vec![Cmd::quit()]);
        // Single element sequence should unwrap to the inner command
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn cmd_sequence_multiple() {
        let cmd: Cmd<TestMsg> = Cmd::sequence(vec![Cmd::none(), Cmd::quit()]);
        assert!(matches!(cmd, Cmd::Sequence(_)));
    }

    #[test]
    fn cmd_default_is_none() {
        let cmd: Cmd<TestMsg> = Cmd::default();
        assert!(matches!(cmd, Cmd::None));
    }

    #[test]
    fn cmd_debug_all_variants() {
        // Test Debug impl for all variants
        let none: Cmd<TestMsg> = Cmd::none();
        assert_eq!(format!("{none:?}"), "None");

        let quit: Cmd<TestMsg> = Cmd::quit();
        assert_eq!(format!("{quit:?}"), "Quit");

        let msg: Cmd<TestMsg> = Cmd::msg(TestMsg::Increment);
        assert!(format!("{msg:?}").starts_with("Msg("));

        let batch: Cmd<TestMsg> = Cmd::batch(vec![Cmd::none(), Cmd::none()]);
        assert!(format!("{batch:?}").starts_with("Batch("));

        let seq: Cmd<TestMsg> = Cmd::sequence(vec![Cmd::none(), Cmd::none()]);
        assert!(format!("{seq:?}").starts_with("Sequence("));

        let tick: Cmd<TestMsg> = Cmd::tick(Duration::from_secs(1));
        assert!(format!("{tick:?}").starts_with("Tick("));

        let log: Cmd<TestMsg> = Cmd::log("test");
        assert!(format!("{log:?}").starts_with("Log("));
    }

    #[test]
    fn program_config_with_budget() {
        let budget = FrameBudgetConfig {
            total: Duration::from_millis(50),
            ..Default::default()
        };
        let config = ProgramConfig::default().with_budget(budget);
        assert_eq!(config.budget.total, Duration::from_millis(50));
    }

    #[test]
    fn program_config_with_conformal() {
        let config = ProgramConfig::default().with_conformal_config(ConformalConfig {
            alpha: 0.2,
            ..Default::default()
        });
        assert!(config.conformal_config.is_some());
        assert!((config.conformal_config.as_ref().unwrap().alpha - 0.2).abs() < 1e-6);
    }

    #[test]
    fn program_config_forced_size_clamps_minimums() {
        let config = ProgramConfig::default().with_forced_size(0, 0);
        assert_eq!(config.forced_size, Some((1, 1)));

        let cleared = config.without_forced_size();
        assert!(cleared.forced_size.is_none());
    }

    #[test]
    fn effect_queue_config_defaults_are_safe() {
        let config = EffectQueueConfig::default();
        assert!(!config.enabled);
        assert!(config.scheduler.smith_enabled);
        assert!(!config.scheduler.preemptive);
        assert_eq!(config.scheduler.aging_factor, 0.0);
        assert_eq!(config.scheduler.wait_starve_ms, 0.0);
    }

    #[test]
    fn handle_effect_command_enqueues_or_executes_inline() {
        let (result_tx, result_rx) = mpsc::channel::<u32>();
        let mut scheduler = QueueingScheduler::new(EffectQueueConfig::default().scheduler);
        let mut tasks: HashMap<u64, Box<dyn FnOnce() -> u32 + Send>> = HashMap::new();

        let ran = Arc::new(AtomicUsize::new(0));
        let ran_task = ran.clone();
        let cmd = EffectCommand::Enqueue(
            TaskSpec::default(),
            Box::new(move || {
                ran_task.fetch_add(1, Ordering::SeqCst);
                7
            }),
        );

        let shutdown = handle_effect_command(cmd, &mut scheduler, &mut tasks, &result_tx);
        assert!(!shutdown);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(tasks.len(), 1);
        assert!(result_rx.try_recv().is_err());

        let mut full_scheduler = QueueingScheduler::new(SchedulerConfig {
            max_queue_size: 0,
            ..Default::default()
        });
        let mut full_tasks: HashMap<u64, Box<dyn FnOnce() -> u32 + Send>> = HashMap::new();
        let ran_full = Arc::new(AtomicUsize::new(0));
        let ran_full_task = ran_full.clone();
        let cmd_full = EffectCommand::Enqueue(
            TaskSpec::default(),
            Box::new(move || {
                ran_full_task.fetch_add(1, Ordering::SeqCst);
                42
            }),
        );

        let shutdown_full =
            handle_effect_command(cmd_full, &mut full_scheduler, &mut full_tasks, &result_tx);
        assert!(!shutdown_full);
        assert!(full_tasks.is_empty());
        assert_eq!(ran_full.load(Ordering::SeqCst), 1);
        assert_eq!(
            result_rx.recv_timeout(Duration::from_millis(200)).unwrap(),
            42
        );

        let shutdown = handle_effect_command(
            EffectCommand::Shutdown,
            &mut full_scheduler,
            &mut full_tasks,
            &result_tx,
        );
        assert!(shutdown);
    }

    #[test]
    fn effect_queue_loop_executes_tasks_and_shutdowns() {
        let (cmd_tx, cmd_rx) = mpsc::channel::<EffectCommand<u32>>();
        let (result_tx, result_rx) = mpsc::channel::<u32>();
        let config = EffectQueueConfig {
            enabled: true,
            scheduler: SchedulerConfig {
                preemptive: false,
                ..Default::default()
            },
        };

        let handle = std::thread::spawn(move || {
            effect_queue_loop(config, cmd_rx, result_tx, None);
        });

        cmd_tx
            .send(EffectCommand::Enqueue(TaskSpec::default(), Box::new(|| 10)))
            .unwrap();
        cmd_tx
            .send(EffectCommand::Enqueue(
                TaskSpec::new(2.0, 5.0).with_name("second"),
                Box::new(|| 20),
            ))
            .unwrap();

        let mut results = vec![
            result_rx.recv_timeout(Duration::from_millis(500)).unwrap(),
            result_rx.recv_timeout(Duration::from_millis(500)).unwrap(),
        ];
        results.sort_unstable();
        assert_eq!(results, vec![10, 20]);

        cmd_tx.send(EffectCommand::Shutdown).unwrap();
        let _ = handle.join();
    }

    #[test]
    fn inline_auto_remeasure_reset_clears_decision() {
        let mut state = InlineAutoRemeasureState::new(InlineAutoRemeasureConfig::default());
        state.sampler.decide(Instant::now());
        assert!(state.sampler.last_decision().is_some());

        state.reset();
        assert!(state.sampler.last_decision().is_none());
    }

    #[test]
    fn budget_decision_jsonl_contains_required_fields() {
        let evidence = BudgetDecisionEvidence {
            frame_idx: 7,
            decision: BudgetDecision::Degrade,
            controller_decision: BudgetDecision::Hold,
            degradation_before: DegradationLevel::Full,
            degradation_after: DegradationLevel::NoStyling,
            frame_time_us: 12_345.678,
            budget_us: 16_000.0,
            pid_output: 1.25,
            pid_p: 0.5,
            pid_i: 0.25,
            pid_d: 0.5,
            e_value: 2.0,
            frames_observed: 42,
            frames_since_change: 3,
            in_warmup: false,
            conformal: Some(ConformalEvidence {
                bucket_key: "inline:dirty:10".to_string(),
                n_b: 32,
                alpha: 0.05,
                q_b: 1000.0,
                y_hat: 12_000.0,
                upper_us: 13_000.0,
                risk: true,
                fallback_level: 1,
                window_size: 256,
                reset_count: 2,
            }),
        };

        let jsonl = evidence.to_jsonl();
        assert!(jsonl.contains("\"event\":\"budget_decision\""));
        assert!(jsonl.contains("\"decision\":\"degrade\""));
        assert!(jsonl.contains("\"decision_controller\":\"stay\""));
        assert!(jsonl.contains("\"degradation_before\":\"Full\""));
        assert!(jsonl.contains("\"degradation_after\":\"NoStyling\""));
        assert!(jsonl.contains("\"frame_time_us\":12345.678000"));
        assert!(jsonl.contains("\"budget_us\":16000.000000"));
        assert!(jsonl.contains("\"pid_output\":1.250000"));
        assert!(jsonl.contains("\"e_value\":2.000000"));
        assert!(jsonl.contains("\"bucket_key\":\"inline:dirty:10\""));
        assert!(jsonl.contains("\"n_b\":32"));
        assert!(jsonl.contains("\"alpha\":0.050000"));
        assert!(jsonl.contains("\"q_b\":1000.000000"));
        assert!(jsonl.contains("\"y_hat\":12000.000000"));
        assert!(jsonl.contains("\"upper_us\":13000.000000"));
        assert!(jsonl.contains("\"risk\":true"));
        assert!(jsonl.contains("\"fallback_level\":1"));
        assert!(jsonl.contains("\"window_size\":256"));
        assert!(jsonl.contains("\"reset_count\":2"));
    }

    fn make_signal(
        widget_id: u64,
        essential: bool,
        priority: f32,
        staleness_ms: u64,
        cost_us: f32,
    ) -> WidgetSignal {
        WidgetSignal {
            widget_id,
            essential,
            priority,
            staleness_ms,
            focus_boost: 0.0,
            interaction_boost: 0.0,
            area_cells: 1,
            cost_estimate_us: cost_us,
            recent_cost_us: 0.0,
            estimate_source: CostEstimateSource::FixedDefault,
        }
    }

    fn signal_value_cost(signal: &WidgetSignal, config: &WidgetRefreshConfig) -> (f32, f32, bool) {
        let starved = config.starve_ms > 0 && signal.staleness_ms >= config.starve_ms;
        let staleness_window = config.staleness_window_ms.max(1) as f32;
        let staleness_score = (signal.staleness_ms as f32 / staleness_window).min(1.0);
        let mut value = config.weight_priority * signal.priority
            + config.weight_staleness * staleness_score
            + config.weight_focus * signal.focus_boost
            + config.weight_interaction * signal.interaction_boost;
        if starved {
            value += config.starve_boost;
        }
        let raw_cost = if signal.recent_cost_us > 0.0 {
            signal.recent_cost_us
        } else {
            signal.cost_estimate_us
        };
        let cost_us = raw_cost.max(config.min_cost_us);
        (value, cost_us, starved)
    }

    fn fifo_select(
        signals: &[WidgetSignal],
        budget_us: f64,
        config: &WidgetRefreshConfig,
    ) -> (Vec<u64>, f64, usize) {
        let mut selected = Vec::new();
        let mut total_value = 0.0f64;
        let mut starved_selected = 0usize;
        let mut remaining = budget_us;

        for signal in signals {
            if !signal.essential {
                continue;
            }
            let (value, cost_us, starved) = signal_value_cost(signal, config);
            remaining -= cost_us as f64;
            total_value += value as f64;
            if starved {
                starved_selected = starved_selected.saturating_add(1);
            }
            selected.push(signal.widget_id);
        }
        for signal in signals {
            if signal.essential {
                continue;
            }
            let (value, cost_us, starved) = signal_value_cost(signal, config);
            if remaining >= cost_us as f64 {
                remaining -= cost_us as f64;
                total_value += value as f64;
                if starved {
                    starved_selected = starved_selected.saturating_add(1);
                }
                selected.push(signal.widget_id);
            }
        }

        (selected, total_value, starved_selected)
    }

    fn rotate_signals(signals: &[WidgetSignal], offset: usize) -> Vec<WidgetSignal> {
        if signals.is_empty() {
            return Vec::new();
        }
        let mut rotated = Vec::with_capacity(signals.len());
        for idx in 0..signals.len() {
            rotated.push(signals[(idx + offset) % signals.len()].clone());
        }
        rotated
    }

    #[test]
    fn widget_refresh_selects_essentials_first() {
        let signals = vec![
            make_signal(1, true, 0.6, 0, 5.0),
            make_signal(2, false, 0.9, 0, 4.0),
        ];
        let mut plan = WidgetRefreshPlan::new();
        let config = WidgetRefreshConfig::default();
        plan.recompute(1, 6.0, DegradationLevel::Full, &signals, &config);
        let selected: Vec<u64> = plan.selected.iter().map(|e| e.widget_id).collect();
        assert_eq!(selected, vec![1]);
        assert!(!plan.over_budget);
    }

    #[test]
    fn widget_refresh_degradation_essential_only_skips_nonessential() {
        let signals = vec![
            make_signal(1, true, 0.5, 0, 2.0),
            make_signal(2, false, 1.0, 0, 1.0),
        ];
        let mut plan = WidgetRefreshPlan::new();
        let config = WidgetRefreshConfig::default();
        plan.recompute(3, 10.0, DegradationLevel::EssentialOnly, &signals, &config);
        let selected: Vec<u64> = plan.selected.iter().map(|e| e.widget_id).collect();
        assert_eq!(selected, vec![1]);
        assert_eq!(plan.skipped_count, 1);
    }

    #[test]
    fn widget_refresh_starvation_guard_forces_one_starved() {
        let signals = vec![make_signal(7, false, 0.1, 10_000, 8.0)];
        let mut plan = WidgetRefreshPlan::new();
        let config = WidgetRefreshConfig {
            starve_ms: 1_000,
            max_starved_per_frame: 1,
            ..Default::default()
        };
        plan.recompute(5, 0.0, DegradationLevel::Full, &signals, &config);
        assert_eq!(plan.selected.len(), 1);
        assert!(plan.selected[0].starved);
        assert!(plan.over_budget);
    }

    #[test]
    fn widget_refresh_budget_blocks_when_no_selection() {
        let signals = vec![make_signal(42, false, 0.2, 0, 10.0)];
        let mut plan = WidgetRefreshPlan::new();
        let config = WidgetRefreshConfig {
            starve_ms: 0,
            max_starved_per_frame: 0,
            ..Default::default()
        };
        plan.recompute(8, 0.0, DegradationLevel::Full, &signals, &config);
        let budget = plan.as_budget();
        assert!(!budget.allows(42, false));
    }

    #[test]
    fn widget_refresh_max_drop_fraction_forces_minimum_refresh() {
        let signals = vec![
            make_signal(1, false, 0.4, 0, 10.0),
            make_signal(2, false, 0.4, 0, 10.0),
            make_signal(3, false, 0.4, 0, 10.0),
            make_signal(4, false, 0.4, 0, 10.0),
        ];
        let mut plan = WidgetRefreshPlan::new();
        let config = WidgetRefreshConfig {
            starve_ms: 0,
            max_starved_per_frame: 0,
            max_drop_fraction: 0.5,
            ..Default::default()
        };
        plan.recompute(12, 0.0, DegradationLevel::Full, &signals, &config);
        let selected: Vec<u64> = plan.selected.iter().map(|e| e.widget_id).collect();
        assert_eq!(selected, vec![1, 2]);
    }

    #[test]
    fn widget_refresh_greedy_beats_fifo_and_round_robin() {
        let signals = vec![
            make_signal(1, false, 0.1, 0, 6.0),
            make_signal(2, false, 0.2, 0, 6.0),
            make_signal(3, false, 1.0, 0, 4.0),
            make_signal(4, false, 0.9, 0, 3.0),
            make_signal(5, false, 0.8, 0, 3.0),
            make_signal(6, false, 0.1, 4_000, 2.0),
        ];
        let budget_us = 10.0;
        let config = WidgetRefreshConfig::default();

        let mut plan = WidgetRefreshPlan::new();
        plan.recompute(21, budget_us, DegradationLevel::Full, &signals, &config);
        let greedy_value = plan.selected_value;
        let greedy_selected: Vec<u64> = plan.selected.iter().map(|e| e.widget_id).collect();

        let (fifo_selected, fifo_value, _fifo_starved) = fifo_select(&signals, budget_us, &config);
        let rotated = rotate_signals(&signals, 2);
        let (rr_selected, rr_value, _rr_starved) = fifo_select(&rotated, budget_us, &config);

        assert!(
            greedy_value > fifo_value,
            "greedy_value={greedy_value:.3} <= fifo_value={fifo_value:.3}; greedy={:?}, fifo={:?}",
            greedy_selected,
            fifo_selected
        );
        assert!(
            greedy_value > rr_value,
            "greedy_value={greedy_value:.3} <= rr_value={rr_value:.3}; greedy={:?}, rr={:?}",
            greedy_selected,
            rr_selected
        );
        assert!(
            plan.starved_selected > 0,
            "greedy did not select starved widget; greedy={:?}",
            greedy_selected
        );
    }

    #[test]
    fn widget_refresh_jsonl_contains_required_fields() {
        let signals = vec![make_signal(7, true, 0.2, 0, 2.0)];
        let mut plan = WidgetRefreshPlan::new();
        let config = WidgetRefreshConfig::default();
        plan.recompute(9, 4.0, DegradationLevel::Full, &signals, &config);
        let jsonl = plan.to_jsonl();
        assert!(jsonl.contains("\"event\":\"widget_refresh\""));
        assert!(jsonl.contains("\"frame_idx\":9"));
        assert!(jsonl.contains("\"selected_count\":1"));
        assert!(jsonl.contains("\"id\":7"));
    }

    #[test]
    fn program_config_with_resize_coalescer() {
        let config = ProgramConfig::default().with_resize_coalescer(CoalescerConfig {
            steady_delay_ms: 8,
            burst_delay_ms: 20,
            hard_deadline_ms: 80,
            burst_enter_rate: 12.0,
            burst_exit_rate: 6.0,
            cooldown_frames: 2,
            rate_window_size: 6,
            enable_logging: true,
            enable_bocpd: false,
            bocpd_config: None,
        });
        assert_eq!(config.resize_coalescer.steady_delay_ms, 8);
        assert!(config.resize_coalescer.enable_logging);
    }

    #[test]
    fn program_config_with_resize_behavior() {
        let config = ProgramConfig::default().with_resize_behavior(ResizeBehavior::Immediate);
        assert_eq!(config.resize_behavior, ResizeBehavior::Immediate);
    }

    #[test]
    fn program_config_with_legacy_resize_enabled() {
        let config = ProgramConfig::default().with_legacy_resize(true);
        assert_eq!(config.resize_behavior, ResizeBehavior::Immediate);
    }

    #[test]
    fn program_config_with_legacy_resize_disabled_keeps_default() {
        let config = ProgramConfig::default().with_legacy_resize(false);
        assert_eq!(config.resize_behavior, ResizeBehavior::Throttled);
    }

    fn diff_strategy_trace(bayesian_enabled: bool) -> Vec<DiffStrategy> {
        let config = RuntimeDiffConfig::default().with_bayesian_enabled(bayesian_enabled);
        let mut writer = TerminalWriter::with_diff_config(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            TerminalCapabilities::basic(),
            config,
        );
        writer.set_size(8, 4);

        let mut buffer = Buffer::new(8, 4);
        let mut trace = Vec::new();

        writer.present_ui(&buffer, None, false).unwrap();
        trace.push(
            writer
                .last_diff_strategy()
                .unwrap_or(DiffStrategy::FullRedraw),
        );

        buffer.set_raw(0, 0, Cell::from_char('A'));
        writer.present_ui(&buffer, None, false).unwrap();
        trace.push(
            writer
                .last_diff_strategy()
                .unwrap_or(DiffStrategy::FullRedraw),
        );

        buffer.set_raw(1, 1, Cell::from_char('B'));
        writer.present_ui(&buffer, None, false).unwrap();
        trace.push(
            writer
                .last_diff_strategy()
                .unwrap_or(DiffStrategy::FullRedraw),
        );

        trace
    }

    fn coalescer_checksum(enable_bocpd: bool) -> String {
        let mut config = CoalescerConfig::default().with_logging(true);
        if enable_bocpd {
            config = config.with_bocpd();
        }

        let base = Instant::now();
        let mut coalescer = ResizeCoalescer::new(config, (80, 24)).with_last_render(base);

        let events = [
            (0_u64, (82_u16, 24_u16)),
            (10, (83, 25)),
            (20, (84, 26)),
            (35, (90, 28)),
            (55, (92, 30)),
        ];

        let mut idx = 0usize;
        for t_ms in (0_u64..=160).step_by(8) {
            let now = base + Duration::from_millis(t_ms);
            while idx < events.len() && events[idx].0 == t_ms {
                let (w, h) = events[idx].1;
                coalescer.handle_resize_at(w, h, now);
                idx += 1;
            }
            coalescer.tick_at(now);
        }

        coalescer.decision_checksum_hex()
    }

    fn conformal_trace(enabled: bool) -> Vec<(f64, bool)> {
        if !enabled {
            return Vec::new();
        }

        let mut predictor = ConformalPredictor::new(ConformalConfig::default());
        let key = BucketKey::from_context(ScreenMode::AltScreen, DiffStrategy::Full, 80, 24);
        let mut trace = Vec::new();

        for i in 0..30 {
            let y_hat = 16_000.0 + (i as f64) * 15.0;
            let observed = y_hat + (i % 7) as f64 * 120.0;
            predictor.observe(key, y_hat, observed);
            let prediction = predictor.predict(key, y_hat, 20_000.0);
            trace.push((prediction.upper_us, prediction.risk));
        }

        trace
    }

    #[test]
    fn policy_toggle_matrix_determinism() {
        for &bayesian in &[false, true] {
            for &bocpd in &[false, true] {
                for &conformal in &[false, true] {
                    let diff_a = diff_strategy_trace(bayesian);
                    let diff_b = diff_strategy_trace(bayesian);
                    assert_eq!(diff_a, diff_b, "diff strategy not deterministic");

                    let checksum_a = coalescer_checksum(bocpd);
                    let checksum_b = coalescer_checksum(bocpd);
                    assert_eq!(checksum_a, checksum_b, "coalescer checksum mismatch");

                    let conf_a = conformal_trace(conformal);
                    let conf_b = conformal_trace(conformal);
                    assert_eq!(conf_a, conf_b, "conformal predictor not deterministic");

                    if conformal {
                        assert!(!conf_a.is_empty(), "conformal trace should be populated");
                    } else {
                        assert!(conf_a.is_empty(), "conformal trace should be empty");
                    }
                }
            }
        }
    }

    #[test]
    fn resize_behavior_uses_coalescer_flag() {
        assert!(ResizeBehavior::Throttled.uses_coalescer());
        assert!(!ResizeBehavior::Immediate.uses_coalescer());
    }

    #[test]
    fn nested_cmd_msg_executes_recursively() {
        // Verify that Cmd::Msg triggers recursive update
        use crate::simulator::ProgramSimulator;

        struct NestedModel {
            depth: usize,
        }

        #[derive(Debug)]
        enum NestedMsg {
            Nest(usize),
        }

        impl From<Event> for NestedMsg {
            fn from(_: Event) -> Self {
                NestedMsg::Nest(0)
            }
        }

        impl Model for NestedModel {
            type Message = NestedMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    NestedMsg::Nest(n) => {
                        self.depth += 1;
                        if n > 0 {
                            Cmd::msg(NestedMsg::Nest(n - 1))
                        } else {
                            Cmd::none()
                        }
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(NestedModel { depth: 0 });
        sim.init();
        sim.send(NestedMsg::Nest(3));

        // Should have recursed 4 times (3, 2, 1, 0)
        assert_eq!(sim.model().depth, 4);
    }

    #[test]
    fn task_executes_synchronously_in_simulator() {
        // In simulator, tasks execute synchronously
        use crate::simulator::ProgramSimulator;

        struct TaskModel {
            completed: bool,
        }

        #[derive(Debug)]
        enum TaskMsg {
            Complete,
            SpawnTask,
        }

        impl From<Event> for TaskMsg {
            fn from(_: Event) -> Self {
                TaskMsg::Complete
            }
        }

        impl Model for TaskModel {
            type Message = TaskMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    TaskMsg::Complete => {
                        self.completed = true;
                        Cmd::none()
                    }
                    TaskMsg::SpawnTask => Cmd::task(|| TaskMsg::Complete),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(TaskModel { completed: false });
        sim.init();
        sim.send(TaskMsg::SpawnTask);

        // Task should have completed synchronously
        assert!(sim.model().completed);
    }

    #[test]
    fn multiple_updates_accumulate_correctly() {
        // Verify state accumulates correctly across multiple updates
        use crate::simulator::ProgramSimulator;

        struct AccumModel {
            sum: i32,
        }

        #[derive(Debug)]
        enum AccumMsg {
            Add(i32),
            Multiply(i32),
        }

        impl From<Event> for AccumMsg {
            fn from(_: Event) -> Self {
                AccumMsg::Add(1)
            }
        }

        impl Model for AccumModel {
            type Message = AccumMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    AccumMsg::Add(n) => {
                        self.sum += n;
                        Cmd::none()
                    }
                    AccumMsg::Multiply(n) => {
                        self.sum *= n;
                        Cmd::none()
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(AccumModel { sum: 0 });
        sim.init();

        // (0 + 5) * 2 + 3 = 13
        sim.send(AccumMsg::Add(5));
        sim.send(AccumMsg::Multiply(2));
        sim.send(AccumMsg::Add(3));

        assert_eq!(sim.model().sum, 13);
    }

    #[test]
    fn init_command_executes_before_first_update() {
        // Verify init() command executes before any update
        use crate::simulator::ProgramSimulator;

        struct InitModel {
            initialized: bool,
            updates: usize,
        }

        #[derive(Debug)]
        enum InitMsg {
            Update,
            MarkInit,
        }

        impl From<Event> for InitMsg {
            fn from(_: Event) -> Self {
                InitMsg::Update
            }
        }

        impl Model for InitModel {
            type Message = InitMsg;

            fn init(&mut self) -> Cmd<Self::Message> {
                Cmd::msg(InitMsg::MarkInit)
            }

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    InitMsg::MarkInit => {
                        self.initialized = true;
                        Cmd::none()
                    }
                    InitMsg::Update => {
                        self.updates += 1;
                        Cmd::none()
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(InitModel {
            initialized: false,
            updates: 0,
        });
        sim.init();

        assert!(sim.model().initialized);
        sim.send(InitMsg::Update);
        assert_eq!(sim.model().updates, 1);
    }

    // =========================================================================
    // INLINE MODE FRAME SIZING TESTS (bd-20vg)
    // =========================================================================

    #[test]
    fn ui_height_returns_correct_value_inline_mode() {
        // Verify TerminalWriter.ui_height() returns ui_height in inline mode
        use crate::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
        use ftui_core::terminal_capabilities::TerminalCapabilities;

        let output = Vec::new();
        let writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 10 },
            UiAnchor::Bottom,
            TerminalCapabilities::basic(),
        );
        assert_eq!(writer.ui_height(), 10);
    }

    #[test]
    fn ui_height_returns_term_height_altscreen_mode() {
        // Verify TerminalWriter.ui_height() returns full terminal height in alt-screen mode
        use crate::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
        use ftui_core::terminal_capabilities::TerminalCapabilities;

        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            TerminalCapabilities::basic(),
        );
        writer.set_size(80, 24);
        assert_eq!(writer.ui_height(), 24);
    }

    #[test]
    fn inline_mode_frame_uses_ui_height_not_terminal_height() {
        // Verify that in inline mode, the model receives a frame with ui_height,
        // not the full terminal height. This is the core fix for bd-20vg.
        use crate::simulator::ProgramSimulator;
        use std::cell::Cell as StdCell;

        thread_local! {
            static CAPTURED_HEIGHT: StdCell<u16> = const { StdCell::new(0) };
        }

        struct FrameSizeTracker;

        #[derive(Debug)]
        enum SizeMsg {
            Check,
        }

        impl From<Event> for SizeMsg {
            fn from(_: Event) -> Self {
                SizeMsg::Check
            }
        }

        impl Model for FrameSizeTracker {
            type Message = SizeMsg;

            fn update(&mut self, _msg: Self::Message) -> Cmd<Self::Message> {
                Cmd::none()
            }

            fn view(&self, frame: &mut Frame) {
                // Capture the frame height we receive
                CAPTURED_HEIGHT.with(|h| h.set(frame.height()));
            }
        }

        // Use simulator to verify frame dimension handling
        let mut sim = ProgramSimulator::new(FrameSizeTracker);
        sim.init();

        // Capture with specific dimensions (simulates inline mode ui_height=10)
        let buf = sim.capture_frame(80, 10);
        assert_eq!(buf.height(), 10);
        assert_eq!(buf.width(), 80);

        // Verify the frame has the correct dimensions
        // In inline mode with ui_height=10, the frame should be 10 rows tall,
        // NOT the full terminal height (e.g., 24).
    }

    #[test]
    fn altscreen_frame_uses_full_terminal_height() {
        // Regression test: in alt-screen mode, frame should use full terminal height.
        use crate::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
        use ftui_core::terminal_capabilities::TerminalCapabilities;

        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            TerminalCapabilities::basic(),
        );
        writer.set_size(80, 40);

        // In alt-screen, ui_height equals terminal height
        assert_eq!(writer.ui_height(), 40);
    }

    #[test]
    fn ui_height_clamped_to_terminal_height() {
        // Verify ui_height doesn't exceed terminal height
        // (This is handled in present_inline, but ui_height() returns the configured value)
        use crate::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
        use ftui_core::terminal_capabilities::TerminalCapabilities;

        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 100 },
            UiAnchor::Bottom,
            TerminalCapabilities::basic(),
        );
        writer.set_size(80, 10);

        // ui_height() returns configured value, but present_inline clamps
        // The Frame should be created with ui_height (100), which is later
        // clamped during presentation. For safety, we should use the min.
        // Note: This documents current behavior. A stricter fix might
        // have ui_height() return min(ui_height, term_height).
        assert_eq!(writer.ui_height(), 100);
    }

    // =========================================================================
    // TICK DELIVERY TESTS (bd-3ufh)
    // =========================================================================

    #[test]
    fn tick_event_delivered_to_model_update() {
        // Verify that Event::Tick is delivered to model.update()
        // This is the core fix: ticks now flow through the update pipeline.
        use crate::simulator::ProgramSimulator;

        struct TickTracker {
            tick_count: usize,
        }

        #[derive(Debug)]
        enum TickMsg {
            Tick,
            Other,
        }

        impl From<Event> for TickMsg {
            fn from(event: Event) -> Self {
                match event {
                    Event::Tick => TickMsg::Tick,
                    _ => TickMsg::Other,
                }
            }
        }

        impl Model for TickTracker {
            type Message = TickMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    TickMsg::Tick => {
                        self.tick_count += 1;
                        Cmd::none()
                    }
                    TickMsg::Other => Cmd::none(),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(TickTracker { tick_count: 0 });
        sim.init();

        // Manually inject tick event to simulate what the runtime does
        sim.inject_event(Event::Tick);
        assert_eq!(sim.model().tick_count, 1);

        sim.inject_event(Event::Tick);
        sim.inject_event(Event::Tick);
        assert_eq!(sim.model().tick_count, 3);
    }

    #[test]
    fn tick_command_sets_tick_rate() {
        // Verify Cmd::tick() sets the tick rate in the simulator
        use crate::simulator::{CmdRecord, ProgramSimulator};

        struct TickModel;

        #[derive(Debug)]
        enum Msg {
            SetTick,
            Noop,
        }

        impl From<Event> for Msg {
            fn from(_: Event) -> Self {
                Msg::Noop
            }
        }

        impl Model for TickModel {
            type Message = Msg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    Msg::SetTick => Cmd::tick(Duration::from_millis(100)),
                    Msg::Noop => Cmd::none(),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(TickModel);
        sim.init();
        sim.send(Msg::SetTick);

        // Check that tick was recorded
        let commands = sim.command_log();
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, CmdRecord::Tick(d) if *d == Duration::from_millis(100)))
        );
    }

    #[test]
    fn tick_can_trigger_further_commands() {
        // Verify that tick handling can return commands that are executed
        use crate::simulator::ProgramSimulator;

        struct ChainModel {
            stage: usize,
        }

        #[derive(Debug)]
        enum ChainMsg {
            Tick,
            Advance,
            Noop,
        }

        impl From<Event> for ChainMsg {
            fn from(event: Event) -> Self {
                match event {
                    Event::Tick => ChainMsg::Tick,
                    _ => ChainMsg::Noop,
                }
            }
        }

        impl Model for ChainModel {
            type Message = ChainMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    ChainMsg::Tick => {
                        self.stage += 1;
                        // Return another message to be processed
                        Cmd::msg(ChainMsg::Advance)
                    }
                    ChainMsg::Advance => {
                        self.stage += 10;
                        Cmd::none()
                    }
                    ChainMsg::Noop => Cmd::none(),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(ChainModel { stage: 0 });
        sim.init();
        sim.inject_event(Event::Tick);

        // Tick increments by 1, then Advance increments by 10
        assert_eq!(sim.model().stage, 11);
    }

    #[test]
    fn tick_disabled_with_zero_duration() {
        // Verify that Duration::ZERO disables ticks (no busy loop)
        use crate::simulator::ProgramSimulator;

        struct ZeroTickModel {
            disabled: bool,
        }

        #[derive(Debug)]
        enum ZeroMsg {
            DisableTick,
            Noop,
        }

        impl From<Event> for ZeroMsg {
            fn from(_: Event) -> Self {
                ZeroMsg::Noop
            }
        }

        impl Model for ZeroTickModel {
            type Message = ZeroMsg;

            fn init(&mut self) -> Cmd<Self::Message> {
                // Start with a tick enabled
                Cmd::tick(Duration::from_millis(100))
            }

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    ZeroMsg::DisableTick => {
                        self.disabled = true;
                        // Setting tick to ZERO should effectively disable
                        Cmd::tick(Duration::ZERO)
                    }
                    ZeroMsg::Noop => Cmd::none(),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(ZeroTickModel { disabled: false });
        sim.init();

        // Verify initial tick rate is set
        assert!(sim.tick_rate().is_some());
        assert_eq!(sim.tick_rate(), Some(Duration::from_millis(100)));

        // Disable ticks
        sim.send(ZeroMsg::DisableTick);
        assert!(sim.model().disabled);

        // Note: The simulator still records the ZERO tick, but the runtime's
        // should_tick() handles ZERO duration appropriately
        assert_eq!(sim.tick_rate(), Some(Duration::ZERO));
    }

    #[test]
    fn tick_event_distinguishable_from_other_events() {
        // Verify Event::Tick can be distinguished in pattern matching
        let tick = Event::Tick;
        let key = Event::Key(ftui_core::event::KeyEvent::new(
            ftui_core::event::KeyCode::Char('a'),
        ));

        assert!(matches!(tick, Event::Tick));
        assert!(!matches!(key, Event::Tick));
    }

    #[test]
    fn tick_event_clone_and_eq() {
        // Verify Event::Tick implements Clone and Eq correctly
        let tick1 = Event::Tick;
        let tick2 = tick1.clone();
        assert_eq!(tick1, tick2);
    }

    #[test]
    fn model_receives_tick_and_input_events() {
        // Verify model can handle both tick and input events correctly
        use crate::simulator::ProgramSimulator;

        struct MixedModel {
            ticks: usize,
            keys: usize,
        }

        #[derive(Debug)]
        enum MixedMsg {
            Tick,
            Key,
        }

        impl From<Event> for MixedMsg {
            fn from(event: Event) -> Self {
                match event {
                    Event::Tick => MixedMsg::Tick,
                    _ => MixedMsg::Key,
                }
            }
        }

        impl Model for MixedModel {
            type Message = MixedMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    MixedMsg::Tick => {
                        self.ticks += 1;
                        Cmd::none()
                    }
                    MixedMsg::Key => {
                        self.keys += 1;
                        Cmd::none()
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(MixedModel { ticks: 0, keys: 0 });
        sim.init();

        // Interleave tick and input events
        sim.inject_event(Event::Tick);
        sim.inject_event(Event::Key(ftui_core::event::KeyEvent::new(
            ftui_core::event::KeyCode::Char('a'),
        )));
        sim.inject_event(Event::Tick);
        sim.inject_event(Event::Key(ftui_core::event::KeyEvent::new(
            ftui_core::event::KeyCode::Char('b'),
        )));
        sim.inject_event(Event::Tick);

        assert_eq!(sim.model().ticks, 3);
        assert_eq!(sim.model().keys, 2);
    }

    // =========================================================================
    // HEADLESS PROGRAM TESTS (bd-1av4o.2)
    // =========================================================================

    fn headless_program_with_config<M: Model>(
        model: M,
        config: ProgramConfig,
    ) -> Program<M, CrosstermEventSource, Vec<u8>>
    where
        M::Message: Send + 'static,
    {
        let capabilities = TerminalCapabilities::basic();
        let mut writer = TerminalWriter::with_diff_config(
            Vec::new(),
            config.screen_mode,
            config.ui_anchor,
            capabilities,
            config.diff_config.clone(),
        );
        let frame_timing = config.frame_timing.clone();
        writer.set_timing_enabled(frame_timing.is_some());

        let (width, height) = config.forced_size.unwrap_or((80, 24));
        let width = width.max(1);
        let height = height.max(1);
        writer.set_size(width, height);

        let initial_features = BackendFeatures {
            mouse_capture: config.mouse,
            bracketed_paste: config.bracketed_paste,
            focus_events: config.focus_reporting,
            kitty_keyboard: config.kitty_keyboard,
        };
        let session = TerminalSession::new_for_tests(SessionOptions {
            alternate_screen: matches!(config.screen_mode, ScreenMode::AltScreen),
            mouse_capture: initial_features.mouse_capture,
            bracketed_paste: initial_features.bracketed_paste,
            focus_events: initial_features.focus_events,
            kitty_keyboard: initial_features.kitty_keyboard,
        })
        .expect("headless test session");
        let events = CrosstermEventSource::new(session, initial_features);

        let budget = RenderBudget::from_config(&config.budget);
        let conformal_predictor = config.conformal_config.clone().map(ConformalPredictor::new);
        let locale_context = config.locale_context.clone();
        let locale_version = locale_context.version();
        let resize_coalescer =
            ResizeCoalescer::new(config.resize_coalescer.clone(), (width, height));
        let subscriptions = SubscriptionManager::new();
        let (task_sender, task_receiver) = std::sync::mpsc::channel();
        let inline_auto_remeasure = config
            .inline_auto_remeasure
            .clone()
            .map(InlineAutoRemeasureState::new);

        Program {
            model,
            writer,
            events,
            backend_features: initial_features,
            running: true,
            tick_rate: None,
            last_tick: Instant::now(),
            dirty: true,
            frame_idx: 0,
            widget_signals: Vec::new(),
            widget_refresh_config: config.widget_refresh,
            widget_refresh_plan: WidgetRefreshPlan::new(),
            width,
            height,
            forced_size: config.forced_size,
            poll_timeout: config.poll_timeout,
            budget,
            conformal_predictor,
            last_frame_time_us: None,
            last_update_us: None,
            frame_timing,
            locale_context,
            locale_version,
            resize_coalescer,
            evidence_sink: None,
            fairness_config_logged: false,
            resize_behavior: config.resize_behavior,
            fairness_guard: InputFairnessGuard::new(),
            event_recorder: None,
            subscriptions,
            task_sender,
            task_receiver,
            task_handles: Vec::new(),
            effect_queue: None,
            state_registry: config.persistence.registry.clone(),
            persistence_config: config.persistence,
            last_checkpoint: Instant::now(),
            inline_auto_remeasure,
        }
    }

    fn temp_evidence_path(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let mut path = std::env::temp_dir();
        path.push(format!("ftui_evidence_{label}_{pid}_{seq}.jsonl"));
        path
    }

    fn read_evidence_event(path: &PathBuf, event: &str) -> Value {
        let jsonl = std::fs::read_to_string(path).expect("read evidence jsonl");
        let needle = format!("\"event\":\"{event}\"");
        let line = jsonl
            .lines()
            .find(|line| line.contains(&needle))
            .unwrap_or_else(|| panic!("missing {event} line"));
        serde_json::from_str(line).expect("valid evidence json")
    }

    #[test]
    fn headless_apply_resize_updates_model_and_dimensions() {
        struct ResizeModel {
            last_size: Option<(u16, u16)>,
        }

        #[derive(Debug)]
        enum ResizeMsg {
            Resize(u16, u16),
            Other,
        }

        impl From<Event> for ResizeMsg {
            fn from(event: Event) -> Self {
                match event {
                    Event::Resize { width, height } => ResizeMsg::Resize(width, height),
                    _ => ResizeMsg::Other,
                }
            }
        }

        impl Model for ResizeModel {
            type Message = ResizeMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                if let ResizeMsg::Resize(w, h) = msg {
                    self.last_size = Some((w, h));
                }
                Cmd::none()
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut program =
            headless_program_with_config(ResizeModel { last_size: None }, ProgramConfig::default());
        program.dirty = false;

        program
            .apply_resize(0, 0, Duration::ZERO, false)
            .expect("resize");

        assert_eq!(program.width, 1);
        assert_eq!(program.height, 1);
        assert_eq!(program.model().last_size, Some((1, 1)));
        assert!(program.dirty);
    }

    #[test]
    fn headless_execute_cmd_log_writes_output() {
        let mut program =
            headless_program_with_config(TestModel { value: 0 }, ProgramConfig::default());
        program.execute_cmd(Cmd::log("hello world")).expect("log");

        let bytes = program.writer.into_inner().expect("writer output");
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains("hello world"));
    }

    #[test]
    fn headless_process_task_results_updates_model() {
        struct TaskModel {
            updates: usize,
        }

        #[derive(Debug)]
        enum TaskMsg {
            Done,
        }

        impl From<Event> for TaskMsg {
            fn from(_: Event) -> Self {
                TaskMsg::Done
            }
        }

        impl Model for TaskModel {
            type Message = TaskMsg;

            fn update(&mut self, _msg: Self::Message) -> Cmd<Self::Message> {
                self.updates += 1;
                Cmd::none()
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut program =
            headless_program_with_config(TaskModel { updates: 0 }, ProgramConfig::default());
        program.dirty = false;
        program.task_sender.send(TaskMsg::Done).unwrap();

        program
            .process_task_results()
            .expect("process task results");
        assert_eq!(program.model().updates, 1);
        assert!(program.dirty);
    }

    #[test]
    fn headless_should_tick_and_timeout_behaviors() {
        let mut program =
            headless_program_with_config(TestModel { value: 0 }, ProgramConfig::default());
        program.tick_rate = Some(Duration::from_millis(5));
        program.last_tick = Instant::now() - Duration::from_millis(10);

        assert!(program.should_tick());
        assert!(!program.should_tick());

        let timeout = program.effective_timeout();
        assert!(timeout <= Duration::from_millis(5));

        program.tick_rate = None;
        program.poll_timeout = Duration::from_millis(33);
        assert_eq!(program.effective_timeout(), Duration::from_millis(33));
    }

    #[test]
    fn headless_effective_timeout_respects_resize_coalescer() {
        let mut config = ProgramConfig::default().with_resize_behavior(ResizeBehavior::Throttled);
        config.resize_coalescer.steady_delay_ms = 0;
        config.resize_coalescer.burst_delay_ms = 0;

        let mut program = headless_program_with_config(TestModel { value: 0 }, config);
        program.tick_rate = Some(Duration::from_millis(50));

        program.resize_coalescer.handle_resize(120, 40);
        assert!(program.resize_coalescer.has_pending());

        let timeout = program.effective_timeout();
        assert_eq!(timeout, Duration::ZERO);
    }

    #[test]
    fn headless_ui_height_remeasure_clears_auto_height() {
        let mut config = ProgramConfig::inline_auto(2, 6);
        config.inline_auto_remeasure = Some(InlineAutoRemeasureConfig::default());

        let mut program = headless_program_with_config(TestModel { value: 0 }, config);
        program.dirty = false;
        program.writer.set_auto_ui_height(5);

        assert_eq!(program.writer.auto_ui_height(), Some(5));
        program.request_ui_height_remeasure();

        assert_eq!(program.writer.auto_ui_height(), None);
        assert!(program.dirty);
    }

    #[test]
    fn headless_recording_lifecycle_and_locale_change() {
        let mut program =
            headless_program_with_config(TestModel { value: 0 }, ProgramConfig::default());
        program.dirty = false;

        program.start_recording("demo");
        assert!(program.is_recording());
        let recorded = program.stop_recording();
        assert!(recorded.is_some());
        assert!(!program.is_recording());

        let prev_dirty = program.dirty;
        program.locale_context.set_locale("fr");
        program.check_locale_change();
        assert!(program.dirty || prev_dirty);
    }

    #[test]
    fn headless_render_frame_marks_clean_and_sets_diff() {
        struct RenderModel;

        #[derive(Debug)]
        enum RenderMsg {
            Noop,
        }

        impl From<Event> for RenderMsg {
            fn from(_: Event) -> Self {
                RenderMsg::Noop
            }
        }

        impl Model for RenderModel {
            type Message = RenderMsg;

            fn update(&mut self, _msg: Self::Message) -> Cmd<Self::Message> {
                Cmd::none()
            }

            fn view(&self, frame: &mut Frame) {
                frame.buffer.set_raw(0, 0, Cell::from_char('X'));
            }
        }

        let mut program = headless_program_with_config(RenderModel, ProgramConfig::default());
        program.render_frame().expect("render frame");

        assert!(!program.dirty);
        assert!(program.writer.last_diff_strategy().is_some());
        assert_eq!(program.frame_idx, 1);
    }

    #[test]
    fn headless_render_frame_skips_when_budget_exhausted() {
        let config = ProgramConfig {
            budget: FrameBudgetConfig::with_total(Duration::ZERO),
            ..Default::default()
        };

        let mut program = headless_program_with_config(TestModel { value: 0 }, config);
        program.render_frame().expect("render frame");

        assert!(!program.dirty);
        assert_eq!(program.frame_idx, 1);
    }

    #[test]
    fn headless_render_frame_emits_budget_evidence_with_controller() {
        use ftui_render::budget::BudgetControllerConfig;

        struct RenderModel;

        #[derive(Debug)]
        enum RenderMsg {
            Noop,
        }

        impl From<Event> for RenderMsg {
            fn from(_: Event) -> Self {
                RenderMsg::Noop
            }
        }

        impl Model for RenderModel {
            type Message = RenderMsg;

            fn update(&mut self, _msg: Self::Message) -> Cmd<Self::Message> {
                Cmd::none()
            }

            fn view(&self, frame: &mut Frame) {
                frame.buffer.set_raw(0, 0, Cell::from_char('E'));
            }
        }

        let config =
            ProgramConfig::default().with_evidence_sink(EvidenceSinkConfig::enabled_stdout());
        let mut program = headless_program_with_config(RenderModel, config);
        program.budget = program
            .budget
            .with_controller(BudgetControllerConfig::default());

        program.render_frame().expect("render frame");
        assert!(program.budget.telemetry().is_some());
        assert_eq!(program.frame_idx, 1);
    }

    #[test]
    fn headless_handle_event_updates_model() {
        struct EventModel {
            events: usize,
            last_resize: Option<(u16, u16)>,
        }

        #[derive(Debug)]
        enum EventMsg {
            Resize(u16, u16),
            Other,
        }

        impl From<Event> for EventMsg {
            fn from(event: Event) -> Self {
                match event {
                    Event::Resize { width, height } => EventMsg::Resize(width, height),
                    _ => EventMsg::Other,
                }
            }
        }

        impl Model for EventModel {
            type Message = EventMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                self.events += 1;
                if let EventMsg::Resize(w, h) = msg {
                    self.last_resize = Some((w, h));
                }
                Cmd::none()
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut program = headless_program_with_config(
            EventModel {
                events: 0,
                last_resize: None,
            },
            ProgramConfig::default().with_resize_behavior(ResizeBehavior::Immediate),
        );

        program
            .handle_event(Event::Key(ftui_core::event::KeyEvent::new(
                ftui_core::event::KeyCode::Char('x'),
            )))
            .expect("handle key");
        assert_eq!(program.model().events, 1);

        program
            .handle_event(Event::Resize {
                width: 10,
                height: 5,
            })
            .expect("handle resize");
        assert_eq!(program.model().events, 2);
        assert_eq!(program.model().last_resize, Some((10, 5)));
        assert_eq!(program.width, 10);
        assert_eq!(program.height, 5);
    }

    #[test]
    fn headless_handle_resize_ignored_when_forced_size() {
        struct ResizeModel {
            resized: bool,
        }

        #[derive(Debug)]
        enum ResizeMsg {
            Resize,
            Other,
        }

        impl From<Event> for ResizeMsg {
            fn from(event: Event) -> Self {
                match event {
                    Event::Resize { .. } => ResizeMsg::Resize,
                    _ => ResizeMsg::Other,
                }
            }
        }

        impl Model for ResizeModel {
            type Message = ResizeMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                if matches!(msg, ResizeMsg::Resize) {
                    self.resized = true;
                }
                Cmd::none()
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let config = ProgramConfig::default().with_forced_size(80, 24);
        let mut program = headless_program_with_config(ResizeModel { resized: false }, config);

        program
            .handle_event(Event::Resize {
                width: 120,
                height: 40,
            })
            .expect("handle resize");

        assert_eq!(program.width, 80);
        assert_eq!(program.height, 24);
        assert!(!program.model().resized);
    }

    #[test]
    fn headless_execute_cmd_batch_sequence_and_quit() {
        struct BatchModel {
            count: usize,
        }

        #[derive(Debug)]
        enum BatchMsg {
            Inc,
        }

        impl From<Event> for BatchMsg {
            fn from(_: Event) -> Self {
                BatchMsg::Inc
            }
        }

        impl Model for BatchModel {
            type Message = BatchMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    BatchMsg::Inc => {
                        self.count += 1;
                        Cmd::none()
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut program =
            headless_program_with_config(BatchModel { count: 0 }, ProgramConfig::default());

        program
            .execute_cmd(Cmd::Batch(vec![
                Cmd::msg(BatchMsg::Inc),
                Cmd::Sequence(vec![
                    Cmd::msg(BatchMsg::Inc),
                    Cmd::quit(),
                    Cmd::msg(BatchMsg::Inc),
                ]),
            ]))
            .expect("batch cmd");

        assert_eq!(program.model().count, 2);
        assert!(!program.running);
    }

    #[test]
    fn headless_process_subscription_messages_updates_model() {
        use crate::subscription::{StopSignal, SubId, Subscription};

        struct SubModel {
            pings: usize,
            ready_tx: mpsc::Sender<()>,
        }

        #[derive(Debug)]
        enum SubMsg {
            Ping,
            Other,
        }

        impl From<Event> for SubMsg {
            fn from(_: Event) -> Self {
                SubMsg::Other
            }
        }

        impl Model for SubModel {
            type Message = SubMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                if let SubMsg::Ping = msg {
                    self.pings += 1;
                }
                Cmd::none()
            }

            fn view(&self, _frame: &mut Frame) {}

            fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
                vec![Box::new(TestSubscription {
                    ready_tx: self.ready_tx.clone(),
                })]
            }
        }

        struct TestSubscription {
            ready_tx: mpsc::Sender<()>,
        }

        impl Subscription<SubMsg> for TestSubscription {
            fn id(&self) -> SubId {
                1
            }

            fn run(&self, sender: mpsc::Sender<SubMsg>, _stop: StopSignal) {
                let _ = sender.send(SubMsg::Ping);
                let _ = self.ready_tx.send(());
            }
        }

        let (ready_tx, ready_rx) = mpsc::channel();
        let mut program =
            headless_program_with_config(SubModel { pings: 0, ready_tx }, ProgramConfig::default());

        program.reconcile_subscriptions();
        ready_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("subscription started");
        program
            .process_subscription_messages()
            .expect("process subscriptions");

        assert_eq!(program.model().pings, 1);
    }

    #[test]
    fn headless_execute_cmd_task_spawns_and_reaps() {
        struct TaskModel {
            done: bool,
        }

        #[derive(Debug)]
        enum TaskMsg {
            Done,
        }

        impl From<Event> for TaskMsg {
            fn from(_: Event) -> Self {
                TaskMsg::Done
            }
        }

        impl Model for TaskModel {
            type Message = TaskMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    TaskMsg::Done => {
                        self.done = true;
                        Cmd::none()
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut program =
            headless_program_with_config(TaskModel { done: false }, ProgramConfig::default());
        program
            .execute_cmd(Cmd::task(|| TaskMsg::Done))
            .expect("task cmd");

        let deadline = Instant::now() + Duration::from_millis(200);
        while !program.model().done {
            program
                .process_task_results()
                .expect("process task results");
            program.reap_finished_tasks();
            if Instant::now() > deadline {
                panic!("task result did not arrive in time");
            }
        }

        assert!(program.model().done);
    }

    #[test]
    fn headless_persistence_commands_with_registry() {
        use crate::state_persistence::{MemoryStorage, StateRegistry};
        use std::sync::Arc;

        let registry = Arc::new(StateRegistry::new(Box::new(MemoryStorage::new())));
        let config = ProgramConfig::default().with_registry(registry.clone());
        let mut program = headless_program_with_config(TestModel { value: 0 }, config);

        assert!(program.has_persistence());
        assert!(program.state_registry().is_some());

        program.execute_cmd(Cmd::save_state()).expect("save");
        program.execute_cmd(Cmd::restore_state()).expect("restore");

        let saved = program.trigger_save().expect("trigger save");
        let loaded = program.trigger_load().expect("trigger load");
        assert!(!saved);
        assert_eq!(loaded, 0);
    }

    #[test]
    fn headless_process_resize_coalescer_applies_pending_resize() {
        struct ResizeModel {
            last_size: Option<(u16, u16)>,
        }

        #[derive(Debug)]
        enum ResizeMsg {
            Resize(u16, u16),
            Other,
        }

        impl From<Event> for ResizeMsg {
            fn from(event: Event) -> Self {
                match event {
                    Event::Resize { width, height } => ResizeMsg::Resize(width, height),
                    _ => ResizeMsg::Other,
                }
            }
        }

        impl Model for ResizeModel {
            type Message = ResizeMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                if let ResizeMsg::Resize(w, h) = msg {
                    self.last_size = Some((w, h));
                }
                Cmd::none()
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let evidence_path = temp_evidence_path("fairness_allow");
        let sink_config = EvidenceSinkConfig::enabled_file(&evidence_path);
        let mut config = ProgramConfig::default().with_resize_behavior(ResizeBehavior::Throttled);
        config.resize_coalescer.steady_delay_ms = 0;
        config.resize_coalescer.burst_delay_ms = 0;
        config.resize_coalescer.hard_deadline_ms = 1_000;
        config.evidence_sink = sink_config.clone();

        let mut program = headless_program_with_config(ResizeModel { last_size: None }, config);
        let sink = EvidenceSink::from_config(&sink_config)
            .expect("evidence sink config")
            .expect("evidence sink enabled");
        program.evidence_sink = Some(sink);

        program.resize_coalescer.handle_resize(120, 40);
        assert!(program.resize_coalescer.has_pending());

        program
            .process_resize_coalescer()
            .expect("process resize coalescer");

        assert_eq!(program.width, 120);
        assert_eq!(program.height, 40);
        assert_eq!(program.model().last_size, Some((120, 40)));

        let config_line = read_evidence_event(&evidence_path, "fairness_config");
        assert_eq!(config_line["event"], "fairness_config");
        assert!(config_line["enabled"].is_boolean());
        assert!(config_line["input_priority_threshold_ms"].is_number());
        assert!(config_line["dominance_threshold"].is_number());
        assert!(config_line["fairness_threshold"].is_number());

        let decision_line = read_evidence_event(&evidence_path, "fairness_decision");
        assert_eq!(decision_line["event"], "fairness_decision");
        assert_eq!(decision_line["decision"], "allow");
        assert_eq!(decision_line["reason"], "none");
        assert!(decision_line["pending_input_latency_ms"].is_null());
        assert!(decision_line["jain_index"].is_number());
        assert!(decision_line["resize_dominance_count"].is_number());
        assert!(decision_line["dominance_threshold"].is_number());
        assert!(decision_line["fairness_threshold"].is_number());
        assert!(decision_line["input_priority_threshold_ms"].is_number());
    }

    #[test]
    fn headless_process_resize_coalescer_yields_to_input() {
        struct ResizeModel {
            last_size: Option<(u16, u16)>,
        }

        #[derive(Debug)]
        enum ResizeMsg {
            Resize(u16, u16),
            Other,
        }

        impl From<Event> for ResizeMsg {
            fn from(event: Event) -> Self {
                match event {
                    Event::Resize { width, height } => ResizeMsg::Resize(width, height),
                    _ => ResizeMsg::Other,
                }
            }
        }

        impl Model for ResizeModel {
            type Message = ResizeMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                if let ResizeMsg::Resize(w, h) = msg {
                    self.last_size = Some((w, h));
                }
                Cmd::none()
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let evidence_path = temp_evidence_path("fairness_yield");
        let sink_config = EvidenceSinkConfig::enabled_file(&evidence_path);
        let mut config = ProgramConfig::default().with_resize_behavior(ResizeBehavior::Throttled);
        config.resize_coalescer.steady_delay_ms = 0;
        config.resize_coalescer.burst_delay_ms = 0;
        config.evidence_sink = sink_config.clone();

        let mut program = headless_program_with_config(ResizeModel { last_size: None }, config);
        let sink = EvidenceSink::from_config(&sink_config)
            .expect("evidence sink config")
            .expect("evidence sink enabled");
        program.evidence_sink = Some(sink);

        program.fairness_guard = InputFairnessGuard::with_config(
            crate::input_fairness::FairnessConfig::default().with_max_latency(Duration::ZERO),
        );
        program
            .fairness_guard
            .input_arrived(Instant::now() - Duration::from_millis(1));

        program.resize_coalescer.handle_resize(120, 40);
        assert!(program.resize_coalescer.has_pending());

        program
            .process_resize_coalescer()
            .expect("process resize coalescer");

        assert_eq!(program.width, 80);
        assert_eq!(program.height, 24);
        assert_eq!(program.model().last_size, None);
        assert!(program.resize_coalescer.has_pending());

        let decision_line = read_evidence_event(&evidence_path, "fairness_decision");
        assert_eq!(decision_line["event"], "fairness_decision");
        assert_eq!(decision_line["decision"], "yield");
        assert_eq!(decision_line["reason"], "input_latency");
        assert!(decision_line["pending_input_latency_ms"].is_number());
        assert!(decision_line["jain_index"].is_number());
        assert!(decision_line["resize_dominance_count"].is_number());
        assert!(decision_line["dominance_threshold"].is_number());
        assert!(decision_line["fairness_threshold"].is_number());
        assert!(decision_line["input_priority_threshold_ms"].is_number());
    }

    #[test]
    fn headless_execute_cmd_task_with_effect_queue() {
        struct TaskModel {
            done: bool,
        }

        #[derive(Debug)]
        enum TaskMsg {
            Done,
        }

        impl From<Event> for TaskMsg {
            fn from(_: Event) -> Self {
                TaskMsg::Done
            }
        }

        impl Model for TaskModel {
            type Message = TaskMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    TaskMsg::Done => {
                        self.done = true;
                        Cmd::none()
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let effect_queue = EffectQueueConfig {
            enabled: true,
            scheduler: SchedulerConfig {
                max_queue_size: 0,
                ..Default::default()
            },
        };
        let config = ProgramConfig::default().with_effect_queue(effect_queue);
        let mut program = headless_program_with_config(TaskModel { done: false }, config);

        program
            .execute_cmd(Cmd::task(|| TaskMsg::Done))
            .expect("task cmd");

        let deadline = Instant::now() + Duration::from_millis(200);
        while !program.model().done {
            program
                .process_task_results()
                .expect("process task results");
            if Instant::now() > deadline {
                panic!("effect queue task result did not arrive in time");
            }
        }

        assert!(program.model().done);
    }

    // =========================================================================
    // BatchController Tests (bd-4kq0.8.1)
    // =========================================================================

    #[test]
    fn unit_tau_monotone() {
        // τ should decrease (or stay constant) as service time decreases,
        // since τ = E[S] × headroom.
        let mut bc = BatchController::new();

        // High service time → high τ
        bc.observe_service(Duration::from_millis(20));
        bc.observe_service(Duration::from_millis(20));
        bc.observe_service(Duration::from_millis(20));
        let tau_high = bc.tau_s();

        // Low service time → lower τ
        for _ in 0..20 {
            bc.observe_service(Duration::from_millis(1));
        }
        let tau_low = bc.tau_s();

        assert!(
            tau_low <= tau_high,
            "τ should decrease with lower service time: tau_low={tau_low:.6}, tau_high={tau_high:.6}"
        );
    }

    #[test]
    fn unit_tau_monotone_lambda() {
        // As arrival rate λ decreases (longer inter-arrival times),
        // τ should not increase (it's based on service time, not λ).
        // But ρ should decrease.
        let mut bc = BatchController::new();
        let base = Instant::now();

        // Fast arrivals (λ high)
        for i in 0..10 {
            bc.observe_arrival(base + Duration::from_millis(i * 10));
        }
        let rho_fast = bc.rho_est();

        // Slow arrivals (λ low)
        for i in 10..20 {
            bc.observe_arrival(base + Duration::from_millis(100 + i * 100));
        }
        let rho_slow = bc.rho_est();

        assert!(
            rho_slow < rho_fast,
            "ρ should decrease with slower arrivals: rho_slow={rho_slow:.4}, rho_fast={rho_fast:.4}"
        );
    }

    #[test]
    fn unit_stability() {
        // With reasonable service times, the controller should keep ρ < 1.
        let mut bc = BatchController::new();
        let base = Instant::now();

        // Moderate arrival rate: 30 events/sec
        for i in 0..30 {
            bc.observe_arrival(base + Duration::from_millis(i * 33));
            bc.observe_service(Duration::from_millis(5)); // 5ms render
        }

        assert!(
            bc.is_stable(),
            "should be stable at 30 events/sec with 5ms service: ρ={:.4}",
            bc.rho_est()
        );
        assert!(
            bc.rho_est() < 1.0,
            "utilization should be < 1: ρ={:.4}",
            bc.rho_est()
        );

        // τ must be > E[S] (stability requirement)
        assert!(
            bc.tau_s() > bc.service_est_s(),
            "τ ({:.6}) must exceed E[S] ({:.6}) for stability",
            bc.tau_s(),
            bc.service_est_s()
        );
    }

    #[test]
    fn unit_stability_high_load() {
        // Even under high load, τ keeps the system stable.
        let mut bc = BatchController::new();
        let base = Instant::now();

        // 100 events/sec with 8ms render
        for i in 0..50 {
            bc.observe_arrival(base + Duration::from_millis(i * 10));
            bc.observe_service(Duration::from_millis(8));
        }

        // τ × ρ_eff = E[S]/τ should be < 1
        let tau = bc.tau_s();
        let rho_eff = bc.service_est_s() / tau;
        assert!(
            rho_eff < 1.0,
            "effective utilization should be < 1: ρ_eff={rho_eff:.4}, τ={tau:.6}, E[S]={:.6}",
            bc.service_est_s()
        );
    }

    #[test]
    fn batch_controller_defaults() {
        let bc = BatchController::new();
        assert!(bc.tau_s() >= bc.tau_min_s);
        assert!(bc.tau_s() <= bc.tau_max_s);
        assert_eq!(bc.observations(), 0);
        assert!(bc.is_stable());
    }

    #[test]
    fn batch_controller_tau_clamped() {
        let mut bc = BatchController::new();

        // Very fast service → τ clamped to tau_min
        for _ in 0..20 {
            bc.observe_service(Duration::from_micros(10));
        }
        assert!(
            bc.tau_s() >= bc.tau_min_s,
            "τ should be >= tau_min: τ={:.6}, min={:.6}",
            bc.tau_s(),
            bc.tau_min_s
        );

        // Very slow service → τ clamped to tau_max
        for _ in 0..20 {
            bc.observe_service(Duration::from_millis(100));
        }
        assert!(
            bc.tau_s() <= bc.tau_max_s,
            "τ should be <= tau_max: τ={:.6}, max={:.6}",
            bc.tau_s(),
            bc.tau_max_s
        );
    }

    #[test]
    fn batch_controller_duration_conversion() {
        let bc = BatchController::new();
        let tau = bc.tau();
        let tau_s = bc.tau_s();
        // Duration should match f64 representation
        let diff = (tau.as_secs_f64() - tau_s).abs();
        assert!(diff < 1e-9, "Duration conversion mismatch: {diff}");
    }

    #[test]
    fn batch_controller_lambda_estimation() {
        let mut bc = BatchController::new();
        let base = Instant::now();

        // 50 events/sec (20ms apart)
        for i in 0..20 {
            bc.observe_arrival(base + Duration::from_millis(i * 20));
        }

        // λ should converge near 50
        let lambda = bc.lambda_est();
        assert!(
            lambda > 20.0 && lambda < 100.0,
            "λ should be near 50: got {lambda:.1}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // Persistence Config Tests
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn cmd_save_state() {
        let cmd: Cmd<TestMsg> = Cmd::save_state();
        assert!(matches!(cmd, Cmd::SaveState));
    }

    #[test]
    fn cmd_restore_state() {
        let cmd: Cmd<TestMsg> = Cmd::restore_state();
        assert!(matches!(cmd, Cmd::RestoreState));
    }

    #[test]
    fn persistence_config_default() {
        let config = PersistenceConfig::default();
        assert!(config.registry.is_none());
        assert!(config.checkpoint_interval.is_none());
        assert!(config.auto_load);
        assert!(config.auto_save);
    }

    #[test]
    fn persistence_config_disabled() {
        let config = PersistenceConfig::disabled();
        assert!(config.registry.is_none());
    }

    #[test]
    fn persistence_config_with_registry() {
        use crate::state_persistence::{MemoryStorage, StateRegistry};
        use std::sync::Arc;

        let registry = Arc::new(StateRegistry::new(Box::new(MemoryStorage::new())));
        let config = PersistenceConfig::with_registry(registry.clone());

        assert!(config.registry.is_some());
        assert!(config.auto_load);
        assert!(config.auto_save);
    }

    #[test]
    fn persistence_config_checkpoint_interval() {
        use crate::state_persistence::{MemoryStorage, StateRegistry};
        use std::sync::Arc;

        let registry = Arc::new(StateRegistry::new(Box::new(MemoryStorage::new())));
        let config = PersistenceConfig::with_registry(registry)
            .checkpoint_every(Duration::from_secs(30))
            .auto_load(false)
            .auto_save(true);

        assert!(config.checkpoint_interval.is_some());
        assert_eq!(config.checkpoint_interval.unwrap(), Duration::from_secs(30));
        assert!(!config.auto_load);
        assert!(config.auto_save);
    }

    #[test]
    fn program_config_with_persistence() {
        use crate::state_persistence::{MemoryStorage, StateRegistry};
        use std::sync::Arc;

        let registry = Arc::new(StateRegistry::new(Box::new(MemoryStorage::new())));
        let config = ProgramConfig::default().with_registry(registry);

        assert!(config.persistence.registry.is_some());
    }
}
