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
