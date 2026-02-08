#![forbid(unsafe_code)]

//! `ftui-web` provides a WASM-friendly backend implementation for FrankenTUI.
//!
//! Design goals:
//! - **Host-driven I/O**: the embedding environment (JS) pushes input events and size changes.
//! - **Deterministic time**: the host advances a monotonic clock explicitly.
//! - **No blocking / no threads**: suitable for `wasm32-unknown-unknown`.
//!
//! This crate intentionally does not bind to `wasm-bindgen` yet. The primary
//! purpose is to provide backend building blocks that `frankenterm-web` can
//! wrap with a stable JS API.

pub mod session_record;
pub mod step_program;

use core::time::Duration;
use std::collections::VecDeque;

use ftui_backend::{Backend, BackendClock, BackendEventSource, BackendFeatures, BackendPresenter};
use ftui_core::event::Event;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;

/// Web backend error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebBackendError {
    /// Generic unsupported operation.
    Unsupported(&'static str),
}

impl core::fmt::Display for WebBackendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for WebBackendError {}

/// Deterministic monotonic clock controlled by the host.
#[derive(Debug, Default, Clone)]
pub struct DeterministicClock {
    now: Duration,
}

impl DeterministicClock {
    /// Create a clock starting at `0`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            now: Duration::ZERO,
        }
    }

    /// Set current monotonic time.
    pub fn set(&mut self, now: Duration) {
        self.now = now;
    }

    /// Advance monotonic time by `dt`.
    pub fn advance(&mut self, dt: Duration) {
        self.now = self.now.saturating_add(dt);
    }
}

impl BackendClock for DeterministicClock {
    fn now_mono(&self) -> Duration {
        self.now
    }
}

/// Host-driven event source for WASM.
///
/// The host is responsible for pushing [`Event`] values and updating size.
#[derive(Debug, Clone)]
pub struct WebEventSource {
    size: (u16, u16),
    features: BackendFeatures,
    queue: VecDeque<Event>,
}

impl WebEventSource {
    /// Create a new event source with an initial size.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            size: (width, height),
            features: BackendFeatures::default(),
            queue: VecDeque::new(),
        }
    }

    /// Update the current size.
    pub fn set_size(&mut self, width: u16, height: u16) {
        self.size = (width, height);
    }

    /// Read back the currently requested backend features.
    #[must_use]
    pub const fn features(&self) -> BackendFeatures {
        self.features
    }

    /// Push a canonical event into the queue.
    pub fn push_event(&mut self, event: Event) {
        self.queue.push_back(event);
    }

    /// Drain all pending events.
    pub fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.queue.drain(..)
    }
}

impl BackendEventSource for WebEventSource {
    type Error = WebBackendError;

    fn size(&self) -> Result<(u16, u16), Self::Error> {
        Ok(self.size)
    }

    fn set_features(&mut self, features: BackendFeatures) -> Result<(), Self::Error> {
        self.features = features;
        Ok(())
    }

    fn poll_event(&mut self, timeout: Duration) -> Result<bool, Self::Error> {
        // WASM backend is host-driven; we never block.
        let _ = timeout;
        Ok(!self.queue.is_empty())
    }

    fn read_event(&mut self) -> Result<Option<Event>, Self::Error> {
        Ok(self.queue.pop_front())
    }
}

/// Captured presentation outputs for host consumption.
#[derive(Debug, Default, Clone)]
pub struct WebOutputs {
    /// Log lines written by the runtime.
    pub logs: Vec<String>,
    /// Last fully-rendered buffer presented.
    pub last_buffer: Option<Buffer>,
    /// Whether the last present requested a full repaint.
    pub last_full_repaint_hint: bool,
}

/// WASM presenter that captures buffers and logs for the host.
#[derive(Debug, Clone)]
pub struct WebPresenter {
    caps: TerminalCapabilities,
    outputs: WebOutputs,
}

impl WebPresenter {
    /// Create a new presenter with modern capabilities.
    #[must_use]
    pub fn new() -> Self {
        Self {
            caps: TerminalCapabilities::modern(),
            outputs: WebOutputs::default(),
        }
    }

    /// Get captured outputs.
    #[must_use]
    pub const fn outputs(&self) -> &WebOutputs {
        &self.outputs
    }

    /// Mutably access captured outputs.
    pub fn outputs_mut(&mut self) -> &mut WebOutputs {
        &mut self.outputs
    }

    /// Take captured outputs, leaving empty defaults.
    pub fn take_outputs(&mut self) -> WebOutputs {
        std::mem::take(&mut self.outputs)
    }
}

impl Default for WebPresenter {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendPresenter for WebPresenter {
    type Error = WebBackendError;

    fn capabilities(&self) -> &TerminalCapabilities {
        &self.caps
    }

    fn write_log(&mut self, text: &str) -> Result<(), Self::Error> {
        self.outputs.logs.push(text.to_owned());
        Ok(())
    }

    fn present_ui(
        &mut self,
        buf: &Buffer,
        diff: Option<&BufferDiff>,
        full_repaint_hint: bool,
    ) -> Result<(), Self::Error> {
        // For now we capture full buffers. A future optimization may store diffs.
        let _ = diff;
        self.outputs.last_buffer = Some(buf.clone());
        self.outputs.last_full_repaint_hint = full_repaint_hint;
        Ok(())
    }
}

/// A minimal, host-driven WASM backend.
///
/// This backend is intended to be driven by a JS host:
/// - push events via [`Self::events_mut`]
/// - advance time via [`Self::clock_mut`]
/// - read rendered buffers via [`Self::presenter_mut`]
#[derive(Debug, Clone)]
pub struct WebBackend {
    clock: DeterministicClock,
    events: WebEventSource,
    presenter: WebPresenter,
}

impl WebBackend {
    /// Create a backend with an initial size.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            clock: DeterministicClock::new(),
            events: WebEventSource::new(width, height),
            presenter: WebPresenter::new(),
        }
    }

    /// Mutably access the clock.
    pub fn clock_mut(&mut self) -> &mut DeterministicClock {
        &mut self.clock
    }

    /// Mutably access the event source.
    pub fn events_mut(&mut self) -> &mut WebEventSource {
        &mut self.events
    }

    /// Mutably access the presenter.
    pub fn presenter_mut(&mut self) -> &mut WebPresenter {
        &mut self.presenter
    }
}

impl Backend for WebBackend {
    type Error = WebBackendError;

    type Clock = DeterministicClock;
    type Events = WebEventSource;
    type Presenter = WebPresenter;

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

    use pretty_assertions::assert_eq;

    #[test]
    fn deterministic_clock_advances_monotonically() {
        let mut c = DeterministicClock::new();
        assert_eq!(c.now_mono(), Duration::ZERO);

        c.advance(Duration::from_millis(10));
        assert_eq!(c.now_mono(), Duration::from_millis(10));

        c.advance(Duration::from_millis(5));
        assert_eq!(c.now_mono(), Duration::from_millis(15));

        // Saturation: don't panic or wrap.
        c.set(Duration::MAX);
        c.advance(Duration::from_secs(1));
        assert_eq!(c.now_mono(), Duration::MAX);
    }

    #[test]
    fn web_event_source_fifo_queue() {
        let mut ev = WebEventSource::new(80, 24);
        assert_eq!(ev.size().unwrap(), (80, 24));
        assert_eq!(ev.poll_event(Duration::from_millis(0)).unwrap(), false);

        ev.push_event(Event::Tick);
        ev.push_event(Event::Resize {
            width: 100,
            height: 40,
        });

        assert_eq!(ev.poll_event(Duration::from_millis(0)).unwrap(), true);
        assert_eq!(ev.read_event().unwrap(), Some(Event::Tick));
        assert_eq!(
            ev.read_event().unwrap(),
            Some(Event::Resize {
                width: 100,
                height: 40,
            })
        );
        assert_eq!(ev.read_event().unwrap(), None);
    }

    #[test]
    fn presenter_captures_logs_and_last_buffer() {
        let mut p = WebPresenter::new();
        p.write_log("hello").unwrap();
        p.write_log("world").unwrap();

        let buf = Buffer::new(2, 2);
        p.present_ui(&buf, None, true).unwrap();

        let outputs = p.take_outputs();
        assert_eq!(outputs.logs, vec!["hello", "world"]);
        assert_eq!(outputs.last_full_repaint_hint, true);
        assert_eq!(outputs.last_buffer.unwrap().width(), 2);
    }
}
