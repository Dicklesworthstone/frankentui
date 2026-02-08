#![forbid(unsafe_code)]
#![doc = "Backend traits for FrankenTUI: platform abstraction for input, presentation, and time."]
#![doc = ""]
#![doc = "This crate defines the boundary between the ftui runtime and platform-specific"]
#![doc = "implementations (native terminal via `ftui-tty`, WASM via `ftui-web`)."]
#![doc = ""]
#![doc = "See ADR-008 for the design rationale."]

use core::time::Duration;

use ftui_core::event::Event;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;

/// Terminal feature toggles that backends must support.
///
/// These map to terminal modes that are enabled/disabled at session start/end.
/// Backends translate these into platform-specific escape sequences or API calls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackendFeatures {
    /// SGR mouse capture (CSI ? 1000;1002;1006 h/l on native).
    pub mouse_capture: bool,
    /// Bracketed paste mode (CSI ? 2004 h/l on native).
    pub bracketed_paste: bool,
    /// Focus-in/focus-out reporting (CSI ? 1004 h/l on native).
    pub focus_events: bool,
    /// Kitty keyboard protocol (CSI > 15 u on native).
    pub kitty_keyboard: bool,
}

/// Monotonic clock abstraction.
///
/// Native backends use `std::time::Instant`; WASM backends use `performance.now()`.
/// The runtime never calls `Instant::now()` directly — all time flows through this trait.
pub trait BackendClock {
    /// Returns elapsed time since an unspecified epoch, monotonically increasing.
    fn now_mono(&self) -> Duration;
}

/// Event source abstraction: terminal size queries, feature toggles, and event I/O.
///
/// This is the input half of the backend boundary. The runtime polls this for
/// canonical `Event` values without knowing whether they come from crossterm,
/// raw Unix reads, or DOM events.
pub trait BackendEventSource {
    /// Platform-specific error type.
    type Error: core::fmt::Debug + core::fmt::Display;

    /// Query current terminal dimensions (columns, rows).
    fn size(&self) -> Result<(u16, u16), Self::Error>;

    /// Enable or disable terminal features (mouse, paste, focus, kitty keyboard).
    ///
    /// Backends must track current state and only emit escape sequences for changes.
    fn set_features(&mut self, features: BackendFeatures) -> Result<(), Self::Error>;

    /// Poll for an available event, returning `true` if one is ready.
    ///
    /// Must not block longer than `timeout`. Returns `Ok(false)` on timeout.
    fn poll_event(&mut self, timeout: Duration) -> Result<bool, Self::Error>;

    /// Read the next available event, or `None` if none is ready.
    ///
    /// Call after `poll_event` returns `true`, or speculatively.
    fn read_event(&mut self) -> Result<Option<Event>, Self::Error>;
}

/// Presentation abstraction: UI rendering and log output.
///
/// This is the output half of the backend boundary. The runtime hands a `Buffer`
/// (and optional `BufferDiff`) to the presenter, which emits platform-specific
/// output (ANSI escape sequences on native, DOM mutations on web).
pub trait BackendPresenter {
    /// Platform-specific error type.
    type Error: core::fmt::Debug + core::fmt::Display;

    /// Terminal capabilities detected by this backend.
    fn capabilities(&self) -> &TerminalCapabilities;

    /// Write a log line to the scrollback region (inline mode) or stderr.
    fn write_log(&mut self, text: &str) -> Result<(), Self::Error>;

    /// Present a UI frame.
    ///
    /// - `buf`: the full rendered buffer for this frame.
    /// - `diff`: optional pre-computed diff (backends may recompute if `None`).
    /// - `full_repaint_hint`: if `true`, the backend should skip diffing and repaint everything.
    fn present_ui(
        &mut self,
        buf: &Buffer,
        diff: Option<&BufferDiff>,
        full_repaint_hint: bool,
    ) -> Result<(), Self::Error>;

    /// Optional: release resources held by the presenter (e.g., grapheme pool compaction).
    fn gc(&mut self) {}
}

/// Unified backend combining clock, event source, and presenter.
///
/// The `Program` runtime is generic over this trait. Concrete implementations:
/// - `ftui-tty`: native Unix/macOS terminal (and eventually Windows).
/// - `ftui-web`: WASM + DOM + WebGPU renderer.
pub trait Backend {
    /// Platform-specific error type shared across sub-traits.
    type Error: core::fmt::Debug + core::fmt::Display;

    /// Clock implementation.
    type Clock: BackendClock;

    /// Event source implementation.
    type Events: BackendEventSource<Error = Self::Error>;

    /// Presenter implementation.
    type Presenter: BackendPresenter<Error = Self::Error>;

    /// Access the monotonic clock.
    fn clock(&self) -> &Self::Clock;

    /// Access the event source (mutable for polling/reading).
    fn events(&mut self) -> &mut Self::Events;

    /// Access the presenter (mutable for rendering).
    fn presenter(&mut self) -> &mut Self::Presenter;
}
