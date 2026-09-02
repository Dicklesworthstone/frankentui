#![forbid(unsafe_code)]

//! Keybinding sequence detection and action mapping.
//!
//! This module implements the keybinding policy specification (bd-2vne.1) for
//! detecting multi-key sequences like Esc Esc and mapping keys to actions based
//! on application state.
//!
//! # Key Concepts
//!
//! - **SequenceDetector**: State machine that detects Esc Esc sequences with
//!   configurable timeout. Single Esc is emitted after timeout or when another
//!   key is pressed.
//!
//! - **SequenceConfig**: Configuration for sequence detection including timeout
//!   windows and debounce settings.
//!
//! - **ActionMapper**: Maps key events to high-level actions based on application
//!   state (input buffer, running tasks, modals, overlays). Integrates with
//!   SequenceDetector to handle Esc sequences.
//!
//! - **AppState**: Runtime state flags that affect action resolution.
//!
//! - **Action**: High-level commands like ClearInput, CancelTask, ToggleTreeView.
//!
//! # State Machine
//!
//! ```text
//!                                     ┌─────────────────────────────────────┐
//!                                     │                                     │
//!                                     ▼                                     │
//! ┌──────────┐   Esc   ┌────────────────────┐  timeout    ┌─────────┐      │
//! │  Idle    │───────▶│  AwaitingSecondEsc  │────────────▶│ Emit(Esc)│      │
//! └──────────┘         └────────────────────┘              └─────────┘      │
//!      ▲                        │                                           │
//!      │                        │ Esc (within timeout)                      │
//!      │                        ▼                                           │
//!      │               ┌─────────────────┐                                  │
//!      │               │ Emit(EscEsc)    │──────────────────────────────────┘
//!      │               └─────────────────┘
//!      │
//!      │  other key
//!      └───────────────────────────────────────────────────────────────────
//! ```
//!
//! # Example
//!
//! ```
//! use std::time::{Duration, Instant};
//! use ftui_core::keybinding::{SequenceDetector, SequenceConfig, SequenceOutput};
//! use ftui_core::event::{KeyCode, KeyEvent, Modifiers, KeyEventKind};
//!
//! let mut detector = SequenceDetector::new(SequenceConfig::default());
//! let now = Instant::now();
//!
//! // First Esc: starts the sequence
//! let esc = KeyEvent::new(KeyCode::Escape);
//! let output = detector.feed(&esc, now);
//! assert!(matches!(output, SequenceOutput::Pending));
//!
//! // Second Esc within timeout: emits EscEsc
//! let later = now + Duration::from_millis(100);
//! let output = detector.feed(&esc, later);
//! assert!(matches!(output, SequenceOutput::EscEsc));
//! ```
//!
//! # Action Mapping Example
//!
//! ```
//! use std::time::Instant;
//! use ftui_core::keybinding::{ActionMapper, ActionConfig, AppState, Action};
//! use ftui_core::event::{KeyCode, KeyEvent, Modifiers};
//!
//! let mut mapper = ActionMapper::new(ActionConfig::default());
//! let now = Instant::now();
//!
//! // Ctrl+C with non-empty input: clears input
//! let state = AppState { input_nonempty: true, ..Default::default() };
//! let ctrl_c = KeyEvent::new(KeyCode::Char('c')).with_modifiers(Modifiers::CTRL);
//! let action = mapper.map(&ctrl_c, &state, now);
//! assert!(matches!(action, Some(Action::ClearInput)));
//!
//! // Ctrl+C with empty input and no task: quits (by default)
//! let idle_state = AppState::default();
//! let action = mapper.map(&ctrl_c, &idle_state, now);
//! assert!(matches!(action, Some(Action::Quit)));
//! ```

use web_time::{Duration, Instant};

use crate::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

// ---------------------------------------------------------------------------
// Configuration Constants
// ---------------------------------------------------------------------------

/// Default timeout for detecting Esc Esc sequence.
pub const DEFAULT_ESC_SEQ_TIMEOUT_MS: u64 = 250;

/// Minimum allowed value for Esc sequence timeout.
pub const MIN_ESC_SEQ_TIMEOUT_MS: u64 = 150;

/// Maximum allowed value for Esc sequence timeout.
pub const MAX_ESC_SEQ_TIMEOUT_MS: u64 = 400;

/// Default debounce before emitting single Esc.
pub const DEFAULT_ESC_DEBOUNCE_MS: u64 = 50;

/// Minimum allowed value for Esc debounce.
pub const MIN_ESC_DEBOUNCE_MS: u64 = 0;

/// Maximum allowed value for Esc debounce.
pub const MAX_ESC_DEBOUNCE_MS: u64 = 100;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the sequence detector.
///
/// # Timing Defaults
///
/// | Setting | Default | Range | Description |
/// |---------|---------|-------|-------------|
/// | `esc_seq_timeout` | 250ms | 150-400ms | Window for detecting Esc Esc |
/// | `esc_debounce` | 50ms | 0-100ms | Minimum wait before single Esc |
///
/// # Environment Variables
///
/// | Variable | Type | Default | Description |
/// |----------|------|---------|-------------|
/// | `FTUI_ESC_SEQ_TIMEOUT_MS` | u64 | 250 | Esc Esc detection window |
/// | `FTUI_ESC_DEBOUNCE_MS` | u64 | 50 | Minimum Esc wait |
/// | `FTUI_DISABLE_ESC_SEQ` | bool | false | Disable multi-key sequences |
///
/// # Example
///
/// ```bash
/// # Faster double-tap detection (200ms window)
/// export FTUI_ESC_SEQ_TIMEOUT_MS=200
///
/// # Disable Esc Esc entirely (for strict terminals)
/// export FTUI_DISABLE_ESC_SEQ=1
/// ```
#[derive(Debug, Clone)]
pub struct SequenceConfig {
    /// Maximum gap between Esc presses to detect Esc Esc sequence.
    /// Default: 250ms.
    pub esc_seq_timeout: Duration,

    /// Minimum debounce before emitting single Esc.
    /// Default: 50ms.
    pub esc_debounce: Duration,

    /// Whether to disable multi-key sequences entirely.
    /// When true, all Esc keys are immediately emitted as single Esc.
    /// Default: false.
    pub disable_sequences: bool,
}

impl Default for SequenceConfig {
    fn default() -> Self {
        Self {
            esc_seq_timeout: Duration::from_millis(DEFAULT_ESC_SEQ_TIMEOUT_MS),
            esc_debounce: Duration::from_millis(DEFAULT_ESC_DEBOUNCE_MS),
            disable_sequences: false,
        }
    }
}

impl SequenceConfig {
    /// Create a new config with custom timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.esc_seq_timeout = timeout;
        self
    }

    /// Create a new config with custom debounce.
    #[must_use]
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.esc_debounce = debounce;
        self
    }

    /// Disable sequence detection (treat all Esc as single).
    #[must_use]
    pub fn disable_sequences(mut self) -> Self {
        self.disable_sequences = true;
        self
    }

    /// Load config from environment variables.
    ///
    /// Reads:
    /// - `FTUI_ESC_SEQ_TIMEOUT_MS`: Esc Esc detection window in milliseconds
    /// - `FTUI_ESC_DEBOUNCE_MS`: Minimum Esc wait in milliseconds
    /// - `FTUI_DISABLE_ESC_SEQ`: Set to "1" or "true" to disable sequences
    ///
    /// Values are automatically clamped to valid ranges.
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(val) = std::env::var("FTUI_ESC_SEQ_TIMEOUT_MS")
            && let Ok(ms) = val.parse::<u64>()
        {
            config.esc_seq_timeout = Duration::from_millis(ms);
        }

        if let Ok(val) = std::env::var("FTUI_ESC_DEBOUNCE_MS")
            && let Ok(ms) = val.parse::<u64>()
        {
            config.esc_debounce = Duration::from_millis(ms);
        }

        if let Ok(val) = std::env::var("FTUI_DISABLE_ESC_SEQ") {
            config.disable_sequences = val == "1" || val.eq_ignore_ascii_case("true");
        }

        config.validated()
    }

    /// Validate and clamp values to safe ranges.
    ///
    /// Returns a new config with:
    /// - `esc_seq_timeout` clamped to 150-400ms
    /// - `esc_debounce` clamped to 0-100ms
    /// - `esc_debounce` <= `esc_seq_timeout` (debounce is capped at timeout)
    ///
    /// # Example
    ///
    /// ```
    /// use ftui_core::keybinding::SequenceConfig;
    /// use std::time::Duration;
    ///
    /// let config = SequenceConfig::default()
    ///     .with_timeout(Duration::from_millis(1000))  // Too high
    ///     .validated();
    ///
    /// // Clamped to max 400ms
    /// assert_eq!(config.esc_seq_timeout.as_millis(), 400);
    /// ```
    #[must_use]
    pub fn validated(mut self) -> Self {
        // Clamp timeout to valid range
        let timeout_ms = self.esc_seq_timeout.as_millis() as u64;
        let clamped_timeout = timeout_ms.clamp(MIN_ESC_SEQ_TIMEOUT_MS, MAX_ESC_SEQ_TIMEOUT_MS);
        self.esc_seq_timeout = Duration::from_millis(clamped_timeout);

        // Clamp debounce to valid range
        let debounce_ms = self.esc_debounce.as_millis() as u64;
        let clamped_debounce = debounce_ms.clamp(MIN_ESC_DEBOUNCE_MS, MAX_ESC_DEBOUNCE_MS);

        // Ensure debounce <= timeout (debounce shouldn't exceed the timeout window)
        let final_debounce = clamped_debounce.min(clamped_timeout);
        self.esc_debounce = Duration::from_millis(final_debounce);

        self
    }

    /// Check if values are within valid ranges.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let timeout_ms = self.esc_seq_timeout.as_millis() as u64;
        let debounce_ms = self.esc_debounce.as_millis() as u64;

        (MIN_ESC_SEQ_TIMEOUT_MS..=MAX_ESC_SEQ_TIMEOUT_MS).contains(&timeout_ms)
            && (MIN_ESC_DEBOUNCE_MS..=MAX_ESC_DEBOUNCE_MS).contains(&debounce_ms)
            && debounce_ms <= timeout_ms
    }
}

// ---------------------------------------------------------------------------
// Sequence Output
// ---------------------------------------------------------------------------

/// Output from the sequence detector after processing a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceOutput {
    /// No action yet; waiting for timeout or more input.
    Pending,

    /// Single Escape key was detected.
    Esc,

    /// Double Escape (Esc Esc) sequence was detected.
    EscEsc,

    /// Pass through the original key event (not part of a sequence).
    PassThrough,
}

// ---------------------------------------------------------------------------
// Sequence Detector
// ---------------------------------------------------------------------------

/// Internal state of the sequence detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectorState {
    /// Idle: waiting for input.
    Idle,

    /// First Esc received; waiting for second or timeout.
    AwaitingSecondEsc { first_esc_time: Instant },
}

/// Stateful detector for multi-key sequences (currently Esc Esc).
///
/// This detector transforms a stream of [`KeyEvent`]s into [`SequenceOutput`]s,
/// detecting Esc Esc sequences with configurable timeout handling.
///
/// # Usage
///
/// Call [`feed`](SequenceDetector::feed) for each key event. The detector returns:
/// - `Pending`: First Esc received, waiting for more input or timeout.
/// - `Esc`: Single Esc was detected (after timeout or other key).
/// - `EscEsc`: Double Esc sequence was detected.
/// - `PassThrough`: Key is not Esc, pass through to normal handling.
///
/// Call [`check_timeout`](SequenceDetector::check_timeout) periodically (e.g., on
/// tick) to emit pending single Esc after timeout expires.
#[derive(Debug)]
pub struct SequenceDetector {
    config: SequenceConfig,
    state: DetectorState,
}

impl SequenceDetector {
    /// Create a new sequence detector with the given configuration.
    #[must_use]
    pub fn new(config: SequenceConfig) -> Self {
        Self {
            config,
            state: DetectorState::Idle,
        }
    }

    /// Create a new sequence detector with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(SequenceConfig::default())
    }

    /// Process a key event and return the sequence output.
    ///
    /// Only key press events are considered; repeat and release are ignored.
    pub fn feed(&mut self, event: &KeyEvent, now: Instant) -> SequenceOutput {
        // Only process press events
        if event.kind != KeyEventKind::Press {
            return SequenceOutput::PassThrough;
        }

        // If sequences are disabled, handle Esc immediately
        if self.config.disable_sequences {
            return if event.code == KeyCode::Escape {
                SequenceOutput::Esc
            } else {
                SequenceOutput::PassThrough
            };
        }

        match self.state {
            DetectorState::Idle => {
                if event.code == KeyCode::Escape {
                    // First Esc: transition to awaiting second
                    self.state = DetectorState::AwaitingSecondEsc {
                        first_esc_time: now,
                    };
                    SequenceOutput::Pending
                } else {
                    // Non-Esc key: pass through
                    SequenceOutput::PassThrough
                }
            }

            DetectorState::AwaitingSecondEsc { first_esc_time } => {
                let elapsed = now.saturating_duration_since(first_esc_time);

                if event.code == KeyCode::Escape {
                    // Second Esc received
                    if elapsed <= self.config.esc_seq_timeout {
                        // Within timeout: emit EscEsc
                        self.state = DetectorState::Idle;
                        SequenceOutput::EscEsc
                    } else {
                        // Past timeout: first Esc already timed out, this starts new
                        self.state = DetectorState::AwaitingSecondEsc {
                            first_esc_time: now,
                        };
                        SequenceOutput::Esc
                    }
                } else {
                    // Other key received: emit pending Esc, then pass through
                    // The caller should handle the Esc first, then re-feed this key
                    self.state = DetectorState::Idle;
                    // Return Esc; caller must re-feed the current key
                    SequenceOutput::Esc
                }
            }
        }
    }

    /// Check for timeout and emit pending Esc if expired.
    ///
    /// Call this periodically (e.g., on tick) to handle the case where
    /// the user pressed Esc once and is waiting.
    ///
    /// Returns `Some(SequenceOutput::Esc)` if timeout expired,
    /// `None` otherwise.
    pub fn check_timeout(&mut self, now: Instant) -> Option<SequenceOutput> {
        if let DetectorState::AwaitingSecondEsc { first_esc_time } = self.state {
            let elapsed = now.saturating_duration_since(first_esc_time);
            if elapsed > self.config.esc_seq_timeout {
                self.state = DetectorState::Idle;
                return Some(SequenceOutput::Esc);
            }
        }
        None
    }

    /// Whether the detector is waiting for a second Esc.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        matches!(self.state, DetectorState::AwaitingSecondEsc { .. })
    }

    /// Reset the detector to idle state.
    ///
    /// Any pending Esc is discarded.
    pub fn reset(&mut self) {
        self.state = DetectorState::Idle;
    }

    /// Get a reference to the current configuration.
    #[must_use]
    pub fn config(&self) -> &SequenceConfig {
        &self.config
    }

    /// Update the configuration.
    ///
    /// Does not reset pending state.
    pub fn set_config(&mut self, config: SequenceConfig) {
        self.config = config;
    }
}

// ---------------------------------------------------------------------------
// Application State
// ---------------------------------------------------------------------------

/// Runtime state flags that affect keybinding resolution.
///
/// These flags are queried at the moment a key event is resolved to an action.
/// The priority of actions changes based on these flags per the policy spec.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppState {
    /// True if the text input buffer contains characters.
    pub input_nonempty: bool,

    /// True if a background task/command is executing.
    pub task_running: bool,

    /// True if a modal dialog or overlay is visible.
    pub modal_open: bool,

    /// True if a secondary view (tree, debug, HUD) is active.
    pub view_overlay: bool,
}

impl AppState {
    /// Create a new state with all flags false.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            input_nonempty: false,
            task_running: false,
            modal_open: false,
            view_overlay: false,
        }
    }

    /// Set input_nonempty flag.
    #[must_use]
    pub const fn with_input(mut self, nonempty: bool) -> Self {
        self.input_nonempty = nonempty;
        self
    }

    /// Set task_running flag.
    #[must_use]
    pub const fn with_task(mut self, running: bool) -> Self {
        self.task_running = running;
        self
    }

    /// Set modal_open flag.
    #[must_use]
    pub const fn with_modal(mut self, open: bool) -> Self {
        self.modal_open = open;
        self
    }

    /// Set view_overlay flag.
    #[must_use]
    pub const fn with_overlay(mut self, active: bool) -> Self {
        self.view_overlay = active;
        self
    }

    /// Check if in idle state (no input, no task, no modal).
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        !self.input_nonempty && !self.task_running && !self.modal_open
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// High-level actions that can result from keybinding resolution.
///
/// These actions are returned by the [`ActionMapper`] and should be handled
/// by the application's event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Empty the input buffer, keep cursor at start.
    ClearInput,

    /// Send cancel signal to running task, update status.
    CancelTask,

    /// Close topmost modal, return focus to parent.
    DismissModal,

    /// Deactivate view overlay (tree view, debug HUD).
    CloseOverlay,

    /// Toggle the tree/file view overlay.
    ToggleTreeView,

    /// Clean exit via quit command.
    Quit,

    /// Quit if idle, otherwise cancel current operation.
    SoftQuit,

    /// Immediate quit (bypass confirmation if any).
    HardQuit,

    /// Emit terminal bell (BEL character).
    Bell,

    /// Forward event to focused widget/input.
    ///
    /// This indicates the key should be passed through to normal input handling.
    PassThrough,
}

impl Action {
    /// Check if this action consumes the event (vs passing through).
    #[must_use]
    pub const fn consumes_event(&self) -> bool {
        !matches!(self, Action::PassThrough)
    }

    /// Check if this is a quit-related action.
    #[must_use]
    pub const fn is_quit(&self) -> bool {
        matches!(self, Action::Quit | Action::SoftQuit | Action::HardQuit)
    }
}

// ---------------------------------------------------------------------------
// Ctrl+C Idle Action
// ---------------------------------------------------------------------------

/// Behavior when Ctrl+C is pressed with empty input and no running task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CtrlCIdleAction {
    /// Exit the application.
    #[default]
    Quit,

    /// Do nothing.
    Noop,

    /// Emit terminal bell (BEL).
    Bell,
}

impl CtrlCIdleAction {
    /// Parse from string (environment variable value).
    #[must_use]
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "quit" => Some(Self::Quit),
            "noop" | "none" | "ignore" => Some(Self::Noop),
            "bell" | "beep" => Some(Self::Bell),
            _ => None,
        }
    }

    /// Convert to the corresponding Action (or None for Noop).
    #[must_use]
    pub const fn to_action(self) -> Option<Action> {
        match self {
            Self::Quit => Some(Action::Quit),
            Self::Noop => None,
            Self::Bell => Some(Action::Bell),
        }
    }
}

// ---------------------------------------------------------------------------
// Action Configuration
// ---------------------------------------------------------------------------

/// Configuration for action mapping behavior.
///
/// This struct combines sequence detection settings with keybinding behavior
/// configuration. It controls how keys like Ctrl+C, Ctrl+D, Esc, and Esc Esc
/// are interpreted based on application state.
///
/// # Environment Variables
///
/// | Variable | Type | Default | Description |
/// |----------|------|---------|-------------|
/// | `FTUI_CTRL_C_IDLE_ACTION` | string | "quit" | Action when Ctrl+C in idle state |
/// | `FTUI_ESC_SEQ_TIMEOUT_MS` | u64 | 250 | Esc Esc detection window |
/// | `FTUI_ESC_DEBOUNCE_MS` | u64 | 50 | Minimum Esc wait |
/// | `FTUI_DISABLE_ESC_SEQ` | bool | false | Disable Esc Esc sequences |
///
/// # Example: Configure via environment
///
/// ```bash
/// # Make Ctrl+C do nothing when idle (instead of quit)
/// export FTUI_CTRL_C_IDLE_ACTION=noop
///
/// # Or make it beep
/// export FTUI_CTRL_C_IDLE_ACTION=bell
///
/// # Faster double-Esc detection
/// export FTUI_ESC_SEQ_TIMEOUT_MS=200
/// ```
///
/// # Example: Configure in code
///
/// ```
/// use ftui_core::keybinding::{ActionConfig, CtrlCIdleAction, SequenceConfig};
/// use std::time::Duration;
///
/// let config = ActionConfig::default()
///     .with_ctrl_c_idle(CtrlCIdleAction::Bell)
///     .with_sequence_config(
///         SequenceConfig::default()
///             .with_timeout(Duration::from_millis(200))
///     );
/// ```
#[derive(Debug, Clone)]
pub struct ActionConfig {
    /// Sequence detection configuration (timeouts, debounce, disable flag).
    pub sequence_config: SequenceConfig,

    /// Action when Ctrl+C pressed with empty input and no task.
    ///
    /// - `Quit` (default): Exit the application
    /// - `Noop`: Do nothing
    /// - `Bell`: Emit terminal bell
    pub ctrl_c_idle_action: CtrlCIdleAction,
}

impl Default for ActionConfig {
    fn default() -> Self {
        Self {
            sequence_config: SequenceConfig::default(),
            ctrl_c_idle_action: CtrlCIdleAction::Quit,
        }
    }
}

impl ActionConfig {
    /// Create config with custom sequence settings.
    #[must_use]
    pub fn with_sequence_config(mut self, config: SequenceConfig) -> Self {
        self.sequence_config = config;
        self
    }

    /// Set Ctrl+C idle action.
    #[must_use]
    pub fn with_ctrl_c_idle(mut self, action: CtrlCIdleAction) -> Self {
        self.ctrl_c_idle_action = action;
        self
    }

    /// Load config from environment variables.
    ///
    /// Reads:
    /// - `FTUI_CTRL_C_IDLE_ACTION`: "quit", "noop", or "bell"
    /// - Plus all environment variables from [`SequenceConfig::from_env`]
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self {
            sequence_config: SequenceConfig::from_env(),
            ctrl_c_idle_action: CtrlCIdleAction::Quit,
        };

        if let Ok(val) = std::env::var("FTUI_CTRL_C_IDLE_ACTION")
            && let Some(action) = CtrlCIdleAction::from_str_opt(&val)
        {
            config.ctrl_c_idle_action = action;
        }

        config
    }

    /// Validate and return a config with clamped sequence values.
    ///
    /// Delegates to [`SequenceConfig::validated`] for timing bounds.
    #[must_use]
    pub fn validated(mut self) -> Self {
        self.sequence_config = self.sequence_config.validated();
        self
    }
}

// ---------------------------------------------------------------------------
// Action Mapper
// ---------------------------------------------------------------------------

/// Maps key events to high-level actions based on application state.
///
/// The `ActionMapper` integrates the sequence detector and implements the
/// priority table from the keybinding policy specification (bd-2vne.1).
///
/// # Priority Order
///
/// Actions are resolved in priority order (first match wins):
///
/// | Priority | Condition | Key | Action |
/// |----------|-----------|-----|--------|
/// | 1 | `modal_open` | Esc | DismissModal |
/// | 2 | `modal_open` | Ctrl+C | DismissModal |
/// | 3 | `input_nonempty` | Ctrl+C | ClearInput |
/// | 4 | `task_running` | Ctrl+C | CancelTask |
/// | 5 | idle | Ctrl+C | Quit (configurable) |
/// | 6 | `view_overlay` | Esc | CloseOverlay |
/// | 7 | `input_nonempty` | Esc | ClearInput |
/// | 8 | `task_running` | Esc | CancelTask |
/// | 9 | always | Esc Esc | ToggleTreeView |
/// | 10 | always | Ctrl+D | SoftQuit |
/// | 11 | always | Ctrl+Q | HardQuit |
///
/// # Usage
///
/// ```
/// use std::time::Instant;
/// use ftui_core::keybinding::{ActionMapper, ActionConfig, AppState, Action};
/// use ftui_core::event::{KeyCode, KeyEvent, Modifiers};
///
/// let mut mapper = ActionMapper::new(ActionConfig::default());
/// let now = Instant::now();
/// let state = AppState::default();
///
/// let key = KeyEvent::new(KeyCode::Char('q')).with_modifiers(Modifiers::CTRL);
/// let action = mapper.map(&key, &state, now);
/// assert!(matches!(action, Some(Action::HardQuit)));
/// ```
#[derive(Debug)]
pub struct ActionMapper {
    config: ActionConfig,
    sequence_detector: SequenceDetector,
}

impl ActionMapper {
    /// Create a new action mapper with the given configuration.
    #[must_use]
    pub fn new(config: ActionConfig) -> Self {
        let sequence_detector = SequenceDetector::new(config.sequence_config.clone());
        Self {
            config,
            sequence_detector,
        }
    }

    /// Create a new action mapper with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(ActionConfig::default())
    }

    /// Create a new action mapper loading config from environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(ActionConfig::from_env())
    }

    /// Map a key event to an action based on current application state.
    ///
    /// Returns `Some(action)` if the key resolves to an action, or `None`
    /// if the event should be ignored (e.g., Noop on Ctrl+C when idle).
    ///
    /// # Arguments
    ///
    /// * `event` - The key event to process
    /// * `state` - Current application state flags
    /// * `now` - Current timestamp for sequence detection
    pub fn map(&mut self, event: &KeyEvent, state: &AppState, now: Instant) -> Option<Action> {
        // Only process press events
        if event.kind != KeyEventKind::Press {
            return Some(Action::PassThrough);
        }

        // Check for Ctrl+C, Ctrl+D, Ctrl+Q first (they don't participate in sequences)
        if event.modifiers.contains(Modifiers::CTRL)
            && let KeyCode::Char(c) = event.code
        {
            match c.to_ascii_lowercase() {
                'c' => return self.resolve_ctrl_c(state),
                'd' => return Some(Action::SoftQuit),
                'q' => return Some(Action::HardQuit),
                _ => {}
            }
        }

        // Handle Escape through sequence detector
        if event.code == KeyCode::Escape && event.modifiers == Modifiers::NONE {
            return self.handle_esc_sequence(state, now);
        }

        // For non-Esc keys, check if we have a pending Esc
        let seq_output = self.sequence_detector.feed(event, now);
        match seq_output {
            SequenceOutput::Esc => {
                // Pending Esc was interrupted; resolve it and note the key is consumed
                // The caller should re-feed the current key after handling Esc
                // For now we return the Esc action; the current key is lost
                // This matches the spec: "emit pending Esc first, then process"
                self.resolve_single_esc(state)
            }
            SequenceOutput::Pending => {
                // Should not happen for non-Esc keys
                Some(Action::PassThrough)
            }
            SequenceOutput::EscEsc => {
                // Should not happen for non-Esc keys
                Some(Action::ToggleTreeView)
            }
            SequenceOutput::PassThrough => Some(Action::PassThrough),
        }
    }

    /// Handle Escape key through the sequence detector.
    fn handle_esc_sequence(&mut self, state: &AppState, now: Instant) -> Option<Action> {
        let esc_event = KeyEvent::new(KeyCode::Escape);
        let output = self.sequence_detector.feed(&esc_event, now);

        match output {
            SequenceOutput::Pending => {
                // First Esc received, waiting for second
                // Don't emit action yet; the event loop should call check_timeout
                None
            }
            SequenceOutput::Esc => {
                // Single Esc detected (either timeout or past timeout second Esc)
                self.resolve_single_esc(state)
            }
            SequenceOutput::EscEsc => {
                // Double Esc sequence detected
                Some(Action::ToggleTreeView)
            }
            SequenceOutput::PassThrough => {
                // Should not happen for Esc
                Some(Action::PassThrough)
            }
        }
    }

    /// Resolve Ctrl+C based on state.
    fn resolve_ctrl_c(&self, state: &AppState) -> Option<Action> {
        // Priority 2: modal_open -> DismissModal
        if state.modal_open {
            return Some(Action::DismissModal);
        }

        // Priority 3: input_nonempty -> ClearInput
        if state.input_nonempty {
            return Some(Action::ClearInput);
        }

        // Priority 4: task_running -> CancelTask
        if state.task_running {
            return Some(Action::CancelTask);
        }

        // Priority 5: idle -> configurable action
        self.config.ctrl_c_idle_action.to_action()
    }

    /// Resolve single Esc based on state.
    fn resolve_single_esc(&self, state: &AppState) -> Option<Action> {
        // Priority 1: modal_open -> DismissModal
        if state.modal_open {
            return Some(Action::DismissModal);
        }

        // Priority 6: view_overlay -> CloseOverlay
        if state.view_overlay {
            return Some(Action::CloseOverlay);
        }

        // Priority 7: input_nonempty -> ClearInput
        if state.input_nonempty {
            return Some(Action::ClearInput);
        }

        // Priority 8: task_running -> CancelTask
        if state.task_running {
            return Some(Action::CancelTask);
        }

        // No action for Esc in idle state
        Some(Action::PassThrough)
    }

    /// Check for sequence timeout and return pending action if expired.
    ///
    /// Call this periodically (e.g., on tick) to handle single Esc after
    /// the timeout window closes.
    ///
    /// # Arguments
    ///
    /// * `state` - Current application state flags
    /// * `now` - Current timestamp
    pub fn check_timeout(&mut self, state: &AppState, now: Instant) -> Option<Action> {
        if let Some(SequenceOutput::Esc) = self.sequence_detector.check_timeout(now) {
            return self.resolve_single_esc(state);
        }
        None
    }

    /// Whether the mapper is waiting for a second Esc.
    #[must_use]
    pub fn is_pending_esc(&self) -> bool {
        self.sequence_detector.is_pending()
    }

    /// Reset the sequence detector state.
    ///
    /// Any pending Esc is discarded.
    pub fn reset(&mut self) {
        self.sequence_detector.reset();
    }

    /// Get a reference to the current configuration.
    #[must_use]
    pub fn config(&self) -> &ActionConfig {
        &self.config
    }

    /// Update the configuration.
    pub fn set_config(&mut self, config: ActionConfig) {
        self.sequence_detector
            .set_config(config.sequence_config.clone());
        self.config = config;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ===========================================================================
// Declarative keymaps: combos, chords, priorities, contexts, conflict
// detection, and a chord-aware dispatcher
// ===========================================================================
//
// Resolution order, in prose (the keybinding policy spec copies this):
//
// 1. A key press extends the pending prefix. If the extended chord is bound
//    and no longer bound chord starts with it, the binding fires at once.
//    If a longer bound chord starts with it (`g` while `g g` is bound), the
//    dispatcher waits: the exact binding fires on the chord timeout or when a
//    key arrives that cannot extend the chord, so single-key shortcuts are
//    never blocked, only delayed while a real chord is possible.
// 2. A key that cannot extend the pending prefix flushes it (the prefix
//    fires if it is bound, otherwise it is reported as expired) and is then
//    processed on its own.
// 3. Among bindings for the same chord, one attached to an active context
//    beats a context-free one, then the higher `Priority` wins, then the most
//    recently bound. `KeyMap::conflicts` reports every case that needs the
//    tie-break so shadowing is visible instead of silent.
// 4. `Repeat` events re-fire a single-key binding but never extend a chord;
//    `Release` events are reported as unbound.
// 5. `Esc` goes through the embedded `SequenceDetector` (one Esc timer per
//    dispatcher); `Esc` and `Esc Esc` can be bound like any chord.

use std::fmt;
use std::str::FromStr;

/// Why a key, combo, or chord string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyParseError {
    /// The key name (or the whole chord) was empty.
    EmptyKey,
    /// A key name that matches no [`KeyCode`].
    UnknownKey(String),
    /// A modifier name other than `Ctrl`, `Alt`, `Shift`, `Super`.
    UnknownModifier(String),
    /// More than [`Chord::MAX_LEN`] combos in one chord.
    TooManyKeys(usize),
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyKey => f.write_str("empty key"),
            Self::UnknownKey(name) => write!(f, "unknown key `{name}`"),
            Self::UnknownModifier(name) => write!(f, "unknown modifier `{name}`"),
            Self::TooManyKeys(n) => {
                write!(f, "chord has {n} keys; the maximum is {}", Chord::MAX_LEN)
            }
        }
    }
}

impl std::error::Error for KeyParseError {}

/// A single key press with its modifiers (`Ctrl+x`, `Shift+Tab`, `F12`, `g`).
///
/// Combos are normalized so that `Shift+a`, `A`, and a terminal that reports
/// `Char('A')` with the Shift bit all compare equal: alphabetic characters
/// are stored lowercase with [`Modifiers::SHIFT`] set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    /// The key.
    pub code: KeyCode,
    /// Modifier keys held.
    pub modifiers: Modifiers,
}

impl KeyCombo {
    /// Build a normalized combo.
    #[must_use]
    pub fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        match code {
            KeyCode::Char(c) if c.is_alphabetic() && c.is_uppercase() => Self {
                code: KeyCode::Char(c.to_lowercase().next().unwrap_or(c)),
                modifiers: modifiers | Modifiers::SHIFT,
            },
            _ => Self { code, modifiers },
        }
    }

    /// A combo without modifiers.
    #[must_use]
    pub fn key(code: KeyCode) -> Self {
        Self::new(code, Modifiers::NONE)
    }

    /// The combo a key event represents (its kind is ignored).
    #[must_use]
    pub fn from_event(event: &KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }

    /// Whether `event` presses this combo (any kind).
    #[must_use]
    pub fn matches(&self, event: &KeyEvent) -> bool {
        Self::from_event(event) == *self
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut modifiers = self.modifiers;
        let key = match self.code {
            KeyCode::Char(c) if c.is_alphabetic() && modifiers.contains(Modifiers::SHIFT) => {
                modifiers.remove(Modifiers::SHIFT);
                c.to_uppercase().collect::<String>()
            }
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Escape => "Esc".to_string(),
            KeyCode::Backspace => "Backspace".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::BackTab => "BackTab".to_string(),
            KeyCode::Delete => "Delete".to_string(),
            KeyCode::Insert => "Insert".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::PageUp => "PageUp".to_string(),
            KeyCode::PageDown => "PageDown".to_string(),
            KeyCode::Up => "Up".to_string(),
            KeyCode::Down => "Down".to_string(),
            KeyCode::Left => "Left".to_string(),
            KeyCode::Right => "Right".to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Null => "Null".to_string(),
            KeyCode::MediaPlayPause => "MediaPlayPause".to_string(),
            KeyCode::MediaStop => "MediaStop".to_string(),
            KeyCode::MediaNextTrack => "MediaNextTrack".to_string(),
            KeyCode::MediaPrevTrack => "MediaPrevTrack".to_string(),
        };
        for (flag, name) in [
            (Modifiers::CTRL, "Ctrl"),
            (Modifiers::ALT, "Alt"),
            (Modifiers::SHIFT, "Shift"),
            (Modifiers::SUPER, "Super"),
        ] {
            if modifiers.contains(flag) {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&key)
    }
}

/// Parse a key name: a single character, or a named key (case-insensitive:
/// `Enter`, `Esc`, `Tab`, `BackTab`, `Backspace`, `Delete`, `Insert`, `Home`,
/// `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`, `Space`,
/// `F1`..`F24`, media keys).
fn parse_key_name(name: &str) -> Result<KeyCode, KeyParseError> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(KeyCode::Char(c));
    }
    let lower = name.to_ascii_lowercase();
    let code = match lower.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Escape,
        "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "space" => KeyCode::Char(' '),
        "null" => KeyCode::Null,
        "mediaplaypause" => KeyCode::MediaPlayPause,
        "mediastop" => KeyCode::MediaStop,
        "medianexttrack" => KeyCode::MediaNextTrack,
        "mediaprevtrack" => KeyCode::MediaPrevTrack,
        other => {
            if let Some(digits) = other.strip_prefix('f')
                && let Ok(n) = digits.parse::<u8>()
                && (1..=24).contains(&n)
            {
                KeyCode::F(n)
            } else {
                return Err(KeyParseError::UnknownKey(name.to_string()));
            }
        }
    };
    Ok(code)
}

impl FromStr for KeyCombo {
    type Err = KeyParseError;

    /// Parse `Ctrl+x`, `Shift+Tab`, `F12`, `g`, `Ctrl++` (the plus key).
    /// Modifier names are case-insensitive (`Ctrl`/`Control`, `Alt`/`Opt`/
    /// `Option`, `Shift`, `Super`/`Cmd`/`Meta`/`Win`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(KeyParseError::EmptyKey);
        }
        let (modifier_part, key_part) = if s == "+" {
            ("", "+")
        } else if s.ends_with('+') {
            (s[..s.len() - 1].trim_end_matches('+'), "+")
        } else if let Some((modifiers, key)) = s.rsplit_once('+') {
            (modifiers, key)
        } else {
            ("", s)
        };
        let mut modifiers = Modifiers::NONE;
        for part in modifier_part.split('+').filter(|p| !p.is_empty()) {
            modifiers |= match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Modifiers::CTRL,
                "alt" | "opt" | "option" => Modifiers::ALT,
                "shift" => Modifiers::SHIFT,
                "super" | "cmd" | "meta" | "win" => Modifiers::SUPER,
                _ => return Err(KeyParseError::UnknownModifier(part.to_string())),
            };
        }
        if key_part.is_empty() {
            return Err(KeyParseError::EmptyKey);
        }
        Ok(Self::new(parse_key_name(key_part)?, modifiers))
    }
}

/// One to [`Chord::MAX_LEN`] combos pressed in sequence (`g g`, `Ctrl+x Ctrl+s`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord(Vec<KeyCombo>);

impl Chord {
    /// Longest supported chord.
    pub const MAX_LEN: usize = 4;

    /// A one-combo chord.
    #[must_use]
    pub fn single(combo: KeyCombo) -> Self {
        Self(vec![combo])
    }

    /// Build a chord from combos (1..=[`Chord::MAX_LEN`]).
    pub fn new(combos: Vec<KeyCombo>) -> Result<Self, KeyParseError> {
        if combos.is_empty() {
            Err(KeyParseError::EmptyKey)
        } else if combos.len() > Self::MAX_LEN {
            Err(KeyParseError::TooManyKeys(combos.len()))
        } else {
            Ok(Self(combos))
        }
    }

    /// Parse a whitespace-separated chord such as `"g g"` or `"Ctrl+x Ctrl+s"`.
    pub fn parse(s: &str) -> Result<Self, KeyParseError> {
        s.parse()
    }

    /// The combos in order.
    #[must_use]
    pub fn combos(&self) -> &[KeyCombo] {
        &self.0
    }

    /// Number of combos.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Never true for a chord built through the constructors; provided for
    /// API completeness.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether this chord is a strict prefix of `other` (`g` of `g g`).
    #[must_use]
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        self.0.len() < other.0.len() && other.0.starts_with(&self.0)
    }
}

impl FromStr for Chord {
    type Err = KeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let combos = s
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<KeyCombo>, _>>()?;
        Self::new(combos)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, combo) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{combo}")?;
        }
        Ok(())
    }
}

/// Binding priority level; higher wins for the same chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Application-wide default.
    Global = 0,
    /// Active when the app is in a particular mode.
    Mode = 1,
    /// Owned by the focused widget.
    Widget = 2,
}

/// An interned context name (see [`KeyMap::context`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContextId(pub u32);

/// Identifier of one binding inside a [`KeyMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingId(pub u32);

impl fmt::Display for BindingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// One chord bound to an action.
#[derive(Debug, Clone)]
pub struct Binding<A> {
    /// Identifier assigned by the map.
    pub id: BindingId,
    /// The chord that triggers the action.
    pub chord: Chord,
    /// The action to dispatch.
    pub action: A,
    /// Priority level.
    pub priority: Priority,
    /// Context the binding is limited to (`None` = always applicable).
    pub context: Option<ContextId>,
    /// Human-readable label for help bars and conflict reports.
    pub label: Option<String>,
}

/// Lower bound of the chord timeout (ms).
pub const MIN_CHORD_TIMEOUT_MS: u64 = 200;
/// Upper bound of the chord timeout (ms).
pub const MAX_CHORD_TIMEOUT_MS: u64 = 5000;
/// Default chord timeout (ms).
pub const DEFAULT_CHORD_TIMEOUT_MS: u64 = 1000;

/// Timing configuration of a [`KeyMap`].
#[derive(Debug, Clone)]
pub struct KeyMapConfig {
    /// How long a pending chord prefix waits for its next key.
    pub chord_timeout: Duration,
    /// Esc / Esc Esc detection settings for the dispatcher's detector.
    pub esc: SequenceConfig,
}

impl Default for KeyMapConfig {
    fn default() -> Self {
        Self {
            chord_timeout: Duration::from_millis(DEFAULT_CHORD_TIMEOUT_MS),
            esc: SequenceConfig::default(),
        }
    }
}

impl KeyMapConfig {
    /// Set the chord timeout, clamped to `200..=5000` ms.
    #[must_use]
    pub fn with_chord_timeout(mut self, timeout: Duration) -> Self {
        let ms = timeout.as_millis().clamp(
            u128::from(MIN_CHORD_TIMEOUT_MS),
            u128::from(MAX_CHORD_TIMEOUT_MS),
        );
        self.chord_timeout = Duration::from_millis(ms as u64);
        self
    }

    /// Set the Esc sequence configuration.
    #[must_use]
    pub fn with_esc(mut self, esc: SequenceConfig) -> Self {
        self.esc = esc;
        self
    }
}

/// Result of [`KeyMap::lookup`].
#[derive(Debug, Clone, Copy)]
pub struct Lookup<'a, A> {
    /// The winning binding for exactly this chord, if any.
    pub exact: Option<&'a Binding<A>>,
    /// Number of applicable bindings whose chord starts with this chord and
    /// is longer (a pending prefix must wait for them).
    pub longer: usize,
}

impl<A> Lookup<'_, A> {
    /// Neither an exact binding nor a longer chord.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.exact.is_none() && self.longer == 0
    }
}

/// A binding conflict found by [`KeyMap::conflicts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// Same chord, same context, different priority: `winner` hides `loser`.
    Shadowed {
        winner: BindingId,
        loser: BindingId,
        chord: Chord,
    },
    /// `short` is a strict prefix of `long`, so `short` fires only after the
    /// chord timeout or a non-extending key.
    PrefixCollision {
        short: BindingId,
        long: BindingId,
        short_chord: Chord,
        long_chord: Chord,
    },
    /// Same chord, context and priority: the later binding wins.
    Duplicate {
        first: BindingId,
        second: BindingId,
        chord: Chord,
    },
}

/// Every conflict in a map, with a one-line warning per item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConflictReport {
    /// The conflicts, in map order.
    pub items: Vec<Conflict>,
}

impl ConflictReport {
    /// No conflicts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of conflicts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }
}

impl fmt::Display for ConflictReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in &self.items {
            match item {
                Conflict::Shadowed {
                    winner,
                    loser,
                    chord,
                } => writeln!(
                    f,
                    "warning: binding {winner} shadows binding {loser} on `{chord}` (higher priority)"
                )?,
                Conflict::PrefixCollision {
                    short,
                    long,
                    short_chord,
                    long_chord,
                } => writeln!(
                    f,
                    "warning: binding {short} (`{short_chord}`) is a prefix of binding {long} (`{long_chord}`); it fires only after the chord timeout or a non-extending key"
                )?,
                Conflict::Duplicate {
                    first,
                    second,
                    chord,
                } => writeln!(
                    f,
                    "warning: bindings {first} and {second} both bind `{chord}` at the same priority; the later one wins"
                )?,
            }
        }
        Ok(())
    }
}

/// A declarative binding map: chords to actions with priorities and
/// contexts. Actions are any `Clone` type (usually an app enum).
#[derive(Debug, Clone)]
pub struct KeyMap<A> {
    bindings: Vec<Binding<A>>,
    contexts: Vec<String>,
    config: KeyMapConfig,
    next_id: u32,
}

impl<A> Default for KeyMap<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A> KeyMap<A> {
    /// An empty map with default timing.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(KeyMapConfig::default())
    }

    /// An empty map with the given timing.
    #[must_use]
    pub fn with_config(config: KeyMapConfig) -> Self {
        Self {
            bindings: Vec::new(),
            contexts: Vec::new(),
            config,
            next_id: 0,
        }
    }

    /// Timing configuration.
    #[must_use]
    pub fn config(&self) -> &KeyMapConfig {
        &self.config
    }

    /// Intern a context name; the same name always yields the same id.
    pub fn context(&mut self, name: &str) -> ContextId {
        if let Some(index) = self.contexts.iter().position(|n| n == name) {
            return ContextId(index as u32);
        }
        self.contexts.push(name.to_string());
        ContextId((self.contexts.len() - 1) as u32)
    }

    /// Name of an interned context.
    #[must_use]
    pub fn context_name(&self, id: ContextId) -> Option<&str> {
        self.contexts.get(id.0 as usize).map(String::as_str)
    }

    /// Bind a chord at [`Priority::Global`] with no context.
    pub fn bind(&mut self, chord: Chord, action: A) -> BindingId {
        self.bind_in(chord, action, Priority::Global, None)
    }

    /// Bind a chord with an explicit priority and optional context.
    pub fn bind_in(
        &mut self,
        chord: Chord,
        action: A,
        priority: Priority,
        context: Option<ContextId>,
    ) -> BindingId {
        let id = BindingId(self.next_id);
        self.next_id += 1;
        self.bindings.push(Binding {
            id,
            chord,
            action,
            priority,
            context,
            label: None,
        });
        id
    }

    /// Attach a label to a binding; `false` if the id is unknown.
    pub fn set_label(&mut self, id: BindingId, label: impl Into<String>) -> bool {
        match self.bindings.iter_mut().find(|b| b.id == id) {
            Some(binding) => {
                binding.label = Some(label.into());
                true
            }
            None => false,
        }
    }

    /// Remove a binding.
    pub fn unbind(&mut self, id: BindingId) -> Option<Binding<A>> {
        let index = self.bindings.iter().position(|b| b.id == id)?;
        Some(self.bindings.remove(index))
    }

    /// All bindings in bind order.
    #[must_use]
    pub fn bindings(&self) -> &[Binding<A>] {
        &self.bindings
    }

    /// A binding by id.
    #[must_use]
    pub fn get(&self, id: BindingId) -> Option<&Binding<A>> {
        self.bindings.iter().find(|b| b.id == id)
    }

    /// Number of bindings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Whether the map has no bindings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    fn applies(binding: &Binding<A>, active: &[ContextId]) -> bool {
        binding
            .context
            .is_none_or(|context| active.contains(&context))
    }

    /// Ranking used to pick a winner among bindings for the same chord:
    /// active context beats none, then priority, then recency.
    fn rank(binding: &Binding<A>) -> (bool, Priority, BindingId) {
        (binding.context.is_some(), binding.priority, binding.id)
    }

    /// Resolve `chord` against the bindings applicable under `active`
    /// contexts: the winning exact binding and how many longer bound chords
    /// start with it.
    #[must_use]
    pub fn lookup(&self, chord: &Chord, active: &[ContextId]) -> Lookup<'_, A> {
        let mut exact: Option<&Binding<A>> = None;
        let mut longer = 0;
        for binding in &self.bindings {
            if !Self::applies(binding, active) {
                continue;
            }
            if binding.chord == *chord {
                if exact.is_none_or(|current| Self::rank(binding) > Self::rank(current)) {
                    exact = Some(binding);
                }
            } else if chord.is_prefix_of(&binding.chord) {
                longer += 1;
            }
        }
        Lookup { exact, longer }
    }

    /// Report shadowed, duplicate, and prefix-colliding bindings.
    #[must_use]
    pub fn conflicts(&self) -> ConflictReport {
        let mut items = Vec::new();
        for (i, a) in self.bindings.iter().enumerate() {
            for b in &self.bindings[i + 1..] {
                if a.chord == b.chord {
                    if a.context != b.context {
                        // A context-specific override is the intended use.
                        continue;
                    }
                    if a.priority == b.priority {
                        items.push(Conflict::Duplicate {
                            first: a.id,
                            second: b.id,
                            chord: a.chord.clone(),
                        });
                    } else {
                        let (winner, loser) = if a.priority > b.priority {
                            (a.id, b.id)
                        } else {
                            (b.id, a.id)
                        };
                        items.push(Conflict::Shadowed {
                            winner,
                            loser,
                            chord: a.chord.clone(),
                        });
                    }
                } else if a.chord.is_prefix_of(&b.chord) {
                    items.push(Conflict::PrefixCollision {
                        short: a.id,
                        long: b.id,
                        short_chord: a.chord.clone(),
                        long_chord: b.chord.clone(),
                    });
                } else if b.chord.is_prefix_of(&a.chord) {
                    items.push(Conflict::PrefixCollision {
                        short: b.id,
                        long: a.id,
                        short_chord: b.chord.clone(),
                        long_chord: a.chord.clone(),
                    });
                }
            }
        }
        ConflictReport { items }
    }
}

/// What the dispatcher decided for one key event or tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch<A> {
    /// A binding fired.
    Action {
        action: A,
        binding: BindingId,
        chord: Chord,
    },
    /// The key extended a chord prefix; waiting for more keys or the timeout.
    Pending { prefix: Chord },
    /// The key matched nothing (and could not extend a chord).
    Unbound(KeyEvent),
    /// A pending prefix was abandoned (timeout or a non-extending key).
    Expired { prefix: Chord },
    /// Esc sequence detector output for an unbound Esc / Esc Esc.
    Esc(SequenceOutput),
}

/// Counters for evidence rows and hint-usage feedback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchStats {
    /// Bindings fired.
    pub dispatched: u64,
    /// Keys that entered a chord prefix.
    pub pending: u64,
    /// Prefixes abandoned.
    pub expired: u64,
    /// Keys that matched nothing.
    pub unbound: u64,
    /// Esc / Esc Esc verdicts handed back unbound (see [`Dispatch::Esc`]).
    pub esc: u64,
}

fn action_dispatch<A: Clone>(binding: &Binding<A>, chord: Chord) -> Dispatch<A> {
    Dispatch::Action {
        action: binding.action.clone(),
        binding: binding.id,
        chord,
    }
}

/// Chord-aware dispatcher over a [`KeyMap`].
///
/// Feed every key event through [`feed`](Self::feed) and call
/// [`tick`](Self::tick) once per frame so pending chords and the Esc timer
/// expire; every call returns the decisions to act on. See the module
/// section header for the resolution rules.
#[derive(Debug)]
pub struct KeyDispatcher<A> {
    map: KeyMap<A>,
    pending: Vec<KeyCombo>,
    pending_since: Option<Instant>,
    esc: SequenceDetector,
    active_contexts: Vec<ContextId>,
    stats: DispatchStats,
}

impl<A: Clone> KeyDispatcher<A> {
    /// A dispatcher over `map` with no active contexts.
    #[must_use]
    pub fn new(map: KeyMap<A>) -> Self {
        let esc = SequenceDetector::new(map.config().esc.clone());
        Self {
            map,
            pending: Vec::new(),
            pending_since: None,
            esc,
            active_contexts: Vec::new(),
            stats: DispatchStats::default(),
        }
    }

    /// The underlying map.
    #[must_use]
    pub fn map(&self) -> &KeyMap<A> {
        &self.map
    }

    /// Mutable access to the map (rebinding at runtime).
    pub fn map_mut(&mut self) -> &mut KeyMap<A> {
        &mut self.map
    }

    /// Replace the set of active contexts (focused widget, mode, ...).
    pub fn set_active_contexts(&mut self, contexts: &[ContextId]) {
        self.active_contexts.clear();
        self.active_contexts.extend_from_slice(contexts);
    }

    /// Currently active contexts.
    #[must_use]
    pub fn active_contexts(&self) -> &[ContextId] {
        &self.active_contexts
    }

    /// The chord prefix currently waiting for more keys.
    #[must_use]
    pub fn pending_prefix(&self) -> Option<Chord> {
        Chord::new(self.pending.clone()).ok()
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> DispatchStats {
        self.stats
    }

    /// Drop any pending prefix and Esc state.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.pending_since = None;
        self.esc.reset();
    }

    /// Process one key event.
    pub fn feed(&mut self, key: &KeyEvent, now: Instant) -> Vec<Dispatch<A>> {
        let mut out = Vec::with_capacity(2);

        if key.code == KeyCode::Escape {
            if key.kind == KeyEventKind::Press {
                self.flush_pending(&mut out, false);
            }
            match self.esc.feed(key, now) {
                // Repeat / release of Esc: nothing sequence-related to do.
                SequenceOutput::PassThrough => {}
                output => {
                    self.dispatch_esc(output, &mut out);
                    return out;
                }
            }
        }

        match key.kind {
            KeyEventKind::Release => {
                self.stats.unbound += 1;
                out.push(Dispatch::Unbound(*key));
                return out;
            }
            KeyEventKind::Repeat => {
                // A held key re-fires its own single-key binding, never a chord.
                let single = Chord::single(KeyCombo::from_event(key));
                let fired = if self.pending.is_empty() {
                    self.map
                        .lookup(&single, &self.active_contexts)
                        .exact
                        .map(|binding| action_dispatch(binding, single))
                } else {
                    None
                };
                match fired {
                    Some(dispatch) => {
                        self.stats.dispatched += 1;
                        out.push(dispatch);
                    }
                    None => {
                        self.stats.unbound += 1;
                        out.push(Dispatch::Unbound(*key));
                    }
                }
                return out;
            }
            KeyEventKind::Press => {}
        }

        let combo = KeyCombo::from_event(key);
        if self.try_extend(combo, now, &mut out) {
            return out;
        }

        // The key cannot extend the prefix: flush it, then start over with the
        // key on its own so it can fire or begin a new chord.
        if !self.pending.is_empty() {
            self.flush_pending(&mut out, false);
            if self.try_extend(combo, now, &mut out) {
                return out;
            }
        }

        self.stats.unbound += 1;
        out.push(Dispatch::Unbound(*key));
        out
    }

    /// Expire a pending prefix past the chord timeout and drive the Esc timer.
    pub fn tick(&mut self, now: Instant) -> Vec<Dispatch<A>> {
        let mut out = Vec::new();
        if let Some(since) = self.pending_since
            && now.saturating_duration_since(since) >= self.map.config.chord_timeout
        {
            self.flush_pending(&mut out, true);
        }
        if let Some(output) = self.esc.check_timeout(now) {
            self.dispatch_esc(output, &mut out);
        }
        out
    }

    /// Try to treat `combo` as the next key of the pending prefix. Returns
    /// `false` when the extended chord matches nothing (nothing is emitted).
    fn try_extend(&mut self, combo: KeyCombo, now: Instant, out: &mut Vec<Dispatch<A>>) -> bool {
        if self.pending.len() >= Chord::MAX_LEN {
            return false;
        }
        let mut candidate = self.pending.clone();
        candidate.push(combo);
        let chord = Chord(candidate);
        let lookup = self.map.lookup(&chord, &self.active_contexts);
        if let Some(binding) = lookup.exact
            && lookup.longer == 0
        {
            let dispatch = action_dispatch(binding, chord);
            self.pending.clear();
            self.pending_since = None;
            self.stats.dispatched += 1;
            out.push(dispatch);
            return true;
        }
        if lookup.exact.is_some() || lookup.longer > 0 {
            self.pending.clone_from(&chord.0);
            self.pending_since = Some(now);
            self.stats.pending += 1;
            out.push(Dispatch::Pending { prefix: chord });
            return true;
        }
        false
    }

    /// Fire the pending prefix if it is bound, otherwise report it expired;
    /// on a timeout the expiry is reported first so the delay is visible.
    fn flush_pending(&mut self, out: &mut Vec<Dispatch<A>>, timed_out: bool) {
        if self.pending.is_empty() {
            return;
        }
        let prefix = Chord(std::mem::take(&mut self.pending));
        self.pending_since = None;
        let fired = self
            .map
            .lookup(&prefix, &self.active_contexts)
            .exact
            .map(|binding| action_dispatch(binding, prefix.clone()));
        match fired {
            Some(dispatch) => {
                if timed_out {
                    self.stats.expired += 1;
                    out.push(Dispatch::Expired { prefix });
                }
                self.stats.dispatched += 1;
                out.push(dispatch);
            }
            None => {
                self.stats.expired += 1;
                out.push(Dispatch::Expired { prefix });
            }
        }
    }

    /// Route a detector verdict: a bound `Esc` / `Esc Esc` fires its binding,
    /// anything else is handed back as [`Dispatch::Esc`].
    fn dispatch_esc(&mut self, output: SequenceOutput, out: &mut Vec<Dispatch<A>>) {
        let esc = KeyCombo::key(KeyCode::Escape);
        let bound = match output {
            SequenceOutput::Esc => Some(Chord::single(esc)),
            SequenceOutput::EscEsc => Chord::new(vec![esc, esc]).ok(),
            SequenceOutput::Pending | SequenceOutput::PassThrough => None,
        };
        let fired = bound.and_then(|chord| {
            self.map
                .lookup(&chord, &self.active_contexts)
                .exact
                .map(|binding| action_dispatch(binding, chord.clone()))
        });
        match fired {
            Some(dispatch) => {
                self.stats.dispatched += 1;
                out.push(dispatch);
            }
            None => {
                self.stats.esc += 1;
                out.push(Dispatch::Esc(output));
            }
        }
    }
}

#[cfg(test)]
mod keymap_tests {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Act {
        GoTop,
        Help,
        Save,
        Quit,
        Submit,
        Newline,
        Global,
        Mode,
        Widget,
        Down,
    }

    fn press(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c))
    }

    fn kind(mut event: KeyEvent, kind: KeyEventKind) -> KeyEvent {
        event.kind = kind;
        event
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn chord(s: &str) -> Chord {
        Chord::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn actions<A: Clone>(dispatches: &[Dispatch<A>]) -> Vec<A> {
        dispatches
            .iter()
            .filter_map(|d| match d {
                Dispatch::Action { action, .. } => Some(action.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn combo_parse_display_and_normalization() {
        let ctrl_x: KeyCombo = "Ctrl+x".parse().unwrap();
        assert_eq!(ctrl_x, KeyCombo::new(KeyCode::Char('x'), Modifiers::CTRL));
        assert_eq!(ctrl_x.to_string(), "Ctrl+x");

        // Shift+a, A and a terminal reporting Char('A')+SHIFT are one combo.
        let shift_a: KeyCombo = "shift+a".parse().unwrap();
        assert_eq!(shift_a, "A".parse().unwrap());
        assert_eq!(shift_a, KeyCombo::new(KeyCode::Char('A'), Modifiers::SHIFT));
        assert_eq!(shift_a.to_string(), "A");

        assert_eq!("F12".parse::<KeyCombo>().unwrap().code, KeyCode::F(12));
        assert_eq!(
            "Space".parse::<KeyCombo>().unwrap().code,
            KeyCode::Char(' ')
        );
        assert_eq!(
            "Ctrl+Alt+Delete".parse::<KeyCombo>().unwrap().to_string(),
            "Ctrl+Alt+Delete"
        );
        assert_eq!(
            "Shift+Tab".parse::<KeyCombo>().unwrap().to_string(),
            "Shift+Tab"
        );
        // The plus key itself.
        assert_eq!("+".parse::<KeyCombo>().unwrap().code, KeyCode::Char('+'));
        let ctrl_plus: KeyCombo = "Ctrl++".parse().unwrap();
        assert_eq!(
            ctrl_plus,
            KeyCombo::new(KeyCode::Char('+'), Modifiers::CTRL)
        );

        assert_eq!(
            "Hyper+x".parse::<KeyCombo>(),
            Err(KeyParseError::UnknownModifier("Hyper".into()))
        );
        assert_eq!(
            "Banana".parse::<KeyCombo>(),
            Err(KeyParseError::UnknownKey("Banana".into()))
        );
        assert_eq!("".parse::<KeyCombo>(), Err(KeyParseError::EmptyKey));
        assert_eq!(
            "F0".parse::<KeyCombo>(),
            Err(KeyParseError::UnknownKey("F0".into()))
        );
    }

    #[test]
    fn chord_parse_prefix_and_limits() {
        let gg = chord("g g");
        let g = chord("g");
        assert_eq!(gg.len(), 2);
        assert_eq!(gg.to_string(), "g g");
        assert!(g.is_prefix_of(&gg));
        assert!(!gg.is_prefix_of(&g));
        assert!(!g.is_prefix_of(&g), "a chord is not its own prefix");
        assert_eq!(chord("Ctrl+x Ctrl+s").to_string(), "Ctrl+x Ctrl+s");
        assert_eq!(Chord::parse(""), Err(KeyParseError::EmptyKey));
        assert_eq!(
            Chord::parse("a b c d e"),
            Err(KeyParseError::TooManyKeys(5))
        );
    }

    #[test]
    fn chord_completes_within_timeout() {
        let mut map = KeyMap::new();
        map.bind(chord("g g"), Act::GoTop);
        map.bind(chord("x"), Act::Save);
        let mut dispatcher = KeyDispatcher::new(map);
        let t0 = Instant::now();

        let first = dispatcher.feed(&press('g'), t0);
        assert_eq!(first, vec![Dispatch::Pending { prefix: chord("g") }]);
        assert_eq!(dispatcher.pending_prefix(), Some(chord("g")));
        assert!(
            dispatcher.tick(t0 + ms(300)).is_empty(),
            "still inside the timeout"
        );

        let second = dispatcher.feed(&press('g'), t0 + ms(300));
        assert_eq!(actions(&second), vec![Act::GoTop]);
        assert_eq!(dispatcher.pending_prefix(), None);
        assert_eq!(dispatcher.stats().dispatched, 1);
        assert_eq!(dispatcher.stats().pending, 1);
    }

    #[test]
    fn chord_expires_after_timeout() {
        // Prefix that is itself bound: expiry fires it.
        let mut map = KeyMap::new();
        map.bind(chord("g g"), Act::GoTop);
        map.bind(chord("g"), Act::Help);
        let mut dispatcher = KeyDispatcher::new(map);
        let t0 = Instant::now();
        assert_eq!(
            dispatcher.feed(&press('g'), t0),
            vec![Dispatch::Pending { prefix: chord("g") }]
        );
        assert!(dispatcher.tick(t0 + ms(999)).is_empty());
        let expired = dispatcher.tick(t0 + ms(1000));
        assert_eq!(expired[0], Dispatch::Expired { prefix: chord("g") });
        assert_eq!(actions(&expired), vec![Act::Help]);
        assert_eq!(dispatcher.stats().expired, 1);

        // Prefix that is not bound: expiry only.
        let mut map = KeyMap::new();
        map.bind(chord("g g"), Act::GoTop);
        let mut dispatcher = KeyDispatcher::new(map);
        dispatcher.feed(&press('g'), t0);
        assert_eq!(
            dispatcher.tick(t0 + ms(5000)),
            vec![Dispatch::Expired { prefix: chord("g") }]
        );
        assert_eq!(dispatcher.pending_prefix(), None);
    }

    #[test]
    fn single_key_fires_while_chord_pending() {
        let mut map = KeyMap::new();
        map.bind(chord("g g"), Act::GoTop);
        map.bind(chord("x"), Act::Save);
        let mut dispatcher = KeyDispatcher::new(map);
        let t0 = Instant::now();
        dispatcher.feed(&press('g'), t0);
        let out = dispatcher.feed(&press('x'), t0 + ms(10));
        assert_eq!(out[0], Dispatch::Expired { prefix: chord("g") });
        assert_eq!(
            actions(&out),
            vec![Act::Save],
            "x is never blocked by the pending g"
        );

        // Same with a bound prefix: it fires first, then the single key.
        let mut map = KeyMap::new();
        map.bind(chord("g g"), Act::GoTop);
        map.bind(chord("g"), Act::Help);
        map.bind(chord("x"), Act::Save);
        let mut dispatcher = KeyDispatcher::new(map);
        dispatcher.feed(&press('g'), t0);
        let out = dispatcher.feed(&press('x'), t0 + ms(10));
        assert_eq!(actions(&out), vec![Act::Help, Act::Save]);

        // A non-extending key that starts another chord goes pending itself.
        let mut map = KeyMap::new();
        map.bind(chord("g g"), Act::GoTop);
        map.bind(chord("z z"), Act::Quit);
        let mut dispatcher = KeyDispatcher::new(map);
        dispatcher.feed(&press('g'), t0);
        let out = dispatcher.feed(&press('z'), t0 + ms(10));
        assert_eq!(
            out,
            vec![
                Dispatch::Expired { prefix: chord("g") },
                Dispatch::Pending { prefix: chord("z") }
            ]
        );
    }

    #[test]
    fn widget_beats_mode_beats_global() {
        let mut map = KeyMap::new();
        let g = map.bind_in(chord("s"), Act::Global, Priority::Global, None);
        let m = map.bind_in(chord("s"), Act::Mode, Priority::Mode, None);
        let w = map.bind_in(chord("s"), Act::Widget, Priority::Widget, None);
        let lookup = map.lookup(&chord("s"), &[]);
        assert_eq!(lookup.exact.map(|b| b.id), Some(w));
        assert_eq!(lookup.longer, 0);

        let mut dispatcher = KeyDispatcher::new(map);
        assert_eq!(
            actions(&dispatcher.feed(&press('s'), Instant::now())),
            vec![Act::Widget]
        );

        let report = dispatcher.map().conflicts();
        assert_eq!(report.len(), 3, "{report}");
        assert!(report.items.contains(&Conflict::Shadowed {
            winner: w,
            loser: g,
            chord: chord("s")
        }));
        assert!(report.items.contains(&Conflict::Shadowed {
            winner: m,
            loser: g,
            chord: chord("s")
        }));
        assert!(report.items.contains(&Conflict::Shadowed {
            winner: w,
            loser: m,
            chord: chord("s")
        }));
        assert_eq!(report.to_string().lines().count(), 3);

        // Removing the winner promotes the next.
        dispatcher.map_mut().unbind(w);
        assert_eq!(
            actions(&dispatcher.feed(&press('s'), Instant::now())),
            vec![Act::Mode]
        );
    }

    #[test]
    fn active_context_beats_contextless_even_at_lower_priority() {
        let mut map = KeyMap::new();
        let text_input = map.context("text_input");
        assert_eq!(map.context("text_input"), text_input, "interned once");
        assert_eq!(map.context_name(text_input), Some("text_input"));
        map.bind_in(chord("Enter"), Act::Submit, Priority::Widget, None);
        map.bind_in(
            chord("Enter"),
            Act::Newline,
            Priority::Global,
            Some(text_input),
        );
        assert!(
            map.conflicts().is_empty(),
            "a context override is not a conflict"
        );

        let mut dispatcher = KeyDispatcher::new(map);
        let enter = KeyEvent::new(KeyCode::Enter);
        let t0 = Instant::now();
        assert_eq!(actions(&dispatcher.feed(&enter, t0)), vec![Act::Submit]);
        dispatcher.set_active_contexts(&[text_input]);
        assert_eq!(actions(&dispatcher.feed(&enter, t0)), vec![Act::Newline]);
        dispatcher.set_active_contexts(&[]);
        assert_eq!(actions(&dispatcher.feed(&enter, t0)), vec![Act::Submit]);
    }

    #[test]
    fn conflicts_reports_shadowed_prefix_and_duplicate() {
        let mut map = KeyMap::new();
        let long = map.bind(chord("g g"), Act::GoTop);
        let short = map.bind(chord("g"), Act::Help);
        let q1 = map.bind(chord("q"), Act::Quit);
        let q2 = map.bind(chord("q"), Act::Quit);
        map.set_label(q2, "quit");
        assert_eq!(map.get(q2).and_then(|b| b.label.as_deref()), Some("quit"));

        let report = map.conflicts();
        assert_eq!(report.len(), 2, "{report}");
        assert_eq!(
            report.items[0],
            Conflict::PrefixCollision {
                short,
                long,
                short_chord: chord("g"),
                long_chord: chord("g g"),
            }
        );
        assert_eq!(
            report.items[1],
            Conflict::Duplicate {
                first: q1,
                second: q2,
                chord: chord("q")
            }
        );
        let text = report.to_string();
        assert_eq!(text.lines().count(), 2);
        assert!(
            text.contains("warning: binding #1 (`g`) is a prefix of binding #0 (`g g`)"),
            "{text}"
        );
        assert!(text.contains("the later one wins"), "{text}");

        // The later duplicate wins at dispatch.
        assert_eq!(map.lookup(&chord("q"), &[]).exact.map(|b| b.id), Some(q2));
    }

    #[test]
    fn repeat_refires_single_key_binding_but_never_extends_a_chord() {
        let mut map = KeyMap::new();
        map.bind(chord("j"), Act::Down);
        map.bind(chord("g g"), Act::GoTop);
        let mut dispatcher = KeyDispatcher::new(map);
        let t0 = Instant::now();

        let held = kind(press('j'), KeyEventKind::Repeat);
        assert_eq!(actions(&dispatcher.feed(&held, t0)), vec![Act::Down]);

        dispatcher.feed(&press('g'), t0);
        let repeat_g = kind(press('g'), KeyEventKind::Repeat);
        assert_eq!(
            dispatcher.feed(&repeat_g, t0 + ms(10)),
            vec![Dispatch::Unbound(repeat_g)]
        );
        assert_eq!(
            dispatcher.pending_prefix(),
            Some(chord("g")),
            "repeat left the prefix alone"
        );

        let released = kind(press('j'), KeyEventKind::Release);
        assert_eq!(
            dispatcher.feed(&released, t0 + ms(20)),
            vec![Dispatch::Unbound(released)]
        );
    }

    #[test]
    fn esc_goes_through_the_sequence_detector() {
        let mut map = KeyMap::new();
        map.bind(chord("Esc"), Act::Quit);
        map.bind(chord("Esc Esc"), Act::Help);
        map.bind(chord("g g"), Act::GoTop);
        let mut dispatcher = KeyDispatcher::new(map);
        let esc = KeyEvent::new(KeyCode::Escape);
        let t0 = Instant::now();

        // A lone Esc waits for the detector window, then fires its binding.
        assert_eq!(
            dispatcher.feed(&esc, t0),
            vec![Dispatch::Esc(SequenceOutput::Pending)]
        );
        assert_eq!(actions(&dispatcher.tick(t0 + ms(300))), vec![Act::Quit]);

        // Esc Esc inside the window fires the double binding.
        let t1 = t0 + ms(1000);
        dispatcher.feed(&esc, t1);
        assert_eq!(
            actions(&dispatcher.feed(&esc, t1 + ms(100))),
            vec![Act::Help]
        );

        // Esc cancels a pending chord prefix first.
        let t2 = t0 + ms(3000);
        dispatcher.feed(&press('g'), t2);
        let out = dispatcher.feed(&esc, t2 + ms(10));
        assert_eq!(out[0], Dispatch::Expired { prefix: chord("g") });
        assert_eq!(dispatcher.pending_prefix(), None);

        // Unbound Esc surfaces the detector verdict.
        let mut plain = KeyDispatcher::new(KeyMap::<Act>::new());
        plain.feed(&esc, t0);
        assert_eq!(
            plain.tick(t0 + ms(300)),
            vec![Dispatch::Esc(SequenceOutput::Esc)]
        );
    }

    fn arb_key() -> impl Strategy<Value = KeyEvent> {
        let code = prop_oneof![
            Just(KeyCode::Char('a')),
            Just(KeyCode::Char('b')),
            Just(KeyCode::Char('c')),
            Just(KeyCode::Enter),
            Just(KeyCode::Escape),
        ];
        let kind = prop_oneof![
            Just(KeyEventKind::Press),
            Just(KeyEventKind::Repeat),
            Just(KeyEventKind::Release),
        ];
        (code, kind).prop_map(|(code, kind)| KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind,
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        /// No key is ever swallowed: every feed yields at least one dispatch,
        /// and draining the timers afterwards never panics or leaks a prefix.
        #[test]
        fn every_fed_key_yields_a_dispatch(
            keys in proptest::collection::vec(arb_key(), 1..20),
            gaps in proptest::collection::vec(0u64..1500, 1..20),
        ) {
            let mut map = KeyMap::new();
            map.bind(chord("a b"), Act::GoTop);
            map.bind(chord("a"), Act::Help);
            map.bind(chord("c c c"), Act::Save);
            map.bind(chord("Enter"), Act::Submit);
            let mut dispatcher = KeyDispatcher::new(map);
            let mut now = Instant::now();
            for (key, gap) in keys.iter().zip(gaps.iter().cycle()) {
                now += ms(*gap);
                let out = dispatcher.feed(key, now);
                prop_assert!(!out.is_empty(), "{key:?} produced nothing");
                let _ = dispatcher.tick(now);
            }
            let _ = dispatcher.tick(now + ms(10_000));
            prop_assert_eq!(dispatcher.pending_prefix(), None);
            let stats = dispatcher.stats();
            prop_assert!(
                stats.dispatched + stats.pending + stats.expired + stats.unbound + stats.esc > 0
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    fn esc_press() -> KeyEvent {
        KeyEvent::new(KeyCode::Escape)
    }

    fn key_press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code)
    }

    fn esc_release() -> KeyEvent {
        KeyEvent::new(KeyCode::Escape).with_kind(KeyEventKind::Release)
    }

    const MS_50: Duration = Duration::from_millis(50);
    const MS_100: Duration = Duration::from_millis(100);
    const MS_200: Duration = Duration::from_millis(200);
    const MS_300: Duration = Duration::from_millis(300);

    // --- Basic sequence tests ---

    #[test]
    fn single_esc_returns_pending() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        let output = detector.feed(&esc_press(), t);
        assert_eq!(output, SequenceOutput::Pending);
        assert!(detector.is_pending());
    }

    #[test]
    fn esc_esc_within_timeout() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        let output = detector.feed(&esc_press(), t + MS_100);

        assert_eq!(output, SequenceOutput::EscEsc);
        assert!(!detector.is_pending());
    }

    #[test]
    fn esc_esc_at_timeout_boundary() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        // Exactly at 250ms boundary
        let output = detector.feed(&esc_press(), t + Duration::from_millis(250));

        assert_eq!(output, SequenceOutput::EscEsc);
    }

    #[test]
    fn esc_esc_past_timeout() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        // Past 250ms timeout (251ms)
        let output = detector.feed(&esc_press(), t + Duration::from_millis(251));

        // First Esc timed out, second Esc starts new sequence
        assert_eq!(output, SequenceOutput::Esc);
        assert!(detector.is_pending()); // New sequence started
    }

    #[test]
    fn timeout_check_emits_pending_esc() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);

        // Before timeout
        assert!(detector.check_timeout(t + MS_200).is_none());
        assert!(detector.is_pending());

        // After timeout (251ms)
        let output = detector.check_timeout(t + Duration::from_millis(251));
        assert_eq!(output, Some(SequenceOutput::Esc));
        assert!(!detector.is_pending());
    }

    #[test]
    fn other_key_interrupts_sequence() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        let output = detector.feed(&key_press(KeyCode::Char('a')), t + MS_100);

        // Pending Esc is emitted
        assert_eq!(output, SequenceOutput::Esc);
        assert!(!detector.is_pending());
    }

    #[test]
    fn non_esc_key_passes_through() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        let output = detector.feed(&key_press(KeyCode::Char('x')), t);
        assert_eq!(output, SequenceOutput::PassThrough);
    }

    #[test]
    fn release_event_passes_through() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        let output = detector.feed(&esc_release(), t);
        assert_eq!(output, SequenceOutput::PassThrough);
        assert!(!detector.is_pending());
    }

    #[test]
    fn release_during_pending_passes_through() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        let output = detector.feed(&esc_release(), t + MS_50);

        // Release is ignored; still pending
        assert_eq!(output, SequenceOutput::PassThrough);
        assert!(detector.is_pending());
    }

    // --- Config tests ---

    #[test]
    fn custom_timeout() {
        let config = SequenceConfig::default().with_timeout(Duration::from_millis(100));
        let mut detector = SequenceDetector::new(config);
        let t = now();

        detector.feed(&esc_press(), t);
        // 150ms is past 100ms timeout
        let output = detector.feed(&esc_press(), t + Duration::from_millis(150));

        assert_eq!(output, SequenceOutput::Esc);
    }

    #[test]
    fn disabled_sequences() {
        let config = SequenceConfig::default().disable_sequences();
        let mut detector = SequenceDetector::new(config);
        let t = now();

        // First Esc immediately emits Esc
        let output = detector.feed(&esc_press(), t);
        assert_eq!(output, SequenceOutput::Esc);
        assert!(!detector.is_pending());

        // Second Esc also immediately emits Esc
        let output = detector.feed(&esc_press(), t + MS_50);
        assert_eq!(output, SequenceOutput::Esc);
    }

    #[test]
    fn disabled_sequences_passthrough() {
        let config = SequenceConfig::default().disable_sequences();
        let mut detector = SequenceDetector::new(config);
        let t = now();

        let output = detector.feed(&key_press(KeyCode::Char('a')), t);
        assert_eq!(output, SequenceOutput::PassThrough);
    }

    #[test]
    fn config_default_values() {
        let config = SequenceConfig::default();
        assert_eq!(config.esc_seq_timeout, Duration::from_millis(250));
        assert_eq!(config.esc_debounce, Duration::from_millis(50));
        assert!(!config.disable_sequences);
    }

    #[test]
    fn config_builder_chain() {
        let config = SequenceConfig::default()
            .with_timeout(Duration::from_millis(300))
            .with_debounce(Duration::from_millis(100))
            .disable_sequences();

        assert_eq!(config.esc_seq_timeout, Duration::from_millis(300));
        assert_eq!(config.esc_debounce, Duration::from_millis(100));
        assert!(config.disable_sequences);
    }

    // --- Reset tests ---

    #[test]
    fn reset_clears_pending() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        assert!(detector.is_pending());

        detector.reset();
        assert!(!detector.is_pending());

        // After reset, new Esc starts fresh
        let output = detector.feed(&esc_press(), t + MS_100);
        assert_eq!(output, SequenceOutput::Pending);
    }

    #[test]
    fn reset_discards_pending_esc() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        detector.reset();

        // Timeout check should not emit anything
        assert!(detector.check_timeout(t + MS_300).is_none());
    }

    // --- Edge cases ---

    #[test]
    fn rapid_triple_esc() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        // First Esc
        let out1 = detector.feed(&esc_press(), t);
        assert_eq!(out1, SequenceOutput::Pending);

        // Second Esc -> EscEsc
        let out2 = detector.feed(&esc_press(), t + MS_50);
        assert_eq!(out2, SequenceOutput::EscEsc);

        // Third Esc -> starts new sequence
        let out3 = detector.feed(&esc_press(), t + MS_100);
        assert_eq!(out3, SequenceOutput::Pending);
    }

    #[test]
    fn alternating_esc_and_key() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        // Esc -> pending
        detector.feed(&esc_press(), t);

        // 'a' -> emits Esc
        let out1 = detector.feed(&key_press(KeyCode::Char('a')), t + MS_50);
        assert_eq!(out1, SequenceOutput::Esc);

        // Esc -> pending again
        let out2 = detector.feed(&esc_press(), t + MS_100);
        assert_eq!(out2, SequenceOutput::Pending);

        // 'b' -> emits Esc
        let out3 = detector.feed(&key_press(KeyCode::Char('b')), t + MS_200);
        assert_eq!(out3, SequenceOutput::Esc);
    }

    #[test]
    fn enter_key_interrupts() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        let output = detector.feed(&key_press(KeyCode::Enter), t + MS_100);

        assert_eq!(output, SequenceOutput::Esc);
    }

    #[test]
    fn function_key_interrupts() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        let output = detector.feed(&key_press(KeyCode::F(1)), t + MS_100);

        assert_eq!(output, SequenceOutput::Esc);
    }

    #[test]
    fn arrow_key_interrupts() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        let output = detector.feed(&key_press(KeyCode::Up), t + MS_100);

        assert_eq!(output, SequenceOutput::Esc);
    }

    #[test]
    fn config_getter_and_setter() {
        let mut detector = SequenceDetector::with_defaults();
        assert_eq!(
            detector.config().esc_seq_timeout,
            Duration::from_millis(250)
        );

        let new_config = SequenceConfig::default().with_timeout(Duration::from_millis(500));
        detector.set_config(new_config);

        assert_eq!(
            detector.config().esc_seq_timeout,
            Duration::from_millis(500)
        );
    }

    #[test]
    fn set_config_preserves_pending_state() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        detector.feed(&esc_press(), t);
        assert!(detector.is_pending());

        // Change config while pending
        detector.set_config(SequenceConfig::default().with_timeout(Duration::from_millis(500)));

        // Still pending
        assert!(detector.is_pending());

        // New timeout applies
        let output = detector.feed(&esc_press(), t + MS_300);
        assert_eq!(output, SequenceOutput::EscEsc); // Within new 500ms timeout
    }

    #[test]
    fn debug_format() {
        let detector = SequenceDetector::with_defaults();
        let dbg = format!("{:?}", detector);
        assert!(dbg.contains("SequenceDetector"));
    }

    #[test]
    fn config_debug_format() {
        let config = SequenceConfig::default();
        let dbg = format!("{:?}", config);
        assert!(dbg.contains("SequenceConfig"));
    }

    #[test]
    fn output_debug_and_eq() {
        assert_eq!(SequenceOutput::Pending, SequenceOutput::Pending);
        assert_eq!(SequenceOutput::Esc, SequenceOutput::Esc);
        assert_eq!(SequenceOutput::EscEsc, SequenceOutput::EscEsc);
        assert_eq!(SequenceOutput::PassThrough, SequenceOutput::PassThrough);
        assert_ne!(SequenceOutput::Esc, SequenceOutput::EscEsc);

        let dbg = format!("{:?}", SequenceOutput::EscEsc);
        assert!(dbg.contains("EscEsc"));
    }

    // --- Stress / property-like tests ---

    #[test]
    fn no_stuck_state() {
        let mut detector = SequenceDetector::with_defaults();
        let t = now();

        // Many operations should always return to Idle eventually
        for i in 0..100 {
            let offset = Duration::from_millis(i * 10);
            if i % 3 == 0 {
                detector.feed(&esc_press(), t + offset);
            } else {
                detector.feed(&key_press(KeyCode::Char('x')), t + offset);
            }
        }

        // Force timeout check - must be well past the last event (990ms) + timeout (250ms)
        detector.check_timeout(t + Duration::from_secs(2));

        // Should be idle
        assert!(!detector.is_pending());
    }

    #[test]
    fn deterministic_output() {
        // Same inputs should produce same outputs
        let config = SequenceConfig::default();
        let t = now();

        let mut d1 = SequenceDetector::new(config.clone());
        let mut d2 = SequenceDetector::new(config);

        let events = [
            (esc_press(), t),
            (esc_press(), t + MS_100),
            (key_press(KeyCode::Char('a')), t + MS_200),
            (esc_press(), t + MS_300),
        ];

        for (event, time) in &events {
            let out1 = d1.feed(event, *time);
            let out2 = d2.feed(event, *time);
            assert_eq!(out1, out2);
        }
    }

    // =========================================================================
    // ActionMapper Tests
    // =========================================================================

    mod action_mapper_tests {
        use super::*;
        use crate::event::Modifiers;

        fn ctrl_c() -> KeyEvent {
            KeyEvent::new(KeyCode::Char('c')).with_modifiers(Modifiers::CTRL)
        }

        fn ctrl_d() -> KeyEvent {
            KeyEvent::new(KeyCode::Char('d')).with_modifiers(Modifiers::CTRL)
        }

        fn ctrl_q() -> KeyEvent {
            KeyEvent::new(KeyCode::Char('q')).with_modifiers(Modifiers::CTRL)
        }

        fn idle_state() -> AppState {
            AppState::default()
        }

        fn input_state() -> AppState {
            AppState::new().with_input(true)
        }

        fn task_state() -> AppState {
            AppState::new().with_task(true)
        }

        fn modal_state() -> AppState {
            AppState::new().with_modal(true)
        }

        fn overlay_state() -> AppState {
            AppState::new().with_overlay(true)
        }

        // --- Ctrl+C tests (policy priorities 2-5) ---

        #[test]
        fn test_ctrl_c_clears_nonempty_input() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let action = mapper.map(&ctrl_c(), &input_state(), t);
            assert_eq!(action, Some(Action::ClearInput));
        }

        #[test]
        fn test_ctrl_c_cancels_running_task() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let action = mapper.map(&ctrl_c(), &task_state(), t);
            assert_eq!(action, Some(Action::CancelTask));
        }

        #[test]
        fn test_ctrl_c_quits_when_idle() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let action = mapper.map(&ctrl_c(), &idle_state(), t);
            assert_eq!(action, Some(Action::Quit));
        }

        #[test]
        fn test_ctrl_c_dismisses_modal() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let action = mapper.map(&ctrl_c(), &modal_state(), t);
            assert_eq!(action, Some(Action::DismissModal));
        }

        #[test]
        fn test_ctrl_c_modal_priority_over_input() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            // Both modal and input are set
            let state = AppState::new().with_modal(true).with_input(true);
            let action = mapper.map(&ctrl_c(), &state, t);
            assert_eq!(action, Some(Action::DismissModal));
        }

        #[test]
        fn test_ctrl_c_input_priority_over_task() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let state = AppState::new().with_input(true).with_task(true);
            let action = mapper.map(&ctrl_c(), &state, t);
            assert_eq!(action, Some(Action::ClearInput));
        }

        #[test]
        fn test_ctrl_c_idle_config_noop() {
            let config = ActionConfig::default().with_ctrl_c_idle(CtrlCIdleAction::Noop);
            let mut mapper = ActionMapper::new(config);
            let t = now();

            let action = mapper.map(&ctrl_c(), &idle_state(), t);
            assert_eq!(action, None); // Noop returns None
        }

        #[test]
        fn test_ctrl_c_idle_config_bell() {
            let config = ActionConfig::default().with_ctrl_c_idle(CtrlCIdleAction::Bell);
            let mut mapper = ActionMapper::new(config);
            let t = now();

            let action = mapper.map(&ctrl_c(), &idle_state(), t);
            assert_eq!(action, Some(Action::Bell));
        }

        // --- Ctrl+D and Ctrl+Q tests (policy priorities 10-11) ---

        #[test]
        fn test_ctrl_d_soft_quit() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let action = mapper.map(&ctrl_d(), &idle_state(), t);
            assert_eq!(action, Some(Action::SoftQuit));
        }

        #[test]
        fn test_ctrl_d_ignores_state() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            // Ctrl+D always does SoftQuit regardless of state
            let action = mapper.map(&ctrl_d(), &modal_state(), t);
            assert_eq!(action, Some(Action::SoftQuit));

            let action = mapper.map(&ctrl_d(), &input_state(), t);
            assert_eq!(action, Some(Action::SoftQuit));
        }

        #[test]
        fn test_ctrl_q_hard_quit() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let action = mapper.map(&ctrl_q(), &idle_state(), t);
            assert_eq!(action, Some(Action::HardQuit));
        }

        #[test]
        fn test_ctrl_q_ignores_state() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            // Ctrl+Q always does HardQuit regardless of state
            let action = mapper.map(&ctrl_q(), &modal_state(), t);
            assert_eq!(action, Some(Action::HardQuit));
        }

        // --- Esc tests (policy priorities 1, 6-8) ---

        #[test]
        fn test_esc_dismisses_modal() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            // First Esc: pending
            let action1 = mapper.map(&esc_press(), &modal_state(), t);
            assert_eq!(action1, None);

            // Timeout: emit Esc action
            let action2 = mapper.check_timeout(&modal_state(), t + MS_300);
            assert_eq!(action2, Some(Action::DismissModal));
        }

        #[test]
        fn test_esc_clears_input_no_modal() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            mapper.map(&esc_press(), &input_state(), t);
            let action = mapper.check_timeout(&input_state(), t + MS_300);
            assert_eq!(action, Some(Action::ClearInput));
        }

        #[test]
        fn test_esc_cancels_task_empty_input() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            mapper.map(&esc_press(), &task_state(), t);
            let action = mapper.check_timeout(&task_state(), t + MS_300);
            assert_eq!(action, Some(Action::CancelTask));
        }

        #[test]
        fn test_esc_closes_overlay() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            mapper.map(&esc_press(), &overlay_state(), t);
            let action = mapper.check_timeout(&overlay_state(), t + MS_300);
            assert_eq!(action, Some(Action::CloseOverlay));
        }

        #[test]
        fn test_esc_modal_priority_over_overlay() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let state = AppState::new().with_modal(true).with_overlay(true);
            mapper.map(&esc_press(), &state, t);
            let action = mapper.check_timeout(&state, t + MS_300);
            assert_eq!(action, Some(Action::DismissModal));
        }

        #[test]
        fn test_esc_passthrough_when_idle() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            mapper.map(&esc_press(), &idle_state(), t);
            let action = mapper.check_timeout(&idle_state(), t + MS_300);
            assert_eq!(action, Some(Action::PassThrough));
        }

        // --- Esc Esc tests (policy priority 9) ---

        #[test]
        fn test_esc_esc_within_timeout() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            mapper.map(&esc_press(), &idle_state(), t);
            let action = mapper.map(&esc_press(), &idle_state(), t + MS_100);
            assert_eq!(action, Some(Action::ToggleTreeView));
        }

        #[test]
        fn test_esc_esc_ignores_state() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            // Esc Esc always toggles tree view regardless of state
            mapper.map(&esc_press(), &modal_state(), t);
            let action = mapper.map(&esc_press(), &modal_state(), t + MS_100);
            assert_eq!(action, Some(Action::ToggleTreeView));
        }

        #[test]
        fn test_esc_esc_timeout_expired() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            mapper.map(&esc_press(), &input_state(), t);
            // Past 250ms timeout
            let action = mapper.map(&esc_press(), &input_state(), t + MS_300);

            // First Esc timed out -> ClearInput, second starts new pending
            assert_eq!(action, Some(Action::ClearInput));
            assert!(mapper.is_pending_esc());
        }

        // --- Esc then other key ---

        #[test]
        fn test_esc_then_other_key() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            mapper.map(&esc_press(), &input_state(), t);
            let action = mapper.map(&key_press(KeyCode::Char('a')), &input_state(), t + MS_50);

            // Pending Esc is emitted
            assert_eq!(action, Some(Action::ClearInput));
        }

        // --- Other keys passthrough ---

        #[test]
        fn test_regular_key_passthrough() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let action = mapper.map(&key_press(KeyCode::Char('x')), &idle_state(), t);
            assert_eq!(action, Some(Action::PassThrough));
        }

        #[test]
        fn test_release_event_passthrough() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            let release = KeyEvent::new(KeyCode::Char('x')).with_kind(KeyEventKind::Release);
            let action = mapper.map(&release, &idle_state(), t);
            assert_eq!(action, Some(Action::PassThrough));
        }

        // --- State helper tests ---

        #[test]
        fn test_app_state_builders() {
            let state = AppState::new()
                .with_input(true)
                .with_task(true)
                .with_modal(true)
                .with_overlay(true);

            assert!(state.input_nonempty);
            assert!(state.task_running);
            assert!(state.modal_open);
            assert!(state.view_overlay);
            assert!(!state.is_idle());
        }

        #[test]
        fn test_app_state_is_idle() {
            assert!(AppState::default().is_idle());
            assert!(!AppState::new().with_input(true).is_idle());
            assert!(!AppState::new().with_task(true).is_idle());
            assert!(!AppState::new().with_modal(true).is_idle());
            // view_overlay doesn't affect is_idle
            assert!(AppState::new().with_overlay(true).is_idle());
        }

        // --- Action enum tests ---

        #[test]
        fn test_action_consumes_event() {
            assert!(Action::ClearInput.consumes_event());
            assert!(Action::CancelTask.consumes_event());
            assert!(Action::Quit.consumes_event());
            assert!(!Action::PassThrough.consumes_event());
        }

        #[test]
        fn test_action_is_quit() {
            assert!(Action::Quit.is_quit());
            assert!(Action::SoftQuit.is_quit());
            assert!(Action::HardQuit.is_quit());
            assert!(!Action::ClearInput.is_quit());
            assert!(!Action::PassThrough.is_quit());
        }

        // --- Config tests ---

        #[test]
        fn test_ctrl_c_idle_action_from_str() {
            assert_eq!(
                CtrlCIdleAction::from_str_opt("quit"),
                Some(CtrlCIdleAction::Quit)
            );
            assert_eq!(
                CtrlCIdleAction::from_str_opt("QUIT"),
                Some(CtrlCIdleAction::Quit)
            );
            assert_eq!(
                CtrlCIdleAction::from_str_opt("noop"),
                Some(CtrlCIdleAction::Noop)
            );
            assert_eq!(
                CtrlCIdleAction::from_str_opt("none"),
                Some(CtrlCIdleAction::Noop)
            );
            assert_eq!(
                CtrlCIdleAction::from_str_opt("ignore"),
                Some(CtrlCIdleAction::Noop)
            );
            assert_eq!(
                CtrlCIdleAction::from_str_opt("bell"),
                Some(CtrlCIdleAction::Bell)
            );
            assert_eq!(
                CtrlCIdleAction::from_str_opt("beep"),
                Some(CtrlCIdleAction::Bell)
            );
            assert_eq!(CtrlCIdleAction::from_str_opt("invalid"), None);
        }

        #[test]
        fn test_ctrl_c_idle_action_to_action() {
            assert_eq!(CtrlCIdleAction::Quit.to_action(), Some(Action::Quit));
            assert_eq!(CtrlCIdleAction::Noop.to_action(), None);
            assert_eq!(CtrlCIdleAction::Bell.to_action(), Some(Action::Bell));
        }

        #[test]
        fn test_action_config_builder() {
            let config = ActionConfig::default()
                .with_sequence_config(SequenceConfig::default().with_timeout(MS_100))
                .with_ctrl_c_idle(CtrlCIdleAction::Bell);

            assert_eq!(config.sequence_config.esc_seq_timeout, MS_100);
            assert_eq!(config.ctrl_c_idle_action, CtrlCIdleAction::Bell);
        }

        // --- Reset tests ---

        #[test]
        fn test_mapper_reset() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            mapper.map(&esc_press(), &idle_state(), t);
            assert!(mapper.is_pending_esc());

            mapper.reset();
            assert!(!mapper.is_pending_esc());
        }

        // --- Determinism / property tests ---

        #[test]
        fn test_deterministic_action_mapping() {
            let t = now();

            let mut m1 = ActionMapper::with_defaults();
            let mut m2 = ActionMapper::with_defaults();

            let events = [
                (ctrl_c(), input_state()),
                (ctrl_d(), modal_state()),
                (ctrl_q(), idle_state()),
            ];

            for (event, state) in &events {
                let a1 = m1.map(event, state, t);
                let a2 = m2.map(event, state, t);
                assert_eq!(a1, a2);
            }
        }

        #[test]
        fn test_uppercase_ctrl_keys() {
            let mut mapper = ActionMapper::with_defaults();
            let t = now();

            // Ctrl+C with uppercase 'C' should also work
            let ctrl_c_upper = KeyEvent::new(KeyCode::Char('C')).with_modifiers(Modifiers::CTRL);
            let action = mapper.map(&ctrl_c_upper, &idle_state(), t);
            assert_eq!(action, Some(Action::Quit));
        }

        // --- Validation tests ---

        #[test]
        fn test_sequence_config_validation_clamps_high_timeout() {
            let config = SequenceConfig::default()
                .with_timeout(Duration::from_millis(1000)) // Too high
                .validated();

            // Should clamp to MAX_ESC_SEQ_TIMEOUT_MS (400ms)
            assert_eq!(config.esc_seq_timeout.as_millis(), 400);
        }

        #[test]
        fn test_sequence_config_validation_clamps_low_timeout() {
            let config = SequenceConfig::default()
                .with_timeout(Duration::from_millis(50)) // Too low
                .validated();

            // Should clamp to MIN_ESC_SEQ_TIMEOUT_MS (150ms)
            assert_eq!(config.esc_seq_timeout.as_millis(), 150);
        }

        #[test]
        fn test_sequence_config_validation_clamps_high_debounce() {
            let config = SequenceConfig::default()
                .with_debounce(Duration::from_millis(200)) // Too high
                .validated();

            // Should clamp to MAX_ESC_DEBOUNCE_MS (100ms)
            assert_eq!(config.esc_debounce.as_millis(), 100);
        }

        #[test]
        fn test_sequence_config_validation_debounce_not_exceeds_timeout() {
            let config = SequenceConfig::default()
                .with_timeout(Duration::from_millis(150))
                .with_debounce(Duration::from_millis(200)) // Higher than timeout
                .validated();

            // Debounce should be clamped to min(100, 150) = 100,
            // but also can't exceed timeout (150)
            // Since debounce max is 100 and timeout is 150, debounce = 100
            assert!(config.esc_debounce <= config.esc_seq_timeout);
        }

        #[test]
        fn test_sequence_config_is_valid() {
            assert!(SequenceConfig::default().is_valid());

            // Invalid: timeout too high
            let invalid = SequenceConfig::default().with_timeout(Duration::from_millis(500));
            assert!(!invalid.is_valid());

            // Valid after validation
            assert!(invalid.validated().is_valid());
        }

        #[test]
        fn test_sequence_config_constants() {
            // Verify constants match spec
            assert_eq!(DEFAULT_ESC_SEQ_TIMEOUT_MS, 250);
            assert_eq!(MIN_ESC_SEQ_TIMEOUT_MS, 150);
            assert_eq!(MAX_ESC_SEQ_TIMEOUT_MS, 400);
            assert_eq!(DEFAULT_ESC_DEBOUNCE_MS, 50);
            assert_eq!(MIN_ESC_DEBOUNCE_MS, 0);
            assert_eq!(MAX_ESC_DEBOUNCE_MS, 100);
        }

        #[test]
        fn test_action_config_validated() {
            let config = ActionConfig::default()
                .with_sequence_config(
                    SequenceConfig::default().with_timeout(Duration::from_millis(1000)),
                )
                .validated();

            // Sequence config should be validated
            assert_eq!(config.sequence_config.esc_seq_timeout.as_millis(), 400);
        }
    }
}
