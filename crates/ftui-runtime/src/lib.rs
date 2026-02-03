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
pub mod eprocess_throttle;
pub mod input_macro;
pub mod log_sink;
pub mod program;
#[cfg(feature = "render-thread")]
pub mod render_thread;
pub mod resize_coalescer;
pub mod simulator;
#[cfg(feature = "stdio-capture")]
pub mod stdio_capture;
pub mod string_model;
pub mod subscription;
pub mod terminal_writer;
pub mod undo;
pub mod validation_pipeline;

pub mod reactive;

pub use asciicast::{AsciicastRecorder, AsciicastWriter};
pub use input_macro::{
    EventRecorder, FilteredEventRecorder, InputMacro, MacroPlayback, MacroPlayer, MacroRecorder,
    RecordingFilter, RecordingState, TimedEvent,
};
pub use log_sink::LogSink;
pub use program::{
    App, AppBuilder, BatchController, BatchTelemetry, Cmd, Model, Program, ProgramConfig,
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
pub use eprocess_throttle::{
    EProcessThrottle, ThrottleConfig, ThrottleDecision, ThrottleLog, ThrottleStats,
};
pub use reactive::{BatchScope, Binding, Computed, Observable, TwoWayBinding};
pub use resize_coalescer::{
    CoalesceAction, CoalescerConfig, CoalescerStats, DecisionLog, Regime, ResizeCoalescer,
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
