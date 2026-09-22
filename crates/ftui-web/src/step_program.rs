#![forbid(unsafe_code)]

//! Step-based WASM program runner for FrankenTUI.
//!
//! [`StepProgram`] drives an [`ftui_runtime::program::Model`] through
//! init / event / update / view / present cycles without threads or blocking.
//! The host (JavaScript) controls the event loop:
//!
//! 1. Push events via [`StepProgram::push_event`].
//! 2. Advance time via [`StepProgram::advance_time`].
//! 3. Call [`StepProgram::step`] to process one batch of events and render.
//! 4. Read the rendered buffer via [`StepProgram::take_outputs`].
//!
//! # Accessibility
//!
//! Opt in with [`StepProgram::with_accessibility`]. Each rendered frame then
//! collects the widgets' accessibility nodes, finalizes hierarchy and focus,
//! and uses the same diff/announcement policy as the native runtime. A changed
//! tree invokes [`Model::on_accessibility`] after presentation. Mutations and
//! commands from that hook are rendered on a subsequent host step, never by
//! recursively rendering inside the callback.
//!
//! Hosts can read the current tree or bounded text mirror and drain the latest
//! frame's announcements. This local channel is separate from renderer logs:
//! labels, descriptions and input values are never implicitly logged. A host
//! must forward announcements to its accessibility bridge; this runner does
//! not itself create DOM nodes or deliver operating-system accessibility events.
//!
//! # Example
//!
//! ```ignore
//! use ftui_web::step_program::StepProgram;
//! use ftui_core::event::Event;
//! use core::time::Duration;
//!
//! let mut prog = StepProgram::new(MyModel::default(), 80, 24);
//! prog.init().unwrap();
//!
//! // Host-driven frame loop
//! prog.push_event(Event::Tick).expect("tick admission");
//! prog.advance_time(Duration::from_millis(16));
//! let result = prog.step().unwrap();
//!
//! if result.rendered {
//!     let outputs = prog.take_outputs();
//!     // Send outputs.last_buffer to the renderer...
//! }
//! ```

use core::time::Duration;

use ftui_a11y::tree::{
    A11yTree, A11yTreeBuilder, ScreenReaderAnnouncement, ScreenReaderAnnouncements,
    ScreenReaderMirror, ScreenReaderPolicy,
};
use ftui_backend::{BackendClock, BackendEventSource, BackendPresenter};
use ftui_core::event::Event;
use ftui_render::buffer::{Buffer, DoubleBuffer};
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::program::{AccessibilityFrame, Cmd, Model};

use crate::{WebBackend, WebBackendError, WebOutputs};

/// Run grapheme-pool GC every N rendered frames in host-driven WASM mode.
const POOL_GC_INTERVAL_FRAMES: u64 = 256;
/// Minimum supported terminal dimension.
const MIN_TERMINAL_DIMENSION: u16 = 1;

#[inline]
fn clamp_terminal_dimension(value: u16) -> u16 {
    if value < MIN_TERMINAL_DIMENSION {
        MIN_TERMINAL_DIMENSION
    } else {
        value
    }
}

#[inline]
fn clamp_terminal_size(width: u16, height: u16) -> (u16, u16) {
    (
        clamp_terminal_dimension(width),
        clamp_terminal_dimension(height),
    )
}

/// Result of a single [`StepProgram::step`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepResult {
    /// Whether the program is still running (false after `Cmd::Quit`).
    pub running: bool,
    /// Whether a frame was rendered during this step.
    pub rendered: bool,
    /// Number of events processed during this step.
    pub events_processed: u32,
    /// Accepted events still queued, including the unprocessed tail after quit.
    pub events_pending: u32,
    /// Current frame index (monotonically increasing).
    pub frame_idx: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GeometryTransition {
    from_cols: u16,
    from_rows: u16,
    to_cols: u16,
    to_rows: u16,
}

/// Host-driven, non-blocking program runner for WASM.
///
/// Wraps a [`Model`] and a [`WebBackend`], providing a step-based execution
/// model suitable for `wasm32-unknown-unknown`. No threads, no blocking, no
/// `std::time::Instant` — all I/O and time are host-driven.
///
/// # Lifecycle
///
/// 1. [`StepProgram::new`] — create with model and initial terminal size.
/// 2. [`StepProgram::init`] — call once to initialize the model and render the first frame.
/// 3. [`StepProgram::step`] — call repeatedly from the host event loop (e.g., `requestAnimationFrame`).
/// 4. Read outputs after each step via [`StepProgram::take_outputs`].
pub struct StepProgram<M: Model> {
    model: M,
    backend: WebBackend,
    pool: GraphemePool,
    running: bool,
    initialized: bool,
    dirty: bool,
    frame_idx: u64,
    tick_rate: Option<Duration>,
    last_tick: Duration,
    width: u16,
    height: u16,
    /// Double-buffered render target: O(1) swap instead of O(w*h) clone.
    dbl_buf: Option<DoubleBuffer>,
    /// Pending geometry transition that must force a baseline reset + full repaint marker.
    pending_geometry_transition: Option<GeometryTransition>,
    clipboard_requests: Vec<ftui_runtime::program::ClipboardRequest>,
    /// Opt-in, local accessibility channel. No builder is allocated when off.
    accessibility: Option<ScreenReaderPolicy>,
    a11y_tree: A11yTree,
    a11y_order: Vec<u64>,
    a11y_announcements: Vec<ScreenReaderAnnouncement>,
    a11y_dropped: usize,
}

impl<M: Model> StepProgram<M> {
    /// Create a new step program with the given model and initial terminal size.
    #[must_use]
    pub fn new(model: M, width: u16, height: u16) -> Self {
        let (width, height) = clamp_terminal_size(width, height);
        Self {
            model,
            backend: WebBackend::new(width, height),
            pool: GraphemePool::new(),
            running: true,
            initialized: false,
            dirty: true,
            frame_idx: 0,
            tick_rate: None,
            last_tick: Duration::ZERO,
            width,
            height,
            dbl_buf: None,
            pending_geometry_transition: None,
            clipboard_requests: Vec::new(),
            accessibility: None,
            a11y_tree: A11yTree::empty(),
            a11y_order: Vec::new(),
            a11y_announcements: Vec::new(),
            a11y_dropped: 0,
        }
    }

    /// Create a step program with an existing [`WebBackend`].
    #[must_use]
    pub fn with_backend(model: M, mut backend: WebBackend) -> Self {
        let (raw_width, raw_height) = backend.events_mut().size().unwrap_or((80, 24));
        let (width, height) = clamp_terminal_size(raw_width, raw_height);
        backend.events_mut().set_size(width, height);
        Self {
            model,
            backend,
            pool: GraphemePool::new(),
            running: true,
            initialized: false,
            dirty: true,
            frame_idx: 0,
            tick_rate: None,
            last_tick: Duration::ZERO,
            width,
            height,
            dbl_buf: None,
            pending_geometry_transition: None,
            clipboard_requests: Vec::new(),
            accessibility: None,
            a11y_tree: A11yTree::empty(),
            a11y_order: Vec::new(),
            a11y_announcements: Vec::new(),
            a11y_dropped: 0,
        }
    }

    /// Enable the local accessibility channel, normally before [`init`](Self::init).
    ///
    /// A host should choose either its model callback or the drain accessor
    /// for speech delivery, not forward both copies of the same transition.
    #[must_use]
    pub fn with_accessibility(mut self, policy: ScreenReaderPolicy) -> Self {
        self.set_accessibility_policy(Some(policy));
        self
    }

    /// Enable, update, or disable accessibility collection at runtime.
    ///
    /// Disabling immediately releases the previous tree, reading order and
    /// undrained announcements. Re-enabling builds a fresh baseline on the
    /// next rendered step. Changing limits while enabled preserves the tree
    /// baseline so it does not fabricate another focus arrival. Passing the
    /// unchanged policy is a no-op, including for undrained announcements.
    pub fn set_accessibility_policy(&mut self, policy: Option<ScreenReaderPolicy>) {
        if self.accessibility == policy {
            return;
        }
        self.accessibility = policy;
        self.a11y_announcements.clear();
        self.a11y_dropped = 0;
        if policy.is_none() {
            self.a11y_tree = A11yTree::empty();
            self.a11y_order = Vec::new();
            self.a11y_announcements = Vec::new();
        }
        self.dirty = true;
    }

    /// Schedule a repaint for the next [`step`](Self::step).
    ///
    /// Only an event, a tick or an accessibility-policy change marks the frame
    /// stale. A host that reaches the model another way — [`model_mut`] to
    /// select a screen, say — leaves nothing to render, and `step` reports
    /// `rendered: false` until something else happens to arrive.
    ///
    /// [`model_mut`]: Self::model_mut
    pub fn request_redraw(&mut self) {
        self.dirty = true;
    }

    /// The active accessibility policy, or `None` when collection is disabled.
    #[must_use]
    pub fn accessibility_policy(&self) -> Option<ScreenReaderPolicy> {
        self.accessibility
    }

    /// Latest finalized tree (empty until rendering), absent when disabled.
    #[must_use]
    pub fn accessibility_tree(&self) -> Option<&A11yTree> {
        self.accessibility.map(|_| &self.a11y_tree)
    }

    /// Reading order from the latest render, independent of hash-map order.
    #[must_use]
    pub fn accessibility_order(&self) -> &[u64] {
        &self.a11y_order
    }

    /// Announcements from the latest rendered frame, unless already drained.
    ///
    /// Rendering an unchanged tree clears this batch. An idle step that does
    /// not render leaves it alone. Use the drain accessor for at-most-once
    /// forwarding rather than replaying this slice on every animation tick.
    #[must_use]
    pub fn accessibility_announcements(&self) -> &[ScreenReaderAnnouncement] {
        &self.a11y_announcements
    }

    /// Drain the latest frame's bounded batch without consuming visual output.
    ///
    /// Call after `init` and after each rendered `step`. This is a per-frame
    /// channel, not an unbounded queue of old speech: a subsequent render
    /// replaces any undrained batch. `dropped_count` reports the current
    /// frame's policy-cap drops, not missed host reads. A second drain returns
    /// an empty batch with zero drops. The model callback sees the batch before
    /// the host can drain it; hosts must avoid forwarding both channels.
    pub fn take_accessibility_announcements(&mut self) -> ScreenReaderAnnouncements {
        ScreenReaderAnnouncements {
            announcements: std::mem::take(&mut self.a11y_announcements),
            dropped_count: std::mem::take(&mut self.a11y_dropped),
        }
    }

    /// A bounded, deterministic mirror of the latest tree for a host bridge.
    ///
    /// Mirror text is not a live-region announcement. Hosts should expose it
    /// for navigation without automatically speaking the entire tree on every
    /// update. Native semantic bridges should not replay equivalent text too.
    #[must_use]
    pub fn accessibility_mirror(&self) -> Option<ScreenReaderMirror> {
        self.accessibility
            .map(|policy| self.a11y_tree.screen_reader_mirror(policy))
    }

    /// Initialize the model and render the first frame.
    ///
    /// Must be called exactly once before [`step`](Self::step).
    /// Calls `Model::init()`, executes returned commands, and presents
    /// the initial frame.
    pub fn init(&mut self) -> Result<(), WebBackendError> {
        assert!(!self.initialized, "StepProgram::init() called twice");
        self.initialized = true;
        let cmd = self.model.init();
        self.execute_cmd(cmd);
        if self.running {
            self.render_frame()?;
        } else {
            self.backend.events.set_size(self.width, self.height);
        }
        Ok(())
    }

    /// Process one batch of pending events, handle ticks, and render if dirty.
    ///
    /// This is the main entry point for the host event loop. Call this after
    /// pushing events and advancing time.
    ///
    /// Returns [`StepResult`] describing what happened during the step.
    pub fn step(&mut self) -> Result<StepResult, WebBackendError> {
        assert!(self.initialized, "StepProgram::step() called before init()");

        if !self.running {
            return Ok(StepResult {
                running: false,
                rendered: false,
                events_processed: 0,
                events_pending: self.pending_events(),
                frame_idx: self.frame_idx,
            });
        }

        // 1. Process all pending events.
        let mut events_processed: u32 = 0;
        while let Some(event) = self.backend.events.read_event()? {
            events_processed += 1;
            self.handle_event(event);
            if !self.running {
                break;
            }
        }

        // 2. Handle tick if tick_rate is set and enough time has elapsed.
        if self.running
            && let Some(rate) = self.tick_rate
        {
            let now = self.backend.clock.now_mono();
            let delta = now.saturating_sub(self.last_tick);
            let should_tick = if rate.is_zero() { true } else { delta >= rate };
            if should_tick {
                // Preserve remainder when running on high-refresh displays:
                // snapping `last_tick` to the nearest boundary avoids drift and under-ticking.
                if rate.is_zero() {
                    self.last_tick = now;
                } else {
                    let rem_ns = delta.as_nanos() % rate.as_nanos();
                    let rem = Duration::from_nanos(rem_ns as u64);
                    self.last_tick = now.saturating_sub(rem);
                }
                let msg = M::Message::from(Event::Tick);
                let cmd = self.model.update(msg);
                self.dirty = true;
                self.execute_cmd(cmd);
            }
        }

        // 3. Render if dirty.
        let rendered = if self.running && self.dirty {
            self.render_frame()?;
            true
        } else {
            false
        };

        if !self.running {
            // Queued resizes after quit were accepted but never processed.
            self.backend.events.set_size(self.width, self.height);
        }

        Ok(StepResult {
            running: self.running,
            rendered,
            events_processed,
            events_pending: self.pending_events(),
            frame_idx: self.frame_idx,
        })
    }

    /// Push a terminal event into the event queue.
    ///
    /// Accepted events are processed on the next [`step`](Self::step) call.
    /// Rejection leaves the queue and backend size unchanged. Capacity errors
    /// return the event; a stopped program rejects further input as unsupported.
    pub fn push_event(&mut self, event: Event) -> Result<(), WebBackendError> {
        if !self.running {
            return Err(WebBackendError::Unsupported("program has stopped"));
        }
        let event = match event {
            Event::Resize { width, height } => {
                let (width, height) = clamp_terminal_size(width, height);
                Event::Resize { width, height }
            }
            other => other,
        };
        let resize = match &event {
            Event::Resize { width, height } => Some((*width, *height)),
            _ => None,
        };
        self.backend.events_mut().push_event(event)?;
        // Publish the requested size only after its event is admitted. The
        // model and render baseline update when `step()` processes it.
        if let Some((width, height)) = resize {
            self.backend.events_mut().set_size(width, height);
        }
        Ok(())
    }

    /// Advance the deterministic clock by `dt`.
    pub fn advance_time(&mut self, dt: Duration) {
        self.backend.clock_mut().advance(dt);
    }

    /// Set the deterministic clock to an absolute time.
    pub fn set_time(&mut self, now: Duration) {
        self.backend.clock_mut().set(now);
    }

    /// Resize the terminal.
    ///
    /// Pushes a `Resize` event and updates the backend size. The resize
    /// is processed on the next [`step`](Self::step) call.
    pub fn resize(&mut self, width: u16, height: u16) -> Result<(), WebBackendError> {
        self.push_event(Event::Resize { width, height })
    }

    /// Number of accepted events awaiting processing or explicit recovery.
    #[must_use]
    pub fn pending_events(&self) -> u32 {
        self.backend.events.queued_events() as u32
    }

    /// Recover queued events in FIFO order without executing their effects.
    ///
    /// After quit, the host can retain this tail for inspection or another
    /// session. Taking pending input cancels any queued resize request and
    /// restores the backend's requested size to the last processed geometry.
    pub fn take_pending_events(&mut self) -> Vec<Event> {
        let events = self.backend.events.drain_events().collect();
        self.backend.events.set_size(self.width, self.height);
        events
    }

    /// Current deterministic clock value, independently of recording timestamps.
    #[must_use]
    pub fn time(&self) -> Duration {
        self.backend.clock.now_mono()
    }

    /// Take the captured outputs (rendered buffer, logs), leaving empty defaults.
    pub fn take_outputs(&mut self) -> WebOutputs {
        self.backend.presenter_mut().take_outputs()
    }

    /// Drain clipboard effects for the host's clipboard API.
    /// Successful reads should be returned through `push_event(Event::Clipboard(...))`.
    pub fn take_clipboard_requests(&mut self) -> Vec<ftui_runtime::program::ClipboardRequest> {
        std::mem::take(&mut self.clipboard_requests)
    }

    /// Read the captured outputs without consuming them.
    pub fn outputs(&self) -> &WebOutputs {
        self.backend.presenter.outputs()
    }

    /// Access the model.
    pub fn model(&self) -> &M {
        &self.model
    }

    /// Mutably access the model.
    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    /// Access the backend.
    pub fn backend(&self) -> &WebBackend {
        &self.backend
    }

    /// Mutably access the backend.
    pub fn backend_mut(&mut self) -> &mut WebBackend {
        &mut self.backend
    }

    /// Whether the program is still running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether the program has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Current frame index.
    pub fn frame_idx(&self) -> u64 {
        self.frame_idx
    }

    /// Current terminal dimensions.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Current tick rate, if any.
    pub fn tick_rate(&self) -> Option<Duration> {
        self.tick_rate
    }

    /// Access the grapheme pool (needed for deterministic checksumming).
    pub fn pool(&self) -> &GraphemePool {
        &self.pool
    }

    // --- Private helpers ---

    fn handle_event(&mut self, event: Event) {
        if let Event::Resize { width, height } = &event {
            let (prev_width, prev_height) = (self.width, self.height);
            self.width = *width;
            self.height = *height;
            // Invalidate diff baseline for every resize signal.
            // Host-side fit/DPR/zoom transitions can require a repaint boundary
            // even when cols/rows remain numerically unchanged.
            self.dbl_buf = None;
            self.pending_geometry_transition = Some(GeometryTransition {
                from_cols: prev_width,
                from_rows: prev_height,
                to_cols: *width,
                to_rows: *height,
            });
        }
        let msg = M::Message::from(event);
        let cmd = self.model.update(msg);
        self.dirty = true;
        self.execute_cmd(cmd);
    }

    fn render_frame(&mut self) -> Result<(), WebBackendError> {
        // Ensure double buffer exists; first frame triggers allocation.
        let full_repaint = self.dbl_buf.is_none();
        let geometry_transition = if full_repaint {
            self.pending_geometry_transition.take()
        } else {
            None
        };
        if self.dbl_buf.is_none() {
            self.dbl_buf = Some(DoubleBuffer::new(self.width, self.height));
        }

        // Swap: previous current becomes the diff baseline, current is cleared.
        {
            let dbl = self.dbl_buf.as_mut().unwrap();
            dbl.swap();
            dbl.current_mut().clear();
        }

        // Take the cleared buffer out for Frame construction (avoids per-frame
        // allocation). The 1×1 placeholder is trivially cheap.
        let render_buf = std::mem::replace(
            self.dbl_buf.as_mut().unwrap().current_mut(),
            Buffer::new(1, 1),
        );
        let mut a11y_builder = self.accessibility.map(|_| A11yTreeBuilder::new());
        let (rendered_buffer, a11y_order) = {
            let mut frame = Frame::from_buffer(render_buf, &mut self.pool);
            if let Some(builder) = a11y_builder.as_mut() {
                frame.set_a11y(builder);
            }
            self.model.view(&mut frame);
            frame.finish_a11y();
            let order = frame.take_a11y_order();
            (frame.buffer, order)
        };
        let a11y_tree = a11y_builder.map(A11yTreeBuilder::build);

        // Move rendered buffer back into the double buffer's current slot.
        *self.dbl_buf.as_mut().unwrap().current_mut() = rendered_buffer;

        // Compute diff and present.
        let dbl = self.dbl_buf.as_ref().unwrap();
        let diff = if full_repaint {
            None
        } else {
            Some(BufferDiff::compute(dbl.previous(), dbl.current()))
        };
        let buf = dbl.current().clone();

        self.backend
            .presenter_mut()
            .present_ui_owned(buf, diff.as_ref(), full_repaint);

        if let Some(transition) = geometry_transition {
            self.emit_geometry_transition_markers(transition);
        }

        self.dirty = false;
        let rendered_frame_idx = self.frame_idx;
        self.frame_idx += 1;

        // Periodic grapheme-pool GC. Destructure to satisfy the borrow
        // checker: pool and dbl_buf are disjoint fields.
        if self.frame_idx.is_multiple_of(POOL_GC_INTERVAL_FRAMES) {
            let Self { dbl_buf, pool, .. } = self;
            let dbl = dbl_buf.as_ref().unwrap();
            pool.gc(&[dbl.current(), dbl.previous()]);
        }

        if let (Some(tree), Some(policy)) = (a11y_tree, self.accessibility) {
            let diff = tree.diff(&self.a11y_tree);
            let changed = !diff.is_empty();
            let batch = diff.screen_reader_announcements(&tree, policy);
            self.a11y_tree = tree;
            self.a11y_order = a11y_order;
            self.a11y_announcements = batch.announcements;
            self.a11y_dropped = batch.dropped_count;
            if changed {
                // The hook may mutate model state even when it returns None.
                // Match native Program's one-follow-up-frame contract. An
                // identical subsequent tree does not invoke the hook again.
                self.dirty = true;
                let cmd = self.model.on_accessibility(AccessibilityFrame {
                    frame_idx: rendered_frame_idx,
                    tree: &self.a11y_tree,
                    order: &self.a11y_order,
                    announcements: &self.a11y_announcements,
                    dropped: self.a11y_dropped,
                });
                self.execute_cmd(cmd);
            }
        }
        Ok(())
    }

    fn emit_geometry_transition_markers(&mut self, transition: GeometryTransition) {
        let reset_marker = format!(
            r#"{{"event":"diff_baseline_reset","reason":"geometry_transition","from_cols":{},"from_rows":{},"to_cols":{},"to_rows":{},"frame_idx":{}}}"#,
            transition.from_cols,
            transition.from_rows,
            transition.to_cols,
            transition.to_rows,
            self.frame_idx
        );
        let repaint_marker = format!(
            r#"{{"event":"full_repaint_boundary","reason":"geometry_transition","from_cols":{},"from_rows":{},"to_cols":{},"to_rows":{},"frame_idx":{},"full_repaint":true}}"#,
            transition.from_cols,
            transition.from_rows,
            transition.to_cols,
            transition.to_rows,
            self.frame_idx
        );
        let presenter = self.backend.presenter_mut();
        let _ = presenter.write_log(&reset_marker);
        let _ = presenter.write_log(&repaint_marker);
    }

    /// Commands run depth-first from an explicit stack of batch iterators, in
    /// the order recursion gave. Running each `update` result recursively
    /// overflowed the stack for a model that takes one step per `Cmd::Msg`,
    /// which on wasm traps and kills the instance.
    fn execute_cmd(&mut self, cmd: Cmd<M::Message>) {
        let mut batches: Vec<std::vec::IntoIter<Cmd<M::Message>>> = Vec::new();
        let mut next = Some(cmd);
        loop {
            let cmd = match next.take() {
                Some(cmd) => cmd,
                None => {
                    // Resume the innermost batch. A batch stops after any
                    // command that ends the program.
                    let Some(rest) = batches.last_mut() else {
                        return;
                    };
                    if !self.running {
                        return;
                    }
                    match rest.next() {
                        Some(cmd) => cmd,
                        None => {
                            batches.pop();
                            continue;
                        }
                    }
                }
            };
            next = self.execute_one_cmd(cmd, &mut batches);
        }
    }

    /// Execute `cmd` for [`Self::execute_cmd`], returning the command to run
    /// next and pushing the rest of a batch onto `batches`.
    fn execute_one_cmd(
        &mut self,
        cmd: Cmd<M::Message>,
        batches: &mut Vec<std::vec::IntoIter<Cmd<M::Message>>>,
    ) -> Option<Cmd<M::Message>> {
        match cmd {
            Cmd::None => {}
            Cmd::Quit => {
                self.running = false;
            }
            Cmd::Msg(m) => {
                return Some(self.model.update(m));
            }
            Cmd::Batch(cmds) | Cmd::Sequence(cmds) => {
                let mut rest = cmds.into_iter();
                let first = rest.next();
                batches.push(rest);
                return first;
            }
            Cmd::Tick(duration) => {
                self.tick_rate = Some(duration);
            }
            Cmd::Log { text, mode } => {
                let text = ftui_render::sanitize::sanitize_with(&text, mode);
                let _ = self.backend.presenter_mut().write_log(&text);
            }
            Cmd::Task(_spec, f) => {
                // WASM has no threads — execute tasks synchronously.
                let msg = f();
                return Some(self.model.update(msg));
            }
            Cmd::SetMouseCapture(enabled) => {
                let mut features = self.backend.events_mut().features();
                features.mouse_capture = enabled;
                let _ = self.backend.events_mut().set_features(features);
            }
            Cmd::SetClipboard(text) => self
                .clipboard_requests
                .push(ftui_runtime::program::ClipboardRequest::Set(text)),
            Cmd::GetClipboard => self
                .clipboard_requests
                .push(ftui_runtime::program::ClipboardRequest::Get),
            Cmd::SaveState | Cmd::RestoreState => {
                // No persistence in WASM (yet).
            }
            Cmd::SetTickStrategy(_) => {
                // Runtime tick strategy selection is handled by host configuration.
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use ftui_render::cell::Cell;
    use ftui_render::drawing::Draw;
    use pretty_assertions::assert_eq;

    // ---- Test model ----

    #[test]
    fn clipboard_requests_are_drained_for_host_delivery() {
        use ftui_runtime::program::ClipboardRequest;
        let mut program = StepProgram::new(new_counter(0), 10, 5);
        program.init().unwrap();
        program.execute_cmd(Cmd::sequence(vec![
            Cmd::set_clipboard("界"),
            Cmd::get_clipboard(),
        ]));
        assert_eq!(
            program.take_clipboard_requests(),
            [ClipboardRequest::Set("界".into()), ClipboardRequest::Get]
        );
        assert!(program.take_clipboard_requests().is_empty());
        assert_eq!(program.model.value, 0);
    }

    #[test]
    fn long_msg_and_task_chains_run_without_recursion() {
        // One step per `Cmd::Msg` or inline task. Running each `update`
        // result recursively overflowed the stack at a few thousand steps.
        struct Walker {
            steps: u32,
        }

        enum WalkMsg {
            Step(u32),
            Event,
        }

        impl From<Event> for WalkMsg {
            fn from(_: Event) -> Self {
                WalkMsg::Event
            }
        }

        impl Model for Walker {
            type Message = WalkMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    WalkMsg::Step(0) | WalkMsg::Event => Cmd::none(),
                    WalkMsg::Step(n) if n % 2 == 0 => {
                        self.steps += 1;
                        Cmd::msg(WalkMsg::Step(n - 1))
                    }
                    WalkMsg::Step(n) => {
                        self.steps += 1;
                        Cmd::task(move || WalkMsg::Step(n - 1))
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut program = StepProgram::new(Walker { steps: 0 }, 10, 5);
        program.init().unwrap();
        program.execute_cmd(Cmd::msg(WalkMsg::Step(200_000)));
        assert_eq!(program.model.steps, 200_000);
    }

    /// The runtime keeps several command executors — `ProgramSimulator`,
    /// this `StepProgram`, `Program` and the experimental `WasmRunner` — and a
    /// fix to one has repeatedly had to be made in all of them. Nothing held
    /// them to the same answer, so this walks random command trees through two
    /// of them and compares what the model saw.
    #[test]
    fn the_simulator_and_the_step_program_run_a_command_tree_the_same_way() {
        use ftui_runtime::ProgramSimulator;
        use std::cell::RefCell;
        use std::rc::Rc;

        /// A command tree in a form that can be replayed into both runtimes:
        /// `Cmd` holds closures and cannot be cloned.
        #[derive(Clone, Debug)]
        enum Plan {
            None,
            Msg(u32),
            Quit,
            Log(String),
            Batch(Vec<Plan>),
            Sequence(Vec<Plan>),
        }

        fn build(plan: &Plan) -> Cmd<Step> {
            match plan {
                Plan::None => Cmd::none(),
                Plan::Msg(n) => Cmd::msg(Step::Seen(*n)),
                Plan::Quit => Cmd::quit(),
                Plan::Log(text) => Cmd::log(text.clone()),
                Plan::Batch(children) => Cmd::batch(children.iter().map(build).collect::<Vec<_>>()),
                Plan::Sequence(children) => {
                    Cmd::sequence(children.iter().map(build).collect::<Vec<_>>())
                }
            }
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Step {
            Seen(u32),
            Ignored,
        }
        impl From<Event> for Step {
            fn from(_: Event) -> Self {
                Step::Ignored
            }
        }

        struct Recorder {
            plan: Plan,
            seen: Rc<RefCell<Vec<u32>>>,
        }
        impl Model for Recorder {
            type Message = Step;
            fn init(&mut self) -> Cmd<Step> {
                build(&self.plan)
            }
            fn update(&mut self, msg: Step) -> Cmd<Step> {
                if let Step::Seen(n) = msg {
                    self.seen.borrow_mut().push(n);
                }
                Cmd::none()
            }
            fn view(&self, _frame: &mut Frame) {}
        }

        // Deterministic: the same trees on every machine and every run.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let plan = |depth: u32, id: &mut u32, rng: &mut dyn FnMut() -> u64| -> Plan {
            fn go(depth: u32, id: &mut u32, rng: &mut dyn FnMut() -> u64) -> Plan {
                match rng() % if depth == 0 { 4 } else { 6 } {
                    0 => Plan::None,
                    1 | 2 => {
                        *id += 1;
                        Plan::Msg(*id)
                    }
                    3 => {
                        if rng().is_multiple_of(6) {
                            Plan::Quit
                        } else {
                            Plan::Log(format!("log{}", rng() % 100))
                        }
                    }
                    4 => Plan::Batch((0..rng() % 4).map(|_| go(depth - 1, id, rng)).collect()),
                    _ => Plan::Sequence((0..rng() % 4).map(|_| go(depth - 1, id, rng)).collect()),
                }
            }
            go(depth, id, rng)
        };

        let (mut with_messages, mut that_quit) = (0u32, 0u32);
        for trial in 0..2_000 {
            let mut id = 0;
            let tree = plan(3, &mut id, &mut next);

            let simulator_seen = Rc::new(RefCell::new(Vec::new()));
            let mut simulator = ProgramSimulator::new(Recorder {
                plan: tree.clone(),
                seen: Rc::clone(&simulator_seen),
            });
            simulator.init();

            let step_seen = Rc::new(RefCell::new(Vec::new()));
            let mut program = StepProgram::new(
                Recorder {
                    plan: tree.clone(),
                    seen: Rc::clone(&step_seen),
                },
                80,
                24,
            );
            program.init().expect("init the step program");

            let (from_simulator, from_step) =
                (simulator_seen.borrow().clone(), step_seen.borrow().clone());
            if !from_simulator.is_empty() {
                with_messages += 1;
            }
            if !simulator.is_running() {
                that_quit += 1;
            }
            assert_eq!(
                from_simulator, from_step,
                "trial {trial}: the two runtimes delivered different messages for {tree:?}"
            );
            assert_eq!(
                simulator.is_running(),
                program.is_running(),
                "trial {trial}: the two runtimes disagreed about quitting for {tree:?}"
            );
        }

        // An equal pair of empty vectors would prove nothing.
        assert!(
            with_messages > 500 && that_quit > 50,
            "the trees were too dull to compare: {with_messages} delivered a message \
             and {that_quit} quit"
        );
    }

    struct Counter {
        value: i32,
        initialized: bool,
    }

    #[derive(Debug)]
    enum CounterMsg {
        Increment,
        Decrement,
        Reset,
        Quit,
        LogValue,
        BatchIncrement(usize),
        SpawnTask,
    }

    impl From<Event> for CounterMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Key(k) if k.code == KeyCode::Char('+') => CounterMsg::Increment,
                Event::Key(k) if k.code == KeyCode::Char('-') => CounterMsg::Decrement,
                Event::Key(k) if k.code == KeyCode::Char('r') => CounterMsg::Reset,
                Event::Key(k) if k.code == KeyCode::Char('q') => CounterMsg::Quit,
                Event::Tick => CounterMsg::Increment,
                _ => CounterMsg::Increment,
            }
        }
    }

    impl Model for Counter {
        type Message = CounterMsg;

        fn init(&mut self) -> Cmd<Self::Message> {
            self.initialized = true;
            Cmd::none()
        }

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                CounterMsg::Increment => {
                    self.value += 1;
                    Cmd::none()
                }
                CounterMsg::Decrement => {
                    self.value -= 1;
                    Cmd::none()
                }
                CounterMsg::Reset => {
                    self.value = 0;
                    Cmd::none()
                }
                CounterMsg::Quit => Cmd::quit(),
                CounterMsg::LogValue => Cmd::log(format!("value={}", self.value)),
                CounterMsg::BatchIncrement(n) => {
                    let cmds: Vec<_> = (0..n).map(|_| Cmd::msg(CounterMsg::Increment)).collect();
                    Cmd::batch(cmds)
                }
                CounterMsg::SpawnTask => Cmd::task(|| CounterMsg::Increment),
            }
        }

        fn view(&self, frame: &mut Frame) {
            let text = format!("Count: {}", self.value);
            for (i, c) in text.chars().enumerate() {
                if (i as u16) < frame.width() {
                    frame.buffer.set_raw(i as u16, 0, Cell::from_char(c));
                }
            }
        }
    }

    /// Test model that emits a new combining-mark grapheme each frame.
    ///
    /// Used to verify periodic grapheme-pool GC in `StepProgram`.
    struct GraphemeChurn {
        value: u32,
    }

    impl Model for GraphemeChurn {
        type Message = CounterMsg;

        fn init(&mut self) -> Cmd<Self::Message> {
            Cmd::none()
        }

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            if let CounterMsg::Increment = msg {
                self.value = self.value.wrapping_add(1);
            }
            Cmd::none()
        }

        fn view(&self, frame: &mut Frame) {
            let base = char::from_u32(0x4e00 + (self.value % 2048)).unwrap_or('字');
            let text = format!("{base}\u{0301}");
            frame.print_text(0, 0, &text, Cell::default());
        }
    }

    fn key_event(c: char) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    fn new_counter(value: i32) -> Counter {
        Counter {
            value,
            initialized: false,
        }
    }

    fn new_grapheme_churn() -> GraphemeChurn {
        GraphemeChurn { value: 0 }
    }

    // ---- Construction and lifecycle ----

    #[test]
    fn new_creates_uninitialized_program() {
        let prog = StepProgram::new(new_counter(0), 80, 24);
        assert!(!prog.is_initialized());
        assert!(prog.is_running());
        assert_eq!(prog.size(), (80, 24));
        assert_eq!(prog.frame_idx(), 0);
        assert!(prog.tick_rate().is_none());
    }

    #[test]
    fn new_clamps_zero_dimensions_to_minimum() {
        let prog = StepProgram::new(new_counter(0), 0, 0);
        assert_eq!(prog.size(), (1, 1));
    }

    #[test]
    fn with_backend_clamps_zero_dimensions_to_minimum() {
        let backend = WebBackend::new(0, 0);
        let prog = StepProgram::with_backend(new_counter(0), backend);
        assert_eq!(prog.size(), (1, 1));
    }

    #[test]
    fn init_initializes_model_and_renders_first_frame() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        assert!(prog.is_initialized());
        assert!(prog.model().initialized);
        assert_eq!(prog.frame_idx(), 1); // First frame rendered.

        let outputs = prog.outputs();
        assert!(outputs.last_buffer.is_some());
        assert!(outputs.last_full_repaint_hint); // First frame is full repaint.
        assert_eq!(outputs.last_patches.len(), 1);
        let stats = outputs
            .last_patch_stats
            .expect("patch stats should be captured");
        assert_eq!(stats.patch_count, 1);
        assert_eq!(stats.dirty_cells, 80 * 24);
    }

    #[test]
    #[should_panic(expected = "init() called twice")]
    fn double_init_panics() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        prog.init().unwrap();
    }

    #[test]
    #[should_panic(expected = "step() called before init()")]
    fn step_before_init_panics() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        let _ = prog.step();
    }

    // ---- Event processing ----

    #[test]
    fn step_processes_pushed_events() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.push_event(key_event('+')).expect("key admission");
        prog.push_event(key_event('+')).expect("key admission");
        prog.push_event(key_event('+')).expect("key admission");
        let result = prog.step().unwrap();

        assert!(result.running);
        assert!(result.rendered);
        assert_eq!(result.events_processed, 3);
        assert_eq!(prog.model().value, 3);
    }

    #[test]
    fn step_with_no_events_does_not_render() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        // Take initial outputs.
        prog.take_outputs();

        let result = prog.step().unwrap();
        assert!(result.running);
        assert!(!result.rendered);
        assert_eq!(result.events_processed, 0);
    }

    #[test]
    fn quit_event_stops_program() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.push_event(key_event('+')).expect("key admission");
        prog.push_event(key_event('q')).expect("quit admission");
        prog.push_event(key_event('+')).expect("key admission"); // Should not be processed.
        let paste = Event::Paste(ftui_core::event::PasteEvent::bracketed("尾巴 🦀"));
        prog.push_event(paste.clone()).unwrap();
        prog.resize(120, 40).unwrap();
        let result = prog.step().unwrap();

        assert!(!result.running);
        assert!(!result.rendered);
        assert_eq!(result.events_processed, 2);
        assert_eq!(result.events_pending, 3);
        assert!(!prog.is_running());
        assert_eq!(prog.model().value, 1); // Only first '+' processed.
        assert_eq!(prog.size(), (80, 24));
        assert_eq!(prog.backend.events_mut().size().unwrap(), (80, 24));
        assert_eq!(prog.step().unwrap().events_pending, 3);
        assert_eq!(
            prog.take_pending_events(),
            vec![
                key_event('+'),
                paste,
                Event::Resize {
                    width: 120,
                    height: 40
                }
            ]
        );
        assert_eq!(prog.pending_events(), 0);
        assert_eq!(prog.backend.events.queued_payload_bytes(), 0);
        assert!(prog.take_pending_events().is_empty());
        assert_eq!(prog.model().value, 1);
    }

    #[test]
    fn step_after_quit_returns_immediately() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.push_event(key_event('q')).expect("quit admission");
        prog.step().unwrap();

        // Stopped programs reject new input and geometry changes.
        assert_eq!(
            prog.push_event(key_event('+')),
            Err(WebBackendError::Unsupported("program has stopped"))
        );
        assert_eq!(
            prog.resize(120, 40),
            Err(WebBackendError::Unsupported("program has stopped"))
        );
        assert_eq!(prog.size(), (80, 24));
        let result = prog.step().unwrap();
        assert!(!result.running);
        assert!(!result.rendered);
        assert_eq!(result.events_processed, 0);
        assert_eq!(prog.model().value, 0);
    }

    // ---- Resize ----

    #[test]
    fn rejected_resize_preserves_geometry_and_accepted_input_until_recovery() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        for _ in 0..crate::WebEventSource::MAX_EVENTS {
            prog.push_event(key_event('+')).unwrap();
        }
        assert!(matches!(
            prog.resize(120, 40),
            Err(WebBackendError::InputQueueFull {
                limit: crate::WebInputLimit::Events,
                event: Event::Resize {
                    width: 120,
                    height: 40
                },
            })
        ));
        assert_eq!(prog.backend.events_mut().size().unwrap(), (80, 24));
        assert_eq!(prog.size(), (80, 24));
        let result = prog.step().unwrap();
        assert_eq!(
            result.events_processed as usize,
            crate::WebEventSource::MAX_EVENTS
        );
        assert_eq!(
            prog.model().value as usize,
            crate::WebEventSource::MAX_EVENTS
        );
        assert_eq!(prog.size(), (80, 24));
        prog.resize(120, 40).unwrap();
        assert_eq!(prog.step().unwrap().events_processed, 1);
        assert_eq!(prog.size(), (120, 40));
        assert_eq!(prog.backend.events_mut().size().unwrap(), (120, 40));
    }

    #[test]
    fn resize_updates_dimensions() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.resize(120, 40).expect("resize admission");
        prog.step().unwrap();

        assert_eq!(prog.size(), (120, 40));
    }

    #[test]
    fn resize_clamps_zero_dimensions_to_minimum() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.resize(0, 0).expect("resize admission");
        prog.step().unwrap();

        assert_eq!(prog.size(), (1, 1));
        let outputs = prog.outputs();
        let buf = outputs.last_buffer.as_ref().expect("resize should render");
        assert_eq!(buf.width(), 1);
        assert_eq!(buf.height(), 1);
    }

    #[test]
    fn resize_produces_correctly_sized_buffer() {
        let mut prog = StepProgram::new(new_counter(42), 80, 24);
        prog.init().unwrap();

        prog.resize(40, 10).expect("resize admission");
        prog.step().unwrap();

        let outputs = prog.outputs();
        let buf = outputs.last_buffer.as_ref().unwrap();
        assert_eq!(buf.width(), 40);
        assert_eq!(buf.height(), 10);
    }

    #[test]
    fn resize_emits_baseline_reset_and_full_repaint_markers() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        let _ = prog.take_outputs(); // discard init frame

        prog.resize(120, 40).expect("resize admission");
        prog.step().unwrap();

        let outputs = prog.outputs();
        assert!(
            outputs
                .logs
                .iter()
                .any(|line| line.contains(r#""event":"diff_baseline_reset""#))
        );
        assert!(
            outputs
                .logs
                .iter()
                .any(|line| line.contains(r#""event":"full_repaint_boundary""#))
        );
        assert!(
            outputs
                .logs
                .iter()
                .any(|line| line.contains(r#""from_cols":80"#) && line.contains(r#""to_cols":120"#))
        );
    }

    #[test]
    fn same_size_resize_still_forces_repaint_boundary() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        let _ = prog.take_outputs(); // discard init frame

        prog.resize(80, 24).expect("resize admission");
        prog.step().unwrap();

        let outputs = prog.take_outputs();
        assert!(outputs.last_full_repaint_hint);
        assert_eq!(outputs.last_patches.len(), 1);
        assert_eq!(outputs.last_patches[0].offset, 0);
        assert_eq!(outputs.last_patches[0].cells.len(), 80usize * 24usize);
        assert!(outputs.logs.iter().any(|line| {
            line.contains(r#""event":"diff_baseline_reset""#)
                && line.contains(r#""from_cols":80"#)
                && line.contains(r#""to_cols":80"#)
        }));
        assert!(outputs.logs.iter().any(|line| {
            line.contains(r#""event":"full_repaint_boundary""#)
                && line.contains(r#""from_rows":24"#)
                && line.contains(r#""to_rows":24"#)
        }));
    }

    #[test]
    fn resize_oscillation_forces_full_repaint_without_stale_patch_offsets() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        let _ = prog.take_outputs();

        for (w, h) in [(120, 40), (80, 24), (120, 40), (80, 24)] {
            prog.resize(w, h).expect("resize admission");
            prog.step().unwrap();

            let outputs = prog.take_outputs();
            assert!(outputs.last_full_repaint_hint);

            let buf = outputs
                .last_buffer
                .expect("resize render should produce a buffer");
            assert_eq!(buf.width(), w);
            assert_eq!(buf.height(), h);

            let max_cells = usize::from(w) * usize::from(h);
            assert_eq!(outputs.last_patches.len(), 1);
            let run = &outputs.last_patches[0];
            assert_eq!(run.offset, 0);
            assert_eq!(run.cells.len(), max_cells);
            assert!(run.offset as usize + run.cells.len() <= max_cells);
        }
    }

    #[test]
    fn resize_boundary_full_repaint_then_incremental_diff_resume() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        let _ = prog.take_outputs();

        prog.resize(100, 30).expect("resize admission");
        prog.step().unwrap();
        let after_resize = prog.take_outputs();
        assert!(after_resize.last_full_repaint_hint);

        prog.push_event(key_event('+')).expect("key admission");
        prog.step().unwrap();
        let after_increment = prog.take_outputs();
        assert!(!after_increment.last_full_repaint_hint);
        assert!(!after_increment.last_patches.is_empty());
    }

    // ---- Tick handling ----

    #[test]
    fn tick_fires_when_rate_elapsed() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        // Schedule tick at 100ms intervals.
        // Will map to Increment, but we use send for ScheduleTick.
        prog.push_event(key_event('+')).expect("key admission");
        prog.step().unwrap();

        // Manually set tick rate (since our test model doesn't emit ScheduleTick from events).
        prog.model_mut().value = 0;
        // Directly use the Cmd to schedule ticks through a dedicated message.
        prog.execute_cmd(Cmd::tick(Duration::from_millis(100)));
        prog.dirty = false; // Reset dirty so we can detect tick-triggered renders.

        // Advance less than tick rate — no tick.
        prog.advance_time(Duration::from_millis(50));
        let result = prog.step().unwrap();
        assert_eq!(prog.model().value, 0);
        assert!(!result.rendered);

        // Advance past tick rate — tick fires.
        prog.advance_time(Duration::from_millis(60));
        let result = prog.step().unwrap();
        assert_eq!(prog.model().value, 1); // Tick -> Increment.
        assert!(result.rendered);
    }

    #[test]
    fn tick_uses_deterministic_clock() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        prog.execute_cmd(Cmd::tick(Duration::from_millis(100)));

        // Set absolute time to trigger tick.
        prog.set_time(Duration::from_millis(200));
        prog.step().unwrap();
        assert_eq!(prog.model().value, 1);

        // Advance to next tick boundary.
        prog.set_time(Duration::from_millis(350));
        prog.step().unwrap();
        assert_eq!(prog.model().value, 2);
    }

    // ---- Command execution ----

    #[test]
    fn log_command_captures_to_presenter() {
        let mut prog = StepProgram::new(new_counter(5), 80, 24);
        prog.init().unwrap();

        // LogValue emits Cmd::log("value=5").
        prog.execute_cmd(Cmd::msg(CounterMsg::LogValue));

        let outputs = prog.outputs();
        assert_eq!(outputs.logs, vec!["value=5"]);
    }

    #[test]
    fn log_modes_capture_policy_text_without_terminal_formatting() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        let payload = "a\x1b[31mb\x1b[2Jc\x1b]0;secret\x07d\n";
        prog.execute_cmd(Cmd::sequence(vec![
            Cmd::log(payload),
            Cmd::log_sgr_only(payload),
            Cmd::log_raw(payload),
            Cmd::quit(),
            Cmd::log("unreachable"),
        ]));
        assert!(!prog.is_running());
        assert_eq!(prog.outputs().logs, ["abcd\n", "a\x1b[31mbcd\n", payload]);
    }

    #[test]
    fn batch_command_executes_all() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.execute_cmd(Cmd::msg(CounterMsg::BatchIncrement(5)));
        assert_eq!(prog.model().value, 5);
    }

    #[test]
    fn task_executes_synchronously() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.execute_cmd(Cmd::msg(CounterMsg::SpawnTask));
        assert_eq!(prog.model().value, 1); // Task returns Increment.
    }

    #[test]
    fn set_mouse_capture_updates_features() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.execute_cmd(Cmd::set_mouse_capture(true));
        assert!(prog.backend().events.features().mouse_capture);

        prog.execute_cmd(Cmd::set_mouse_capture(false));
        assert!(!prog.backend().events.features().mouse_capture);
    }

    // ---- Rendering ----

    #[test]
    fn rendered_buffer_reflects_model_state() {
        let mut prog = StepProgram::new(new_counter(42), 80, 24);
        prog.init().unwrap();

        let outputs = prog.outputs();
        let buf = outputs.last_buffer.as_ref().unwrap();

        // "Count: 42"
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('C'));
        assert_eq!(buf.get(7, 0).unwrap().content.as_char(), Some('4'));
        assert_eq!(buf.get(8, 0).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn subsequent_renders_produce_diffs() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        // First frame is full repaint.
        let outputs = prog.take_outputs();
        assert!(outputs.last_full_repaint_hint);

        // Second frame after an event should not be full repaint.
        prog.push_event(key_event('+')).expect("key admission");
        prog.step().unwrap();

        let outputs = prog.outputs();
        assert!(!outputs.last_full_repaint_hint);
        assert!(!outputs.last_patches.is_empty());
        let stats = outputs
            .last_patch_stats
            .expect("patch stats should be captured");
        assert!(stats.patch_count >= 1);
        assert!(stats.dirty_cells >= 1);
    }

    #[test]
    fn take_outputs_clears_state() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        let outputs = prog.take_outputs();
        assert!(outputs.last_buffer.is_some());

        // After take, outputs should be empty.
        let outputs = prog.outputs();
        assert!(outputs.last_buffer.is_none());
        assert!(outputs.logs.is_empty());
    }

    // ---- Determinism ----

    #[test]
    fn identical_inputs_produce_identical_outputs() {
        fn run_scenario() -> (i32, u64, Vec<Option<char>>) {
            let mut prog = StepProgram::new(new_counter(0), 20, 1);
            prog.init().unwrap();

            prog.push_event(key_event('+')).expect("key admission");
            prog.push_event(key_event('+')).expect("key admission");
            prog.push_event(key_event('-')).expect("key admission");
            prog.push_event(key_event('+')).expect("key admission");
            prog.step().unwrap();

            let outputs = prog.outputs();
            let buf = outputs.last_buffer.as_ref().unwrap();
            let chars: Vec<Option<char>> = (0..20)
                .map(|x| buf.get(x, 0).and_then(|c| c.content.as_char()))
                .collect();

            (prog.model().value, prog.frame_idx(), chars)
        }

        let (v1, f1, c1) = run_scenario();
        let (v2, f2, c2) = run_scenario();
        let (v3, f3, c3) = run_scenario();

        assert_eq!(v1, v2);
        assert_eq!(v2, v3);
        assert_eq!(v1, 2); // +1+1-1+1 = 2
        assert_eq!(f1, f2);
        assert_eq!(f2, f3);
        assert_eq!(c1, c2);
        assert_eq!(c2, c3);
    }

    // ---- with_backend constructor ----

    #[test]
    fn with_backend_uses_provided_backend() {
        let mut backend = WebBackend::new(100, 50);
        backend.clock_mut().set(Duration::from_secs(10));

        let prog = StepProgram::with_backend(new_counter(0), backend);
        assert_eq!(prog.size(), (100, 50));
    }

    // ---- Multi-step scenario ----

    #[test]
    fn multi_step_interaction() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        // Frame 1: increment twice.
        prog.push_event(key_event('+')).expect("key admission");
        prog.push_event(key_event('+')).expect("key admission");
        let r1 = prog.step().unwrap();
        assert_eq!(r1.events_processed, 2);
        assert!(r1.rendered);
        assert_eq!(prog.model().value, 2);

        // Frame 2: decrement once.
        prog.push_event(key_event('-')).expect("key admission");
        let r2 = prog.step().unwrap();
        assert_eq!(r2.events_processed, 1);
        assert_eq!(prog.model().value, 1);

        // Frame 3: no events.
        let r3 = prog.step().unwrap();
        assert_eq!(r3.events_processed, 0);
        assert!(!r3.rendered);

        // Frame indices are monotonic.
        assert!(r2.frame_idx > r1.frame_idx);
        assert_eq!(r3.frame_idx, r2.frame_idx); // No render, same index.
    }

    #[test]
    fn periodic_pool_gc_bounds_grapheme_growth() {
        let mut prog = StepProgram::new(new_grapheme_churn(), 8, 1);
        prog.init().unwrap();
        prog.execute_cmd(Cmd::tick(Duration::from_millis(1)));

        let mut peak_pool_len = prog.pool().len();
        for _ in 0..2000 {
            prog.advance_time(Duration::from_millis(1));
            let _ = prog.step().unwrap();
            peak_pool_len = peak_pool_len.max(prog.pool().len());
        }

        let final_pool_len = prog.pool().len();
        assert!(
            peak_pool_len <= (POOL_GC_INTERVAL_FRAMES as usize).saturating_add(2),
            "peak grapheme pool length should stay bounded by GC interval (peak={peak_pool_len})"
        );
        assert!(
            final_pool_len <= (POOL_GC_INTERVAL_FRAMES as usize).saturating_add(2),
            "final grapheme pool length should stay bounded by GC interval (final={final_pool_len})"
        );
    }
}
