#![forbid(unsafe_code)]

//! History stack for undo/redo operations.
//!
//! This module provides the [`HistoryManager`] which maintains dual stacks
//! for undo and redo operations with support for:
//!
//! - **Memory limits**: Oldest commands evicted when budget exceeded
//! - **Depth limits**: Maximum number of commands in history
//! - **Branch handling**: New actions clear the redo stack
//! - **Command merging**: Consecutive similar commands batched together
//!
//! # Invariants
//!
//! 1. `total_bytes` always equals sum of `size_bytes()` for all commands
//! 2. `undo_stack.len() <= config.max_depth` (after any operation)
//! 3. `total_bytes <= config.max_bytes` (after any operation, if enforced)
//! 4. Redo stack is cleared whenever a new command is pushed
//!
//! # Memory Model
//!
//! Commands are stored in `VecDeque` for O(1) eviction from the front.
//! Memory accounting uses each command's `size_bytes()` method.
//!
//! ```text
//! push(cmd5)
//! ┌───────────────────────────────────────────────┐
//! │ Undo Stack: [cmd1, cmd2, cmd3, cmd4, cmd5]    │
//! │ Redo Stack: []                                 │
//! └───────────────────────────────────────────────┘
//!
//! undo() x2
//! ┌───────────────────────────────────────────────┐
//! │ Undo Stack: [cmd1, cmd2, cmd3]                │
//! │ Redo Stack: [cmd4, cmd5]                       │
//! └───────────────────────────────────────────────┘
//!
//! push(cmd6)  <-- new branch, clears redo
//! ┌───────────────────────────────────────────────┐
//! │ Undo Stack: [cmd1, cmd2, cmd3, cmd6]          │
//! │ Redo Stack: []                                 │
//! └───────────────────────────────────────────────┘
//! ```

use std::collections::VecDeque;
use std::fmt;

use super::command::{MergeConfig, UndoableCmd};

/// Configuration for the history manager.
#[derive(Debug, Clone)]
pub struct HistoryConfig {
    /// Maximum number of commands to keep in undo history.
    pub max_depth: usize,
    /// Maximum total bytes for all commands (0 = unlimited).
    pub max_bytes: usize,
    /// Configuration for command merging.
    pub merge_config: MergeConfig,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            max_depth: 100,
            max_bytes: 10 * 1024 * 1024, // 10 MB
            merge_config: MergeConfig::default(),
        }
    }
}

impl HistoryConfig {
    /// Create a new configuration with custom limits.
    #[must_use]
    pub fn new(max_depth: usize, max_bytes: usize) -> Self {
        Self {
            max_depth,
            max_bytes,
            merge_config: MergeConfig::default(),
        }
    }

    /// Set the merge configuration.
    #[must_use]
    pub fn with_merge_config(mut self, config: MergeConfig) -> Self {
        self.merge_config = config;
        self
    }

    /// Create unlimited configuration (for testing).
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            max_depth: usize::MAX,
            max_bytes: 0,
            merge_config: MergeConfig::default(),
        }
    }
}

/// Manager for undo/redo history.
///
/// Maintains dual stacks for undo and redo operations with
/// configurable memory and depth limits.
pub struct HistoryManager {
    /// Commands available for undo (newest at back).
    undo_stack: VecDeque<Box<dyn UndoableCmd>>,
    /// Commands available for redo (newest at back).
    redo_stack: VecDeque<Box<dyn UndoableCmd>>,
    /// Configuration for limits and merging.
    config: HistoryConfig,
    /// Total bytes used by all commands.
    total_bytes: usize,
}

impl fmt::Debug for HistoryManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HistoryManager")
            .field("undo_depth", &self.undo_stack.len())
            .field("redo_depth", &self.redo_stack.len())
            .field("total_bytes", &self.total_bytes)
            .field("config", &self.config)
            .finish()
    }
}

impl Default for HistoryManager {
    fn default() -> Self {
        Self::new(HistoryConfig::default())
    }
}

impl HistoryManager {
    /// Create a new history manager with the given configuration.
    #[must_use]
    pub fn new(config: HistoryConfig) -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            config,
            total_bytes: 0,
        }
    }

    // ========================================================================
    // Core Operations
    // ========================================================================

    /// Push a command onto the undo stack.
    ///
    /// This clears the redo stack (new branch) and enforces limits.
    /// The command is NOT executed - it's assumed to have already been executed.
    pub fn push(&mut self, cmd: Box<dyn UndoableCmd>) {
        // Clear redo stack (new branch)
        self.clear_redo();

        // Try to merge with the last command
        let cmd = match self.try_merge(cmd) {
            Ok(()) => {
                // Merged successfully, just enforce limits
                self.enforce_limits();
                return;
            }
            Err(cmd) => cmd,
        };

        // Add to undo stack
        self.total_bytes += cmd.size_bytes();
        self.undo_stack.push_back(cmd);

        // Enforce limits
        self.enforce_limits();
    }

    /// Undo the last command.
    ///
    /// Moves the command from undo stack to redo stack and calls undo().
    ///
    /// # Returns
    ///
    /// - `Ok(description)` if undo succeeded
    /// - `Err(error)` if undo failed (command remains on undo stack)
    /// - `None` if no commands to undo
    pub fn undo(&mut self) -> Option<Result<String, super::command::CommandError>> {
        let mut cmd = self.undo_stack.pop_back()?;
        let description = cmd.description().to_string();

        match cmd.undo() {
            Ok(()) => {
                // Move to redo stack
                self.redo_stack.push_back(cmd);
                Some(Ok(description))
            }
            Err(e) => {
                // Put back on undo stack
                self.undo_stack.push_back(cmd);
                Some(Err(e))
            }
        }
    }

    /// Redo the last undone command.
    ///
    /// Moves the command from redo stack to undo stack and calls redo().
    ///
    /// # Returns
    ///
    /// - `Ok(description)` if redo succeeded
    /// - `Err(error)` if redo failed (command remains on redo stack)
    /// - `None` if no commands to redo
    pub fn redo(&mut self) -> Option<Result<String, super::command::CommandError>> {
        let mut cmd = self.redo_stack.pop_back()?;
        let description = cmd.description().to_string();

        match cmd.redo() {
            Ok(()) => {
                // Move to undo stack
                self.undo_stack.push_back(cmd);
                Some(Ok(description))
            }
            Err(e) => {
                // Put back on redo stack
                self.redo_stack.push_back(cmd);
                Some(Err(e))
            }
        }
    }

    /// Check if undo is available.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Check if redo is available.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    // ========================================================================
    // Info
    // ========================================================================

    /// Get the undo stack depth.
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Get the redo stack depth.
    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Get descriptions for undo commands (most recent first).
    pub fn undo_descriptions(&self, limit: usize) -> Vec<&str> {
        self.undo_stack
            .iter()
            .rev()
            .take(limit)
            .map(|c| c.description())
            .collect()
    }

    /// Get descriptions for redo commands (most recent first).
    pub fn redo_descriptions(&self, limit: usize) -> Vec<&str> {
        self.redo_stack
            .iter()
            .rev()
            .take(limit)
            .map(|c| c.description())
            .collect()
    }

    /// Get the description of the next undo command.
    #[must_use]
    pub fn next_undo_description(&self) -> Option<&str> {
        self.undo_stack.back().map(|c| c.description())
    }

    /// Get the description of the next redo command.
    #[must_use]
    pub fn next_redo_description(&self) -> Option<&str> {
        self.redo_stack.back().map(|c| c.description())
    }

    /// Get total memory usage in bytes.
    #[must_use]
    pub fn memory_usage(&self) -> usize {
        self.total_bytes
    }

    /// Get the current configuration.
    #[must_use]
    pub fn config(&self) -> &HistoryConfig {
        &self.config
    }

    // ========================================================================
    // Maintenance
    // ========================================================================

    /// Clear all history (both undo and redo).
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.total_bytes = 0;
    }

    /// Clear only the redo stack.
    fn clear_redo(&mut self) {
        for cmd in self.redo_stack.drain(..) {
            self.total_bytes = self.total_bytes.saturating_sub(cmd.size_bytes());
        }
    }

    /// Enforce depth and memory limits by evicting oldest commands.
    fn enforce_limits(&mut self) {
        // Enforce depth limit (only applies to undo stack)
        while self.undo_stack.len() > self.config.max_depth {
            if let Some(cmd) = self.undo_stack.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(cmd.size_bytes());
            }
        }

        // Enforce memory limit (if set) - applies to TOTAL history
        if self.config.max_bytes > 0 {
            while self.total_bytes > self.config.max_bytes {
                // First try to drop from redo stack (future/speculative history)
                if let Some(cmd) = self.redo_stack.pop_front() {
                    self.total_bytes = self.total_bytes.saturating_sub(cmd.size_bytes());
                    continue;
                }

                // Then drop from undo stack (oldest history)
                if let Some(cmd) = self.undo_stack.pop_front() {
                    self.total_bytes = self.total_bytes.saturating_sub(cmd.size_bytes());
                } else {
                    // Both stacks empty, nothing to drop
                    break;
                }
            }
        }
    }

    /// Try to merge a command with the last one on the undo stack.
    ///
    /// Returns `Ok(())` if merged, `Err(cmd)` if not merged.
    fn try_merge(&mut self, cmd: Box<dyn UndoableCmd>) -> Result<(), Box<dyn UndoableCmd>> {
        let Some(last) = self.undo_stack.back_mut() else {
            return Err(cmd);
        };

        // Check if merge is possible
        if !last.can_merge(cmd.as_ref(), &self.config.merge_config) {
            return Err(cmd);
        }

        // Verify the command has merge text available
        if cmd.merge_text().is_none() {
            return Err(cmd);
        }

        // Adjust memory accounting (old size will change)
        let old_size = last.size_bytes();

        // Perform the merge (pass full command for position context)
        if !last.accept_merge(cmd.as_ref()) {
            return Err(cmd);
        }

        // Update memory accounting
        let new_size = last.size_bytes();
        self.total_bytes = self.total_bytes.saturating_sub(old_size) + new_size;

        // The incoming command is consumed (merged into last)
        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::undo::command::{TextInsertCmd, WidgetId};
    use std::sync::Arc;
    use std::sync::Mutex;

    /// Helper to create a simple insert command for testing.
    /// The command is pre-executed so undo() will work correctly.
    fn make_insert_cmd(text: &str) -> Box<dyn UndoableCmd> {
        let buffer = Arc::new(Mutex::new(String::new()));
        let b1 = buffer.clone();
        let b2 = buffer.clone();

        let mut cmd = TextInsertCmd::new(WidgetId::new(1), 0, text)
            .with_apply(move |_, pos, txt| {
                let mut buf = b1.lock().unwrap();
                buf.insert_str(pos, txt);
                Ok(())
            })
            .with_remove(move |_, pos, len| {
                let mut buf = b2.lock().unwrap();
                buf.drain(pos..pos + len);
                Ok(())
            });

        // Execute the command first so undo() will work
        cmd.execute().expect("test command should execute");

        Box::new(cmd)
    }

    #[test]
    fn test_new_manager() {
        let mgr = HistoryManager::default();
        assert!(!mgr.can_undo());
        assert!(!mgr.can_redo());
        assert_eq!(mgr.undo_depth(), 0);
        assert_eq!(mgr.redo_depth(), 0);
    }

    #[test]
    fn test_push_enables_undo() {
        let mut mgr = HistoryManager::default();
        mgr.push(make_insert_cmd("hello"));

        assert!(mgr.can_undo());
        assert!(!mgr.can_redo());
        assert_eq!(mgr.undo_depth(), 1);
    }

    #[test]
    fn test_undo_enables_redo() {
        let mut mgr = HistoryManager::default();
        mgr.push(make_insert_cmd("hello"));

        let result = mgr.undo();
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());

        assert!(!mgr.can_undo());
        assert!(mgr.can_redo());
        assert_eq!(mgr.redo_depth(), 1);
    }

    #[test]
    fn test_redo_moves_back_to_undo() {
        let mut mgr = HistoryManager::default();
        mgr.push(make_insert_cmd("hello"));
        mgr.undo();

        let result = mgr.redo();
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());

        assert!(mgr.can_undo());
        assert!(!mgr.can_redo());
    }

    #[test]
    fn test_push_clears_redo() {
        let mut mgr = HistoryManager::default();
        mgr.push(make_insert_cmd("hello"));
        mgr.undo();

        assert!(mgr.can_redo());

        // Push new command
        mgr.push(make_insert_cmd("world"));

        // Redo should be cleared
        assert!(!mgr.can_redo());
        assert_eq!(mgr.redo_depth(), 0);
    }

    #[test]
    fn test_max_depth_enforced() {
        let config = HistoryConfig::new(3, 0);
        let mut mgr = HistoryManager::new(config);

        for i in 0..5 {
            mgr.push(make_insert_cmd(&format!("cmd{}", i)));
        }

        // Should only keep 3 commands
        assert_eq!(mgr.undo_depth(), 3);
    }

    #[test]
    fn test_descriptions() {
        let mut mgr = HistoryManager::default();
        mgr.push(make_insert_cmd("a"));
        mgr.push(make_insert_cmd("b"));
        mgr.push(make_insert_cmd("c"));

        let descs = mgr.undo_descriptions(5);
        assert_eq!(descs.len(), 3);
        // All should be "Insert text"
        assert!(descs.iter().all(|d| *d == "Insert text"));
    }

    #[test]
    fn test_next_descriptions() {
        let mut mgr = HistoryManager::default();
        mgr.push(make_insert_cmd("hello"));

        assert_eq!(mgr.next_undo_description(), Some("Insert text"));
        assert_eq!(mgr.next_redo_description(), None);

        mgr.undo();

        assert_eq!(mgr.next_undo_description(), None);
        assert_eq!(mgr.next_redo_description(), Some("Insert text"));
    }

    #[test]
    fn test_clear() {
        let mut mgr = HistoryManager::default();
        mgr.push(make_insert_cmd("a"));
        mgr.push(make_insert_cmd("b"));
        mgr.undo();

        assert!(mgr.can_undo());
        assert!(mgr.can_redo());

        mgr.clear();

        assert!(!mgr.can_undo());
        assert!(!mgr.can_redo());
        assert_eq!(mgr.memory_usage(), 0);
    }

    #[test]
    fn test_memory_tracking() {
        let mut mgr = HistoryManager::new(HistoryConfig::unlimited());

        let initial = mgr.memory_usage();
        assert_eq!(initial, 0);

        mgr.push(make_insert_cmd("hello"));
        let after_push = mgr.memory_usage();
        assert!(after_push > 0);

        mgr.push(make_insert_cmd("world"));
        let after_second = mgr.memory_usage();
        assert!(after_second > after_push);
    }

    #[test]
    fn test_undo_without_commands() {
        let mut mgr = HistoryManager::default();
        assert!(mgr.undo().is_none());
    }

    #[test]
    fn test_redo_without_commands() {
        let mut mgr = HistoryManager::default();
        assert!(mgr.redo().is_none());
    }

    #[test]
    fn test_multiple_undo_redo_cycle() {
        let mut mgr = HistoryManager::default();

        // Push 3 commands
        mgr.push(make_insert_cmd("a"));
        mgr.push(make_insert_cmd("b"));
        mgr.push(make_insert_cmd("c"));

        assert_eq!(mgr.undo_depth(), 3);
        assert_eq!(mgr.redo_depth(), 0);

        // Undo all
        mgr.undo();
        mgr.undo();
        mgr.undo();

        assert_eq!(mgr.undo_depth(), 0);
        assert_eq!(mgr.redo_depth(), 3);

        // Redo all
        mgr.redo();
        mgr.redo();
        mgr.redo();

        assert_eq!(mgr.undo_depth(), 3);
        assert_eq!(mgr.redo_depth(), 0);
    }

    #[test]
    fn test_config_default() {
        let config = HistoryConfig::default();
        assert_eq!(config.max_depth, 100);
        assert_eq!(config.max_bytes, 10 * 1024 * 1024);
    }

    #[test]
    fn test_config_unlimited() {
        let config = HistoryConfig::unlimited();
        assert_eq!(config.max_depth, usize::MAX);
        assert_eq!(config.max_bytes, 0);
    }

    #[test]
    fn test_debug_impl() {
        let mgr = HistoryManager::default();
        let debug_str = format!("{:?}", mgr);
        assert!(debug_str.contains("HistoryManager"));
        assert!(debug_str.contains("undo_depth"));
    }
}
