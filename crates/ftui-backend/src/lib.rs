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

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt;
    use ftui_core::terminal_capabilities::TerminalCapabilities;

    // -----------------------------------------------------------------------
    // BackendFeatures tests
    // -----------------------------------------------------------------------

    #[test]
    fn backend_features_default_all_false() {
        let f = BackendFeatures::default();
        assert!(!f.mouse_capture);
        assert!(!f.bracketed_paste);
        assert!(!f.focus_events);
        assert!(!f.kitty_keyboard);
    }

    #[test]
    fn backend_features_equality() {
        let a = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: false,
            focus_events: true,
            kitty_keyboard: false,
        };
        let b = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: false,
            focus_events: true,
            kitty_keyboard: false,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn backend_features_inequality() {
        let a = BackendFeatures::default();
        let b = BackendFeatures {
            mouse_capture: true,
            ..BackendFeatures::default()
        };
        assert_ne!(a, b);
    }

    #[test]
    fn backend_features_clone() {
        let a = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
        };
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn backend_features_debug() {
        let f = BackendFeatures::default();
        let debug = format!("{f:?}");
        assert!(debug.contains("BackendFeatures"));
        assert!(debug.contains("mouse_capture"));
    }

    // -----------------------------------------------------------------------
    // Mock implementations for trait testing
    // -----------------------------------------------------------------------

    struct TestClock {
        elapsed: Duration,
    }

    impl BackendClock for TestClock {
        fn now_mono(&self) -> Duration {
            self.elapsed
        }
    }

    #[derive(Debug)]
    struct TestError(String);

    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "TestError: {}", self.0)
        }
    }

    struct TestEventSource {
        features: BackendFeatures,
        events: Vec<Event>,
    }

    impl BackendEventSource for TestEventSource {
        type Error = TestError;

        fn size(&self) -> Result<(u16, u16), Self::Error> {
            Ok((80, 24))
        }

        fn set_features(&mut self, features: BackendFeatures) -> Result<(), Self::Error> {
            self.features = features;
            Ok(())
        }

        fn poll_event(&mut self, _timeout: Duration) -> Result<bool, Self::Error> {
            Ok(!self.events.is_empty())
        }

        fn read_event(&mut self) -> Result<Option<Event>, Self::Error> {
            Ok(if self.events.is_empty() {
                None
            } else {
                Some(self.events.remove(0))
            })
        }
    }

    struct TestPresenter {
        caps: TerminalCapabilities,
        logs: Vec<String>,
        present_count: usize,
        gc_count: usize,
    }

    impl BackendPresenter for TestPresenter {
        type Error = TestError;

        fn capabilities(&self) -> &TerminalCapabilities {
            &self.caps
        }

        fn write_log(&mut self, text: &str) -> Result<(), Self::Error> {
            self.logs.push(text.to_owned());
            Ok(())
        }

        fn present_ui(
            &mut self,
            _buf: &Buffer,
            _diff: Option<&BufferDiff>,
            _full_repaint_hint: bool,
        ) -> Result<(), Self::Error> {
            self.present_count += 1;
            Ok(())
        }

        fn gc(&mut self) {
            self.gc_count += 1;
        }
    }

    struct TestBackend {
        clock: TestClock,
        events: TestEventSource,
        presenter: TestPresenter,
    }

    impl Backend for TestBackend {
        type Error = TestError;
        type Clock = TestClock;
        type Events = TestEventSource;
        type Presenter = TestPresenter;

        fn clock(&self) -> &Self::Clock {
            &self.clock
        }

        fn events(&mut self) -> &mut Self::Events {
            &mut self.events
        }

        fn presenter(&mut self) -> &mut Self::Presenter {
            &mut self.presenter
        }
    }

    fn make_test_backend() -> TestBackend {
        TestBackend {
            clock: TestClock {
                elapsed: Duration::from_millis(42),
            },
            events: TestEventSource {
                features: BackendFeatures::default(),
                events: Vec::new(),
            },
            presenter: TestPresenter {
                caps: TerminalCapabilities::default(),
                logs: Vec::new(),
                present_count: 0,
                gc_count: 0,
            },
        }
    }

    // -----------------------------------------------------------------------
    // BackendClock tests
    // -----------------------------------------------------------------------

    #[test]
    fn clock_returns_elapsed() {
        let clock = TestClock {
            elapsed: Duration::from_secs(5),
        };
        assert_eq!(clock.now_mono(), Duration::from_secs(5));
    }

    #[test]
    fn clock_zero_duration() {
        let clock = TestClock {
            elapsed: Duration::ZERO,
        };
        assert_eq!(clock.now_mono(), Duration::ZERO);
    }

    // -----------------------------------------------------------------------
    // BackendEventSource tests
    // -----------------------------------------------------------------------

    #[test]
    fn event_source_size() {
        let src = TestEventSource {
            features: BackendFeatures::default(),
            events: Vec::new(),
        };
        assert_eq!(src.size().unwrap(), (80, 24));
    }

    #[test]
    fn event_source_set_features() {
        let mut src = TestEventSource {
            features: BackendFeatures::default(),
            events: Vec::new(),
        };
        let features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: false,
            kitty_keyboard: false,
        };
        src.set_features(features).unwrap();
        assert!(src.features.mouse_capture);
        assert!(src.features.bracketed_paste);
    }

    #[test]
    fn event_source_poll_empty() {
        let mut src = TestEventSource {
            features: BackendFeatures::default(),
            events: Vec::new(),
        };
        assert!(!src.poll_event(Duration::from_millis(10)).unwrap());
    }

    #[test]
    fn event_source_read_none_when_empty() {
        let mut src = TestEventSource {
            features: BackendFeatures::default(),
            events: Vec::new(),
        };
        assert!(src.read_event().unwrap().is_none());
    }

    #[test]
    fn event_source_poll_with_events() {
        let mut src = TestEventSource {
            features: BackendFeatures::default(),
            events: vec![Event::Focus(true)],
        };
        assert!(src.poll_event(Duration::from_millis(10)).unwrap());
    }

    #[test]
    fn event_source_read_drains_events() {
        let mut src = TestEventSource {
            features: BackendFeatures::default(),
            events: vec![Event::Focus(true), Event::Focus(false)],
        };
        let e1 = src.read_event().unwrap();
        assert!(e1.is_some());
        let e2 = src.read_event().unwrap();
        assert!(e2.is_some());
        let e3 = src.read_event().unwrap();
        assert!(e3.is_none());
    }

    // -----------------------------------------------------------------------
    // BackendPresenter tests
    // -----------------------------------------------------------------------

    #[test]
    fn presenter_capabilities() {
        let p = TestPresenter {
            caps: TerminalCapabilities::default(),
            logs: Vec::new(),
            present_count: 0,
            gc_count: 0,
        };
        let _caps = p.capabilities();
    }

    #[test]
    fn presenter_write_log() {
        let mut p = TestPresenter {
            caps: TerminalCapabilities::default(),
            logs: Vec::new(),
            present_count: 0,
            gc_count: 0,
        };
        p.write_log("hello").unwrap();
        p.write_log("world").unwrap();
        assert_eq!(p.logs.len(), 2);
        assert_eq!(p.logs[0], "hello");
        assert_eq!(p.logs[1], "world");
    }

    #[test]
    fn presenter_present_ui() {
        let mut p = TestPresenter {
            caps: TerminalCapabilities::default(),
            logs: Vec::new(),
            present_count: 0,
            gc_count: 0,
        };
        let buf = Buffer::new(10, 5);
        p.present_ui(&buf, None, false).unwrap();
        p.present_ui(&buf, None, true).unwrap();
        assert_eq!(p.present_count, 2);
    }

    #[test]
    fn presenter_gc() {
        let mut p = TestPresenter {
            caps: TerminalCapabilities::default(),
            logs: Vec::new(),
            present_count: 0,
            gc_count: 0,
        };
        p.gc();
        p.gc();
        assert_eq!(p.gc_count, 2);
    }

    // -----------------------------------------------------------------------
    // Unified Backend tests
    // -----------------------------------------------------------------------

    #[test]
    fn backend_clock_access() {
        let backend = make_test_backend();
        assert_eq!(backend.clock().now_mono(), Duration::from_millis(42));
    }

    #[test]
    fn backend_events_access() {
        let mut backend = make_test_backend();
        let size = backend.events().size().unwrap();
        assert_eq!(size, (80, 24));
    }

    #[test]
    fn backend_presenter_access() {
        let mut backend = make_test_backend();
        let buf = Buffer::new(10, 5);
        backend.presenter().present_ui(&buf, None, false).unwrap();
        assert_eq!(backend.presenter.present_count, 1);
    }

    #[test]
    fn backend_full_cycle() {
        let mut backend = make_test_backend();

        // Clock
        let _now = backend.clock().now_mono();

        // Features
        backend
            .events()
            .set_features(BackendFeatures {
                mouse_capture: true,
                ..BackendFeatures::default()
            })
            .unwrap();
        assert!(backend.events.features.mouse_capture);

        // Present
        let buf = Buffer::new(80, 24);
        backend.presenter().write_log("frame start").unwrap();
        backend.presenter().present_ui(&buf, None, false).unwrap();
        backend.presenter().gc();

        assert_eq!(backend.presenter.logs.len(), 1);
        assert_eq!(backend.presenter.present_count, 1);
        assert_eq!(backend.presenter.gc_count, 1);
    }

    // -----------------------------------------------------------------------
    // Error type tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_error_display() {
        let err = TestError("something failed".into());
        assert_eq!(format!("{err}"), "TestError: something failed");
    }

    #[test]
    fn test_error_debug() {
        let err = TestError("oops".into());
        let debug = format!("{err:?}");
        assert!(debug.contains("oops"));
    }
}
