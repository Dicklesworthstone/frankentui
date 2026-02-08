#![forbid(unsafe_code)]
#![doc = "Native Unix terminal backend for FrankenTUI."]
#![doc = ""]
#![doc = "This crate implements the `ftui-backend` traits for native Unix/macOS terminals."]
#![doc = "It replaces Crossterm as the terminal I/O layer (Unix-first; Windows deferred)."]
#![doc = ""]
#![doc = "## Crate Status"]
#![doc = ""]
#![doc = "Skeleton — trait implementations compile but are not yet functional."]
#![doc = "Concrete I/O is added by downstream beads:"]
#![doc = "- bd-lff4p.4.2: raw mode + feature toggles"]
#![doc = "- bd-lff4p.4.3: Unix input reader"]
#![doc = "- bd-lff4p.4.4: resize detection (SIGWINCH)"]

use core::time::Duration;
use std::io;

use ftui_backend::{Backend, BackendClock, BackendEventSource, BackendFeatures, BackendPresenter};
use ftui_core::event::Event;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;

// ── Clock ────────────────────────────────────────────────────────────────

/// Monotonic clock backed by `std::time::Instant`.
pub struct TtyClock {
    epoch: std::time::Instant,
}

impl TtyClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: std::time::Instant::now(),
        }
    }
}

impl Default for TtyClock {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendClock for TtyClock {
    fn now_mono(&self) -> Duration {
        self.epoch.elapsed()
    }
}

// ── Event Source ──────────────────────────────────────────────────────────

/// Native Unix event source (raw terminal bytes → `Event`).
///
/// Currently a skeleton. Real I/O is added by bd-lff4p.4.3.
pub struct TtyEventSource {
    features: BackendFeatures,
    width: u16,
    height: u16,
}

impl TtyEventSource {
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            features: BackendFeatures::default(),
            width,
            height,
        }
    }
}

impl BackendEventSource for TtyEventSource {
    type Error = io::Error;

    fn size(&self) -> Result<(u16, u16), Self::Error> {
        Ok((self.width, self.height))
    }

    fn set_features(&mut self, features: BackendFeatures) -> Result<(), Self::Error> {
        self.features = features;
        // TODO(bd-lff4p.4.2): emit escape sequences for feature toggles
        Ok(())
    }

    fn poll_event(&mut self, _timeout: Duration) -> Result<bool, Self::Error> {
        // TODO(bd-lff4p.4.3): poll raw stdin
        Ok(false)
    }

    fn read_event(&mut self) -> Result<Option<Event>, Self::Error> {
        // TODO(bd-lff4p.4.3): parse raw bytes into Event
        Ok(None)
    }
}

// ── Presenter ────────────────────────────────────────────────────────────

/// Native ANSI presenter (Buffer → escape sequences → stdout).
///
/// Currently a skeleton. Real rendering is wired by later integration beads.
pub struct TtyPresenter {
    capabilities: TerminalCapabilities,
}

impl TtyPresenter {
    #[must_use]
    pub fn new(capabilities: TerminalCapabilities) -> Self {
        Self { capabilities }
    }
}

impl BackendPresenter for TtyPresenter {
    type Error = io::Error;

    fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }

    fn write_log(&mut self, _text: &str) -> Result<(), Self::Error> {
        // TODO: write to scrollback region or stderr
        Ok(())
    }

    fn present_ui(
        &mut self,
        _buf: &Buffer,
        _diff: Option<&BufferDiff>,
        _full_repaint_hint: bool,
    ) -> Result<(), Self::Error> {
        // TODO: emit ANSI escape sequences to stdout
        Ok(())
    }
}

// ── Backend ──────────────────────────────────────────────────────────────

/// Native Unix terminal backend.
///
/// Combines `TtyClock`, `TtyEventSource`, and `TtyPresenter` into a single
/// `Backend` implementation that the ftui runtime can drive.
pub struct TtyBackend {
    clock: TtyClock,
    events: TtyEventSource,
    presenter: TtyPresenter,
}

impl TtyBackend {
    /// Create a new TTY backend with detected terminal capabilities.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            clock: TtyClock::new(),
            events: TtyEventSource::new(width, height),
            presenter: TtyPresenter::new(TerminalCapabilities::detect()),
        }
    }

    /// Create a new TTY backend with explicit capabilities (useful for testing).
    #[must_use]
    pub fn with_capabilities(width: u16, height: u16, capabilities: TerminalCapabilities) -> Self {
        Self {
            clock: TtyClock::new(),
            events: TtyEventSource::new(width, height),
            presenter: TtyPresenter::new(capabilities),
        }
    }
}

impl Backend for TtyBackend {
    type Error = io::Error;
    type Clock = TtyClock;
    type Events = TtyEventSource;
    type Presenter = TtyPresenter;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_monotonic() {
        let clock = TtyClock::new();
        let t1 = clock.now_mono();
        // Spin briefly to ensure measurable elapsed time.
        std::hint::black_box(0..1000).for_each(|_| {});
        let t2 = clock.now_mono();
        assert!(t2 >= t1, "clock must be monotonic");
    }

    #[test]
    fn event_source_reports_size() {
        let src = TtyEventSource::new(80, 24);
        let (w, h) = src.size().unwrap();
        assert_eq!(w, 80);
        assert_eq!(h, 24);
    }

    #[test]
    fn event_source_set_features() {
        let mut src = TtyEventSource::new(80, 24);
        let features = BackendFeatures {
            mouse_capture: true,
            bracketed_paste: true,
            focus_events: false,
            kitty_keyboard: false,
        };
        src.set_features(features).unwrap();
        assert_eq!(src.features, features);
    }

    #[test]
    fn poll_returns_false_on_skeleton() {
        let mut src = TtyEventSource::new(80, 24);
        assert!(!src.poll_event(Duration::from_millis(10)).unwrap());
    }

    #[test]
    fn read_returns_none_on_skeleton() {
        let mut src = TtyEventSource::new(80, 24);
        assert!(src.read_event().unwrap().is_none());
    }

    #[test]
    fn presenter_capabilities() {
        let caps = TerminalCapabilities::detect();
        let presenter = TtyPresenter::new(caps);
        // Just verify it doesn't panic.
        let _c = presenter.capabilities();
    }

    #[test]
    fn backend_construction() {
        let backend = TtyBackend::new(120, 40);
        let (w, h) = backend.events.size().unwrap();
        assert_eq!(w, 120);
        assert_eq!(h, 40);
    }

    #[test]
    fn backend_trait_impl() {
        let mut backend = TtyBackend::new(80, 24);
        let _t = backend.clock().now_mono();
        let (w, h) = backend.events().size().unwrap();
        assert_eq!((w, h), (80, 24));
        let _c = backend.presenter().capabilities();
    }
}
