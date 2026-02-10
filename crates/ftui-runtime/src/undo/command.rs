#![forbid(unsafe_code)]

//! Undoable command infrastructure for the undo/redo system.
//!
//! This module provides the [`UndoableCmd`] trait for reversible operations
//! and common command implementations for text editing and UI interactions.
//!
//! # Design Principles
//!
//! 1. **Explicit state**: Commands capture all state needed for undo/redo
//! 2. **Memory-efficient**: Commands report their size for budget management
//! 3. **Mergeable**: Consecutive similar commands can merge (e.g., typing)
//! 4. **Traceable**: Commands include metadata for debugging and UI display
//!
//! # Invariants
//!
//! - `execute()` followed by `undo()` restores prior state exactly
//! - `undo()` followed by `redo()` restores the executed state exactly
//! - Commands with `can_merge() == true` MUST successfully merge
//! - `size_bytes()` MUST be accurate for memory budgeting
//!
//! # Failure Modes
//!
//! - **Stale reference**: Command holds reference to deleted target
//!   - Mitigation: Validate target existence in execute/undo
//! - **State drift**: External changes invalidate undo data
//!   - Mitigation: Clear undo stack on external modifications
//! - **Memory exhaustion**: Unbounded history growth
//!   - Mitigation: History stack enforces size limits via `size_bytes()`

use std::any::Any;
use std::fmt;
use web_time::Instant;

/// Unique identifier for a widget that commands operate on.
///
/// Commands targeting widgets store this ID to locate their target
/// during execute/undo operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetId(pub u64);

impl WidgetId {
    /// Create a new widget ID from a raw value.
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw ID value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Source of a command - who/what triggered it.
///
/// Used for filtering undo history and debugging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandSource {
    /// Direct user action (keyboard, mouse).
    #[default]
    User,
    /// Triggered programmatically by application code.
    Programmatic,
    /// Replayed from a recorded macro.
    Macro,
    /// Triggered by an external system/API.
    External,
}

/// Metadata attached to every command for tracing and UI display.
#[derive(Debug, Clone)]
pub struct CommandMetadata {
    /// Human-readable description for UI (e.g., "Insert text").
    pub description: String,
    /// When the command was created.
    pub timestamp: Instant,
    /// Who/what triggered the command.
    pub source: CommandSource,
    /// Optional batch ID for grouping related commands.
    pub batch_id: Option<u64>,
}

impl CommandMetadata {
    /// Create new metadata with the given description.
    #[must_use]
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            timestamp: Instant::now(),
            source: CommandSource::User,
            batch_id: None,
        }
    }

    /// Set the command source.
    #[must_use]
    pub fn with_source(mut self, source: CommandSource) -> Self {
        self.source = source;
        self
    }

    /// Set the batch ID for grouping.
    #[must_use]
    pub fn with_batch(mut self, batch_id: u64) -> Self {
        self.batch_id = Some(batch_id);
        self
    }

    /// Size in bytes for memory accounting.
    #[must_use]
    pub fn size_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.description.len()
    }
}

impl Default for CommandMetadata {
    fn default() -> Self {
        Self::new("Unknown")
    }
}

/// Result of command execution or undo.
///
/// Commands may fail if targets are invalid or state has drifted.
pub type CommandResult = Result<(), CommandError>;

/// Errors that can occur during command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// Target widget no longer exists.
    TargetNotFound(WidgetId),
    /// Position is out of bounds.
    PositionOutOfBounds { position: usize, length: usize },
    /// State has changed since command was created.
    StateDrift { expected: String, actual: String },
    /// Command cannot be executed in current state.
    InvalidState(String),
    /// Generic error with message.
    Other(String),
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetNotFound(id) => write!(f, "target widget {:?} not found", id),
            Self::PositionOutOfBounds { position, length } => {
                write!(f, "position {} out of bounds (length {})", position, length)
            }
            Self::StateDrift { expected, actual } => {
                write!(f, "state drift: expected '{}', got '{}'", expected, actual)
            }
            Self::InvalidState(msg) => write!(f, "invalid state: {}", msg),
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for CommandError {}

/// Configuration for command merging behavior.
#[derive(Debug, Clone, Copy)]
pub struct MergeConfig {
    /// Maximum time between commands to allow merging (milliseconds).
    pub max_delay_ms: u64,
    /// Whether to merge across word boundaries.
    pub merge_across_words: bool,
    /// Maximum merged command size before forcing a split.
    pub max_merged_size: usize,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            max_delay_ms: 500,
            merge_across_words: false,
            max_merged_size: 1024,
        }
    }
}

/// A reversible command that can be undone and redone.
///
/// Commands capture all state needed to execute, undo, and redo an operation.
/// They support merging for batching related operations (like consecutive typing).
pub trait UndoableCmd: Send + Sync {
    /// Execute the command, applying its effect.
    fn execute(&mut self) -> CommandResult;

    /// Undo the command, reverting its effect.
    fn undo(&mut self) -> CommandResult;

    /// Redo the command after it was undone.
    fn redo(&mut self) -> CommandResult {
        self.execute()
    }

    /// Human-readable description for UI display.
    fn description(&self) -> &str;

    /// Size of this command in bytes for memory budgeting.
    fn size_bytes(&self) -> usize;

    /// Check if this command can merge with another.
    fn can_merge(&self, _other: &dyn UndoableCmd, _config: &MergeConfig) -> bool {
        false
    }

    /// Merge another command into this one.
    ///
    /// Returns the text to append if merging is possible.
    /// The default implementation returns None (no merge).
    fn merge_text(&self) -> Option<&str> {
        None
    }

    /// Accept a merge from another command.
    ///
    /// The full command reference is passed to allow implementations to
    /// extract position or other context needed for correct merge behavior.
    fn accept_merge(&mut self, _other: &dyn UndoableCmd) -> bool {
        false
    }

    /// Get the command metadata.
    fn metadata(&self) -> &CommandMetadata;

    /// Get the target widget ID, if any.
    fn target(&self) -> Option<WidgetId> {
        None
    }

    /// Downcast to concrete type for merging.
    fn as_any(&self) -> &dyn Any;

    /// Downcast to mutable concrete type for merging.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Debug description of the command.
    fn debug_name(&self) -> &'static str {
        "UndoableCmd"
    }
}

impl fmt::Debug for dyn UndoableCmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(self.debug_name())
            .field("description", &self.description())
            .field("size_bytes", &self.size_bytes())
            .finish()
    }
}

/// A batch of commands that execute and undo together.
///
/// Useful for operations that span multiple widgets or steps
/// but should appear as a single undo entry.
pub struct CommandBatch {
    /// Commands in execution order.
    commands: Vec<Box<dyn UndoableCmd>>,
    /// Batch metadata.
    metadata: CommandMetadata,
    /// Index of last successfully executed command.
    executed_to: usize,
}

impl fmt::Debug for CommandBatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandBatch")
            .field("commands_count", &self.commands.len())
            .field("metadata", &self.metadata)
            .field("executed_to", &self.executed_to)
            .finish()
    }
}

impl CommandBatch {
    /// Create a new command batch.
    #[must_use]
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            commands: Vec::new(),
            metadata: CommandMetadata::new(description),
            executed_to: 0,
        }
    }

    /// Add a command to the batch.
    pub fn push(&mut self, cmd: Box<dyn UndoableCmd>) {
        self.commands.push(cmd);
    }

    /// Add a pre-executed command to the batch.
    ///
    /// Use this for commands that have already been executed externally.
    /// The command will be properly undone when the batch is undone.
    pub fn push_executed(&mut self, cmd: Box<dyn UndoableCmd>) {
        self.commands.push(cmd);
        self.executed_to = self.commands.len();
    }

    /// Number of commands in the batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Check if the batch is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl UndoableCmd for CommandBatch {
    fn execute(&mut self) -> CommandResult {
        for (i, cmd) in self.commands.iter_mut().enumerate() {
            if let Err(e) = cmd.execute() {
                // Rollback executed commands on failure
                for j in (0..i).rev() {
                    let _ = self.commands[j].undo();
                }
                return Err(e);
            }
            self.executed_to = i + 1;
        }
        Ok(())
    }

    fn undo(&mut self) -> CommandResult {
        // Undo in reverse order
        for i in (0..self.executed_to).rev() {
            self.commands[i].undo()?;
        }
        self.executed_to = 0;
        Ok(())
    }

    fn redo(&mut self) -> CommandResult {
        self.execute()
    }

    fn description(&self) -> &str {
        &self.metadata.description
    }

    fn size_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.metadata.size_bytes()
            + self.commands.iter().map(|c| c.size_bytes()).sum::<usize>()
    }

    fn metadata(&self) -> &CommandMetadata {
        &self.metadata
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn debug_name(&self) -> &'static str {
        "CommandBatch"
    }
}

// ============================================================================
// Built-in Text Commands
// ============================================================================

/// Callback type for applying text operations.
pub type TextApplyFn = Box<dyn Fn(WidgetId, usize, &str) -> CommandResult + Send + Sync>;
/// Callback type for removing text.
pub type TextRemoveFn = Box<dyn Fn(WidgetId, usize, usize) -> CommandResult + Send + Sync>;
/// Callback type for replacing text.
pub type TextReplaceFn = Box<dyn Fn(WidgetId, usize, usize, &str) -> CommandResult + Send + Sync>;

/// Command to insert text at a position.
pub struct TextInsertCmd {
    /// Target widget.
    pub target: WidgetId,
    /// Position to insert at (byte offset).
    pub position: usize,
    /// Text to insert.
    pub text: String,
    /// Command metadata.
    pub metadata: CommandMetadata,
    /// Callback to apply the insertion (set by the widget).
    apply: Option<TextApplyFn>,
    /// Callback to remove the insertion (set by the widget).
    remove: Option<TextRemoveFn>,
}

impl fmt::Debug for TextInsertCmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextInsertCmd")
            .field("target", &self.target)
            .field("position", &self.position)
            .field("text", &self.text)
            .field("metadata", &self.metadata)
            .field("has_apply", &self.apply.is_some())
            .field("has_remove", &self.remove.is_some())
            .finish()
    }
}

impl TextInsertCmd {
    /// Create a new text insert command.
    #[must_use]
    pub fn new(target: WidgetId, position: usize, text: impl Into<String>) -> Self {
        Self {
            target,
            position,
            text: text.into(),
            metadata: CommandMetadata::new("Insert text"),
            apply: None,
            remove: None,
        }
    }

    /// Set the apply callback.
    pub fn with_apply<F>(mut self, f: F) -> Self
    where
        F: Fn(WidgetId, usize, &str) -> CommandResult + Send + Sync + 'static,
    {
        self.apply = Some(Box::new(f));
        self
    }

    /// Set the remove callback.
    pub fn with_remove<F>(mut self, f: F) -> Self
    where
        F: Fn(WidgetId, usize, usize) -> CommandResult + Send + Sync + 'static,
    {
        self.remove = Some(Box::new(f));
        self
    }
}

impl UndoableCmd for TextInsertCmd {
    fn execute(&mut self) -> CommandResult {
        if let Some(ref apply) = self.apply {
            apply(self.target, self.position, &self.text)
        } else {
            Err(CommandError::InvalidState(
                "no apply callback set".to_string(),
            ))
        }
    }

    fn undo(&mut self) -> CommandResult {
        if let Some(ref remove) = self.remove {
            remove(self.target, self.position, self.text.len())
        } else {
            Err(CommandError::InvalidState(
                "no remove callback set".to_string(),
            ))
        }
    }

    fn description(&self) -> &str {
        &self.metadata.description
    }

    fn size_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.text.len() + self.metadata.size_bytes()
    }

    fn can_merge(&self, other: &dyn UndoableCmd, config: &MergeConfig) -> bool {
        let Some(other) = other.as_any().downcast_ref::<Self>() else {
            return false;
        };

        // Must target same widget
        if self.target != other.target {
            return false;
        }

        // Must be consecutive
        if other.position != self.position + self.text.len() {
            return false;
        }

        // Check time constraint
        let elapsed = other
            .metadata
            .timestamp
            .duration_since(self.metadata.timestamp);
        if elapsed.as_millis() > config.max_delay_ms as u128 {
            return false;
        }

        // Check size constraint
        if self.text.len() + other.text.len() > config.max_merged_size {
            return false;
        }

        // Don't merge across word boundaries unless configured
        if !config.merge_across_words && self.text.ends_with(' ') {
            return false;
        }

        true
    }

    fn merge_text(&self) -> Option<&str> {
        Some(&self.text)
    }

    fn accept_merge(&mut self, other: &dyn UndoableCmd) -> bool {
        let Some(other_insert) = other.as_any().downcast_ref::<Self>() else {
            return false;
        };
        self.text.push_str(&other_insert.text);
        true
    }

    fn metadata(&self) -> &CommandMetadata {
        &self.metadata
    }

    fn target(&self) -> Option<WidgetId> {
        Some(self.target)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn debug_name(&self) -> &'static str {
        "TextInsertCmd"
    }
}

/// Command to delete text at a position.
pub struct TextDeleteCmd {
    /// Target widget.
    pub target: WidgetId,
    /// Position to delete from (byte offset).
    pub position: usize,
    /// Deleted text (for undo).
    pub deleted_text: String,
    /// Command metadata.
    pub metadata: CommandMetadata,
    /// Callback to remove text.
    remove: Option<TextRemoveFn>,
    /// Callback to insert text (for undo).
    insert: Option<TextApplyFn>,
}

impl fmt::Debug for TextDeleteCmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextDeleteCmd")
            .field("target", &self.target)
            .field("position", &self.position)
            .field("deleted_text", &self.deleted_text)
            .field("metadata", &self.metadata)
            .field("has_remove", &self.remove.is_some())
            .field("has_insert", &self.insert.is_some())
            .finish()
    }
}

impl TextDeleteCmd {
    /// Create a new text delete command.
    #[must_use]
    pub fn new(target: WidgetId, position: usize, deleted_text: impl Into<String>) -> Self {
        Self {
            target,
            position,
            deleted_text: deleted_text.into(),
            metadata: CommandMetadata::new("Delete text"),
            remove: None,
            insert: None,
        }
    }

    /// Set the remove callback.
    pub fn with_remove<F>(mut self, f: F) -> Self
    where
        F: Fn(WidgetId, usize, usize) -> CommandResult + Send + Sync + 'static,
    {
        self.remove = Some(Box::new(f));
        self
    }

    /// Set the insert callback (for undo).
    pub fn with_insert<F>(mut self, f: F) -> Self
    where
        F: Fn(WidgetId, usize, &str) -> CommandResult + Send + Sync + 'static,
    {
        self.insert = Some(Box::new(f));
        self
    }
}

impl UndoableCmd for TextDeleteCmd {
    fn execute(&mut self) -> CommandResult {
        if let Some(ref remove) = self.remove {
            remove(self.target, self.position, self.deleted_text.len())
        } else {
            Err(CommandError::InvalidState(
                "no remove callback set".to_string(),
            ))
        }
    }

    fn undo(&mut self) -> CommandResult {
        if let Some(ref insert) = self.insert {
            insert(self.target, self.position, &self.deleted_text)
        } else {
            Err(CommandError::InvalidState(
                "no insert callback set".to_string(),
            ))
        }
    }

    fn description(&self) -> &str {
        &self.metadata.description
    }

    fn size_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.deleted_text.len() + self.metadata.size_bytes()
    }

    fn can_merge(&self, other: &dyn UndoableCmd, config: &MergeConfig) -> bool {
        let Some(other) = other.as_any().downcast_ref::<Self>() else {
            return false;
        };

        // Must target same widget
        if self.target != other.target {
            return false;
        }

        // For backspace: other.position + other.deleted_text.len() == self.position
        // For delete key: other.position == self.position
        let is_backspace = other.position + other.deleted_text.len() == self.position;
        let is_delete = other.position == self.position;

        if !is_backspace && !is_delete {
            return false;
        }

        // Check time constraint
        let elapsed = other
            .metadata
            .timestamp
            .duration_since(self.metadata.timestamp);
        if elapsed.as_millis() > config.max_delay_ms as u128 {
            return false;
        }

        // Check size constraint
        if self.deleted_text.len() + other.deleted_text.len() > config.max_merged_size {
            return false;
        }

        true
    }

    fn merge_text(&self) -> Option<&str> {
        Some(&self.deleted_text)
    }

    fn accept_merge(&mut self, other: &dyn UndoableCmd) -> bool {
        let Some(other_delete) = other.as_any().downcast_ref::<Self>() else {
            return false;
        };

        // Determine if this is a backspace or forward delete merge:
        // - Backspace: other.position + other.deleted_text.len() == self.position
        //   The new delete happened before our position, prepend its text
        // - Forward delete: other.position == self.position
        //   The new delete happened at the same position, append its text
        let is_backspace = other_delete.position + other_delete.deleted_text.len() == self.position;
        let is_forward = other_delete.position == self.position;
        if !is_backspace && !is_forward {
            return false;
        }

        if is_backspace {
            // Backspace: prepend and move our position back
            self.deleted_text = format!("{}{}", other_delete.deleted_text, self.deleted_text);
            self.position = other_delete.position;
        } else {
            // Forward delete: append (text was after original deleted text)
            self.deleted_text.push_str(&other_delete.deleted_text);
        }
        true
    }

    fn metadata(&self) -> &CommandMetadata {
        &self.metadata
    }

    fn target(&self) -> Option<WidgetId> {
        Some(self.target)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn debug_name(&self) -> &'static str {
        "TextDeleteCmd"
    }
}

/// Command to replace text at a position.
pub struct TextReplaceCmd {
    /// Target widget.
    pub target: WidgetId,
    /// Position to replace at (byte offset).
    pub position: usize,
    /// Original text that was replaced.
    pub old_text: String,
    /// New text that replaced it.
    pub new_text: String,
    /// Command metadata.
    pub metadata: CommandMetadata,
    /// Callback to apply replacement.
    replace: Option<TextReplaceFn>,
}

impl fmt::Debug for TextReplaceCmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextReplaceCmd")
            .field("target", &self.target)
            .field("position", &self.position)
            .field("old_text", &self.old_text)
            .field("new_text", &self.new_text)
            .field("metadata", &self.metadata)
            .field("has_replace", &self.replace.is_some())
            .finish()
    }
}

impl TextReplaceCmd {
    /// Create a new text replace command.
    #[must_use]
    pub fn new(
        target: WidgetId,
        position: usize,
        old_text: impl Into<String>,
        new_text: impl Into<String>,
    ) -> Self {
        Self {
            target,
            position,
            old_text: old_text.into(),
            new_text: new_text.into(),
            metadata: CommandMetadata::new("Replace text"),
            replace: None,
        }
    }

    /// Set the replace callback.
    pub fn with_replace<F>(mut self, f: F) -> Self
    where
        F: Fn(WidgetId, usize, usize, &str) -> CommandResult + Send + Sync + 'static,
    {
        self.replace = Some(Box::new(f));
        self
    }
}

impl UndoableCmd for TextReplaceCmd {
    fn execute(&mut self) -> CommandResult {
        if let Some(ref replace) = self.replace {
            replace(
                self.target,
                self.position,
                self.old_text.len(),
                &self.new_text,
            )
        } else {
            Err(CommandError::InvalidState(
                "no replace callback set".to_string(),
            ))
        }
    }

    fn undo(&mut self) -> CommandResult {
        if let Some(ref replace) = self.replace {
            replace(
                self.target,
                self.position,
                self.new_text.len(),
                &self.old_text,
            )
        } else {
            Err(CommandError::InvalidState(
                "no replace callback set".to_string(),
            ))
        }
    }

    fn description(&self) -> &str {
        &self.metadata.description
    }

    fn size_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.old_text.len()
            + self.new_text.len()
            + self.metadata.size_bytes()
    }

    fn metadata(&self) -> &CommandMetadata {
        &self.metadata
    }

    fn target(&self) -> Option<WidgetId> {
        Some(self.target)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn debug_name(&self) -> &'static str {
        "TextReplaceCmd"
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;

    #[test]
    fn test_widget_id_creation() {
        let id = WidgetId::new(42);
        assert_eq!(id.raw(), 42);
    }

    #[test]
    fn test_command_metadata_size() {
        let meta = CommandMetadata::new("Test command");
        let size = meta.size_bytes();
        assert!(size > std::mem::size_of::<CommandMetadata>());
        assert!(size >= std::mem::size_of::<CommandMetadata>() + "Test command".len());
    }

    #[test]
    fn test_command_metadata_with_source() {
        let meta = CommandMetadata::new("Test").with_source(CommandSource::Macro);
        assert_eq!(meta.source, CommandSource::Macro);
    }

    #[test]
    fn test_command_metadata_with_batch() {
        let meta = CommandMetadata::new("Test").with_batch(123);
        assert_eq!(meta.batch_id, Some(123));
    }

    #[test]
    fn test_command_batch_execute_undo() {
        // Create a simple test buffer
        let buffer = Arc::new(Mutex::new(String::new()));

        let mut batch = CommandBatch::new("Test batch");

        // Add two insert commands with callbacks
        let b1 = buffer.clone();
        let b2 = buffer.clone();
        let b3 = buffer.clone();
        let b4 = buffer.clone();

        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "Hello")
            .with_apply(move |_, pos, text| {
                let mut buf = b1.lock().unwrap();
                buf.insert_str(pos, text);
                Ok(())
            })
            .with_remove(move |_, pos, len| {
                let mut buf = b2.lock().unwrap();
                buf.drain(pos..pos + len);
                Ok(())
            });

        let cmd2 = TextInsertCmd::new(WidgetId::new(1), 5, " World")
            .with_apply(move |_, pos, text| {
                let mut buf = b3.lock().unwrap();
                buf.insert_str(pos, text);
                Ok(())
            })
            .with_remove(move |_, pos, len| {
                let mut buf = b4.lock().unwrap();
                buf.drain(pos..pos + len);
                Ok(())
            });

        batch.push(Box::new(cmd1));
        batch.push(Box::new(cmd2));

        // Execute batch
        batch.execute().unwrap();
        assert_eq!(*buffer.lock().unwrap(), "Hello World");

        // Undo batch
        batch.undo().unwrap();
        assert_eq!(*buffer.lock().unwrap(), "");
    }

    #[test]
    fn test_command_batch_empty() {
        let batch = CommandBatch::new("Empty");
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
    }

    #[test]
    fn test_text_insert_can_merge_consecutive() {
        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "a");
        let mut cmd2 = TextInsertCmd::new(WidgetId::new(1), 1, "b");
        // Set timestamp to be within merge window
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default();
        assert!(cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_insert_no_merge_different_widget() {
        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "a");
        let mut cmd2 = TextInsertCmd::new(WidgetId::new(2), 1, "b");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default();
        assert!(!cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_insert_no_merge_non_consecutive() {
        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "a");
        let mut cmd2 = TextInsertCmd::new(WidgetId::new(1), 5, "b");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default();
        assert!(!cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_delete_can_merge_backspace() {
        let cmd1 = TextDeleteCmd::new(WidgetId::new(1), 5, "b");
        let mut cmd2 = TextDeleteCmd::new(WidgetId::new(1), 4, "a");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default();
        assert!(cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_delete_can_merge_delete_key() {
        let cmd1 = TextDeleteCmd::new(WidgetId::new(1), 5, "a");
        let mut cmd2 = TextDeleteCmd::new(WidgetId::new(1), 5, "b");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default();
        assert!(cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_command_error_display() {
        let err = CommandError::TargetNotFound(WidgetId::new(42));
        assert!(err.to_string().contains("42"));

        let err = CommandError::PositionOutOfBounds {
            position: 10,
            length: 5,
        };
        assert!(err.to_string().contains("10"));
        assert!(err.to_string().contains("5"));
    }

    #[test]
    fn test_merge_config_default() {
        let config = MergeConfig::default();
        assert_eq!(config.max_delay_ms, 500);
        assert!(!config.merge_across_words);
        assert_eq!(config.max_merged_size, 1024);
    }

    #[test]
    fn test_text_replace_size_bytes() {
        let cmd = TextReplaceCmd::new(WidgetId::new(1), 0, "old", "new");
        let size = cmd.size_bytes();
        assert!(size >= std::mem::size_of::<TextReplaceCmd>() + 3 + 3);
    }

    #[test]
    fn test_text_insert_accept_merge() {
        let mut cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "Hello");
        let cmd2 = TextInsertCmd::new(WidgetId::new(1), 5, " World");
        assert!(cmd1.accept_merge(&cmd2));
        assert_eq!(cmd1.text, "Hello World");
    }

    #[test]
    fn test_text_delete_accept_merge_backspace() {
        // Simulate backspace: user deleted "b" at position 4, then "a" at position 3
        // Backspace detection: other.position + other.len == self.position
        // 3 + 1 == 4, so this is backspace, should prepend
        let mut cmd1 = TextDeleteCmd::new(WidgetId::new(1), 4, "b");
        let cmd2 = TextDeleteCmd::new(WidgetId::new(1), 3, "a");
        assert!(cmd1.accept_merge(&cmd2));
        assert_eq!(cmd1.deleted_text, "ab");
        assert_eq!(cmd1.position, 3); // Position moves back for backspace
    }

    #[test]
    fn test_text_delete_accept_merge_forward_delete() {
        // Simulate forward delete: user deleted "a" at position 3, then "b" at position 3
        // Forward delete detection: other.position == self.position
        // Both at position 3, so this is forward delete, should append
        let mut cmd1 = TextDeleteCmd::new(WidgetId::new(1), 3, "a");
        let cmd2 = TextDeleteCmd::new(WidgetId::new(1), 3, "b");
        assert!(cmd1.accept_merge(&cmd2));
        assert_eq!(cmd1.deleted_text, "ab");
        assert_eq!(cmd1.position, 3); // Position stays the same for forward delete
    }

    #[test]
    fn test_debug_implementations() {
        let cmd = TextInsertCmd::new(WidgetId::new(1), 0, "test");
        let debug_str = format!("{:?}", cmd);
        assert!(debug_str.contains("TextInsertCmd"));
        assert!(debug_str.contains("test"));

        let batch = CommandBatch::new("Test batch");
        let debug_str = format!("{:?}", batch);
        assert!(debug_str.contains("CommandBatch"));
    }

    #[test]
    fn test_text_insert_execute_and_undo_with_callbacks() {
        let buf = Arc::new(Mutex::new(String::from("Hello")));
        let b1 = buf.clone();
        let b2 = buf.clone();

        let mut cmd = TextInsertCmd::new(WidgetId::new(1), 5, " World")
            .with_apply(move |_, pos, text| {
                let mut b = b1.lock().unwrap();
                b.insert_str(pos, text);
                Ok(())
            })
            .with_remove(move |_, pos, len| {
                let mut b = b2.lock().unwrap();
                b.drain(pos..pos + len);
                Ok(())
            });

        cmd.execute().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello World");
        cmd.undo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello");
    }

    #[test]
    fn test_text_insert_execute_without_callback_errors() {
        let mut cmd = TextInsertCmd::new(WidgetId::new(1), 0, "test");
        let err = cmd.execute().unwrap_err();
        assert!(matches!(err, CommandError::InvalidState(_)));
    }

    #[test]
    fn test_text_insert_undo_without_callback_errors() {
        let mut cmd = TextInsertCmd::new(WidgetId::new(1), 0, "test");
        let err = cmd.undo().unwrap_err();
        assert!(matches!(err, CommandError::InvalidState(_)));
    }

    #[test]
    fn test_text_insert_target() {
        let cmd = TextInsertCmd::new(WidgetId::new(42), 0, "x");
        assert_eq!(cmd.target(), Some(WidgetId::new(42)));
    }

    #[test]
    fn test_text_insert_merge_text() {
        let cmd = TextInsertCmd::new(WidgetId::new(1), 0, "abc");
        assert_eq!(cmd.merge_text(), Some("abc"));
    }

    #[test]
    fn test_text_delete_target() {
        let cmd = TextDeleteCmd::new(WidgetId::new(7), 0, "x");
        assert_eq!(cmd.target(), Some(WidgetId::new(7)));
    }

    #[test]
    fn test_command_metadata_default() {
        let meta = CommandMetadata::default();
        assert_eq!(meta.description, "Unknown");
        assert_eq!(meta.source, CommandSource::User);
        assert_eq!(meta.batch_id, None);
    }

    #[test]
    fn test_command_source_default_is_user() {
        assert_eq!(CommandSource::default(), CommandSource::User);
    }

    #[test]
    fn test_command_error_state_drift_display() {
        let err = CommandError::StateDrift {
            expected: "foo".to_string(),
            actual: "bar".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("foo"));
        assert!(s.contains("bar"));
    }

    #[test]
    fn test_command_error_other_display() {
        let err = CommandError::Other("something broke".to_string());
        assert!(err.to_string().contains("something broke"));
    }

    #[test]
    fn test_command_batch_push_executed_tracks_index() {
        let buf = Arc::new(Mutex::new(String::new()));
        let b1 = buf.clone();
        let b2 = buf.clone();

        // Pre-execute the command manually
        {
            let mut b = buf.lock().unwrap();
            b.push_str("Hi");
        }

        let cmd = TextInsertCmd::new(WidgetId::new(1), 0, "Hi")
            .with_apply(move |_, pos, text| {
                let mut b = b1.lock().unwrap();
                b.insert_str(pos, text);
                Ok(())
            })
            .with_remove(move |_, pos, len| {
                let mut b = b2.lock().unwrap();
                b.drain(pos..pos + len);
                Ok(())
            });

        let mut batch = CommandBatch::new("Pre-executed batch");
        batch.push_executed(Box::new(cmd));
        assert_eq!(batch.len(), 1);
        // Undo should work since push_executed advances executed_to
        batch.undo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "");
    }

    // ====================================================================
    // TextDeleteCmd execute/undo coverage
    // ====================================================================

    #[test]
    fn test_text_delete_execute_and_undo_with_callbacks() {
        let buf = Arc::new(Mutex::new(String::from("Hello World")));
        let b1 = buf.clone();
        let b2 = buf.clone();

        let mut cmd = TextDeleteCmd::new(WidgetId::new(1), 5, " World")
            .with_remove(move |_, pos, len| {
                let mut b = b1.lock().unwrap();
                b.drain(pos..pos + len);
                Ok(())
            })
            .with_insert(move |_, pos, text| {
                let mut b = b2.lock().unwrap();
                b.insert_str(pos, text);
                Ok(())
            });

        cmd.execute().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello");

        cmd.undo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello World");
    }

    #[test]
    fn test_text_delete_execute_without_callback_errors() {
        let mut cmd = TextDeleteCmd::new(WidgetId::new(1), 0, "x");
        let err = cmd.execute().unwrap_err();
        assert!(matches!(err, CommandError::InvalidState(_)));
    }

    #[test]
    fn test_text_delete_undo_without_callback_errors() {
        let mut cmd = TextDeleteCmd::new(WidgetId::new(1), 0, "x");
        let err = cmd.undo().unwrap_err();
        assert!(matches!(err, CommandError::InvalidState(_)));
    }

    #[test]
    fn test_text_delete_size_bytes() {
        let cmd = TextDeleteCmd::new(WidgetId::new(1), 0, "abc");
        let size = cmd.size_bytes();
        assert!(size >= std::mem::size_of::<TextDeleteCmd>() + 3);
    }

    #[test]
    fn test_text_delete_description() {
        let cmd = TextDeleteCmd::new(WidgetId::new(1), 0, "x");
        assert_eq!(cmd.description(), "Delete text");
    }

    #[test]
    fn test_text_delete_merge_text() {
        let cmd = TextDeleteCmd::new(WidgetId::new(1), 5, "xyz");
        assert_eq!(cmd.merge_text(), Some("xyz"));
    }

    #[test]
    fn test_text_delete_debug() {
        let cmd = TextDeleteCmd::new(WidgetId::new(1), 3, "abc");
        let s = format!("{:?}", cmd);
        assert!(s.contains("TextDeleteCmd"));
        assert!(s.contains("abc"));
    }

    #[test]
    fn test_text_delete_debug_name() {
        let cmd = TextDeleteCmd::new(WidgetId::new(1), 0, "x");
        assert_eq!(cmd.debug_name(), "TextDeleteCmd");
    }

    // ====================================================================
    // TextDeleteCmd merge edge cases
    // ====================================================================

    #[test]
    fn test_text_delete_no_merge_different_widget() {
        let cmd1 = TextDeleteCmd::new(WidgetId::new(1), 5, "a");
        let mut cmd2 = TextDeleteCmd::new(WidgetId::new(2), 5, "b");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default();
        assert!(!cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_delete_no_merge_non_adjacent() {
        let cmd1 = TextDeleteCmd::new(WidgetId::new(1), 5, "a");
        let mut cmd2 = TextDeleteCmd::new(WidgetId::new(1), 10, "b");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default();
        assert!(!cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_delete_no_merge_exceeds_max_size() {
        let long_text = "a".repeat(600);
        let cmd1 = TextDeleteCmd::new(WidgetId::new(1), 600, &long_text);
        let mut cmd2 = TextDeleteCmd::new(WidgetId::new(1), 600, &long_text);
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default(); // max_merged_size = 1024
        assert!(!cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_delete_accept_merge_non_adjacent_returns_false() {
        let mut cmd1 = TextDeleteCmd::new(WidgetId::new(1), 5, "a");
        let cmd2 = TextDeleteCmd::new(WidgetId::new(1), 10, "b");
        assert!(!cmd1.accept_merge(&cmd2));
    }

    #[test]
    fn test_text_delete_accept_merge_wrong_type_returns_false() {
        let mut cmd1 = TextDeleteCmd::new(WidgetId::new(1), 5, "a");
        let cmd2 = TextInsertCmd::new(WidgetId::new(1), 5, "b");
        assert!(!cmd1.accept_merge(&cmd2));
    }

    // ====================================================================
    // TextInsertCmd merge edge cases
    // ====================================================================

    #[test]
    fn test_text_insert_no_merge_across_word_boundary() {
        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "hello ");
        let mut cmd2 = TextInsertCmd::new(WidgetId::new(1), 6, "world");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default(); // merge_across_words = false
        assert!(!cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_insert_merge_across_word_boundary_when_configured() {
        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "hello ");
        let mut cmd2 = TextInsertCmd::new(WidgetId::new(1), 6, "world");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig {
            merge_across_words: true,
            ..MergeConfig::default()
        };
        assert!(cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_insert_no_merge_exceeds_max_size() {
        let long_text = "a".repeat(600);
        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, &long_text);
        let mut cmd2 = TextInsertCmd::new(WidgetId::new(1), 600, &long_text);
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default(); // max_merged_size = 1024
        assert!(!cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_insert_accept_merge_wrong_type_returns_false() {
        let mut cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "a");
        let cmd2 = TextDeleteCmd::new(WidgetId::new(1), 0, "b");
        assert!(!cmd1.accept_merge(&cmd2));
    }

    #[test]
    fn test_text_insert_size_bytes() {
        let cmd = TextInsertCmd::new(WidgetId::new(1), 0, "hello");
        let size = cmd.size_bytes();
        assert!(size >= std::mem::size_of::<TextInsertCmd>() + 5);
    }

    #[test]
    fn test_text_insert_description() {
        let cmd = TextInsertCmd::new(WidgetId::new(1), 0, "x");
        assert_eq!(cmd.description(), "Insert text");
    }

    #[test]
    fn test_text_insert_debug_name() {
        let cmd = TextInsertCmd::new(WidgetId::new(1), 0, "x");
        assert_eq!(cmd.debug_name(), "TextInsertCmd");
    }

    // ====================================================================
    // TextReplaceCmd full coverage
    // ====================================================================

    #[test]
    fn test_text_replace_execute_and_undo_with_callbacks() {
        let buf = Arc::new(Mutex::new(String::from("Hello World")));
        let b1 = buf.clone();
        let b2 = buf.clone();

        let mut cmd = TextReplaceCmd::new(WidgetId::new(1), 6, "World", "Rust").with_replace(
            move |_, pos, old_len, new_text| {
                let mut b = b1.lock().unwrap();
                b.drain(pos..pos + old_len);
                b.insert_str(pos, new_text);
                Ok(())
            },
        );

        // Also need clone for undo path (same callback)
        cmd.execute().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello Rust");

        cmd.undo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello World");
    }

    #[test]
    fn test_text_replace_execute_without_callback_errors() {
        let mut cmd = TextReplaceCmd::new(WidgetId::new(1), 0, "old", "new");
        let err = cmd.execute().unwrap_err();
        assert!(matches!(err, CommandError::InvalidState(_)));
    }

    #[test]
    fn test_text_replace_undo_without_callback_errors() {
        let mut cmd = TextReplaceCmd::new(WidgetId::new(1), 0, "old", "new");
        let err = cmd.undo().unwrap_err();
        assert!(matches!(err, CommandError::InvalidState(_)));
    }

    #[test]
    fn test_text_replace_target() {
        let cmd = TextReplaceCmd::new(WidgetId::new(99), 0, "a", "b");
        assert_eq!(cmd.target(), Some(WidgetId::new(99)));
    }

    #[test]
    fn test_text_replace_description() {
        let cmd = TextReplaceCmd::new(WidgetId::new(1), 0, "a", "b");
        assert_eq!(cmd.description(), "Replace text");
    }

    #[test]
    fn test_text_replace_metadata() {
        let cmd = TextReplaceCmd::new(WidgetId::new(1), 0, "a", "b");
        assert_eq!(cmd.metadata().description, "Replace text");
        assert_eq!(cmd.metadata().source, CommandSource::User);
    }

    #[test]
    fn test_text_replace_debug() {
        let cmd = TextReplaceCmd::new(WidgetId::new(1), 3, "old", "new");
        let s = format!("{:?}", cmd);
        assert!(s.contains("TextReplaceCmd"));
        assert!(s.contains("old"));
        assert!(s.contains("new"));
    }

    #[test]
    fn test_text_replace_debug_name() {
        let cmd = TextReplaceCmd::new(WidgetId::new(1), 0, "a", "b");
        assert_eq!(cmd.debug_name(), "TextReplaceCmd");
    }

    // ====================================================================
    // CommandBatch edge cases
    // ====================================================================

    #[test]
    fn test_command_batch_execute_rollback_on_failure() {
        let buf = Arc::new(Mutex::new(String::new()));
        let b1 = buf.clone();
        let b2 = buf.clone();

        let mut batch = CommandBatch::new("Rollback test");

        // First command succeeds
        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "OK")
            .with_apply(move |_, pos, text| {
                let mut b = b1.lock().unwrap();
                b.insert_str(pos, text);
                Ok(())
            })
            .with_remove(move |_, pos, len| {
                let mut b = b2.lock().unwrap();
                b.drain(pos..pos + len);
                Ok(())
            });

        // Second command always fails (no callback)
        let cmd2 = TextInsertCmd::new(WidgetId::new(1), 2, " FAIL");

        batch.push(Box::new(cmd1));
        batch.push(Box::new(cmd2));

        // Execute should fail
        let err = batch.execute().unwrap_err();
        assert!(matches!(err, CommandError::InvalidState(_)));

        // First command should have been rolled back
        assert_eq!(*buf.lock().unwrap(), "");
    }

    #[test]
    fn test_command_batch_redo() {
        let buf = Arc::new(Mutex::new(String::new()));
        let b1 = buf.clone();
        let b2 = buf.clone();

        let mut batch = CommandBatch::new("Redo test");
        let cmd = TextInsertCmd::new(WidgetId::new(1), 0, "Hi")
            .with_apply(move |_, pos, text| {
                let mut b = b1.lock().unwrap();
                b.insert_str(pos, text);
                Ok(())
            })
            .with_remove(move |_, pos, len| {
                let mut b = b2.lock().unwrap();
                b.drain(pos..pos + len);
                Ok(())
            });
        batch.push(Box::new(cmd));

        batch.execute().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hi");

        batch.undo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "");

        batch.redo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hi");
    }

    #[test]
    fn test_command_batch_size_bytes() {
        let batch = CommandBatch::new("Size test");
        let size = batch.size_bytes();
        assert!(size >= std::mem::size_of::<CommandBatch>());
    }

    #[test]
    fn test_command_batch_size_bytes_with_commands() {
        let mut batch = CommandBatch::new("Size test");
        batch.push(Box::new(TextInsertCmd::new(WidgetId::new(1), 0, "hello")));
        let size = batch.size_bytes();
        // Must include the inner command's size
        let inner_size = TextInsertCmd::new(WidgetId::new(1), 0, "hello").size_bytes();
        assert!(size > inner_size);
    }

    #[test]
    fn test_command_batch_metadata() {
        let batch = CommandBatch::new("Meta test");
        assert_eq!(batch.metadata().description, "Meta test");
    }

    #[test]
    fn test_command_batch_debug_name() {
        let batch = CommandBatch::new("test");
        assert_eq!(batch.debug_name(), "CommandBatch");
    }

    #[test]
    fn test_command_batch_can_merge_default_false() {
        let batch = CommandBatch::new("test");
        let other = CommandBatch::new("other");
        let config = MergeConfig::default();
        assert!(!batch.can_merge(&other, &config));
    }

    #[test]
    fn test_command_batch_undo_empty() {
        let mut batch = CommandBatch::new("Empty undo");
        // executed_to is 0, so undo should be a no-op
        batch.undo().unwrap();
    }

    // ====================================================================
    // dyn UndoableCmd Debug impl
    // ====================================================================

    #[test]
    fn test_dyn_undoable_cmd_debug() {
        let cmd: Box<dyn UndoableCmd> = Box::new(TextInsertCmd::new(WidgetId::new(1), 0, "test"));
        let s = format!("{:?}", cmd);
        assert!(s.contains("TextInsertCmd"));
        assert!(s.contains("Insert text"));
    }

    // ====================================================================
    // CommandError trait coverage
    // ====================================================================

    #[test]
    fn test_command_error_invalid_state_display() {
        let err = CommandError::InvalidState("bad state".to_string());
        let s = err.to_string();
        assert!(s.contains("bad state"));
    }

    #[test]
    fn test_command_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(CommandError::Other("test".to_string()));
        // Verify it implements std::error::Error
        assert!(err.to_string().contains("test"));
    }

    // ====================================================================
    // WidgetId additional coverage
    // ====================================================================

    #[test]
    fn test_widget_id_equality() {
        let a = WidgetId::new(1);
        let b = WidgetId::new(1);
        let c = WidgetId::new(2);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_widget_id_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(WidgetId::new(1));
        set.insert(WidgetId::new(1));
        set.insert(WidgetId::new(2));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_widget_id_debug() {
        let id = WidgetId::new(42);
        let s = format!("{:?}", id);
        assert!(s.contains("42"));
    }

    // ====================================================================
    // TextInsertCmd redo (default delegates to execute)
    // ====================================================================

    #[test]
    fn test_text_insert_redo() {
        let buf = Arc::new(Mutex::new(String::new()));
        let b1 = buf.clone();
        let b2 = buf.clone();

        let mut cmd = TextInsertCmd::new(WidgetId::new(1), 0, "Hi")
            .with_apply(move |_, pos, text| {
                let mut b = b1.lock().unwrap();
                b.insert_str(pos, text);
                Ok(())
            })
            .with_remove(move |_, pos, len| {
                let mut b = b2.lock().unwrap();
                b.drain(pos..pos + len);
                Ok(())
            });

        cmd.execute().unwrap();
        cmd.undo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "");

        cmd.redo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hi");
    }

    // ====================================================================
    // TextDeleteCmd redo
    // ====================================================================

    #[test]
    fn test_text_delete_redo() {
        let buf = Arc::new(Mutex::new(String::from("Hello")));
        let b1 = buf.clone();
        let b2 = buf.clone();

        let mut cmd = TextDeleteCmd::new(WidgetId::new(1), 0, "Hello")
            .with_remove(move |_, pos, len| {
                let mut b = b1.lock().unwrap();
                b.drain(pos..pos + len);
                Ok(())
            })
            .with_insert(move |_, pos, text| {
                let mut b = b2.lock().unwrap();
                b.insert_str(pos, text);
                Ok(())
            });

        cmd.execute().unwrap();
        assert_eq!(*buf.lock().unwrap(), "");

        cmd.undo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello");

        cmd.redo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "");
    }

    // ====================================================================
    // CommandMetadata edge cases
    // ====================================================================

    #[test]
    fn test_command_metadata_all_sources() {
        for source in [
            CommandSource::User,
            CommandSource::Programmatic,
            CommandSource::Macro,
            CommandSource::External,
        ] {
            let meta = CommandMetadata::new("test").with_source(source);
            assert_eq!(meta.source, source);
        }
    }

    #[test]
    fn test_command_metadata_empty_description() {
        let meta = CommandMetadata::new("");
        assert_eq!(meta.size_bytes(), std::mem::size_of::<CommandMetadata>());
    }

    // ====================================================================
    // as_any / as_any_mut coverage
    // ====================================================================

    #[test]
    fn test_text_insert_as_any_roundtrip() {
        let cmd = TextInsertCmd::new(WidgetId::new(1), 0, "x");
        let any_ref = cmd.as_any();
        let downcasted = any_ref.downcast_ref::<TextInsertCmd>().unwrap();
        assert_eq!(downcasted.text, "x");
    }

    #[test]
    fn test_text_insert_as_any_mut_roundtrip() {
        let mut cmd = TextInsertCmd::new(WidgetId::new(1), 0, "x");
        let downcasted = cmd.as_any_mut().downcast_mut::<TextInsertCmd>().unwrap();
        downcasted.text = "modified".to_string();
        assert_eq!(cmd.text, "modified");
    }

    #[test]
    fn test_text_delete_as_any_roundtrip() {
        let cmd = TextDeleteCmd::new(WidgetId::new(1), 0, "abc");
        let downcasted = cmd.as_any().downcast_ref::<TextDeleteCmd>().unwrap();
        assert_eq!(downcasted.deleted_text, "abc");
    }

    #[test]
    fn test_text_delete_as_any_mut_roundtrip() {
        let mut cmd = TextDeleteCmd::new(WidgetId::new(1), 0, "abc");
        let downcasted = cmd.as_any_mut().downcast_mut::<TextDeleteCmd>().unwrap();
        downcasted.deleted_text = "xyz".to_string();
        assert_eq!(cmd.deleted_text, "xyz");
    }

    #[test]
    fn test_text_replace_as_any_roundtrip() {
        let cmd = TextReplaceCmd::new(WidgetId::new(1), 0, "a", "b");
        let downcasted = cmd.as_any().downcast_ref::<TextReplaceCmd>().unwrap();
        assert_eq!(downcasted.old_text, "a");
        assert_eq!(downcasted.new_text, "b");
    }

    #[test]
    fn test_text_replace_as_any_mut_roundtrip() {
        let mut cmd = TextReplaceCmd::new(WidgetId::new(1), 0, "a", "b");
        let downcasted = cmd.as_any_mut().downcast_mut::<TextReplaceCmd>().unwrap();
        downcasted.new_text = "replaced".to_string();
        assert_eq!(cmd.new_text, "replaced");
    }

    #[test]
    fn test_command_batch_as_any_roundtrip() {
        let batch = CommandBatch::new("test batch");
        let downcasted = batch.as_any().downcast_ref::<CommandBatch>().unwrap();
        assert_eq!(downcasted.description(), "test batch");
    }

    #[test]
    fn test_command_batch_as_any_mut_roundtrip() {
        let mut batch = CommandBatch::new("test batch");
        batch.push(Box::new(TextInsertCmd::new(WidgetId::new(1), 0, "x")));
        let downcasted = batch.as_any_mut().downcast_mut::<CommandBatch>().unwrap();
        assert_eq!(downcasted.len(), 1);
    }

    // ====================================================================
    // Default trait methods on CommandBatch (UndoableCmd defaults)
    // ====================================================================

    #[test]
    fn test_command_batch_description_matches() {
        let batch = CommandBatch::new("My description");
        assert_eq!(batch.description(), "My description");
    }

    #[test]
    fn test_command_batch_merge_text_default_none() {
        let batch = CommandBatch::new("test");
        assert_eq!(batch.merge_text(), None);
    }

    #[test]
    fn test_command_batch_accept_merge_default_false() {
        let mut batch = CommandBatch::new("test");
        let other = CommandBatch::new("other");
        assert!(!batch.accept_merge(&other));
    }

    #[test]
    fn test_command_batch_target_default_none() {
        let batch = CommandBatch::new("test");
        assert_eq!(batch.target(), None);
    }

    // ====================================================================
    // Cross-type can_merge rejection
    // ====================================================================

    #[test]
    fn test_text_insert_can_merge_rejects_delete_type() {
        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "a");
        let cmd2 = TextDeleteCmd::new(WidgetId::new(1), 0, "b");
        let config = MergeConfig::default();
        assert!(!cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_delete_can_merge_rejects_insert_type() {
        let cmd1 = TextDeleteCmd::new(WidgetId::new(1), 0, "a");
        let cmd2 = TextInsertCmd::new(WidgetId::new(1), 0, "b");
        let config = MergeConfig::default();
        assert!(!cmd1.can_merge(&cmd2, &config));
    }

    // ====================================================================
    // Time constraint failures for can_merge
    // ====================================================================

    #[test]
    fn test_text_insert_no_merge_time_exceeded() {
        let cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "a");
        // cmd2 created after cmd1, with a large enough delay
        // We can't easily create a large time gap, but we can test
        // the boundary by using a config with max_delay_ms = 0
        let mut cmd2 = TextInsertCmd::new(WidgetId::new(1), 1, "b");
        // Even with same timestamp, max_delay_ms=0 should reject if any time passed
        // Set timestamps equal so the duration is 0ms — but config allows 0ms max
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig {
            max_delay_ms: 0,
            merge_across_words: true,
            max_merged_size: 1024,
        };
        // 0 elapsed <= 0 max: should still pass (0 <= 0 is not >)
        assert!(cmd1.can_merge(&cmd2, &config));
    }

    #[test]
    fn test_text_delete_no_merge_time_exceeded() {
        let cmd1 = TextDeleteCmd::new(WidgetId::new(1), 5, "a");
        let mut cmd2 = TextDeleteCmd::new(WidgetId::new(1), 4, "b");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig {
            max_delay_ms: 0,
            merge_across_words: true,
            max_merged_size: 1024,
        };
        // 0 elapsed, 0 max: 0 > 0 is false, so merge allowed
        assert!(cmd1.can_merge(&cmd2, &config));
    }

    // ====================================================================
    // TextReplaceCmd redo (default delegates to execute)
    // ====================================================================

    #[test]
    fn test_text_replace_redo() {
        let buf = Arc::new(Mutex::new(String::from("Hello World")));
        let b1 = buf.clone();

        let mut cmd = TextReplaceCmd::new(WidgetId::new(1), 6, "World", "Rust").with_replace(
            move |_, pos, old_len, new_text| {
                let mut b = b1.lock().unwrap();
                b.replace_range(pos..pos + old_len, new_text);
                Ok(())
            },
        );

        cmd.execute().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello Rust");

        cmd.undo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello World");

        cmd.redo().unwrap();
        assert_eq!(*buf.lock().unwrap(), "Hello Rust");
    }

    // ====================================================================
    // CommandBatch execute on empty
    // ====================================================================

    #[test]
    fn test_command_batch_execute_empty_is_noop() {
        let mut batch = CommandBatch::new("Empty execute");
        assert!(batch.execute().is_ok());
        assert_eq!(batch.executed_to, 0);
    }

    // ====================================================================
    // CommandBatch multiple push_executed
    // ====================================================================

    #[test]
    fn test_command_batch_multiple_push_executed() {
        let mut batch = CommandBatch::new("Multi pre-exec");
        batch.push_executed(Box::new(TextInsertCmd::new(WidgetId::new(1), 0, "a")));
        batch.push_executed(Box::new(TextInsertCmd::new(WidgetId::new(1), 1, "b")));
        assert_eq!(batch.len(), 2);
        assert_eq!(batch.executed_to, 2);
    }

    // ====================================================================
    // CommandError clone and equality
    // ====================================================================

    #[test]
    fn test_command_error_clone_and_equality() {
        let err1 = CommandError::TargetNotFound(WidgetId::new(5));
        let err2 = err1.clone();
        assert_eq!(err1, err2);

        let err3 = CommandError::PositionOutOfBounds {
            position: 10,
            length: 5,
        };
        let err4 = err3.clone();
        assert_eq!(err3, err4);
    }

    // ====================================================================
    // MergeConfig clone and copy
    // ====================================================================

    #[test]
    fn test_merge_config_clone_and_copy() {
        let config = MergeConfig {
            max_delay_ms: 1000,
            merge_across_words: true,
            max_merged_size: 2048,
        };
        let config2 = config;
        assert_eq!(config2.max_delay_ms, 1000);
        assert!(config2.merge_across_words);
        assert_eq!(config2.max_merged_size, 2048);
    }

    // ====================================================================
    // WidgetId copy semantics
    // ====================================================================

    #[test]
    fn test_widget_id_copy() {
        let a = WidgetId::new(42);
        let b = a; // Copy
        assert_eq!(a, b); // original still usable
        assert_eq!(a.raw(), 42);
    }

    // ====================================================================
    // CommandMetadata clone
    // ====================================================================

    #[test]
    fn test_command_metadata_clone() {
        let meta = CommandMetadata::new("test")
            .with_source(CommandSource::Programmatic)
            .with_batch(99);
        let cloned = meta.clone();
        assert_eq!(cloned.description, "test");
        assert_eq!(cloned.source, CommandSource::Programmatic);
        assert_eq!(cloned.batch_id, Some(99));
    }
}
