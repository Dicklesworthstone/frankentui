#![forbid(unsafe_code)]

//! FrankenTUI error model and graceful degradation (bd-1q5.19).
//!
//! # Design Principles
//!
//! 1. **Result everywhere** — no panics in the render path.
//! 2. **Domain-specific errors** — each subsystem has its own typed error so
//!    callers can match on what matters and let the rest propagate.
//! 3. **Graceful degradation** — every error variant maps to a
//!    [`DegradationAction`] that the runtime uses to keep the UI alive.
//! 4. **Observability** — errors carry enough context for tracing spans and
//!    metric counters without requiring the error types to depend on tracing.

use std::fmt;

// ── Domain-Specific Error Types ─────────────────────────────────────────

/// Terminal session and capability errors.
#[derive(Debug)]
pub enum TerminalError {
    /// I/O failure on the terminal file descriptor.
    Io(std::io::Error),
    /// A required capability is missing (e.g. no true-color).
    MissingCapability(&'static str),
    /// Raw mode or alt-screen toggle failed.
    SessionSetup(String),
    /// Terminal size query returned invalid dimensions.
    InvalidSize { width: u16, height: u16 },
}

/// Render pipeline errors.
#[derive(Debug)]
pub enum RenderError {
    /// Buffer allocation failed (e.g. zero-area frame).
    BufferAllocation { width: u16, height: u16 },
    /// Diff computation hit an inconsistency.
    DiffInconsistency(String),
    /// Presenter failed to encode ANSI output.
    PresenterEncode(String),
    /// Frame budget exceeded and degradation reached SkipFrame.
    BudgetExhausted { frame: u64 },
}

/// Layout computation errors.
#[derive(Debug)]
pub enum LayoutError {
    /// Constraints are unsatisfiable (over-constrained).
    UnsatisfiableConstraints(String),
    /// Geometry operation produced invalid result (e.g. negative area).
    InvalidGeometry(String),
    /// Recursive layout depth exceeded the limit.
    RecursionLimit { depth: u32 },
}

/// Widget rendering errors.
#[derive(Debug)]
pub enum WidgetError {
    /// A widget panicked during render (caught by error boundary).
    Panicked {
        widget_name: &'static str,
        message: String,
    },
    /// Widget state is inconsistent and cannot be rendered.
    InvalidState {
        widget_name: &'static str,
        detail: String,
    },
    /// Recovery attempts exhausted after repeated failures.
    RecoveryExhausted {
        widget_name: &'static str,
        attempts: u32,
    },
}

/// Protocol and input parsing errors.
#[derive(Debug)]
pub enum ProtocolError {
    /// VT sequence could not be decoded.
    InvalidSequence(String),
    /// Input stream contained unexpected bytes.
    MalformedInput { offset: usize, byte: u8 },
    /// Clipboard or OSC response was malformed.
    MalformedResponse(String),
}

// ── Unified Error ───────────────────────────────────────────────────────

/// Top-level error type for ftui apps.
///
/// Each variant wraps a domain-specific error. Use [`Error::degradation`] to
/// determine the appropriate recovery action.
#[derive(Debug)]
pub enum Error {
    /// Terminal session or capability failure.
    Terminal(TerminalError),
    /// Render pipeline failure.
    Render(RenderError),
    /// Layout computation failure.
    Layout(LayoutError),
    /// Widget rendering failure.
    Widget(WidgetError),
    /// Protocol or input parsing failure.
    Protocol(ProtocolError),
    /// Raw I/O error (convenience variant for `?` on io::Result).
    Io(std::io::Error),
}

/// Standard result type for ftui APIs.
pub type Result<T> = std::result::Result<T, Error>;

// ── Graceful Degradation ────────────────────────────────────────────────

/// What the runtime should do when an error occurs.
///
/// The runtime inspects this to decide whether to retry, fall back to a
/// simpler rendering mode, or shut down gracefully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradationAction {
    /// Use fallback rendering (ASCII borders, no styling, etc.).
    FallbackRender,
    /// Reuse the previous frame's layout — do not recompute.
    ReusePreviousLayout,
    /// Replace the widget with an error placeholder.
    ErrorPlaceholder,
    /// Skip the current frame entirely.
    SkipFrame,
    /// Drop the malformed input and continue processing.
    DropInput,
    /// The error is unrecoverable — shut down gracefully.
    Shutdown,
}

impl Error {
    /// Determine the graceful degradation action for this error.
    ///
    /// The runtime calls this to decide what to do instead of panicking.
    pub fn degradation(&self) -> DegradationAction {
        match self {
            // Terminal errors
            Self::Terminal(TerminalError::MissingCapability(_)) => {
                DegradationAction::FallbackRender
            }
            Self::Terminal(TerminalError::InvalidSize { .. }) => DegradationAction::SkipFrame,
            Self::Terminal(TerminalError::SessionSetup(_)) => DegradationAction::Shutdown,
            Self::Terminal(TerminalError::Io(_)) => DegradationAction::Shutdown,

            // Render errors
            Self::Render(RenderError::BufferAllocation { .. }) => DegradationAction::SkipFrame,
            Self::Render(RenderError::DiffInconsistency(_)) => DegradationAction::FallbackRender,
            Self::Render(RenderError::PresenterEncode(_)) => DegradationAction::FallbackRender,
            Self::Render(RenderError::BudgetExhausted { .. }) => DegradationAction::SkipFrame,

            // Layout errors
            Self::Layout(LayoutError::UnsatisfiableConstraints(_)) => {
                DegradationAction::ReusePreviousLayout
            }
            Self::Layout(LayoutError::InvalidGeometry(_)) => DegradationAction::ReusePreviousLayout,
            Self::Layout(LayoutError::RecursionLimit { .. }) => {
                DegradationAction::ReusePreviousLayout
            }

            // Widget errors
            Self::Widget(WidgetError::Panicked { .. }) => DegradationAction::ErrorPlaceholder,
            Self::Widget(WidgetError::InvalidState { .. }) => DegradationAction::ErrorPlaceholder,
            Self::Widget(WidgetError::RecoveryExhausted { .. }) => {
                DegradationAction::ErrorPlaceholder
            }

            // Protocol errors
            Self::Protocol(ProtocolError::InvalidSequence(_)) => DegradationAction::DropInput,
            Self::Protocol(ProtocolError::MalformedInput { .. }) => DegradationAction::DropInput,
            Self::Protocol(ProtocolError::MalformedResponse(_)) => DegradationAction::DropInput,

            // Raw I/O
            Self::Io(_) => DegradationAction::Shutdown,
        }
    }

    /// Error type label for metrics and tracing.
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Terminal(_) => "terminal",
            Self::Render(_) => "render",
            Self::Layout(_) => "layout",
            Self::Widget(_) => "widget",
            Self::Protocol(_) => "protocol",
            Self::Io(_) => "io",
        }
    }

    /// Whether the error is recoverable (does not require shutdown).
    pub fn is_recoverable(&self) -> bool {
        !matches!(self.degradation(), DegradationAction::Shutdown)
    }
}

// ── Display ─────────────────────────────────────────────────────────────

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "terminal I/O: {err}"),
            Self::MissingCapability(cap) => write!(f, "missing capability: {cap}"),
            Self::SessionSetup(msg) => write!(f, "session setup: {msg}"),
            Self::InvalidSize { width, height } => {
                write!(f, "invalid terminal size: {width}x{height}")
            }
        }
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferAllocation { width, height } => {
                write!(f, "buffer allocation failed: {width}x{height}")
            }
            Self::DiffInconsistency(msg) => write!(f, "diff inconsistency: {msg}"),
            Self::PresenterEncode(msg) => write!(f, "presenter encode: {msg}"),
            Self::BudgetExhausted { frame } => write!(f, "frame budget exhausted at frame {frame}"),
        }
    }
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsatisfiableConstraints(msg) => {
                write!(f, "unsatisfiable constraints: {msg}")
            }
            Self::InvalidGeometry(msg) => write!(f, "invalid geometry: {msg}"),
            Self::RecursionLimit { depth } => write!(f, "recursion limit at depth {depth}"),
        }
    }
}

impl fmt::Display for WidgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panicked {
                widget_name,
                message,
            } => write!(f, "widget '{widget_name}' panicked: {message}"),
            Self::InvalidState {
                widget_name,
                detail,
            } => write!(f, "widget '{widget_name}' invalid state: {detail}"),
            Self::RecoveryExhausted {
                widget_name,
                attempts,
            } => write!(
                f,
                "widget '{widget_name}' recovery exhausted after {attempts} attempts"
            ),
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSequence(msg) => write!(f, "invalid VT sequence: {msg}"),
            Self::MalformedInput { offset, byte } => {
                write!(f, "malformed input at offset {offset}: byte 0x{byte:02X}")
            }
            Self::MalformedResponse(msg) => write!(f, "malformed response: {msg}"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Terminal(err) => write!(f, "{err}"),
            Self::Render(err) => write!(f, "{err}"),
            Self::Layout(err) => write!(f, "{err}"),
            Self::Widget(err) => write!(f, "{err}"),
            Self::Protocol(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "I/O: {err}"),
        }
    }
}

// ── std::error::Error ───────────────────────────────────────────────────

impl std::error::Error for TerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl std::error::Error for RenderError {}
impl std::error::Error for LayoutError {}
impl std::error::Error for WidgetError {}
impl std::error::Error for ProtocolError {}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Terminal(err) => Some(err),
            Self::Render(err) => Some(err),
            Self::Layout(err) => Some(err),
            Self::Widget(err) => Some(err),
            Self::Protocol(err) => Some(err),
            Self::Io(err) => Some(err),
        }
    }
}

// ── From conversions ────────────────────────────────────────────────────

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<TerminalError> for Error {
    fn from(err: TerminalError) -> Self {
        Self::Terminal(err)
    }
}

impl From<RenderError> for Error {
    fn from(err: RenderError) -> Self {
        Self::Render(err)
    }
}

impl From<LayoutError> for Error {
    fn from(err: LayoutError) -> Self {
        Self::Layout(err)
    }
}

impl From<WidgetError> for Error {
    fn from(err: WidgetError) -> Self {
        Self::Widget(err)
    }
}

impl From<ProtocolError> for Error {
    fn from(err: ProtocolError) -> Self {
        Self::Protocol(err)
    }
}

impl From<std::io::Error> for TerminalError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

// ── DegradationAction Display ───────────────────────────────────────────

impl fmt::Display for DegradationAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FallbackRender => write!(f, "fallback_render"),
            Self::ReusePreviousLayout => write!(f, "reuse_previous_layout"),
            Self::ErrorPlaceholder => write!(f, "error_placeholder"),
            Self::SkipFrame => write!(f, "skip_frame"),
            Self::DropInput => write!(f, "drop_input"),
            Self::Shutdown => write!(f, "shutdown"),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use super::*;

    // ── TerminalError ───────────────────────────────────────────────

    #[test]
    fn terminal_io_error() {
        let io = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broke");
        let err = TerminalError::from(io);
        assert!(format!("{err}").contains("pipe broke"));
        assert!(StdError::source(&err).is_some());
    }

    #[test]
    fn terminal_missing_capability() {
        let err = TerminalError::MissingCapability("true-color");
        assert!(format!("{err}").contains("true-color"));
        assert!(StdError::source(&err).is_none());
    }

    #[test]
    fn terminal_session_setup() {
        let err = TerminalError::SessionSetup("raw mode failed".into());
        assert!(format!("{err}").contains("raw mode failed"));
    }

    #[test]
    fn terminal_invalid_size() {
        let err = TerminalError::InvalidSize {
            width: 0,
            height: 0,
        };
        assert!(format!("{err}").contains("0x0"));
    }

    // ── RenderError ─────────────────────────────────────────────────

    #[test]
    fn render_buffer_allocation() {
        let err = RenderError::BufferAllocation {
            width: 0,
            height: 25,
        };
        assert!(format!("{err}").contains("0x25"));
    }

    #[test]
    fn render_diff_inconsistency() {
        let err = RenderError::DiffInconsistency("size mismatch".into());
        assert!(format!("{err}").contains("size mismatch"));
    }

    #[test]
    fn render_presenter_encode() {
        let err = RenderError::PresenterEncode("write failed".into());
        assert!(format!("{err}").contains("write failed"));
    }

    #[test]
    fn render_budget_exhausted() {
        let err = RenderError::BudgetExhausted { frame: 42 };
        assert!(format!("{err}").contains("42"));
    }

    // ── LayoutError ─────────────────────────────────────────────────

    #[test]
    fn layout_unsatisfiable() {
        let err = LayoutError::UnsatisfiableConstraints("min > max".into());
        assert!(format!("{err}").contains("min > max"));
    }

    #[test]
    fn layout_invalid_geometry() {
        let err = LayoutError::InvalidGeometry("negative width".into());
        assert!(format!("{err}").contains("negative width"));
    }

    #[test]
    fn layout_recursion_limit() {
        let err = LayoutError::RecursionLimit { depth: 256 };
        assert!(format!("{err}").contains("256"));
    }

    // ── WidgetError ─────────────────────────────────────────────────

    #[test]
    fn widget_panicked() {
        let err = WidgetError::Panicked {
            widget_name: "Sparkline",
            message: "index out of bounds".into(),
        };
        assert!(format!("{err}").contains("Sparkline"));
        assert!(format!("{err}").contains("index out of bounds"));
    }

    #[test]
    fn widget_invalid_state() {
        let err = WidgetError::InvalidState {
            widget_name: "Table",
            detail: "selection > len".into(),
        };
        assert!(format!("{err}").contains("Table"));
    }

    #[test]
    fn widget_recovery_exhausted() {
        let err = WidgetError::RecoveryExhausted {
            widget_name: "Chart",
            attempts: 3,
        };
        assert!(format!("{err}").contains("3 attempts"));
    }

    // ── ProtocolError ───────────────────────────────────────────────

    #[test]
    fn protocol_invalid_sequence() {
        let err = ProtocolError::InvalidSequence("ESC[???".into());
        assert!(format!("{err}").contains("ESC[???"));
    }

    #[test]
    fn protocol_malformed_input() {
        let err = ProtocolError::MalformedInput {
            offset: 42,
            byte: 0xFF,
        };
        let msg = format!("{err}");
        assert!(msg.contains("42"));
        assert!(msg.contains("0xFF"));
    }

    #[test]
    fn protocol_malformed_response() {
        let err = ProtocolError::MalformedResponse("bad OSC".into());
        assert!(format!("{err}").contains("bad OSC"));
    }

    // ── Unified Error ───────────────────────────────────────────────

    #[test]
    fn error_from_io() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err: Error = Error::from(io);
        assert!(matches!(err, Error::Io(_)));
        assert!(format!("{err}").contains("file missing"));
    }

    #[test]
    fn error_from_terminal() {
        let err: Error = TerminalError::MissingCapability("mouse").into();
        assert!(matches!(err, Error::Terminal(_)));
    }

    #[test]
    fn error_from_render() {
        let err: Error = RenderError::BudgetExhausted { frame: 1 }.into();
        assert!(matches!(err, Error::Render(_)));
    }

    #[test]
    fn error_from_layout() {
        let err: Error = LayoutError::RecursionLimit { depth: 10 }.into();
        assert!(matches!(err, Error::Layout(_)));
    }

    #[test]
    fn error_from_widget() {
        let err: Error = WidgetError::Panicked {
            widget_name: "X",
            message: "boom".into(),
        }
        .into();
        assert!(matches!(err, Error::Widget(_)));
    }

    #[test]
    fn error_from_protocol() {
        let err: Error = ProtocolError::InvalidSequence("x".into()).into();
        assert!(matches!(err, Error::Protocol(_)));
    }

    // ── Degradation Mapping ─────────────────────────────────────────

    #[test]
    fn degradation_terminal_missing_cap() {
        let err: Error = TerminalError::MissingCapability("tc").into();
        assert_eq!(err.degradation(), DegradationAction::FallbackRender);
    }

    #[test]
    fn degradation_terminal_invalid_size() {
        let err: Error = TerminalError::InvalidSize {
            width: 0,
            height: 0,
        }
        .into();
        assert_eq!(err.degradation(), DegradationAction::SkipFrame);
    }

    #[test]
    fn degradation_terminal_session_is_shutdown() {
        let err: Error = TerminalError::SessionSetup("fail".into()).into();
        assert_eq!(err.degradation(), DegradationAction::Shutdown);
    }

    #[test]
    fn degradation_terminal_io_is_shutdown() {
        let io = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "");
        let err: Error = TerminalError::Io(io).into();
        assert_eq!(err.degradation(), DegradationAction::Shutdown);
    }

    #[test]
    fn degradation_render_buffer_skips_frame() {
        let err: Error = RenderError::BufferAllocation {
            width: 0,
            height: 0,
        }
        .into();
        assert_eq!(err.degradation(), DegradationAction::SkipFrame);
    }

    #[test]
    fn degradation_render_diff_fallback() {
        let err: Error = RenderError::DiffInconsistency("x".into()).into();
        assert_eq!(err.degradation(), DegradationAction::FallbackRender);
    }

    #[test]
    fn degradation_render_presenter_fallback() {
        let err: Error = RenderError::PresenterEncode("x".into()).into();
        assert_eq!(err.degradation(), DegradationAction::FallbackRender);
    }

    #[test]
    fn degradation_render_budget_skips_frame() {
        let err: Error = RenderError::BudgetExhausted { frame: 1 }.into();
        assert_eq!(err.degradation(), DegradationAction::SkipFrame);
    }

    #[test]
    fn degradation_layout_reuses_previous() {
        let err: Error = LayoutError::UnsatisfiableConstraints("x".into()).into();
        assert_eq!(err.degradation(), DegradationAction::ReusePreviousLayout);
    }

    #[test]
    fn degradation_layout_geometry_reuses_previous() {
        let err: Error = LayoutError::InvalidGeometry("x".into()).into();
        assert_eq!(err.degradation(), DegradationAction::ReusePreviousLayout);
    }

    #[test]
    fn degradation_layout_recursion_reuses_previous() {
        let err: Error = LayoutError::RecursionLimit { depth: 1 }.into();
        assert_eq!(err.degradation(), DegradationAction::ReusePreviousLayout);
    }

    #[test]
    fn degradation_widget_panicked_placeholder() {
        let err: Error = WidgetError::Panicked {
            widget_name: "X",
            message: "y".into(),
        }
        .into();
        assert_eq!(err.degradation(), DegradationAction::ErrorPlaceholder);
    }

    #[test]
    fn degradation_widget_invalid_placeholder() {
        let err: Error = WidgetError::InvalidState {
            widget_name: "X",
            detail: "y".into(),
        }
        .into();
        assert_eq!(err.degradation(), DegradationAction::ErrorPlaceholder);
    }

    #[test]
    fn degradation_widget_exhausted_placeholder() {
        let err: Error = WidgetError::RecoveryExhausted {
            widget_name: "X",
            attempts: 3,
        }
        .into();
        assert_eq!(err.degradation(), DegradationAction::ErrorPlaceholder);
    }

    #[test]
    fn degradation_protocol_drops_input() {
        let err: Error = ProtocolError::InvalidSequence("x".into()).into();
        assert_eq!(err.degradation(), DegradationAction::DropInput);
    }

    #[test]
    fn degradation_protocol_malformed_drops_input() {
        let err: Error = ProtocolError::MalformedInput { offset: 0, byte: 0 }.into();
        assert_eq!(err.degradation(), DegradationAction::DropInput);
    }

    #[test]
    fn degradation_protocol_response_drops_input() {
        let err: Error = ProtocolError::MalformedResponse("x".into()).into();
        assert_eq!(err.degradation(), DegradationAction::DropInput);
    }

    #[test]
    fn degradation_io_is_shutdown() {
        let io = std::io::Error::other("");
        let err: Error = Error::Io(io);
        assert_eq!(err.degradation(), DegradationAction::Shutdown);
    }

    // ── Observability helpers ───────────────────────────────────────

    #[test]
    fn error_type_labels() {
        let cases: Vec<(Error, &str)> = vec![
            (TerminalError::MissingCapability("x").into(), "terminal"),
            (RenderError::BudgetExhausted { frame: 1 }.into(), "render"),
            (LayoutError::RecursionLimit { depth: 1 }.into(), "layout"),
            (
                WidgetError::Panicked {
                    widget_name: "X",
                    message: "y".into(),
                }
                .into(),
                "widget",
            ),
            (
                ProtocolError::InvalidSequence("x".into()).into(),
                "protocol",
            ),
            (Error::Io(std::io::Error::other("")), "io"),
        ];

        for (err, expected) in cases {
            assert_eq!(err.error_type(), expected);
        }
    }

    #[test]
    fn is_recoverable() {
        // Recoverable
        assert!(Error::from(TerminalError::MissingCapability("x")).is_recoverable());
        assert!(Error::from(RenderError::BudgetExhausted { frame: 1 }).is_recoverable());
        assert!(Error::from(LayoutError::RecursionLimit { depth: 1 }).is_recoverable());
        assert!(Error::from(ProtocolError::InvalidSequence("x".into())).is_recoverable());

        // Unrecoverable
        assert!(!Error::from(TerminalError::SessionSetup("x".into())).is_recoverable());
        assert!(!Error::Io(std::io::Error::other("")).is_recoverable());
    }

    // ── Error chain ─────────────────────────────────────────────────

    #[test]
    fn error_source_chain() {
        use std::error::Error as StdError;

        let io = std::io::Error::other("root cause");
        let terminal = TerminalError::Io(io);
        let err: Error = terminal.into();

        // Error -> TerminalError
        let source = err.source().expect("should have source");
        assert!(source.to_string().contains("root cause"));

        // TerminalError -> io::Error
        let root = source.source().expect("should chain to io::Error");
        assert!(root.to_string().contains("root cause"));
    }

    // ── Display / Debug ─────────────────────────────────────────────

    #[test]
    fn degradation_action_display() {
        assert_eq!(
            format!("{}", DegradationAction::FallbackRender),
            "fallback_render"
        );
        assert_eq!(
            format!("{}", DegradationAction::ReusePreviousLayout),
            "reuse_previous_layout"
        );
        assert_eq!(
            format!("{}", DegradationAction::ErrorPlaceholder),
            "error_placeholder"
        );
        assert_eq!(format!("{}", DegradationAction::SkipFrame), "skip_frame");
        assert_eq!(format!("{}", DegradationAction::DropInput), "drop_input");
        assert_eq!(format!("{}", DegradationAction::Shutdown), "shutdown");
    }

    #[test]
    fn result_type_alias_with_domain_errors() {
        fn try_render() -> Result<()> {
            Err(RenderError::BudgetExhausted { frame: 1 }.into())
        }

        fn try_layout() -> Result<()> {
            Err(LayoutError::RecursionLimit { depth: 10 }.into())
        }

        assert!(try_render().is_err());
        assert!(try_layout().is_err());
    }

    #[test]
    fn question_mark_propagation() {
        fn io_to_error() -> Result<()> {
            let _ = std::fs::read("/dev/null/nonexistent")?;
            Ok(())
        }

        let result = io_to_error();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().error_type(), "io");
    }
}
