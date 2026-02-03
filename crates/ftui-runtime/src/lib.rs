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

pub mod allocation_budget;
pub mod asciicast;
pub mod conformal_alert;
pub mod eprocess_throttle;
pub mod flake_detector;
pub mod input_fairness;
pub mod input_macro;
pub mod locale;
pub mod log_sink;
pub mod program;
#[cfg(feature = "render-thread")]
pub mod render_thread;
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

pub mod reactive;
pub mod schedule_trace;
#[cfg(feature = "telemetry")]
pub mod telemetry;

pub use asciicast::{AsciicastRecorder, AsciicastWriter};
pub use input_macro::{
    EventRecorder, FilteredEventRecorder, InputMacro, MacroPlayback, MacroPlayer, MacroRecorder,
    RecordingFilter, RecordingState, TimedEvent,
};
pub use locale::{
    Locale, LocaleContext, LocaleOverride, current_locale, detect_system_locale, set_locale,
};
pub use log_sink::LogSink;
pub use program::{
    App, AppBuilder, BatchController, Cmd, Model, PersistenceConfig, Program, ProgramConfig,
    ResizeBehavior,
};
pub use simulator::ProgramSimulator;
pub use string_model::{StringModel, StringModelAdapter};
pub use subscription::{Every, MockSubscription, StopSignal, SubId, Subscription};
pub use terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};

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
pub use eprocess_throttle::{
    EProcessThrottle, ThrottleConfig, ThrottleDecision, ThrottleLog, ThrottleStats,
};
pub use flake_detector::{EvidenceLog, FlakeConfig, FlakeDecision, FlakeDetector, FlakeSummary};
pub use reactive::{BatchScope, Binding, BindingScope, Computed, Observable, TwoWayBinding};
pub use resize_coalescer::{
    CoalesceAction, CoalescerConfig, CoalescerStats, CycleTimePercentiles, DecisionLog,
    DecisionSummary, Regime, ResizeCoalescer, TelemetryHooks,
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

#[cfg(feature = "telemetry")]
pub use telemetry::{
    DecisionEvidence, EnabledReason, EndpointSource, EvidenceLedger, Protocol, SCHEMA_VERSION,
    SpanId, TelemetryConfig, TelemetryError, TelemetryGuard, TraceContextSource, TraceId,
    is_safe_env_var, redact,
};
