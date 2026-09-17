//! Widget state persistence for save/restore across sessions.
//!
//! This module provides the [`StateRegistry`] and [`StorageBackend`] infrastructure
//! for persisting widget state. It works with the `Stateful` trait from `ftui-widgets`.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │                      StateRegistry                            │
//! │   - In-memory cache of widget states                          │
//! │   - Delegates to StorageBackend for persistence               │
//! │   - Provides load/save/clear operations                       │
//! └──────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌──────────────────────────────────────────────────────────────┐
//! │                     StorageBackend                            │
//! │   - MemoryStorage: in-memory (testing, ephemeral)             │
//! │   - FileStorage: JSON file (requires state-persistence)       │
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Design Invariants
//!
//! 1. **Graceful degradation**: Storage failures never panic; operations return `Result`.
//! 2. **Atomic writes**: File storage uses write-rename pattern to prevent corruption.
//! 3. **Partial load tolerance**: Missing or corrupt entries use `Default::default()`.
//! 4. **Type safety**: Registry is type-erased internally but type-safe at boundaries.
//!
//! # Failure Modes
//!
//! | Failure | Cause | Behavior |
//! |---------|-------|----------|
//! | `StorageError::Io` | File I/O failure | Returns error, cache unaffected |
//! | `StorageError::Serialization` | JSON encode/decode | Entry skipped, logged |
//! | `StorageError::Corruption` | Invalid file format | Load returns partial data |
//! | Missing entry | First run, key changed | `Default::default()` used |
//!
//! # Feature Gates
//!
//! - `state-persistence`: Enables `FileStorage` with JSON serialization.
//!   Without this feature, only `MemoryStorage` is available.
//!
//! `Stateful` lives in `ftui_widgets::stateful`.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

// ─────────────────────────────────────────────────────────────────────────────
// Error Types
// ─────────────────────────────────────────────────────────────────────────────

/// Errors that can occur during state storage operations.
#[derive(Debug)]
pub enum StorageError {
    /// I/O error during file operations.
    Io(std::io::Error),
    /// Serialization or deserialization error.
    #[cfg(feature = "state-persistence")]
    Serialization(String),
    /// Storage file is corrupted or invalid format.
    Corruption(String),
    /// Backend is not available (e.g., file storage without feature).
    Unavailable(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Io(e) => write!(f, "I/O error: {e}"),
            #[cfg(feature = "state-persistence")]
            StorageError::Serialization(msg) => write!(f, "serialization error: {msg}"),
            StorageError::Corruption(msg) => write!(f, "storage corruption: {msg}"),
            StorageError::Unavailable(msg) => write!(f, "storage unavailable: {msg}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StorageError::Io(e) => Some(e),
            #[cfg(feature = "state-persistence")]
            StorageError::Serialization(_) => None,
            StorageError::Corruption(_) => None,
            StorageError::Unavailable(_) => None,
        }
    }
}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        StorageError::Io(e)
    }
}

/// Result type for storage operations.
pub type StorageResult<T> = Result<T, StorageError>;

// ─────────────────────────────────────────────────────────────────────────────
// Storage Backend Trait
// ─────────────────────────────────────────────────────────────────────────────

/// A serialized state entry with version metadata.
///
/// This is the storage format used by backends. The actual state data
/// is serialized to bytes by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredEntry {
    /// The canonical state key (widget_type::instance_id).
    pub key: String,
    /// Schema version from `Stateful::state_version()`.
    pub version: u32,
    /// Serialized state data (JSON bytes with `state-persistence` feature).
    pub data: Vec<u8>,
}

/// Trait for pluggable state storage backends.
///
/// Implementations must be thread-safe (`Send + Sync`) to support
/// concurrent access from the registry.
///
/// # Implementation Notes
///
/// - `load_all` should be resilient to partial corruption.
/// - `save_all` should be atomic (write-then-rename pattern for files).
/// - `clear` should remove all stored state for the application.
pub trait StorageBackend: Send + Sync {
    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// Load all stored state entries.
    ///
    /// Returns an empty map if no state exists (first run).
    /// Skips corrupted entries rather than failing entirely.
    fn load_all(&self) -> StorageResult<HashMap<String, StoredEntry>>;

    /// Save all state entries atomically.
    ///
    /// This should replace all existing state (not merge).
    fn save_all(&self, entries: &HashMap<String, StoredEntry>) -> StorageResult<()>;

    /// Clear all stored state.
    fn clear(&self) -> StorageResult<()>;

    /// Check if the backend is available and functional.
    fn is_available(&self) -> bool {
        true
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Memory Storage (always available)
// ─────────────────────────────────────────────────────────────────────────────

/// In-memory storage backend for testing and ephemeral state.
///
/// State is lost when the process exits. Useful for:
/// - Unit testing widget persistence logic
/// - Applications that don't need cross-session persistence
/// - Development/debugging without file I/O
#[derive(Default)]
pub struct MemoryStorage {
    data: RwLock<HashMap<String, StoredEntry>>,
}

impl MemoryStorage {
    /// Create a new empty memory storage.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create memory storage pre-populated with entries.
    #[must_use]
    pub fn with_entries(entries: HashMap<String, StoredEntry>) -> Self {
        Self {
            data: RwLock::new(entries),
        }
    }
}

impl StorageBackend for MemoryStorage {
    fn name(&self) -> &str {
        "MemoryStorage"
    }

    fn load_all(&self) -> StorageResult<HashMap<String, StoredEntry>> {
        let guard = self
            .data
            .read()
            .map_err(|_| StorageError::Corruption("lock poisoned".into()))?;
        Ok(guard.clone())
    }

    fn save_all(&self, entries: &HashMap<String, StoredEntry>) -> StorageResult<()> {
        let mut guard = self
            .data
            .write()
            .map_err(|_| StorageError::Corruption("lock poisoned".into()))?;
        *guard = entries.clone();
        Ok(())
    }

    fn clear(&self) -> StorageResult<()> {
        let mut guard = self
            .data
            .write()
            .map_err(|_| StorageError::Corruption("lock poisoned".into()))?;
        guard.clear();
        Ok(())
    }
}

impl fmt::Debug for MemoryStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.data.read().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("MemoryStorage")
            .field("entries", &count)
            .finish()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// File Storage (requires state-persistence feature)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "state-persistence")]
mod file_storage {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::fs::{self, File};
    use std::io::{BufReader, BufWriter, Write};
    use std::path::{Path, PathBuf};

    /// File format for stored state (JSON).
    #[derive(Serialize, Deserialize)]
    struct StateFile {
        /// Format version for future migrations.
        format_version: u32,
        /// Map of canonical key -> entry.
        entries: HashMap<String, FileEntry>,
    }

    /// Serialized entry in the state file.
    #[derive(Serialize, Deserialize)]
    struct FileEntry {
        version: u32,
        /// Base64-encoded data for binary safety.
        data_base64: String,
    }

    impl StateFile {
        const FORMAT_VERSION: u32 = 1;

        fn new() -> Self {
            Self {
                format_version: Self::FORMAT_VERSION,
                entries: HashMap::new(),
            }
        }
    }

    /// File-based storage backend using JSON.
    ///
    /// State is persisted to a JSON file with atomic write-rename pattern.
    /// Suitable for applications that need cross-session persistence.
    ///
    /// # File Format
    ///
    /// ```json
    /// {
    ///   "format_version": 1,
    ///   "entries": {
    ///     "ScrollView::main": {
    ///       "version": 1,
    ///       "data_base64": "eyJzY3JvbGxfb2Zmc2V0IjogNDJ9"
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// # Atomic Writes
    ///
    /// Writes use a temporary file + rename pattern to prevent corruption:
    /// 1. Write to `{path}.tmp`
    /// 2. Flush and sync
    /// 3. Rename `{path}.tmp` -> `{path}`
    pub struct FileStorage {
        path: PathBuf,
    }

    impl FileStorage {
        /// Create a file storage at the given path.
        ///
        /// The file does not need to exist; it will be created on first save.
        #[must_use]
        pub fn new(path: impl AsRef<Path>) -> Self {
            Self {
                path: path.as_ref().to_path_buf(),
            }
        }

        /// Create storage at the default location for the application.
        ///
        /// Uses `$XDG_STATE_HOME/ftui/{app_name}/state.json` on Linux,
        /// or platform-appropriate equivalents.
        #[must_use]
        pub fn default_for_app(app_name: &str) -> Self {
            let base = dirs_or_fallback();
            let path = base.join("ftui").join(app_name).join("state.json");
            Self { path }
        }

        fn temp_path(&self) -> PathBuf {
            let mut tmp = self.path.clone();
            tmp.set_extension("json.tmp");
            tmp
        }
    }

    /// Get state directory, falling back to current dir if unavailable.
    fn dirs_or_fallback() -> PathBuf {
        // Try XDG_STATE_HOME first
        if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
            return PathBuf::from(state_home);
        }
        // Fall back to ~/.local/state
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".local").join("state");
        }
        // Last resort: current directory
        PathBuf::from(".")
    }

    impl StorageBackend for FileStorage {
        fn name(&self) -> &str {
            "FileStorage"
        }

        fn load_all(&self) -> StorageResult<HashMap<String, StoredEntry>> {
            if !self.path.exists() {
                // First run - no state yet
                return Ok(HashMap::new());
            }

            let file = File::open(&self.path)?;
            let reader = BufReader::new(file);

            let state_file: StateFile = serde_json::from_reader(reader).map_err(|e| {
                StorageError::Serialization(format!("failed to parse state file: {e}"))
            })?;

            // Validate format version
            if state_file.format_version != StateFile::FORMAT_VERSION {
                tracing::warn!(
                    stored = state_file.format_version,
                    expected = StateFile::FORMAT_VERSION,
                    "state file format version mismatch, ignoring stored state"
                );
                return Ok(HashMap::new());
            }

            // Convert file entries to StoredEntry
            let mut result = HashMap::new();
            for (key, entry) in state_file.entries {
                use base64::Engine;
                let data = match base64::engine::general_purpose::STANDARD
                    .decode(&entry.data_base64)
                {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(key = %key, error = %e, "failed to decode state entry, skipping");
                        continue;
                    }
                };
                result.insert(
                    key.clone(),
                    StoredEntry {
                        key,
                        version: entry.version,
                        data,
                    },
                );
            }

            Ok(result)
        }

        fn save_all(&self, entries: &HashMap<String, StoredEntry>) -> StorageResult<()> {
            use base64::Engine;

            // Ensure parent directory exists
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent)?;
            }

            // Build file content
            let mut state_file = StateFile::new();
            for (key, entry) in entries {
                state_file.entries.insert(
                    key.clone(),
                    FileEntry {
                        version: entry.version,
                        data_base64: base64::engine::general_purpose::STANDARD.encode(&entry.data),
                    },
                );
            }

            // Write to temp file first (atomic pattern)
            let tmp_path = self.temp_path();
            {
                let file = File::create(&tmp_path)?;
                let mut writer = BufWriter::new(file);
                serde_json::to_writer_pretty(&mut writer, &state_file).map_err(|e| {
                    StorageError::Serialization(format!("failed to serialize state: {e}"))
                })?;
                writer.flush()?;
                writer.get_ref().sync_all()?;
            }

            // Atomic rename
            fs::rename(&tmp_path, &self.path)?;

            tracing::debug!(
                path = %self.path.display(),
                entries = entries.len(),
                "saved widget state"
            );

            Ok(())
        }

        fn clear(&self) -> StorageResult<()> {
            if self.path.exists() {
                fs::remove_file(&self.path)?;
            }
            Ok(())
        }

        fn is_available(&self) -> bool {
            // Check if we can write to the directory
            if let Some(parent) = self.path.parent() {
                if !parent.exists() {
                    return std::fs::create_dir_all(parent).is_ok();
                }
                // Check write permission (try to create temp file)
                let test_path = parent.join(".ftui_test_write");
                if std::fs::write(&test_path, b"test").is_ok() {
                    let _ = std::fs::remove_file(&test_path);
                    return true;
                }
            }
            false
        }
    }

    impl fmt::Debug for FileStorage {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("FileStorage")
                .field("path", &self.path)
                .finish()
        }
    }
}

#[cfg(feature = "state-persistence")]
pub use file_storage::FileStorage;

// ─────────────────────────────────────────────────────────────────────────────
// State Registry
// ─────────────────────────────────────────────────────────────────────────────

/// Central registry for widget state persistence.
///
/// The registry maintains an in-memory cache of widget states and delegates
/// to a [`StorageBackend`] for persistence. It provides the main API for
/// save/restore operations.
///
/// # Thread Safety
///
/// The registry is `Send + Sync` and uses internal locking for thread-safe access.
///
/// # Example
///
/// ```ignore
/// use ftui_runtime::state_persistence::{StateRegistry, MemoryStorage};
///
/// // Create registry with memory storage
/// let registry = StateRegistry::new(Box::new(MemoryStorage::new()));
///
/// // Load state for a widget
/// if let Some(entry) = registry.get("ScrollView::main") {
///     // Deserialize and restore...
/// }
///
/// // Save state
/// registry.set("ScrollView::main", 1, serialized_data);
/// registry.flush()?;
/// ```
pub struct StateRegistry {
    backend: Box<dyn StorageBackend>,
    cache: RwLock<HashMap<String, StoredEntry>>,
    dirty: RwLock<bool>,
}

impl StateRegistry {
    /// Create a new registry with the given storage backend.
    ///
    /// Does not automatically load from storage; call [`load`](Self::load) first.
    #[must_use]
    pub fn new(backend: Box<dyn StorageBackend>) -> Self {
        Self {
            backend,
            cache: RwLock::new(HashMap::new()),
            dirty: RwLock::new(false),
        }
    }

    /// Create a registry with memory storage (ephemeral, for testing).
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(Box::new(MemoryStorage::new()))
    }

    /// Create a registry with file storage at the given path.
    #[cfg(feature = "state-persistence")]
    #[must_use]
    pub fn with_file(path: impl AsRef<std::path::Path>) -> Self {
        Self::new(Box::new(FileStorage::new(path)))
    }

    /// Load all state from the storage backend.
    ///
    /// This replaces the in-memory cache with stored data.
    /// Safe to call multiple times; later calls refresh the cache.
    pub fn load(&self) -> StorageResult<usize> {
        let entries = self.backend.load_all()?;
        let count = entries.len();

        let mut cache = self
            .cache
            .write()
            .map_err(|_| StorageError::Corruption("cache lock poisoned".into()))?;
        *cache = entries;

        let mut dirty = self
            .dirty
            .write()
            .map_err(|_| StorageError::Corruption("dirty lock poisoned".into()))?;
        *dirty = false;

        tracing::debug!(backend = %self.backend.name(), count, "loaded widget state");
        Ok(count)
    }

    /// Flush dirty state to the storage backend.
    ///
    /// Only writes if changes have been made since last flush.
    /// Returns `Ok(true)` if data was written, `Ok(false)` if no changes.
    pub fn flush(&self) -> StorageResult<bool> {
        let dirty = {
            let guard = self
                .dirty
                .read()
                .map_err(|_| StorageError::Corruption("dirty lock poisoned".into()))?;
            *guard
        };

        if !dirty {
            return Ok(false);
        }

        let cache_snapshot = {
            let cache_guard = self
                .cache
                .read()
                .map_err(|_| StorageError::Corruption("cache lock poisoned".into()))?;
            cache_guard.clone()
        };

        self.backend.save_all(&cache_snapshot)?;

        let cache_guard = self
            .cache
            .read()
            .map_err(|_| StorageError::Corruption("cache lock poisoned".into()))?;
        let mut dirty_guard = self
            .dirty
            .write()
            .map_err(|_| StorageError::Corruption("dirty lock poisoned".into()))?;
        *dirty_guard = *cache_guard != cache_snapshot;

        Ok(true)
    }

    /// Get a stored state entry by canonical key.
    ///
    /// Returns `None` if no state exists for the key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<StoredEntry> {
        let cache = self.cache.read().ok()?;
        cache.get(key).cloned()
    }

    /// Set a state entry.
    ///
    /// Marks the registry as dirty; call [`flush`](Self::flush) to persist.
    pub fn set(&self, key: impl Into<String>, version: u32, data: Vec<u8>) {
        let key = key.into();
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(key.clone(), StoredEntry { key, version, data });
            if let Ok(mut dirty) = self.dirty.write() {
                *dirty = true;
            }
        }
    }

    /// Remove a state entry.
    ///
    /// Returns the removed entry if it existed.
    pub fn remove(&self, key: &str) -> Option<StoredEntry> {
        let result = self.cache.write().ok()?.remove(key);
        if result.is_some()
            && let Ok(mut dirty) = self.dirty.write()
        {
            *dirty = true;
        }
        result
    }

    /// Clear all state from both cache and storage.
    pub fn clear(&self) -> StorageResult<()> {
        self.backend.clear()?;
        if let Ok(mut cache) = self.cache.write() {
            cache.clear();
        }
        if let Ok(mut dirty) = self.dirty.write() {
            *dirty = false;
        }
        Ok(())
    }

    /// Get the number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.read().map(|c| c.len()).unwrap_or(0)
    }

    /// Check if the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Check if there are unsaved changes.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty.read().map(|d| *d).unwrap_or(false)
    }

    /// Get the backend name for logging.
    #[must_use]
    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }

    /// Check if the storage backend is available.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.backend.is_available()
    }

    /// Get all cached keys.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.cache
            .read()
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default()
    }
}

impl fmt::Debug for StateRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateRegistry")
            .field("backend", &self.backend.name())
            .field("entries", &self.len())
            .field("dirty", &self.is_dirty())
            .finish()
    }
}

// Make it Arc-able for shared ownership
impl StateRegistry {
    /// Wrap in Arc for shared ownership.
    #[must_use]
    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Statistics and Diagnostics
// ─────────────────────────────────────────────────────────────────────────────

/// Statistics about the state registry.
#[derive(Clone, Debug, Default)]
pub struct RegistryStats {
    /// Number of entries in cache.
    pub entry_count: usize,
    /// Total bytes of state data.
    pub total_bytes: usize,
    /// Whether there are unsaved changes.
    pub dirty: bool,
    /// Backend name.
    pub backend: String,
}

impl StateRegistry {
    /// Get statistics about the registry.
    #[must_use]
    pub fn stats(&self) -> RegistryStats {
        let (entry_count, total_bytes) = self
            .cache
            .read()
            .map(|c| {
                let count = c.len();
                let bytes: usize = c.values().map(|e| e.data.len()).sum();
                (count, bytes)
            })
            .unwrap_or((0, 0));

        RegistryStats {
            entry_count,
            total_bytes,
            dirty: self.is_dirty(),
            backend: self.backend.name().to_string(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, Weak};
    use std::thread;
    use web_time::Duration;

    #[derive(Default)]
    struct ReentrantFlushBackendState {
        registry: Mutex<Option<Weak<StateRegistry>>>,
        injected_during_save: AtomicBool,
        saved_entries: RwLock<HashMap<String, StoredEntry>>,
    }

    impl ReentrantFlushBackendState {
        fn bind_registry(&self, registry: &Arc<StateRegistry>) {
            *self.registry.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Arc::downgrade(registry));
        }

        fn saved_entries(&self) -> HashMap<String, StoredEntry> {
            self.saved_entries
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    #[derive(Clone)]
    struct ReentrantFlushBackend {
        state: Arc<ReentrantFlushBackendState>,
    }

    impl StorageBackend for ReentrantFlushBackend {
        fn name(&self) -> &str {
            "ReentrantFlushBackend"
        }

        fn load_all(&self) -> StorageResult<HashMap<String, StoredEntry>> {
            Ok(self.state.saved_entries())
        }

        fn save_all(&self, entries: &HashMap<String, StoredEntry>) -> StorageResult<()> {
            *self
                .state
                .saved_entries
                .write()
                .map_err(|_| StorageError::Corruption("saved entries lock poisoned".into()))? =
                entries.clone();

            if !self.state.injected_during_save.swap(true, Ordering::SeqCst)
                && let Some(registry) = self
                    .state
                    .registry
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .and_then(Weak::upgrade)
            {
                registry.set("backend::late", 2, b"late".to_vec());
            }

            Ok(())
        }

        fn clear(&self) -> StorageResult<()> {
            self.state
                .saved_entries
                .write()
                .map_err(|_| StorageError::Corruption("saved entries lock poisoned".into()))?
                .clear();
            Ok(())
        }
    }

    #[test]
    fn memory_storage_basic_operations() {
        let storage = MemoryStorage::new();

        // Initially empty
        let entries = storage.load_all().unwrap();
        assert!(entries.is_empty());

        // Save some entries
        let mut data = HashMap::new();
        data.insert(
            "key1".to_string(),
            StoredEntry {
                key: "key1".to_string(),
                version: 1,
                data: b"hello".to_vec(),
            },
        );
        storage.save_all(&data).unwrap();

        // Load back
        let loaded = storage.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["key1"].data, b"hello");

        // Clear
        storage.clear().unwrap();
        assert!(storage.load_all().unwrap().is_empty());
    }

    #[test]
    fn memory_storage_with_entries() {
        let mut entries = HashMap::new();
        entries.insert(
            "test".to_string(),
            StoredEntry {
                key: "test".to_string(),
                version: 2,
                data: vec![1, 2, 3],
            },
        );
        let storage = MemoryStorage::with_entries(entries);

        let loaded = storage.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["test"].version, 2);
    }

    #[test]
    fn registry_basic_operations() {
        let registry = StateRegistry::in_memory();

        // Initially empty
        assert!(registry.is_empty());
        assert!(!registry.is_dirty());

        // Set an entry
        registry.set("widget::1", 1, b"data".to_vec());
        assert_eq!(registry.len(), 1);
        assert!(registry.is_dirty());

        // Get the entry
        let entry = registry.get("widget::1").unwrap();
        assert_eq!(entry.version, 1);
        assert_eq!(entry.data, b"data");

        // Get non-existent
        assert!(registry.get("widget::99").is_none());

        // Flush
        assert!(registry.flush().unwrap());
        assert!(!registry.is_dirty());

        // No-op flush when clean
        assert!(!registry.flush().unwrap());

        // Remove
        let removed = registry.remove("widget::1").unwrap();
        assert_eq!(removed.data, b"data");
        assert!(registry.is_empty());
        assert!(registry.is_dirty());
    }

    #[test]
    fn registry_load_and_flush() {
        let storage = MemoryStorage::new();
        let mut initial = HashMap::new();
        initial.insert(
            "pre::existing".to_string(),
            StoredEntry {
                key: "pre::existing".to_string(),
                version: 5,
                data: b"old".to_vec(),
            },
        );
        storage.save_all(&initial).unwrap();

        let registry = StateRegistry::new(Box::new(storage));

        // Load existing data
        let count = registry.load().unwrap();
        assert_eq!(count, 1);
        assert!(!registry.is_dirty());

        let entry = registry.get("pre::existing").unwrap();
        assert_eq!(entry.version, 5);
    }

    #[test]
    fn registry_clear() {
        let registry = StateRegistry::in_memory();
        registry.set("a", 1, vec![]);
        registry.set("b", 1, vec![]);
        assert_eq!(registry.len(), 2);

        registry.clear().unwrap();
        assert!(registry.is_empty());
        assert!(!registry.is_dirty());
    }

    #[test]
    fn registry_keys() {
        let registry = StateRegistry::in_memory();
        registry.set("widget::a", 1, vec![]);
        registry.set("widget::b", 1, vec![]);

        let mut keys = registry.keys();
        keys.sort();
        assert_eq!(keys, vec!["widget::a", "widget::b"]);
    }

    #[test]
    fn registry_stats() {
        let registry = StateRegistry::in_memory();
        registry.set("x", 1, vec![1, 2, 3, 4, 5]);
        registry.set("y", 1, vec![6, 7, 8]);

        let stats = registry.stats();
        assert_eq!(stats.entry_count, 2);
        assert_eq!(stats.total_bytes, 8);
        assert!(stats.dirty);
        assert_eq!(stats.backend, "MemoryStorage");
    }

    #[test]
    fn registry_shared() {
        let registry = StateRegistry::in_memory().shared();
        registry.set("test", 1, vec![42]);

        let registry2 = Arc::clone(&registry);
        assert_eq!(registry2.get("test").unwrap().data, vec![42]);
    }

    #[test]
    fn storage_error_display() {
        let io_err = StorageError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
        assert!(io_err.to_string().contains("I/O error"));

        let corrupt = StorageError::Corruption("bad data".into());
        assert!(corrupt.to_string().contains("corruption"));

        let unavail = StorageError::Unavailable("no backend".into());
        assert!(unavail.to_string().contains("unavailable"));
    }

    // ── StorageError ────────────────────────────────────────────────────

    #[test]
    fn storage_error_source_io() {
        let err = StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "broken",
        ));
        let source = std::error::Error::source(&err);
        assert!(source.is_some());
    }

    #[test]
    fn storage_error_source_corruption_none() {
        let err = StorageError::Corruption("test".into());
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn storage_error_source_unavailable_none() {
        let err = StorageError::Unavailable("test".into());
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn storage_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout");
        let err: StorageError = io_err.into();
        match err {
            StorageError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::TimedOut),
            _ => panic!("expected Io variant"),
        }
    }

    #[test]
    fn storage_error_debug_format() {
        let err = StorageError::Corruption("test".into());
        let dbg = format!("{:?}", err);
        assert!(dbg.contains("Corruption"));
    }

    // ── MemoryStorage ───────────────────────────────────────────────────

    #[test]
    fn memory_storage_name() {
        let storage = MemoryStorage::new();
        assert_eq!(storage.name(), "MemoryStorage");
    }

    #[test]
    fn memory_storage_is_available() {
        let storage = MemoryStorage::new();
        assert!(storage.is_available());
    }

    #[test]
    fn memory_storage_debug_format() {
        let storage = MemoryStorage::new();
        let dbg = format!("{:?}", storage);
        assert!(dbg.contains("MemoryStorage"));
        assert!(dbg.contains("entries"));
    }

    #[test]
    fn memory_storage_debug_shows_count() {
        let mut entries = HashMap::new();
        entries.insert(
            "a".to_string(),
            StoredEntry {
                key: "a".to_string(),
                version: 1,
                data: vec![],
            },
        );
        let storage = MemoryStorage::with_entries(entries);
        let dbg = format!("{:?}", storage);
        assert!(dbg.contains("1"));
    }

    #[test]
    fn memory_storage_save_replaces_all() {
        let storage = MemoryStorage::new();

        let mut data1 = HashMap::new();
        data1.insert(
            "old".to_string(),
            StoredEntry {
                key: "old".to_string(),
                version: 1,
                data: vec![],
            },
        );
        storage.save_all(&data1).unwrap();

        let mut data2 = HashMap::new();
        data2.insert(
            "new".to_string(),
            StoredEntry {
                key: "new".to_string(),
                version: 2,
                data: vec![],
            },
        );
        storage.save_all(&data2).unwrap();

        let loaded = storage.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains_key("new"));
        assert!(!loaded.contains_key("old"));
    }

    // ── StateRegistry ───────────────────────────────────────────────────

    #[test]
    fn registry_backend_name() {
        let registry = StateRegistry::in_memory();
        assert_eq!(registry.backend_name(), "MemoryStorage");
    }

    #[test]
    fn registry_is_available() {
        let registry = StateRegistry::in_memory();
        assert!(registry.is_available());
    }

    #[test]
    fn registry_debug_format() {
        let registry = StateRegistry::in_memory();
        registry.set("x", 1, vec![]);
        let dbg = format!("{:?}", registry);
        assert!(dbg.contains("StateRegistry"));
        assert!(dbg.contains("MemoryStorage"));
        assert!(dbg.contains("dirty"));
    }

    #[test]
    fn registry_set_overwrites() {
        let registry = StateRegistry::in_memory();
        registry.set("k", 1, b"first".to_vec());
        registry.set("k", 2, b"second".to_vec());

        assert_eq!(registry.len(), 1);
        let entry = registry.get("k").unwrap();
        assert_eq!(entry.version, 2);
        assert_eq!(entry.data, b"second");
    }

    #[test]
    fn registry_remove_nonexistent_returns_none() {
        let registry = StateRegistry::in_memory();
        assert!(registry.remove("nonexistent").is_none());
    }

    #[test]
    fn registry_load_replaces_cache() {
        let storage = MemoryStorage::new();
        let mut initial = HashMap::new();
        initial.insert(
            "backend_key".to_string(),
            StoredEntry {
                key: "backend_key".to_string(),
                version: 1,
                data: b"from_backend".to_vec(),
            },
        );
        storage.save_all(&initial).unwrap();

        let registry = StateRegistry::new(Box::new(storage));
        registry.set("local_key", 1, b"local".to_vec());
        assert!(registry.get("local_key").is_some());

        // Load replaces entire cache
        registry.load().unwrap();
        assert!(registry.get("local_key").is_none());
        assert!(registry.get("backend_key").is_some());
    }

    #[test]
    fn registry_load_clears_dirty_flag() {
        let registry = StateRegistry::in_memory();
        registry.set("x", 1, vec![]);
        assert!(registry.is_dirty());

        registry.load().unwrap();
        assert!(!registry.is_dirty());
    }

    #[test]
    fn registry_flush_persists_to_backend() {
        let registry = StateRegistry::in_memory();
        registry.set("widget::foo", 3, b"bar".to_vec());
        registry.flush().unwrap();

        // Load fresh and verify data survived
        let count = registry.load().unwrap();
        assert_eq!(count, 1);
        let entry = registry.get("widget::foo").unwrap();
        assert_eq!(entry.version, 3);
        assert_eq!(entry.data, b"bar");
    }

    #[test]
    fn registry_flush_drops_cache_lock_before_backend_save() {
        let backend_state = Arc::new(ReentrantFlushBackendState::default());
        let registry = Arc::new(StateRegistry::new(Box::new(ReentrantFlushBackend {
            state: Arc::clone(&backend_state),
        })));
        backend_state.bind_registry(&registry);

        registry.set("widget::foo", 1, b"bar".to_vec());

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let registry_for_thread = Arc::clone(&registry);
        let handle = thread::spawn(move || {
            let result = registry_for_thread.flush();
            done_tx.send(result).expect("flush result");
        });

        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("flush should complete without deadlocking")
            .expect("flush succeeds");
        handle.join().expect("flush thread");

        let saved_entries = backend_state.saved_entries();
        assert!(saved_entries.contains_key("widget::foo"));
    }

    #[test]
    fn registry_flush_preserves_dirty_when_backend_mutates_registry() {
        let backend_state = Arc::new(ReentrantFlushBackendState::default());
        let registry = Arc::new(StateRegistry::new(Box::new(ReentrantFlushBackend {
            state: Arc::clone(&backend_state),
        })));
        backend_state.bind_registry(&registry);

        registry.set("widget::foo", 1, b"bar".to_vec());
        assert!(registry.flush().unwrap());

        let first_saved = backend_state.saved_entries();
        assert!(first_saved.contains_key("widget::foo"));
        assert!(!first_saved.contains_key("backend::late"));
        assert!(registry.is_dirty());
        assert_eq!(registry.get("backend::late").unwrap().data, b"late");

        assert!(registry.flush().unwrap());

        let second_saved = backend_state.saved_entries();
        assert!(second_saved.contains_key("backend::late"));
        assert!(!registry.is_dirty());
    }

    #[test]
    fn registry_multiple_keys() {
        let registry = StateRegistry::in_memory();
        registry.set("a", 1, vec![1]);
        registry.set("b", 2, vec![2]);
        registry.set("c", 3, vec![3]);

        assert_eq!(registry.len(), 3);
        assert!(!registry.is_empty());

        let mut keys = registry.keys();
        keys.sort();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn registry_remove_marks_dirty() {
        let registry = StateRegistry::in_memory();
        registry.set("x", 1, vec![]);
        registry.flush().unwrap();
        assert!(!registry.is_dirty());

        registry.remove("x");
        assert!(registry.is_dirty());
    }

    #[test]
    fn registry_clear_after_set_and_flush() {
        let registry = StateRegistry::in_memory();
        registry.set("a", 1, vec![]);
        registry.flush().unwrap();
        registry.clear().unwrap();

        assert!(registry.is_empty());
        assert!(!registry.is_dirty());

        // Backend is also cleared
        let count = registry.load().unwrap();
        assert_eq!(count, 0);
    }

    // ── RegistryStats ───────────────────────────────────────────────────

    #[test]
    fn registry_stats_default() {
        let stats = RegistryStats::default();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.total_bytes, 0);
        assert!(!stats.dirty);
        assert_eq!(stats.backend, "");
    }

    #[test]
    fn registry_stats_empty() {
        let registry = StateRegistry::in_memory();
        let stats = registry.stats();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.total_bytes, 0);
        assert!(!stats.dirty);
    }

    // ── StoredEntry ─────────────────────────────────────────────────────

    #[test]
    fn stored_entry_clone() {
        let entry = StoredEntry {
            key: "test".to_string(),
            version: 7,
            data: vec![1, 2, 3],
        };
        let cloned = entry.clone();
        assert_eq!(cloned.key, "test");
        assert_eq!(cloned.version, 7);
        assert_eq!(cloned.data, vec![1, 2, 3]);
    }

    #[test]
    fn stored_entry_debug() {
        let entry = StoredEntry {
            key: "k".to_string(),
            version: 1,
            data: vec![],
        };
        let dbg = format!("{:?}", entry);
        assert!(dbg.contains("StoredEntry"));
    }

    // ── Arc shared access ───────────────────────────────────────────────

    #[test]
    fn registry_shared_concurrent_access() {
        let registry = StateRegistry::in_memory().shared();
        let r2 = Arc::clone(&registry);

        registry.set("from_1", 1, vec![10]);
        r2.set("from_2", 1, vec![20]);

        assert_eq!(registry.len(), 2);
        assert!(r2.get("from_1").is_some());
        assert!(registry.get("from_2").is_some());
    }
}

#[cfg(all(test, feature = "state-persistence"))]
mod file_storage_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn file_storage_round_trip() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-state-persistence-")
            .disable_cleanup(true)
            .tempdir()
            .unwrap();
        let path = tmp.path().join("state.json");
        let storage = FileStorage::new(&path);

        // Save
        let mut entries = HashMap::new();
        entries.insert(
            "widget::test".to_string(),
            StoredEntry {
                key: "widget::test".to_string(),
                version: 3,
                data: b"hello world".to_vec(),
            },
        );
        storage.save_all(&entries).unwrap();

        // File should exist
        assert!(path.exists());

        // Load back
        let loaded = storage.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["widget::test"].version, 3);
        assert_eq!(loaded["widget::test"].data, b"hello world");
    }

    #[test]
    fn file_storage_load_nonexistent() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-state-persistence-")
            .disable_cleanup(true)
            .tempdir()
            .unwrap();
        let path = tmp.path().join("does_not_exist.json");
        let storage = FileStorage::new(&path);

        let entries = storage.load_all().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn file_storage_clear() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-state-persistence-")
            .disable_cleanup(true)
            .tempdir()
            .unwrap();
        let path = tmp.path().join("state.json");

        // Create file
        std::fs::write(&path, "{}").unwrap();
        assert!(path.exists());

        let storage = FileStorage::new(&path);
        storage.clear().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn file_storage_creates_parent_dirs() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-state-persistence-")
            .disable_cleanup(true)
            .tempdir()
            .unwrap();
        let path = tmp.path().join("nested").join("dirs").join("state.json");
        let storage = FileStorage::new(&path);

        let mut entries = HashMap::new();
        entries.insert(
            "k".to_string(),
            StoredEntry {
                key: "k".to_string(),
                version: 1,
                data: vec![],
            },
        );
        storage.save_all(&entries).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn file_storage_handles_corrupt_entry() {
        let tmp = tempfile::Builder::new()
            .prefix("ftui-state-persistence-")
            .disable_cleanup(true)
            .tempdir()
            .unwrap();
        let path = tmp.path().join("state.json");

        // Write valid JSON but with invalid base64
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"format_version":1,"entries":{{"bad":{{"version":1,"data_base64":"!!invalid!!"}},"good":{{"version":1,"data_base64":"aGVsbG8="}}}}}}"#
        )
        .unwrap();

        let storage = FileStorage::new(&path);
        let loaded = storage.load_all().unwrap();

        // Bad entry skipped, good entry loaded
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains_key("good"));
        assert_eq!(loaded["good"].data, b"hello");
    }
}
