#![forbid(unsafe_code)]

//! FrankenTUI Runtime
//!
//! This crate provides the runtime components that tie together the core,
//! render, and layout crates into a complete terminal application framework.
//!
//! # Key Components
//!
//! - [`TerminalWriter`] - Unified terminal output coordinator with inline mode support
//! - [`LogSink`] - Line-buffered writer for sanitized log output
//! - [`Program`] - Bubbletea/Elm-style runtime for terminal applications
//! - [`Model`] - Trait for application state and behavior
//! - [`Cmd`] - Commands for side effects
//! - [`Subscription`] - Trait for continuous event sources
//! - [`Every`] - Built-in tick subscription
//!
//! # Role in FrankenTUI
//! `ftui-runtime` is the orchestrator. It consumes input events from
//! `ftui-core`, drives your `Model::update`, calls `Model::view` to render
//! frames, and delegates rendering to `ftui-render` via `TerminalWriter`.
//!
//! # How it fits in the system
//! The runtime is the center of the architecture: it is the bridge between
//! input (`ftui-core`) and output (`ftui-render`). Widgets and layout are
//! optional layers used by your `view()` to construct UI output.

pub mod allocation_budget;
pub mod alpha_investing;
pub mod asciicast;
pub mod bocpd;
pub mod cancellation;

pub mod conformal_alert;
pub mod conformal_frame_guard;
pub mod conformal_predictor;
pub mod conformal_stages;
pub mod cost_model;
pub mod debug_trace;
pub mod decision_core;
pub mod degradation_cascade;
pub mod demo;
pub mod diff_evidence;
pub mod effect_system;
pub mod eprocess_throttle;
#[cfg(feature = "event-trace")]
pub mod event_trace;
pub mod evidence_bridges;
pub mod evidence_sink;
pub mod evidence_telemetry;
pub mod flake_detector;
pub mod flat_combine;
pub mod input_fairness;
pub mod input_macro;
pub mod ivm;
pub mod lens;
pub mod locale;
pub mod log_sink;
pub mod metrics_registry;
pub mod pane_keymap;
pub mod policy_config;
pub mod policy_registry;
pub mod process_subscription;
pub mod program;
pub mod queueing_scheduler;
#[cfg(feature = "render-thread")]
pub mod render_thread;
pub mod render_trace;
pub mod resize_coalescer;
pub mod resize_sla;
pub mod retry;
pub mod reversible;
pub mod rough_path;
pub mod schema_compat;
pub mod simulator;
pub mod slo;
pub mod sos_barrier;
pub mod state_persistence;
#[cfg(feature = "stdio-capture")]
pub mod stdio_capture;
pub mod string_model;
pub mod subscription;
pub mod telemetry_schema;
pub mod terminal_writer;
pub mod tick_strategy;
pub mod transparency;
pub mod undo;
pub mod unified_evidence;
pub mod validation_pipeline;
pub mod voi_sampling;
pub mod wasm_runner;

pub mod reactive;
pub mod schedule_trace;
#[cfg(feature = "telemetry")]
pub mod telemetry;
pub mod voi_telemetry;

pub use asciicast::{AsciicastRecorder, AsciicastWriter};
pub use cancellation::{CancellationSource, CancellationToken};
pub use diff_evidence::{
    DiffEvidenceLedger, DiffRegime, DiffStrategyRecord, Observation, RegimeTransition,
};
pub use evidence_sink::{EvidenceSink, EvidenceSinkConfig, EvidenceSinkDestination};
pub use evidence_telemetry::{
    BudgetDecisionSnapshot, ConformalSnapshot, DiffDecisionSnapshot, ResizeDecisionSnapshot,
    budget_snapshot, clear_budget_snapshot, clear_diff_snapshot, clear_resize_snapshot,
    diff_snapshot, resize_snapshot, set_budget_snapshot, set_diff_snapshot, set_resize_snapshot,
};
pub use ftui_backend::{BackendEventSource, BackendFeatures};
#[cfg(feature = "native-backend")]
pub use ftui_tty::TtyBackend;
pub use input_macro::{
    EventRecorder, FilteredEventRecorder, InputMacro, MacroPlayback, MacroPlayer, MacroRecorder,
    RecordingFilter, RecordingState, TimedEvent,
};
pub use locale::{
    Locale, LocaleContext, LocaleOverride, current_locale, detect_system_locale, set_locale,
};
pub use log_sink::LogSink;
pub use process_subscription::{ProcessEvent, ProcessSubscription};
#[cfg(feature = "crossterm-compat")]
pub use program::CrosstermEventSource;
pub use program::{
    App, AppBuilder, BatchController, Cmd, EffectQueueConfig, FrameTiming, FrameTimingConfig,
    FrameTimingSink, HeadlessEventSource, InlineAutoRemeasureConfig, LoadGovernorConfig, Model,
    MouseCapturePolicy, PaneTerminalAdapter, PaneTerminalAdapterConfig, PaneTerminalDispatch,
    PaneTerminalIgnoredReason, PaneTerminalLifecyclePhase, PaneTerminalLogEntry,
    PaneTerminalLogOutcome, PaneTerminalSplitterHandle, PersistenceConfig, Program, ProgramConfig,
    ResizeBehavior, RolloutPolicy, RuntimeLane, TaskExecutorBackend, TaskSpec, WidgetRefreshConfig,
    pane_terminal_resolve_splitter_target, pane_terminal_splitter_handles,
    pane_terminal_target_from_hit, register_pane_terminal_splitter_hits,
};
pub use render_trace::{
    RENDER_TRACE_SCHEMA_VERSION, RenderTraceConfig, RenderTraceContext, RenderTraceFrame,
    RenderTraceRecorder,
};
pub use retry::{BackoffStrategy, RetryPolicy, task_with_retry, task_with_timeout};
pub use simulator::{ProgramSimulator, SimulatorError};
pub use string_model::{StringModel, StringModelAdapter};
pub use subscription::{Every, StopSignal, SubId, Subscription};
pub use terminal_writer::{ScreenMode, TerminalWriter, UiAnchor, inline_active_widgets};
pub use tick_strategy::{
    ActiveOnly, ActivePlusAdjacent, AllocationCurve, Custom, DecayConfig, MarkovPredictor,
    Predictive, PredictiveConfig, PredictiveStrategyConfig, ScreenPrediction, ScreenTickDispatch,
    TickAllocation, TickDecision, TickStrategy, TickStrategyKind, TransitionCounter,
    TransitionEntry, TransitionHistory, Uniform,
};
#[cfg(feature = "state-persistence")]
pub use tick_strategy::{load_transitions, save_transitions};
pub use voi_telemetry::{
    clear_inline_auto_voi_snapshot, inline_auto_voi_snapshot, set_inline_auto_voi_snapshot,
};

#[cfg(feature = "render-thread")]
pub use render_thread::{OutMsg, RenderThread};

#[cfg(feature = "stdio-capture")]
pub use stdio_capture::{CapturedWriter, StdioCapture, StdioCaptureError};

pub use allocation_budget::{
    AllocationBudget, BudgetAlert, BudgetConfig, BudgetEvidence, BudgetSummary,
};
pub use conformal_alert::{
    AlertConfig, AlertDecision, AlertEvidence, AlertReason, AlertStats, ConformalAlert,
};
pub use conformal_frame_guard::{
    ConformalFrameGuard, ConformalFrameGuardConfig, ConformalFrameGuardTelemetry, GuardState,
    NonconformitySummary, P99Prediction,
};
pub use conformal_predictor::{
    BucketKey, ConformalConfig, ConformalPrediction, ConformalPredictor, ConformalUpdate,
    DiffBucket, ModeBucket,
};
pub use cost_model::{
    BatchCostParams, BatchCostResult, CacheCostParams, CacheCostResult, PipelineCostParams,
    PipelineCostResult, StageStats,
};
pub use decision_core::{
    Action as DecisionAction, Decision, DecisionCore, Outcome as DecisionOutcome, Posterior,
    State as DecisionState, argmin_expected_loss, second_best_loss,
};
pub use degradation_cascade::{
    CascadeConfig, CascadeDecision, CascadeEvidence, CascadeTelemetry, DegradationCascade,
    PreRenderResult,
};
pub use demo::{DemoDefinition, DemoParseError, DemoStep, parse_demo_yaml, validate_demos};
pub use effect_system::{
    QueueTelemetry, effects_command_total, effects_executed_total, effects_queue_dropped,
    effects_queue_enqueued, effects_queue_high_water, effects_queue_processed,
    effects_subscription_total, queue_telemetry, record_command_effect, record_subscription_start,
    record_subscription_stop, trace_command_effect,
};
pub use eprocess_throttle::{
    EProcessThrottle, ThrottleConfig, ThrottleDecision, ThrottleLog, ThrottleStats,
    eprocess_rejections_total,
};
#[cfg(feature = "event-trace")]
pub use event_trace::{
    EventReplayer, EventTraceReader, EventTraceWriter, EvidenceMismatch, EvidenceVerifier,
    SerDecisionDomain, SerEvidenceEntry, SerEvidenceTerm, TraceFile, TraceRecord,
};
pub use flake_detector::{EvidenceLog, FlakeConfig, FlakeDecision, FlakeDetector, FlakeSummary};
pub use flat_combine::{CombinerStats, FlatCombiner};
pub use lens::{AtIndex, Composed, Fst, Identity, Lens, Prism, Snd, SomePrism, at_index, compose};
pub use metrics_registry::{
    BuiltinCounter, BuiltinGauge, BuiltinHistogram, Counter as MetricsCounter,
    Gauge as MetricsGauge, Histogram as MetricsHistogram, METRICS, MetricsRegistry,
};
pub use policy_config::{
    BocpdPolicyConfig, CascadePolicyConfig, ConformalPolicyConfig, EProcessBudgetPolicyConfig,
    EProcessThrottlePolicyConfig, EvidencePolicyConfig, FrameGuardPolicyConfig, PidPolicyConfig,
    PolicyConfig, PolicyConfigError, VoiPolicyConfig,
};
pub use policy_registry::{PolicyRegistry, PolicyRegistryError, PolicySwitchEvent};
pub use reactive::{BatchScope, Binding, BindingScope, Computed, Observable, TwoWayBinding};
pub use resize_coalescer::{
    CoalesceAction, CoalescerConfig, CoalescerStats, CycleTimePercentiles, DecisionLog,
    DecisionSummary, Regime, ResizeCoalescer,
};
pub use resize_sla::{
    ResizeEvidence, ResizeSlaMonitor, SlaConfig, SlaLogEntry, SlaSummary, make_sla_hooks,
};
pub use reversible::{
    AddOp, InsertOp, Journal, MulOp, PushOp, RemoveOp, Reversible, Sequence, SetOp, SwapOp, XorOp,
};
pub use schema_compat::{
    CompatCheckResult, Compatibility, MatrixEntry, SchemaKind, check_event_trace_compat,
    check_evidence_compat, check_golden_trace_compat, check_render_trace_compat,
    check_schema_compat, default_compatibility_matrix, run_compatibility_matrix,
};
pub use slo::{
    BreachResult, BreachSeverity, MetricSlo, MetricType, SafeModeDecision, SloSchema,
    SloSchemaError, check_breach, check_safe_mode, emit_slo_check, parse_slo_yaml, run_slo_check,
};
pub use undo::{
    CommandBatch, CommandError, CommandMetadata, CommandResult, CommandSource, HistoryConfig,
    HistoryManager, MergeConfig, TextDeleteCmd, TextInsertCmd, TextReplaceCmd, Transaction,
    TransactionScope, UndoableCmd, WidgetId,
};
pub use unified_evidence::{
    DecisionDomain, DomainSummary, EmitsEvidence, EvidenceEntry, EvidenceEntryBuilder,
    EvidenceTerm, LedgerSummary, UnifiedEvidenceLedger,
};
pub use validation_pipeline::{
    LedgerEntry, PipelineConfig, PipelineResult, PipelineSummary, ValidationOutcome,
    ValidationPipeline, ValidatorStats,
};
pub use voi_sampling::{
    DeferredRefinementConfig, DeferredRefinementPlan, DeferredRefinementScheduler,
    RefinementCandidate, RefinementSelection, VoiConfig, VoiDecision, VoiLogEntry, VoiObservation,
    VoiSampler, VoiSamplerSnapshot, VoiSummary, voi_samples_skipped_total, voi_samples_taken_total,
};

// State persistence
#[cfg(feature = "state-persistence")]
pub use state_persistence::FileStorage;
pub use state_persistence::{
    MemoryStorage, RegistryStats, StateRegistry, StorageBackend, StorageError, StorageResult,
    StoredEntry,
};

pub use schedule_trace::{
    CancelReason, GoldenCompareResult, IsomorphismProof, ScheduleTrace, SchedulerPolicy, TaskEvent,
    TraceConfig, TraceEntry, TraceSummary, WakeupReason, compare_golden,
};

// Diff strategy (re-exports from ftui-render)
pub use ftui_render::diff_strategy::{
    DiffStrategy, DiffStrategyConfig, DiffStrategySelector, StrategyEvidence,
};
pub use terminal_writer::RuntimeDiffConfig;
pub use wasm_runner::{RenderedFrame, StepResult, WasmRunner};

#[cfg(feature = "telemetry")]
pub use telemetry::{
    DecisionEvidence, EnabledReason, EndpointSource, EvidenceLedger, Protocol, SCHEMA_VERSION,
    SpanId, TelemetryConfig, TelemetryError, TelemetryGuard, TraceContextSource, TraceId,
    is_safe_env_var, redact,
};
