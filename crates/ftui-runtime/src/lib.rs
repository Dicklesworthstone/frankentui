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
pub mod asciicast;
pub mod bocpd;
pub mod conformal_alert;
pub mod conformal_predictor;
pub mod debug_trace;
pub mod eprocess_throttle;
pub mod evidence_sink;
pub mod evidence_telemetry;
pub mod flake_detector;
pub mod input_fairness;
pub mod input_macro;
pub mod locale;
pub mod log_sink;
pub mod program;
pub mod queueing_scheduler;
#[cfg(feature = "render-thread")]
pub mod render_thread;
pub mod render_trace;
pub mod resize_coalescer;
pub mod resize_sla;
pub mod simulator;
pub mod state_persistence;
#[cfg(feature = "stdio-capture")]
pub mod stdio_capture;
pub mod string_model;
pub mod subscription;
pub mod terminal_writer;
pub mod undo;
pub mod validation_pipeline;
pub mod voi_sampling;

pub mod reactive;
pub mod schedule_trace;
#[cfg(feature = "telemetry")]
pub mod telemetry;
pub mod voi_telemetry;

pub use asciicast::{AsciicastRecorder, AsciicastWriter};
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
pub use program::{
    App, AppBuilder, BatchController, Cmd, CrosstermEventSource, EffectQueueConfig, FrameTiming,
    FrameTimingConfig, FrameTimingSink, InlineAutoRemeasureConfig, Model, PersistenceConfig,
    Program, ProgramConfig, ResizeBehavior, TaskSpec, WidgetRefreshConfig,
};
pub use render_trace::{
    RenderTraceConfig, RenderTraceContext, RenderTraceFrame, RenderTraceRecorder,
};
pub use simulator::ProgramSimulator;
pub use string_model::{StringModel, StringModelAdapter};
pub use subscription::{Every, StopSignal, SubId, Subscription};
pub use terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
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
pub use conformal_predictor::{
    BucketKey, ConformalConfig, ConformalPrediction, ConformalPredictor, ConformalUpdate,
    DiffBucket, ModeBucket,
};
pub use eprocess_throttle::{
    EProcessThrottle, ThrottleConfig, ThrottleDecision, ThrottleLog, ThrottleStats,
};
pub use flake_detector::{EvidenceLog, FlakeConfig, FlakeDecision, FlakeDetector, FlakeSummary};
pub use reactive::{BatchScope, Binding, BindingScope, Computed, Observable, TwoWayBinding};
pub use resize_coalescer::{
    CoalesceAction, CoalescerConfig, CoalescerStats, CycleTimePercentiles, DecisionLog,
    DecisionSummary, Regime, ResizeCoalescer,
};
pub use resize_sla::{
    ResizeEvidence, ResizeSlaMonitor, SlaConfig, SlaLogEntry, SlaSummary, make_sla_hooks,
};
pub use undo::{
    CommandBatch, CommandError, CommandMetadata, CommandResult, CommandSource, HistoryConfig,
    HistoryManager, MergeConfig, TextDeleteCmd, TextInsertCmd, TextReplaceCmd, Transaction,
    TransactionScope, UndoableCmd, WidgetId,
};
pub use validation_pipeline::{
    LedgerEntry, PipelineConfig, PipelineResult, PipelineSummary, ValidationOutcome,
    ValidationPipeline, ValidatorStats,
};
pub use voi_sampling::{
    VoiConfig, VoiDecision, VoiLogEntry, VoiObservation, VoiSampler, VoiSamplerSnapshot, VoiSummary,
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

#[cfg(feature = "telemetry")]
pub use telemetry::{
    DecisionEvidence, EnabledReason, EndpointSource, EvidenceLedger, Protocol, SCHEMA_VERSION,
    SpanId, TelemetryConfig, TelemetryError, TelemetryGuard, TraceContextSource, TraceId,
    is_safe_env_var, redact,
};
