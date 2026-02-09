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
use ftui_render::cell::{Cell, CellContent};
use ftui_render::diff::BufferDiff;

const GRAPHEME_FALLBACK_CODEPOINT: u32 = '□' as u32;
const ATTR_STYLE_MASK: u32 = 0xFF;
const ATTR_LINK_ID_MAX: u32 = 0x00FF_FFFF;
const WEB_PATCH_CELL_BYTES: u64 = 16;
const PATCH_HASH_ALGO: &str = "fnv1a64";
const FNV64_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV64_PRIME: u64 = 0x100000001b3;

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
    /// Last emitted incremental/full patch runs in row-major order.
    pub last_patches: Vec<WebPatchRun>,
    /// Aggregate patch upload accounting for the last present.
    pub last_patch_stats: Option<WebPatchStats>,
    /// Deterministic hash of the last patch batch (row-major run order).
    pub last_patch_hash: Option<String>,
    /// Whether the last present requested a full repaint.
    pub last_full_repaint_hint: bool,
}

impl WebOutputs {
    /// Flatten patch runs for low-overhead JS/WASM bridge transport.
    ///
    /// Cells are emitted as a contiguous `u32` payload in:
    /// `[bg, fg, glyph, attrs]` order. Spans are emitted as `u32` pairs:
    /// `[offset, len, offset, len, ...]`.
    #[must_use]
    pub fn flatten_patches_u32(&self) -> WebFlatPatchBatch {
        let total_cells = self
            .last_patches
            .iter()
            .map(|patch| patch.cells.len())
            .sum::<usize>();
        let mut cells = Vec::with_capacity(total_cells.saturating_mul(4));
        let mut spans = Vec::with_capacity(self.last_patches.len().saturating_mul(2));

        for patch in &self.last_patches {
            spans.push(patch.offset);
            let len = patch.cells.len().min(u32::MAX as usize) as u32;
            spans.push(len);

            for cell in &patch.cells {
                cells.push(cell.bg);
                cells.push(cell.fg);
                cells.push(cell.glyph);
                cells.push(cell.attrs);
            }
        }

        WebFlatPatchBatch { cells, spans }
    }
}

/// One GPU patch cell payload (`bg`, `fg`, `glyph`, `attrs`) matching the
/// `frankenterm-web` `applyPatch` schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebPatchCell {
    pub bg: u32,
    pub fg: u32,
    pub glyph: u32,
    pub attrs: u32,
}

/// One contiguous run of changed cells starting at linear `offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebPatchRun {
    pub offset: u32,
    pub cells: Vec<WebPatchCell>,
}

/// Compact, flat patch payload for JS/WASM transport.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WebFlatPatchBatch {
    /// Cell payload in `[bg, fg, glyph, attrs]` order.
    pub cells: Vec<u32>,
    /// Span payload in `[offset, len, offset, len, ...]` order.
    pub spans: Vec<u32>,
}

/// Aggregate patch-upload stats for host instrumentation and JSONL reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebPatchStats {
    pub dirty_cells: u32,
    pub patch_count: u32,
    pub bytes_uploaded: u64,
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

    /// Present a frame, taking ownership of the buffer to avoid cloning.
    ///
    /// This is the zero-copy fast path for callers that can give up ownership
    /// (e.g. `StepProgram::render_frame`). The buffer is moved directly into
    /// `last_buffer` instead of being cloned.
    pub fn present_ui_owned(
        &mut self,
        buf: Buffer,
        diff: Option<&BufferDiff>,
        full_repaint_hint: bool,
    ) {
        let patches = build_patch_runs(&buf, diff, full_repaint_hint);
        let stats = patch_batch_stats(&patches);
        let patch_hash = patch_batch_hash(&patches);
        self.outputs.last_buffer = Some(buf);
        self.outputs.last_patches = patches;
        self.outputs.last_patch_stats = Some(stats);
        self.outputs.last_patch_hash = Some(patch_hash);
        self.outputs.last_full_repaint_hint = full_repaint_hint;
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
        let patches = build_patch_runs(buf, diff, full_repaint_hint);
        let stats = patch_batch_stats(&patches);
        let patch_hash = patch_batch_hash(&patches);
        self.outputs.last_buffer = Some(buf.clone());
        self.outputs.last_patches = patches;
        self.outputs.last_patch_stats = Some(stats);
        self.outputs.last_patch_hash = Some(patch_hash);
        self.outputs.last_full_repaint_hint = full_repaint_hint;
        Ok(())
    }
}

#[must_use]
fn fnv1a64_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV64_PRIME);
    }
    hash
}

#[must_use]
fn cell_to_patch(cell: &Cell) -> WebPatchCell {
    let glyph = match cell.content {
        CellContent::EMPTY | CellContent::CONTINUATION => 0,
        other if other.is_grapheme() => GRAPHEME_FALLBACK_CODEPOINT,
        other => other.as_char().map_or(0, |c| c as u32),
    };
    let style_bits = u32::from(cell.attrs.flags().bits()) & ATTR_STYLE_MASK;
    let link_id = cell.attrs.link_id().min(ATTR_LINK_ID_MAX);
    WebPatchCell {
        bg: cell.bg.0,
        fg: cell.fg.0,
        glyph,
        attrs: style_bits | (link_id << 8),
    }
}

#[must_use]
fn full_buffer_patch(buffer: &Buffer) -> WebPatchRun {
    let cols = buffer.width();
    let rows = buffer.height();
    let total = usize::from(cols) * usize::from(rows);
    let mut cells = Vec::with_capacity(total);
    for y in 0..rows {
        for x in 0..cols {
            cells.push(cell_to_patch(buffer.get_unchecked(x, y)));
        }
    }
    WebPatchRun { offset: 0, cells }
}

#[must_use]
fn diff_to_patches(buffer: &Buffer, diff: &BufferDiff) -> Vec<WebPatchRun> {
    if diff.is_empty() {
        return Vec::new();
    }
    let cols = u32::from(buffer.width());
    let mut patches = Vec::new();
    let mut span_start: u32 = 0;
    let mut span_cells: Vec<WebPatchCell> = Vec::new();
    let mut prev_offset: u32 = 0;
    let mut has_span = false;

    for &(x, y) in diff.changes() {
        let offset = u32::from(y) * cols + u32::from(x);
        if !has_span {
            span_start = offset;
            prev_offset = offset;
            has_span = true;
            span_cells.push(cell_to_patch(buffer.get_unchecked(x, y)));
            continue;
        }
        if offset == prev_offset {
            continue;
        }
        if offset == prev_offset + 1 {
            span_cells.push(cell_to_patch(buffer.get_unchecked(x, y)));
        } else {
            patches.push(WebPatchRun {
                offset: span_start,
                cells: std::mem::take(&mut span_cells),
            });
            span_start = offset;
            span_cells.push(cell_to_patch(buffer.get_unchecked(x, y)));
        }
        prev_offset = offset;
    }
    if !span_cells.is_empty() {
        patches.push(WebPatchRun {
            offset: span_start,
            cells: span_cells,
        });
    }
    patches
}

#[must_use]
fn build_patch_runs(
    buffer: &Buffer,
    diff: Option<&BufferDiff>,
    full_repaint_hint: bool,
) -> Vec<WebPatchRun> {
    if full_repaint_hint {
        return vec![full_buffer_patch(buffer)];
    }
    match diff {
        Some(dirty) => diff_to_patches(buffer, dirty),
        None => vec![full_buffer_patch(buffer)],
    }
}

#[must_use]
fn patch_batch_stats(patches: &[WebPatchRun]) -> WebPatchStats {
    let dirty_cells_u64 = patches
        .iter()
        .map(|patch| patch.cells.len() as u64)
        .sum::<u64>();
    let dirty_cells = dirty_cells_u64.min(u64::from(u32::MAX)) as u32;
    let patch_count = patches.len().min(u32::MAX as usize) as u32;
    let bytes_uploaded = dirty_cells_u64.saturating_mul(WEB_PATCH_CELL_BYTES);
    WebPatchStats {
        dirty_cells,
        patch_count,
        bytes_uploaded,
    }
}

#[must_use]
fn patch_batch_hash(patches: &[WebPatchRun]) -> String {
    let mut hash = FNV64_OFFSET_BASIS;
    let patch_count = u64::try_from(patches.len()).unwrap_or(u64::MAX);
    hash = fnv1a64_extend(hash, &patch_count.to_le_bytes());

    for patch in patches {
        let cell_count = u64::try_from(patch.cells.len()).unwrap_or(u64::MAX);
        hash = fnv1a64_extend(hash, &patch.offset.to_le_bytes());
        hash = fnv1a64_extend(hash, &cell_count.to_le_bytes());
        for cell in &patch.cells {
            hash = fnv1a64_extend(hash, &cell.bg.to_le_bytes());
            hash = fnv1a64_extend(hash, &cell.fg.to_le_bytes());
            hash = fnv1a64_extend(hash, &cell.glyph.to_le_bytes());
            hash = fnv1a64_extend(hash, &cell.attrs.to_le_bytes());
        }
    }

    format!("{PATCH_HASH_ALGO}:{hash:016x}")
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
    use ftui_render::cell::Cell;

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
        assert_eq!(outputs.last_patches.len(), 1);
        let stats = outputs.last_patch_stats.expect("stats should be present");
        assert_eq!(stats.patch_count, 1);
        assert_eq!(stats.dirty_cells, 4);
        assert_eq!(stats.bytes_uploaded, 64);
        let hash = outputs.last_patch_hash.expect("hash should be present");
        assert!(hash.starts_with("fnv1a64:"));
    }

    #[test]
    fn presenter_emits_incremental_patch_runs_from_diff() {
        let mut presenter = WebPresenter::new();
        let old = Buffer::new(6, 2);
        presenter.present_ui(&old, None, true).unwrap();

        let mut next = Buffer::new(6, 2);
        next.set_raw(2, 0, Cell::from_char('A'));
        next.set_raw(3, 0, Cell::from_char('B'));
        next.set_raw(0, 1, Cell::from_char('C'));
        let diff = BufferDiff::compute(&old, &next);
        presenter.present_ui(&next, Some(&diff), false).unwrap();

        let outputs = presenter.take_outputs();
        assert_eq!(outputs.last_full_repaint_hint, false);
        assert_eq!(outputs.last_patches.len(), 2);
        assert_eq!(outputs.last_patches[0].offset, 2);
        assert_eq!(outputs.last_patches[0].cells.len(), 2);
        assert_eq!(outputs.last_patches[1].offset, 6);
        assert_eq!(outputs.last_patches[1].cells.len(), 1);
        let stats = outputs.last_patch_stats.expect("stats should be present");
        assert_eq!(stats.patch_count, 2);
        assert_eq!(stats.dirty_cells, 3);
        assert_eq!(stats.bytes_uploaded, 48);
        let hash = outputs.last_patch_hash.expect("hash should be present");
        assert!(hash.starts_with("fnv1a64:"));
    }

    #[test]
    fn patch_batch_hash_is_deterministic() {
        let patches = vec![
            WebPatchRun {
                offset: 2,
                cells: vec![
                    WebPatchCell {
                        bg: 0x1122_3344,
                        fg: 0x5566_7788,
                        glyph: 'A' as u32,
                        attrs: 0x0000_0001,
                    },
                    WebPatchCell {
                        bg: 0x1122_3344,
                        fg: 0x5566_7788,
                        glyph: 'B' as u32,
                        attrs: 0x0000_0002,
                    },
                ],
            },
            WebPatchRun {
                offset: 10,
                cells: vec![WebPatchCell {
                    bg: 0xAABB_CCDD,
                    fg: 0xDDEE_FF00,
                    glyph: '中' as u32,
                    attrs: 0x0000_0010,
                }],
            },
        ];

        let hash_a = patch_batch_hash(&patches);
        let hash_b = patch_batch_hash(&patches);
        assert_eq!(hash_a, hash_b);
        assert!(hash_a.starts_with("fnv1a64:"));
    }

    #[test]
    fn patch_batch_hash_changes_with_patch_payload() {
        let baseline = vec![WebPatchRun {
            offset: 4,
            cells: vec![WebPatchCell {
                bg: 0x0000_00FF,
                fg: 0xFFFF_FFFF,
                glyph: 'x' as u32,
                attrs: 0x0000_0001,
            }],
        }];
        let mut changed = baseline.clone();
        changed[0].offset = 5;

        let base_hash = patch_batch_hash(&baseline);
        let changed_hash = patch_batch_hash(&changed);
        assert_ne!(base_hash, changed_hash);

        changed[0].offset = 4;
        changed[0].cells[0].glyph = 'y' as u32;
        let changed_glyph_hash = patch_batch_hash(&changed);
        assert_ne!(base_hash, changed_glyph_hash);
    }

    #[test]
    fn flatten_patches_u32_emits_row_major_cells_and_spans() {
        let outputs = WebOutputs {
            last_patches: vec![
                WebPatchRun {
                    offset: 2,
                    cells: vec![
                        WebPatchCell {
                            bg: 10,
                            fg: 11,
                            glyph: 12,
                            attrs: 13,
                        },
                        WebPatchCell {
                            bg: 20,
                            fg: 21,
                            glyph: 22,
                            attrs: 23,
                        },
                    ],
                },
                WebPatchRun {
                    offset: 9,
                    cells: vec![WebPatchCell {
                        bg: 30,
                        fg: 31,
                        glyph: 32,
                        attrs: 33,
                    }],
                },
            ],
            ..WebOutputs::default()
        };

        let flat = outputs.flatten_patches_u32();
        assert_eq!(flat.spans, vec![2, 2, 9, 1]);
        assert_eq!(
            flat.cells,
            vec![
                10, 11, 12, 13, //
                20, 21, 22, 23, //
                30, 31, 32, 33
            ]
        );
    }

    #[test]
    fn flatten_patches_u32_handles_empty_payload() {
        let outputs = WebOutputs::default();
        let flat = outputs.flatten_patches_u32();
        assert!(flat.cells.is_empty());
        assert!(flat.spans.is_empty());
    }
}
