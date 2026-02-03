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
use std::time::Instant;

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

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
///
/// # Implementing `UndoableCmd`
///
/// ```ignore
/// struct MyCommand {
///     target: WidgetId,
///     old_value: String,
///     new_value: String,
///     metadata: CommandMetadata,
/// }
///
/// impl UndoableCmd for MyCommand {
///     fn execute(&mut self) -> CommandResult {
///         // Apply new_value to target
///         Ok(())
///     }
///
///     fn undo(&mut self) -> CommandResult {
///         // Restore old_value to target
///         Ok(())
///     }
///
///     fn description(&self) -> &str {
///         &self.metadata.description
///     }
///
///     fn size_bytes(&self) -> usize {
///         std::mem::size_of::<Self>()
///             + self.old_value.len()
///             + self.new_value.len()
///             + self.metadata.size_bytes()
///     }
/// }
/// ```
pub trait UndoableCmd: Send + Sync {
    /// Execute the command, applying its effect.
    ///
    /// # Errors
    ///
    /// Returns error if the command cannot be executed.
    fn execute(&mut self) -> CommandResult;

    /// Undo the command, reverting its effect.
    ///
    /// After undo, the system state MUST match the state before execute().
    ///
    /// # Errors
    ///
    /// Returns error if the command cannot be undone.
    fn undo(&mut self) -> CommandResult;

    /// Redo the command after it was undone.
    ///
    /// Default implementation calls execute().
    ///
    /// # Errors
    ///
    /// Returns error if the command cannot be redone.
    fn redo(&mut self) -> CommandResult {
        self.execute()
    }

    /// Human-readable description for UI display.
    ///
    /// Should be concise (e.g., "Insert text", "Delete selection").
    fn description(&self) -> &str;

    /// Size of this command in bytes for memory budgeting.
    ///
    /// MUST include all heap allocations (strings, vectors, etc.).
    fn size_bytes(&self) -> usize;

    /// Check if this command can merge with another.
    ///
    /// Called by the undo stack to batch related commands.
    /// If true, `merge()` will be called.
    ///
    /// # Arguments
    ///
    /// * `other` - The newer command to potentially merge
    /// * `config` - Merge configuration (timing, size limits)
    fn can_merge(&self, _other: &dyn UndoableCmd, _config: &MergeConfig) -> bool {
        false
    }

    /// Merge another command into this one.
    ///
    /// Only called if `can_merge()` returned true.
    /// After merge, this command represents both operations.
    ///
    /// # Returns
    ///
    /// `Ok(())` if merge succeeded, `Err(other)` if merge failed
    /// (the other command is returned for separate handling).
    fn merge(&mut self, _other: Box<dyn UndoableCmd>) -> Result<(), Box<dyn UndoableCmd>> {
        Err(_other)
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
}

/// A batch of commands that execute and undo together.
///
/// Useful for operations that span multiple widgets or steps
/// but should appear as a single undo entry.
#[derive(Debug)]
pub struct CommandBatch {
    /// Commands in execution order.
    commands: Vec<Box<dyn UndoableCmd>>,
    /// Batch metadata.
    metadata: CommandMetadata,
    /// Index of last successfully executed command.
    executed_to: usize,
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
}

// ============================================================================
// Built-in Text Commands
// ============================================================================

/// Command to insert text at a position.
#[derive(Debug)]
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
    apply: Option<Box<dyn Fn(WidgetId, usize, &str) -> CommandResult + Send + Sync>>,
    /// Callback to remove the insertion (set by the widget).
    remove: Option<Box<dyn Fn(WidgetId, usize, usize) -> CommandResult + Send + Sync>>,
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
        let elapsed = other.metadata.timestamp.duration_since(self.metadata.timestamp);
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

    fn merge(&mut self, other: Box<dyn UndoableCmd>) -> Result<(), Box<dyn UndoableCmd>> {
        let other = match other.as_any().downcast_ref::<Self>() {
            Some(_) => {
                // Safe to downcast since can_merge passed
                let other = unsafe {
                    Box::from_raw(Box::into_raw(other) as *mut Self)
                };
                other
            }
            None => return Err(other),
        };

        self.text.push_str(&other.text);
        Ok(())
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
}

/// Command to delete text at a position.
#[derive(Debug)]
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
    remove: Option<Box<dyn Fn(WidgetId, usize, usize) -> CommandResult + Send + Sync>>,
    /// Callback to insert text (for undo).
    insert: Option<Box<dyn Fn(WidgetId, usize, &str) -> CommandResult + Send + Sync>>,
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
        let elapsed = other.metadata.timestamp.duration_since(self.metadata.timestamp);
        if elapsed.as_millis() > config.max_delay_ms as u128 {
            return false;
        }

        // Check size constraint
        if self.deleted_text.len() + other.deleted_text.len() > config.max_merged_size {
            return false;
        }

        true
    }

    fn merge(&mut self, other: Box<dyn UndoableCmd>) -> Result<(), Box<dyn UndoableCmd>> {
        let other = match other.as_any().downcast_ref::<Self>() {
            Some(_) => {
                // Safe to downcast since can_merge passed
                let other = unsafe {
                    Box::from_raw(Box::into_raw(other) as *mut Self)
                };
                other
            }
            None => return Err(other),
        };

        // Determine merge direction
        if other.position + other.deleted_text.len() == self.position {
            // Backspace: prepend
            self.deleted_text = format!("{}{}", other.deleted_text, self.deleted_text);
            self.position = other.position;
        } else {
            // Delete key: append
            self.deleted_text.push_str(&other.deleted_text);
        }

        Ok(())
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
}

/// Command to replace text at a position.
#[derive(Debug)]
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
    replace: Option<Box<dyn Fn(WidgetId, usize, usize, &str) -> CommandResult + Send + Sync>>,
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
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
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

        let mut cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "Hello");
        cmd1.apply = Some(Box::new(move |_, pos, text| {
            let mut buf = b1.lock().unwrap();
            buf.insert_str(pos, text);
            Ok(())
        }));
        cmd1.remove = Some(Box::new(move |_, pos, len| {
            let mut buf = b2.lock().unwrap();
            buf.drain(pos..pos + len);
            Ok(())
        }));

        let mut cmd2 = TextInsertCmd::new(WidgetId::new(1), 5, " World");
        cmd2.apply = Some(Box::new(move |_, pos, text| {
            let mut buf = b3.lock().unwrap();
            buf.insert_str(pos, text);
            Ok(())
        }));
        cmd2.remove = Some(Box::new(move |_, pos, len| {
            let mut buf = b4.lock().unwrap();
            buf.drain(pos..pos + len);
            Ok(())
        }));

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
    fn test_text_insert_merge() {
        let mut cmd1 = TextInsertCmd::new(WidgetId::new(1), 0, "Hello");
        let mut cmd2 = TextInsertCmd::new(WidgetId::new(1), 5, " World");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default();
        assert!(cmd1.can_merge(&cmd2, &config));

        let boxed: Box<dyn UndoableCmd> = Box::new(cmd2);
        cmd1.merge(boxed).unwrap();
        assert_eq!(cmd1.text, "Hello World");
    }

    #[test]
    fn test_text_delete_merge_backspace() {
        // Simulate backspace: deleting 'b' at position 5, then 'a' at position 4
        let mut cmd1 = TextDeleteCmd::new(WidgetId::new(1), 5, "b");
        let mut cmd2 = TextDeleteCmd::new(WidgetId::new(1), 4, "a");
        cmd2.metadata.timestamp = cmd1.metadata.timestamp;

        let config = MergeConfig::default();
        assert!(cmd1.can_merge(&cmd2, &config));

        let boxed: Box<dyn UndoableCmd> = Box::new(cmd2);
        cmd1.merge(boxed).unwrap();

        // After merging, should have "ab" deleted starting at position 4
        assert_eq!(cmd1.deleted_text, "ab");
        assert_eq!(cmd1.position, 4);
    }
}
