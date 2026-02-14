#![forbid(unsafe_code)]

//! Undo/Redo command history framework.
//!
//! This module provides infrastructure for reversible operations in FrankenTUI
//! applications. It implements the Command Pattern with support for:
//!
//! - **Reversibility**: Every command can be undone and redone
//! - **Merging**: Consecutive similar commands batch together (e.g., typing)
//! - **Memory management**: Commands report size for bounded history
//! - **Batching**: Multiple commands group into atomic operations
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                        UndoStack                                  │
//! │  ┌──────────────────┐          ┌──────────────────┐             │
//! │  │   Undo Stack     │          │   Redo Stack     │             │
//! │  │  ┌────────────┐  │          │  ┌────────────┐  │             │
//! │  │  │ CommandN   │  │  undo()  │  │ Command1   │  │             │
//! │  │  ├────────────┤  │ ──────►  │  ├────────────┤  │             │
//! │  │  │ Command2   │  │          │  │ Command2   │  │             │
//! │  │  ├────────────┤  │  ◄────── │  ├────────────┤  │             │
//! │  │  │ Command1   │  │  redo()  │  │ CommandN   │  │             │
//! │  │  └────────────┘  │          │  └────────────┘  │             │
//! │  └──────────────────┘          └──────────────────┘             │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Quick Start
//!
//! ```ignore
//! use ftui_runtime::undo::{UndoableCmd, CommandMetadata, TextInsertCmd, WidgetId};
//!
//! // Create a command
//! let cmd = TextInsertCmd::new(WidgetId::new(1), 0, "Hello")
//!     .with_apply(|id, pos, text| {
//!         // Apply the insertion
//!         Ok(())
//!     })
//!     .with_remove(|id, pos, len| {
//!         // Remove the insertion
//!         Ok(())
//!     });
//!
//! // Execute the command
//! cmd.execute()?;
//!
//! // Later, undo it
//! cmd.undo()?;
//! ```
//!
//! # Module Structure
//!
//! - [`command`]: Core `UndoableCmd` trait and built-in commands
//!
//! # Design Notes
//!
//! ## Why Commands Store Callbacks
//!
//! Commands need to interact with widget state, but we can't store references
//! to widgets (lifetime issues). Instead, commands store callbacks that are
//! set by the widget when the command is created. This allows:
//!
//! 1. Commands to be stored in history (owned, not borrowed)
//! 2. Widgets to control how operations are applied
//! 3. Commands to work with any widget implementation
//!
//! ## Merge Strategy
//!
//! Command merging reduces memory usage and makes undo more natural:
//!
//! - Typing "hello" creates 5 insert commands
//! - Merged, they become 1 command that inserts "hello"
//! - Undo removes "hello" in one step (more intuitive)
//!
//! Merge decisions use:
//! - Time window (500ms default)
//! - Word boundaries (optional)
//! - Size limits (prevent unbounded growth)
//!
//! ## Memory Budget
//!
//! Every command reports its size via `size_bytes()`. The undo stack uses
//! this to enforce memory limits:
//!
//! - Default: 10MB history
//! - Oldest commands evicted when limit exceeded
//! - Commands can estimate or measure their size

pub mod command;
pub mod history;
pub mod snapshot_store;
pub mod transaction;

// Re-export commonly used types
pub use command::{
    CommandBatch, CommandError, CommandMetadata, CommandResult, CommandSource, MergeConfig,
    TextDeleteCmd, TextInsertCmd, TextReplaceCmd, UndoableCmd, WidgetId,
};
pub use history::{HistoryConfig, HistoryManager};
pub use snapshot_store::{SnapshotConfig, SnapshotStore};
pub use transaction::{Transaction, TransactionScope};
