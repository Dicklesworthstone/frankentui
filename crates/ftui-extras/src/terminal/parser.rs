//! ANSI escape sequence parser using the `vte` crate.
//!
//! This module provides a callback-based ANSI parser that dispatches parsed
//! sequences to an [`AnsiHandler`] implementation.
//!
//! # Invariants
//!
//! 1. **Complete UTF-8**: The parser correctly handles multi-byte UTF-8 sequences.
//! 2. **Sequence isolation**: Each escape sequence is fully parsed before dispatch.
//! 3. **State recovery**: Malformed sequences return to ground state gracefully.
//!
//! # Failure Modes
//!
//! | Failure | Cause | Behavior |
//! |---------|-------|----------|
//! | Invalid UTF-8 | Corrupted input | Replacement character dispatched |
//! | Unknown CSI | Unrecognized sequence | Silently ignored |
//! | Truncated sequence | Incomplete input | Buffered for next parse call |

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ftui_core::with_panic_cleanup_suppressed;
use vte::{Parser, Perform};

/// Opaque identifier returned when registering a parser hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HookId(u64);

impl HookId {
    /// Return the numeric id value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Hook classes supported by [`AnsiParser`] registration APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookClass {
    /// CSI (`ESC [`) hook pipeline.
    Csi,
    /// OSC (`ESC ]`) hook pipeline.
    Osc,
    /// ESC (single-byte final) hook pipeline.
    Esc,
    /// DCS (`ESC P`) hook pipeline.
    Dcs,
}

/// Hook dispatch result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDisposition {
    /// Continue dispatching remaining hooks, then fallback to the handler.
    Continue,
    /// Stop hook dispatch and suppress fallback handler dispatch.
    Consume,
    /// Reject the hook invocation and continue with fallback behavior.
    Reject,
}

/// Structured policy rejection codes emitted in hook traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookRejectReason {
    /// Hook class disabled by capability profile.
    CapabilityDisabled,
    /// Parse-time quota exceeded.
    QuotaExceeded,
    /// Hook callback ran longer than the configured max runtime.
    TimeoutExceeded,
    /// Hook callback panicked and was isolated.
    HookPanicked,
    /// Hook explicitly rejected processing.
    HookRejected,
}

/// Hook trace stage for deterministic postmortem replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTraceStage {
    /// A registered hook callback was invoked.
    HookInvoked,
    /// A hook consumed the event and stopped fallback dispatch.
    HookConsumed,
    /// No hook consumed the event, so fallback handler dispatch occurred.
    FallbackDispatched,
    /// Policy/capability/isolation rejected hook execution.
    PolicyRejected,
}

/// Replay-grade structured hook trace event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookTraceEvent {
    /// Unix timestamp in milliseconds.
    pub ts_ms: u64,
    /// Monotonic correlation id for deterministic event ordering.
    pub correlation_id: u64,
    /// Hook class for this record.
    pub class: HookClass,
    /// Dispatch stage.
    pub stage: HookTraceStage,
    /// Hook id, when the record corresponds to a specific callback.
    pub hook_id: Option<HookId>,
    /// Rejection reason, if the stage is `PolicyRejected`.
    pub reject_reason: Option<HookRejectReason>,
    /// Callback elapsed microseconds when available.
    pub elapsed_us: Option<u64>,
}

/// Parse-time capability profile for parser hook classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookCapabilities {
    /// Allow CSI hooks.
    pub csi: bool,
    /// Allow OSC hooks.
    pub osc: bool,
    /// Allow ESC hooks.
    pub esc: bool,
    /// Allow DCS hooks.
    pub dcs: bool,
}

impl Default for HookCapabilities {
    fn default() -> Self {
        Self {
            csi: true,
            osc: true,
            esc: true,
            dcs: true,
        }
    }
}

/// Parse-time parser hook policy (quotas + callback runtime bounds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookPolicy {
    /// Max CSI hook dispatches per `parse()` call.
    pub max_csi_invocations_per_parse: u32,
    /// Max OSC hook dispatches per `parse()` call.
    pub max_osc_invocations_per_parse: u32,
    /// Max ESC hook dispatches per `parse()` call.
    pub max_esc_invocations_per_parse: u32,
    /// Max DCS hook dispatches per `parse()` call.
    pub max_dcs_invocations_per_parse: u32,
    /// Max DCS `put` bytes delivered to hooks per `parse()` call.
    pub max_dcs_bytes_per_parse: usize,
    /// Max callback runtime before timeout rejection.
    pub max_hook_runtime: Duration,
}

impl Default for HookPolicy {
    fn default() -> Self {
        Self {
            max_csi_invocations_per_parse: 2_048,
            max_osc_invocations_per_parse: 1_024,
            max_esc_invocations_per_parse: 2_048,
            max_dcs_invocations_per_parse: 8_192,
            max_dcs_bytes_per_parse: 256 * 1024,
            max_hook_runtime: Duration::from_millis(5),
        }
    }
}

/// CSI hook event payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsiHookEvent {
    /// CSI numeric params.
    pub params: Vec<i64>,
    /// CSI intermediate bytes.
    pub intermediates: Vec<u8>,
    /// CSI final byte.
    pub final_byte: char,
}

/// OSC hook event payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscHookEvent {
    /// OSC semicolon-separated params.
    pub params: Vec<Vec<u8>>,
}

/// ESC hook event payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscHookEvent {
    /// ESC intermediates.
    pub intermediates: Vec<u8>,
    /// ESC final byte.
    pub final_byte: char,
}

/// DCS hook event payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DcsHookEvent {
    /// DCS sequence start (`hook`).
    Hook {
        /// DCS numeric params.
        params: Vec<i64>,
        /// DCS intermediate bytes.
        intermediates: Vec<u8>,
        /// DCS final byte.
        final_byte: char,
    },
    /// DCS payload byte (`put`).
    Put {
        /// DCS data byte.
        byte: u8,
    },
    /// DCS sequence end (`unhook`).
    Unhook,
}

type CsiHookCallback = Box<dyn FnMut(&CsiHookEvent) -> HookDisposition>;
type OscHookCallback = Box<dyn FnMut(&OscHookEvent) -> HookDisposition>;
type EscHookCallback = Box<dyn FnMut(&EscHookEvent) -> HookDisposition>;
type DcsHookCallback = Box<dyn FnMut(&DcsHookEvent) -> HookDisposition>;

struct HookEntry<T> {
    id: HookId,
    callback: T,
}

#[derive(Default)]
struct HookRegistry {
    next_id: u64,
    csi: Vec<HookEntry<CsiHookCallback>>,
    osc: Vec<HookEntry<OscHookCallback>>,
    esc: Vec<HookEntry<EscHookCallback>>,
    dcs: Vec<HookEntry<DcsHookCallback>>,
}

impl HookRegistry {
    fn register_csi<F>(&mut self, callback: F) -> HookId
    where
        F: FnMut(&CsiHookEvent) -> HookDisposition + 'static,
    {
        let id = self.next_hook_id();
        self.csi.push(HookEntry {
            id,
            callback: Box::new(callback),
        });
        id
    }

    fn register_osc<F>(&mut self, callback: F) -> HookId
    where
        F: FnMut(&OscHookEvent) -> HookDisposition + 'static,
    {
        let id = self.next_hook_id();
        self.osc.push(HookEntry {
            id,
            callback: Box::new(callback),
        });
        id
    }

    fn register_esc<F>(&mut self, callback: F) -> HookId
    where
        F: FnMut(&EscHookEvent) -> HookDisposition + 'static,
    {
        let id = self.next_hook_id();
        self.esc.push(HookEntry {
            id,
            callback: Box::new(callback),
        });
        id
    }

    fn register_dcs<F>(&mut self, callback: F) -> HookId
    where
        F: FnMut(&DcsHookEvent) -> HookDisposition + 'static,
    {
        let id = self.next_hook_id();
        self.dcs.push(HookEntry {
            id,
            callback: Box::new(callback),
        });
        id
    }

    fn deregister(&mut self, id: HookId) -> bool {
        fn remove_hook<T>(hooks: &mut Vec<HookEntry<T>>, id: HookId) -> bool {
            if let Some(idx) = hooks.iter().position(|entry| entry.id == id) {
                hooks.remove(idx);
                return true;
            }
            false
        }

        remove_hook(&mut self.csi, id)
            || remove_hook(&mut self.osc, id)
            || remove_hook(&mut self.esc, id)
            || remove_hook(&mut self.dcs, id)
    }

    fn clear(&mut self) {
        self.csi.clear();
        self.osc.clear();
        self.esc.clear();
        self.dcs.clear();
    }

    fn next_hook_id(&mut self) -> HookId {
        self.next_id = self.next_id.saturating_add(1);
        HookId(self.next_id)
    }
}

#[derive(Default)]
struct HookDispatchBudget {
    csi_invocations: u32,
    osc_invocations: u32,
    esc_invocations: u32,
    dcs_invocations: u32,
    dcs_bytes: usize,
}

fn system_time_ms() -> u64 {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(ts).unwrap_or(u64::MAX)
}

fn push_hook_trace(
    traces: &mut Vec<HookTraceEvent>,
    next_correlation_id: &mut u64,
    class: HookClass,
    stage: HookTraceStage,
    hook_id: Option<HookId>,
    reject_reason: Option<HookRejectReason>,
    elapsed: Option<Duration>,
) {
    let correlation_id = *next_correlation_id;
    *next_correlation_id = next_correlation_id.saturating_add(1);
    let elapsed_us = elapsed.map(|dur| u64::try_from(dur.as_micros()).unwrap_or(u64::MAX));
    traces.push(HookTraceEvent {
        ts_ms: system_time_ms(),
        correlation_id,
        class,
        stage,
        hook_id,
        reject_reason,
        elapsed_us,
    });
}

fn dispatch_registered_hooks<E, F>(
    hooks: &mut [HookEntry<F>],
    event: &E,
    class: HookClass,
    max_runtime: Duration,
    traces: &mut Vec<HookTraceEvent>,
    next_correlation_id: &mut u64,
) -> bool
where
    F: FnMut(&E) -> HookDisposition,
{
    for entry in hooks {
        let started = Instant::now();
        let outcome = with_panic_cleanup_suppressed(|| {
            catch_unwind(AssertUnwindSafe(|| (entry.callback)(event)))
        });
        let elapsed = started.elapsed();

        push_hook_trace(
            traces,
            next_correlation_id,
            class,
            HookTraceStage::HookInvoked,
            Some(entry.id),
            None,
            Some(elapsed),
        );

        if elapsed > max_runtime {
            push_hook_trace(
                traces,
                next_correlation_id,
                class,
                HookTraceStage::PolicyRejected,
                Some(entry.id),
                Some(HookRejectReason::TimeoutExceeded),
                Some(elapsed),
            );
            continue;
        }

        match outcome {
            Ok(HookDisposition::Continue) => {}
            Ok(HookDisposition::Consume) => {
                push_hook_trace(
                    traces,
                    next_correlation_id,
                    class,
                    HookTraceStage::HookConsumed,
                    Some(entry.id),
                    None,
                    Some(elapsed),
                );
                return true;
            }
            Ok(HookDisposition::Reject) => {
                push_hook_trace(
                    traces,
                    next_correlation_id,
                    class,
                    HookTraceStage::PolicyRejected,
                    Some(entry.id),
                    Some(HookRejectReason::HookRejected),
                    Some(elapsed),
                );
            }
            Err(_) => {
                push_hook_trace(
                    traces,
                    next_correlation_id,
                    class,
                    HookTraceStage::PolicyRejected,
                    Some(entry.id),
                    Some(HookRejectReason::HookPanicked),
                    Some(elapsed),
                );
            }
        }
    }
    false
}

/// Handler trait for ANSI escape sequence events.
///
/// Implement this trait to receive parsed ANSI events from [`AnsiParser`].
///
/// # Example
///
/// ```ignore
/// use ftui_extras::terminal::AnsiHandler;
///
/// struct MyTerminal {
///     cursor_x: u16,
///     cursor_y: u16,
/// }
///
/// impl AnsiHandler for MyTerminal {
///     fn print(&mut self, c: char) {
///         // Handle printable character
///     }
///
///     fn csi_dispatch(&mut self, params: &[i64], intermediates: &[u8], c: char) {
///         match c {
///             'A' => self.cursor_y = self.cursor_y.saturating_sub(params.first().copied().unwrap_or(1) as u16),
///             'B' => self.cursor_y += params.first().copied().unwrap_or(1) as u16,
///             _ => {}
///         }
///     }
///     // ... other methods
/// }
/// ```
pub trait AnsiHandler {
    /// Handle a printable character.
    ///
    /// Called for each printable Unicode character in the input stream.
    fn print(&mut self, c: char);

    /// Handle a C0/C1 control code.
    ///
    /// Common codes:
    /// - `0x07` (BEL): Bell
    /// - `0x08` (BS): Backspace
    /// - `0x09` (HT): Horizontal tab
    /// - `0x0A` (LF): Line feed
    /// - `0x0D` (CR): Carriage return
    fn execute(&mut self, byte: u8);

    /// Handle a CSI (Control Sequence Introducer) sequence.
    ///
    /// CSI sequences start with `ESC [` and are the primary mechanism for
    /// cursor movement, text styling, and screen manipulation.
    ///
    /// # Arguments
    ///
    /// * `params` - Numeric parameters (semicolon-separated in the sequence)
    /// * `intermediates` - Intermediate bytes (e.g., `?` for DEC private modes)
    /// * `c` - The final byte that identifies the command
    ///
    /// # Common Commands
    ///
    /// | Final | Meaning |
    /// |-------|---------|
    /// | `A` | Cursor up |
    /// | `B` | Cursor down |
    /// | `C` | Cursor forward |
    /// | `D` | Cursor back |
    /// | `H` | Cursor position |
    /// | `J` | Erase display |
    /// | `K` | Erase line |
    /// | `m` | SGR (Select Graphic Rendition) |
    /// | `h` | Set mode (with `?` for DEC private) |
    /// | `l` | Reset mode (with `?` for DEC private) |
    fn csi_dispatch(&mut self, params: &[i64], intermediates: &[u8], c: char);

    /// Handle an OSC (Operating System Command) sequence.
    ///
    /// OSC sequences start with `ESC ]` and are used for things like
    /// setting the window title or clipboard operations.
    ///
    /// # Arguments
    ///
    /// * `params` - The parsed OSC parameters (semicolon-separated strings)
    ///
    /// # Common OSC Commands
    ///
    /// | Code | Meaning |
    /// |------|---------|
    /// | 0 | Set icon name and window title |
    /// | 2 | Set window title |
    /// | 52 | Clipboard operations |
    fn osc_dispatch(&mut self, params: &[&[u8]]);

    /// Handle an ESC sequence (non-CSI, non-OSC).
    ///
    /// # Arguments
    ///
    /// * `intermediates` - Intermediate bytes between ESC and final byte
    /// * `c` - The final byte
    ///
    /// # Common Sequences
    ///
    /// | Sequence | Meaning |
    /// |----------|---------|
    /// | `ESC 7` | Save cursor (DECSC) |
    /// | `ESC 8` | Restore cursor (DECRC) |
    /// | `ESC D` | Index (move down, scroll if needed) |
    /// | `ESC M` | Reverse index (move up, scroll if needed) |
    /// | `ESC c` | Full reset (RIS) |
    fn esc_dispatch(&mut self, intermediates: &[u8], c: char);

    /// Handle a DCS (Device Control String) hook.
    ///
    /// Called when entering a DCS sequence. Override if you need to handle
    /// sixel graphics or other DCS data.
    #[allow(unused_variables)]
    fn hook(&mut self, params: &[i64], intermediates: &[u8], c: char) {
        // Default: ignore
    }

    /// Handle DCS data bytes.
    #[allow(unused_variables)]
    fn put(&mut self, byte: u8) {
        // Default: ignore
    }

    /// Handle DCS sequence end.
    fn unhook(&mut self) {
        // Default: ignore
    }
}

/// Adapter that bridges vte's `Perform` trait to our `AnsiHandler` trait.
struct VteAdapter<'a, H: AnsiHandler> {
    handler: &'a mut H,
    hooks: &'a mut HookRegistry,
    policy: HookPolicy,
    capabilities: HookCapabilities,
    traces: &'a mut Vec<HookTraceEvent>,
    next_correlation_id: &'a mut u64,
    budget: HookDispatchBudget,
}

impl<H: AnsiHandler> VteAdapter<'_, H> {
    fn record_policy_rejection(&mut self, class: HookClass, reason: HookRejectReason) {
        push_hook_trace(
            self.traces,
            self.next_correlation_id,
            class,
            HookTraceStage::PolicyRejected,
            None,
            Some(reason),
            None,
        );
    }

    fn record_fallback(&mut self, class: HookClass) {
        push_hook_trace(
            self.traces,
            self.next_correlation_id,
            class,
            HookTraceStage::FallbackDispatched,
            None,
            None,
            None,
        );
    }

    fn dispatch_csi_hooks(&mut self, event: &CsiHookEvent) -> bool {
        if self.hooks.csi.is_empty() {
            return false;
        }

        if !self.capabilities.csi {
            self.record_policy_rejection(HookClass::Csi, HookRejectReason::CapabilityDisabled);
            return false;
        }

        if self.budget.csi_invocations >= self.policy.max_csi_invocations_per_parse {
            self.record_policy_rejection(HookClass::Csi, HookRejectReason::QuotaExceeded);
            return false;
        }
        self.budget.csi_invocations = self.budget.csi_invocations.saturating_add(1);

        dispatch_registered_hooks(
            &mut self.hooks.csi,
            event,
            HookClass::Csi,
            self.policy.max_hook_runtime,
            self.traces,
            self.next_correlation_id,
        )
    }

    fn dispatch_osc_hooks(&mut self, event: &OscHookEvent) -> bool {
        if self.hooks.osc.is_empty() {
            return false;
        }

        if !self.capabilities.osc {
            self.record_policy_rejection(HookClass::Osc, HookRejectReason::CapabilityDisabled);
            return false;
        }

        if self.budget.osc_invocations >= self.policy.max_osc_invocations_per_parse {
            self.record_policy_rejection(HookClass::Osc, HookRejectReason::QuotaExceeded);
            return false;
        }
        self.budget.osc_invocations = self.budget.osc_invocations.saturating_add(1);

        dispatch_registered_hooks(
            &mut self.hooks.osc,
            event,
            HookClass::Osc,
            self.policy.max_hook_runtime,
            self.traces,
            self.next_correlation_id,
        )
    }

    fn dispatch_esc_hooks(&mut self, event: &EscHookEvent) -> bool {
        if self.hooks.esc.is_empty() {
            return false;
        }

        if !self.capabilities.esc {
            self.record_policy_rejection(HookClass::Esc, HookRejectReason::CapabilityDisabled);
            return false;
        }

        if self.budget.esc_invocations >= self.policy.max_esc_invocations_per_parse {
            self.record_policy_rejection(HookClass::Esc, HookRejectReason::QuotaExceeded);
            return false;
        }
        self.budget.esc_invocations = self.budget.esc_invocations.saturating_add(1);

        dispatch_registered_hooks(
            &mut self.hooks.esc,
            event,
            HookClass::Esc,
            self.policy.max_hook_runtime,
            self.traces,
            self.next_correlation_id,
        )
    }

    fn dispatch_dcs_hooks(&mut self, event: &DcsHookEvent) -> bool {
        if self.hooks.dcs.is_empty() {
            return false;
        }

        if !self.capabilities.dcs {
            self.record_policy_rejection(HookClass::Dcs, HookRejectReason::CapabilityDisabled);
            return false;
        }

        if self.budget.dcs_invocations >= self.policy.max_dcs_invocations_per_parse {
            self.record_policy_rejection(HookClass::Dcs, HookRejectReason::QuotaExceeded);
            return false;
        }

        if matches!(event, DcsHookEvent::Put { .. })
            && self.budget.dcs_bytes >= self.policy.max_dcs_bytes_per_parse
        {
            self.record_policy_rejection(HookClass::Dcs, HookRejectReason::QuotaExceeded);
            return false;
        }

        self.budget.dcs_invocations = self.budget.dcs_invocations.saturating_add(1);
        if matches!(event, DcsHookEvent::Put { .. }) {
            self.budget.dcs_bytes = self.budget.dcs_bytes.saturating_add(1);
        }

        dispatch_registered_hooks(
            &mut self.hooks.dcs,
            event,
            HookClass::Dcs,
            self.policy.max_hook_runtime,
            self.traces,
            self.next_correlation_id,
        )
    }
}

impl<H: AnsiHandler> Perform for VteAdapter<'_, H> {
    fn print(&mut self, c: char) {
        self.handler.print(c);
    }

    fn execute(&mut self, byte: u8) {
        self.handler.execute(byte);
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, c: char) {
        // Convert vte::Params to Vec<i64>
        let params: Vec<i64> = params
            .iter()
            .map(|subparams| {
                // Take the first value of each subparam group (handles colon-separated params)
                subparams.first().copied().map(i64::from).unwrap_or(0)
            })
            .collect();
        let intermediates = intermediates.to_vec();
        let event = CsiHookEvent {
            params: params.clone(),
            intermediates: intermediates.clone(),
            final_byte: c,
        };

        if self.dispatch_csi_hooks(&event) {
            return;
        }

        self.record_fallback(HookClass::Csi);
        self.handler.csi_dispatch(&params, &intermediates, c);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let params: Vec<Vec<u8>> = params.iter().map(|param| param.to_vec()).collect();
        let event = OscHookEvent {
            params: params.clone(),
        };
        if self.dispatch_osc_hooks(&event) {
            return;
        }

        self.record_fallback(HookClass::Osc);
        let refs: Vec<&[u8]> = params.iter().map(Vec::as_slice).collect();
        self.handler.osc_dispatch(&refs);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        let final_byte = char::from(byte);
        let intermediates = intermediates.to_vec();
        let event = EscHookEvent {
            intermediates: intermediates.clone(),
            final_byte,
        };
        if self.dispatch_esc_hooks(&event) {
            return;
        }

        self.record_fallback(HookClass::Esc);
        self.handler.esc_dispatch(&intermediates, final_byte);
    }

    fn hook(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, c: char) {
        let params: Vec<i64> = params
            .iter()
            .map(|subparams| subparams.first().copied().map(i64::from).unwrap_or(0))
            .collect();
        let intermediates = intermediates.to_vec();
        let event = DcsHookEvent::Hook {
            params: params.clone(),
            intermediates: intermediates.clone(),
            final_byte: c,
        };
        if self.dispatch_dcs_hooks(&event) {
            return;
        }

        self.record_fallback(HookClass::Dcs);
        self.handler.hook(&params, &intermediates, c);
    }

    fn put(&mut self, byte: u8) {
        let event = DcsHookEvent::Put { byte };
        if self.dispatch_dcs_hooks(&event) {
            return;
        }

        self.record_fallback(HookClass::Dcs);
        self.handler.put(byte);
    }

    fn unhook(&mut self) {
        let event = DcsHookEvent::Unhook;
        if self.dispatch_dcs_hooks(&event) {
            return;
        }

        self.record_fallback(HookClass::Dcs);
        self.handler.unhook();
    }
}

/// ANSI escape sequence parser.
///
/// Wraps the `vte` crate's parser and dispatches events to an [`AnsiHandler`].
///
/// # Example
///
/// ```ignore
/// use ftui_extras::terminal::{AnsiParser, AnsiHandler};
///
/// struct MyHandler;
/// impl AnsiHandler for MyHandler {
///     fn print(&mut self, c: char) { print!("{}", c); }
///     fn execute(&mut self, byte: u8) {}
///     fn csi_dispatch(&mut self, params: &[i64], intermediates: &[u8], c: char) {}
///     fn osc_dispatch(&mut self, params: &[&[u8]]) {}
///     fn esc_dispatch(&mut self, intermediates: &[u8], c: char) {}
/// }
///
/// let mut parser = AnsiParser::new();
/// let mut handler = MyHandler;
/// parser.parse(b"\x1b[31mHello\x1b[0m", &mut handler);
/// ```
pub struct AnsiParser {
    inner: Parser,
    hooks: HookRegistry,
    hook_policy: HookPolicy,
    hook_capabilities: HookCapabilities,
    hook_trace: Vec<HookTraceEvent>,
    next_correlation_id: u64,
}

impl Default for AnsiParser {
    fn default() -> Self {
        Self::new()
    }
}

impl AnsiParser {
    /// Create a new ANSI parser.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Parser::new(),
            hooks: HookRegistry::default(),
            hook_policy: HookPolicy::default(),
            hook_capabilities: HookCapabilities::default(),
            hook_trace: Vec::new(),
            next_correlation_id: 1,
        }
    }

    /// Parse bytes and dispatch events to the handler.
    ///
    /// This method can be called repeatedly with chunks of data. The parser
    /// maintains state between calls to handle sequences that span chunks.
    pub fn parse<H: AnsiHandler>(&mut self, data: &[u8], handler: &mut H) {
        let mut adapter = VteAdapter {
            handler,
            hooks: &mut self.hooks,
            policy: self.hook_policy,
            capabilities: self.hook_capabilities,
            traces: &mut self.hook_trace,
            next_correlation_id: &mut self.next_correlation_id,
            budget: HookDispatchBudget::default(),
        };
        self.inner.advance(&mut adapter, data);
    }

    /// Register a deterministic CSI hook callback.
    ///
    /// Hooks are invoked in registration order.
    pub fn register_csi_hook<F>(&mut self, callback: F) -> HookId
    where
        F: FnMut(&CsiHookEvent) -> HookDisposition + 'static,
    {
        self.hooks.register_csi(callback)
    }

    /// Register a deterministic OSC hook callback.
    ///
    /// Hooks are invoked in registration order.
    pub fn register_osc_hook<F>(&mut self, callback: F) -> HookId
    where
        F: FnMut(&OscHookEvent) -> HookDisposition + 'static,
    {
        self.hooks.register_osc(callback)
    }

    /// Register a deterministic ESC hook callback.
    ///
    /// Hooks are invoked in registration order.
    pub fn register_esc_hook<F>(&mut self, callback: F) -> HookId
    where
        F: FnMut(&EscHookEvent) -> HookDisposition + 'static,
    {
        self.hooks.register_esc(callback)
    }

    /// Register a deterministic DCS hook callback.
    ///
    /// Hooks are invoked in registration order.
    pub fn register_dcs_hook<F>(&mut self, callback: F) -> HookId
    where
        F: FnMut(&DcsHookEvent) -> HookDisposition + 'static,
    {
        self.hooks.register_dcs(callback)
    }

    /// Deregister a previously registered hook.
    ///
    /// Returns `true` when a hook was removed.
    pub fn deregister_hook(&mut self, id: HookId) -> bool {
        self.hooks.deregister(id)
    }

    /// Remove all registered parser hooks.
    pub fn clear_hooks(&mut self) {
        self.hooks.clear();
    }

    /// Set parse-time hook policy controls.
    pub fn set_hook_policy(&mut self, policy: HookPolicy) {
        self.hook_policy = policy;
    }

    /// Read the active parse-time hook policy.
    #[must_use]
    pub const fn hook_policy(&self) -> HookPolicy {
        self.hook_policy
    }

    /// Set hook capability gates.
    pub fn set_hook_capabilities(&mut self, capabilities: HookCapabilities) {
        self.hook_capabilities = capabilities;
    }

    /// Read active hook capability gates.
    #[must_use]
    pub const fn hook_capabilities(&self) -> HookCapabilities {
        self.hook_capabilities
    }

    /// Borrow the accumulated structured hook trace log.
    #[must_use]
    pub fn hook_trace(&self) -> &[HookTraceEvent] {
        &self.hook_trace
    }

    /// Drain and return structured hook trace records.
    pub fn drain_hook_trace(&mut self) -> Vec<HookTraceEvent> {
        std::mem::take(&mut self.hook_trace)
    }

    /// Reset the parser to initial state.
    ///
    /// Call this after a protocol error or when starting a new parsing session.
    pub fn reset(&mut self) {
        self.inner = Parser::new();
    }
}

impl std::fmt::Debug for AnsiParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnsiParser").finish_non_exhaustive()
    }
}

/// SGR (Select Graphic Rendition) parameter values.
///
/// These constants map to the numeric parameters used in `ESC [ ... m` sequences.
pub mod sgr {
    /// Reset all attributes
    pub const RESET: i64 = 0;
    /// Bold/bright
    pub const BOLD: i64 = 1;
    /// Dim/faint
    pub const DIM: i64 = 2;
    /// Italic
    pub const ITALIC: i64 = 3;
    /// Underline
    pub const UNDERLINE: i64 = 4;
    /// Slow blink
    pub const BLINK: i64 = 5;
    /// Reverse video
    pub const REVERSE: i64 = 7;
    /// Hidden/invisible
    pub const HIDDEN: i64 = 8;
    /// Strikethrough
    pub const STRIKETHROUGH: i64 = 9;

    /// Reset bold/dim
    pub const NORMAL_INTENSITY: i64 = 22;
    /// Reset italic
    pub const NO_ITALIC: i64 = 23;
    /// Reset underline
    pub const NO_UNDERLINE: i64 = 24;
    /// Reset blink
    pub const NO_BLINK: i64 = 25;
    /// Reset reverse
    pub const NO_REVERSE: i64 = 27;
    /// Reset hidden
    pub const NO_HIDDEN: i64 = 28;
    /// Reset strikethrough
    pub const NO_STRIKETHROUGH: i64 = 29;

    /// Black foreground
    pub const FG_BLACK: i64 = 30;
    /// Red foreground
    pub const FG_RED: i64 = 31;
    /// Green foreground
    pub const FG_GREEN: i64 = 32;
    /// Yellow foreground
    pub const FG_YELLOW: i64 = 33;
    /// Blue foreground
    pub const FG_BLUE: i64 = 34;
    /// Magenta foreground
    pub const FG_MAGENTA: i64 = 35;
    /// Cyan foreground
    pub const FG_CYAN: i64 = 36;
    /// White foreground
    pub const FG_WHITE: i64 = 37;
    /// Extended foreground color (256 or RGB)
    pub const FG_EXTENDED: i64 = 38;
    /// Default foreground
    pub const FG_DEFAULT: i64 = 39;

    /// Black background
    pub const BG_BLACK: i64 = 40;
    /// Red background
    pub const BG_RED: i64 = 41;
    /// Green background
    pub const BG_GREEN: i64 = 42;
    /// Yellow background
    pub const BG_YELLOW: i64 = 43;
    /// Blue background
    pub const BG_BLUE: i64 = 44;
    /// Magenta background
    pub const BG_MAGENTA: i64 = 45;
    /// Cyan background
    pub const BG_CYAN: i64 = 46;
    /// White background
    pub const BG_WHITE: i64 = 47;
    /// Extended background color (256 or RGB)
    pub const BG_EXTENDED: i64 = 48;
    /// Default background
    pub const BG_DEFAULT: i64 = 49;

    /// Bright black foreground
    pub const FG_BRIGHT_BLACK: i64 = 90;
    /// Bright red foreground
    pub const FG_BRIGHT_RED: i64 = 91;
    /// Bright green foreground
    pub const FG_BRIGHT_GREEN: i64 = 92;
    /// Bright yellow foreground
    pub const FG_BRIGHT_YELLOW: i64 = 93;
    /// Bright blue foreground
    pub const FG_BRIGHT_BLUE: i64 = 94;
    /// Bright magenta foreground
    pub const FG_BRIGHT_MAGENTA: i64 = 95;
    /// Bright cyan foreground
    pub const FG_BRIGHT_CYAN: i64 = 96;
    /// Bright white foreground
    pub const FG_BRIGHT_WHITE: i64 = 97;

    /// Bright black background
    pub const BG_BRIGHT_BLACK: i64 = 100;
    /// Bright red background
    pub const BG_BRIGHT_RED: i64 = 101;
    /// Bright green background
    pub const BG_BRIGHT_GREEN: i64 = 102;
    /// Bright yellow background
    pub const BG_BRIGHT_YELLOW: i64 = 103;
    /// Bright blue background
    pub const BG_BRIGHT_BLUE: i64 = 104;
    /// Bright magenta background
    pub const BG_BRIGHT_MAGENTA: i64 = 105;
    /// Bright cyan background
    pub const BG_BRIGHT_CYAN: i64 = 106;
    /// Bright white background
    pub const BG_BRIGHT_WHITE: i64 = 107;

    /// 256-color mode indicator (used after FG_EXTENDED or BG_EXTENDED)
    pub const COLOR_256: i64 = 5;
    /// RGB color mode indicator (used after FG_EXTENDED or BG_EXTENDED)
    pub const COLOR_RGB: i64 = 2;
}

/// DEC private mode numbers (used with CSI ? h/l sequences).
pub mod dec_mode {
    /// Cursor visible
    pub const CURSOR_VISIBLE: i64 = 25;
    /// Alternate screen buffer
    pub const ALT_SCREEN: i64 = 1049;
    /// Alternate screen (no save/restore)
    pub const ALT_SCREEN_NO_CLEAR: i64 = 1047;
    /// Save cursor before alt screen
    pub const SAVE_CURSOR: i64 = 1048;
    /// Mouse tracking: normal
    pub const MOUSE_TRACKING: i64 = 1000;
    /// Mouse tracking: button events
    pub const MOUSE_BUTTON: i64 = 1002;
    /// Mouse tracking: any event
    pub const MOUSE_ANY: i64 = 1003;
    /// Mouse tracking: SGR extended mode
    pub const MOUSE_SGR: i64 = 1006;
    /// Focus events
    pub const FOCUS: i64 = 1004;
    /// Bracketed paste mode
    pub const BRACKETED_PASTE: i64 = 2004;
}

/// Parse SGR parameters into attribute changes.
///
/// This is a helper for implementing `csi_dispatch` when `c == 'm'`.
///
/// # Returns
///
/// An iterator over `SgrChange` values describing the attribute changes.
pub fn parse_sgr(params: &[i64]) -> impl Iterator<Item = SgrChange> + '_ {
    SgrIterator::new(params)
}

/// A single SGR attribute change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SgrChange {
    /// Reset all attributes
    Reset,
    /// Set bold
    Bold(bool),
    /// Set dim
    Dim(bool),
    /// Set italic
    Italic(bool),
    /// Set underline
    Underline(bool),
    /// Set blink
    Blink(bool),
    /// Set reverse video
    Reverse(bool),
    /// Set hidden
    Hidden(bool),
    /// Set strikethrough
    Strikethrough(bool),
    /// Set foreground to ANSI color (0-7)
    FgAnsi(u8),
    /// Set foreground to bright ANSI color (0-7 maps to 8-15)
    FgBrightAnsi(u8),
    /// Set foreground to 256-color palette
    Fg256(u8),
    /// Set foreground to RGB
    FgRgb(u8, u8, u8),
    /// Reset foreground to default
    FgDefault,
    /// Set background to ANSI color (0-7)
    BgAnsi(u8),
    /// Set background to bright ANSI color (0-7 maps to 8-15)
    BgBrightAnsi(u8),
    /// Set background to 256-color palette
    Bg256(u8),
    /// Set background to RGB
    BgRgb(u8, u8, u8),
    /// Reset background to default
    BgDefault,
}

/// Iterator over SGR changes.
struct SgrIterator<'a> {
    params: &'a [i64],
    index: usize,
}

impl<'a> SgrIterator<'a> {
    fn new(params: &'a [i64]) -> Self {
        Self { params, index: 0 }
    }

    fn next_param(&mut self) -> Option<i64> {
        if self.index < self.params.len() {
            let val = self.params[self.index];
            self.index += 1;
            Some(val)
        } else {
            None
        }
    }

    fn parse_extended_color(&mut self) -> Option<SgrChange> {
        let mode = self.next_param()?;
        match mode {
            5 => {
                // 256-color mode: 38;5;N or 48;5;N
                let color = self.next_param()?;
                Some(SgrChange::Fg256(color as u8))
            }
            2 => {
                // RGB mode: 38;2;R;G;B or 48;2;R;G;B
                let r = self.next_param()?;
                let g = self.next_param()?;
                let b = self.next_param()?;
                Some(SgrChange::FgRgb(r as u8, g as u8, b as u8))
            }
            _ => None,
        }
    }

    fn parse_extended_bg_color(&mut self) -> Option<SgrChange> {
        let mode = self.next_param()?;
        match mode {
            5 => {
                let color = self.next_param()?;
                Some(SgrChange::Bg256(color as u8))
            }
            2 => {
                let r = self.next_param()?;
                let g = self.next_param()?;
                let b = self.next_param()?;
                Some(SgrChange::BgRgb(r as u8, g as u8, b as u8))
            }
            _ => None,
        }
    }
}

impl Iterator for SgrIterator<'_> {
    type Item = SgrChange;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let param = self.next_param()?;

            let change = match param {
                0 => SgrChange::Reset,
                1 => SgrChange::Bold(true),
                2 => SgrChange::Dim(true),
                3 => SgrChange::Italic(true),
                4 => SgrChange::Underline(true),
                5 => SgrChange::Blink(true),
                7 => SgrChange::Reverse(true),
                8 => SgrChange::Hidden(true),
                9 => SgrChange::Strikethrough(true),
                22 => {
                    // Reset both bold and dim
                    return Some(SgrChange::Bold(false));
                }
                23 => SgrChange::Italic(false),
                24 => SgrChange::Underline(false),
                25 => SgrChange::Blink(false),
                27 => SgrChange::Reverse(false),
                28 => SgrChange::Hidden(false),
                29 => SgrChange::Strikethrough(false),
                30..=37 => SgrChange::FgAnsi((param - 30) as u8),
                38 => {
                    if let Some(change) = self.parse_extended_color() {
                        return Some(change);
                    }
                    continue;
                }
                39 => SgrChange::FgDefault,
                40..=47 => SgrChange::BgAnsi((param - 40) as u8),
                48 => {
                    if let Some(change) = self.parse_extended_bg_color() {
                        return Some(change);
                    }
                    continue;
                }
                49 => SgrChange::BgDefault,
                90..=97 => SgrChange::FgBrightAnsi((param - 90) as u8),
                100..=107 => SgrChange::BgBrightAnsi((param - 100) as u8),
                _ => continue, // Unknown SGR parameter
            };

            return Some(change);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::thread;

    /// Test handler that records all events.
    #[derive(Default)]
    #[allow(clippy::type_complexity)]
    struct TestHandler {
        printed: RefCell<Vec<char>>,
        executed: RefCell<Vec<u8>>,
        csi_calls: RefCell<Vec<(Vec<i64>, Vec<u8>, char)>>,
        osc_calls: RefCell<Vec<Vec<Vec<u8>>>>,
        esc_calls: RefCell<Vec<(Vec<u8>, char)>>,
    }

    impl AnsiHandler for TestHandler {
        fn print(&mut self, c: char) {
            self.printed.borrow_mut().push(c);
        }

        fn execute(&mut self, byte: u8) {
            self.executed.borrow_mut().push(byte);
        }

        fn csi_dispatch(&mut self, params: &[i64], intermediates: &[u8], c: char) {
            self.csi_calls
                .borrow_mut()
                .push((params.to_vec(), intermediates.to_vec(), c));
        }

        fn osc_dispatch(&mut self, params: &[&[u8]]) {
            self.osc_calls
                .borrow_mut()
                .push(params.iter().map(|p| p.to_vec()).collect());
        }

        fn esc_dispatch(&mut self, intermediates: &[u8], c: char) {
            self.esc_calls
                .borrow_mut()
                .push((intermediates.to_vec(), c));
        }
    }

    #[test]
    fn parse_plain_text() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"Hello", &mut handler);

        let printed: Vec<char> = handler.printed.borrow().clone();
        assert_eq!(printed, vec!['H', 'e', 'l', 'l', 'o']);
    }

    #[test]
    fn parse_control_codes() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"A\nB\rC\tD", &mut handler);

        let printed: Vec<char> = handler.printed.borrow().clone();
        assert_eq!(printed, vec!['A', 'B', 'C', 'D']);

        let executed: Vec<u8> = handler.executed.borrow().clone();
        assert_eq!(executed, vec![b'\n', b'\r', b'\t']);
    }

    #[test]
    fn parse_csi_cursor_up() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ 5 A - cursor up 5
        parser.parse(b"\x1b[5A", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![5]);
        assert_eq!(csi_calls[0].2, 'A');
    }

    #[test]
    fn parse_csi_cursor_position() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ 10 ; 20 H - cursor to row 10, col 20
        parser.parse(b"\x1b[10;20H", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![10, 20]);
        assert_eq!(csi_calls[0].2, 'H');
    }

    #[test]
    fn parse_csi_sgr_colors() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ 1 ; 31 m - bold red
        parser.parse(b"\x1b[1;31m", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![1, 31]);
        assert_eq!(csi_calls[0].2, 'm');
    }

    #[test]
    fn parse_csi_dec_private_mode() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ ? 25 h - show cursor
        parser.parse(b"\x1b[?25h", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].1, vec![b'?']);
        assert_eq!(csi_calls[0].2, 'h');
    }

    #[test]
    fn parse_osc_title() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // OSC 0 ; title BEL
        parser.parse(b"\x1b]0;My Title\x07", &mut handler);

        let osc_calls = handler.osc_calls.borrow();
        assert_eq!(osc_calls.len(), 1);
        assert_eq!(osc_calls[0].len(), 2);
        assert_eq!(osc_calls[0][0], b"0");
        assert_eq!(osc_calls[0][1], b"My Title");
    }

    #[test]
    fn parse_esc_save_cursor() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC 7 - save cursor
        parser.parse(b"\x1b7", &mut handler);

        let esc_calls = handler.esc_calls.borrow();
        assert_eq!(esc_calls.len(), 1);
        assert_eq!(esc_calls[0].1, '7');
    }

    #[test]
    fn parse_mixed_content() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"\x1b[31mHello\x1b[0m World", &mut handler);

        let printed: Vec<char> = handler.printed.borrow().clone();
        assert_eq!(
            printed,
            vec!['H', 'e', 'l', 'l', 'o', ' ', 'W', 'o', 'r', 'l', 'd']
        );

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 2); // [31m and [0m
    }

    #[test]
    fn parse_utf8() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse("Hello, 世界! 🎉".as_bytes(), &mut handler);

        let printed: Vec<char> = handler.printed.borrow().clone();
        assert!(printed.contains(&'世'));
        assert!(printed.contains(&'界'));
        assert!(printed.contains(&'🎉'));
    }

    #[test]
    fn parse_incomplete_sequence_buffered() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // Send partial sequence
        parser.parse(b"\x1b[1", &mut handler);
        assert!(handler.csi_calls.borrow().is_empty());

        // Complete the sequence
        parser.parse(b";31m", &mut handler);
        assert_eq!(handler.csi_calls.borrow().len(), 1);
    }

    #[test]
    fn reset_clears_state() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // Start a sequence
        parser.parse(b"\x1b[1", &mut handler);

        // Reset
        parser.reset();

        // New sequence should work
        parser.parse(b"\x1b[5A", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![5]);
    }

    #[test]
    fn csi_hook_registration_is_ordered_and_deregisterable() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();
        let order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

        let order_first = Rc::clone(&order);
        parser.register_csi_hook(move |_event| {
            order_first.borrow_mut().push("first");
            HookDisposition::Continue
        });

        let order_second = Rc::clone(&order);
        let consumer_id = parser.register_csi_hook(move |_event| {
            order_second.borrow_mut().push("second");
            HookDisposition::Consume
        });

        parser.parse(b"\x1b[5A", &mut handler);

        assert_eq!(*order.borrow(), vec!["first", "second"]);
        assert!(handler.csi_calls.borrow().is_empty());
        assert!(
            parser
                .hook_trace()
                .iter()
                .any(|event| event.class == HookClass::Csi
                    && event.stage == HookTraceStage::HookConsumed)
        );

        assert!(parser.deregister_hook(consumer_id));
        parser.drain_hook_trace();
        parser.parse(b"\x1b[6A", &mut handler);
        assert_eq!(handler.csi_calls.borrow().len(), 1);
    }

    #[test]
    fn capability_gating_skips_hooks_and_records_rejection() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();
        let calls = Rc::new(RefCell::new(0_u32));
        let calls_for_hook = Rc::clone(&calls);

        parser.register_csi_hook(move |_event| {
            *calls_for_hook.borrow_mut() += 1;
            HookDisposition::Continue
        });

        parser.set_hook_capabilities(HookCapabilities {
            csi: false,
            ..HookCapabilities::default()
        });

        parser.parse(b"\x1b[1A", &mut handler);

        assert_eq!(*calls.borrow(), 0);
        assert_eq!(handler.csi_calls.borrow().len(), 1);

        let trace = parser.drain_hook_trace();
        assert!(trace.iter().any(|event| {
            event.class == HookClass::Csi
                && event.stage == HookTraceStage::PolicyRejected
                && event.reject_reason == Some(HookRejectReason::CapabilityDisabled)
        }));
        assert!(trace.iter().any(|event| {
            event.class == HookClass::Csi && event.stage == HookTraceStage::FallbackDispatched
        }));
    }

    #[test]
    fn csi_hook_quota_is_enforced_per_parse_call() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();
        let calls = Rc::new(RefCell::new(0_u32));
        let calls_for_hook = Rc::clone(&calls);

        parser.set_hook_policy(HookPolicy {
            max_csi_invocations_per_parse: 1,
            ..HookPolicy::default()
        });

        parser.register_csi_hook(move |_event| {
            *calls_for_hook.borrow_mut() += 1;
            HookDisposition::Continue
        });

        parser.parse(b"\x1b[1A\x1b[2B", &mut handler);

        assert_eq!(*calls.borrow(), 1);
        assert_eq!(handler.csi_calls.borrow().len(), 2);

        let trace = parser.drain_hook_trace();
        assert!(trace.iter().any(|event| {
            event.class == HookClass::Csi
                && event.stage == HookTraceStage::PolicyRejected
                && event.reject_reason == Some(HookRejectReason::QuotaExceeded)
        }));
    }

    #[test]
    fn hook_panic_is_isolated_and_falls_back_to_handler() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.register_csi_hook(|_event| -> HookDisposition {
            panic!("panic from hook should be isolated");
        });

        parser.parse(b"\x1b[3A", &mut handler);

        assert_eq!(handler.csi_calls.borrow().len(), 1);
        let trace = parser.drain_hook_trace();
        assert!(trace.iter().any(|event| {
            event.class == HookClass::Csi
                && event.stage == HookTraceStage::PolicyRejected
                && event.reject_reason == Some(HookRejectReason::HookPanicked)
        }));
    }

    #[test]
    fn hook_timeout_isolated_and_falls_back_to_handler() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.set_hook_policy(HookPolicy {
            max_hook_runtime: Duration::from_millis(1),
            ..HookPolicy::default()
        });
        parser.register_csi_hook(|_event| {
            thread::sleep(Duration::from_millis(3));
            HookDisposition::Consume
        });

        parser.parse(b"\x1b[4A", &mut handler);

        assert_eq!(handler.csi_calls.borrow().len(), 1);
        let trace = parser.drain_hook_trace();
        assert!(trace.iter().any(|event| {
            event.class == HookClass::Csi
                && event.stage == HookTraceStage::PolicyRejected
                && event.reject_reason == Some(HookRejectReason::TimeoutExceeded)
        }));
    }

    // ── SGR parsing tests ─────────────────────────────────────────────

    #[test]
    fn sgr_reset() {
        let changes: Vec<_> = parse_sgr(&[0]).collect();
        assert_eq!(changes, vec![SgrChange::Reset]);
    }

    #[test]
    fn sgr_bold_italic() {
        let changes: Vec<_> = parse_sgr(&[1, 3]).collect();
        assert_eq!(
            changes,
            vec![SgrChange::Bold(true), SgrChange::Italic(true)]
        );
    }

    #[test]
    fn sgr_fg_color() {
        let changes: Vec<_> = parse_sgr(&[31]).collect();
        assert_eq!(changes, vec![SgrChange::FgAnsi(1)]);
    }

    #[test]
    fn sgr_256_color() {
        let changes: Vec<_> = parse_sgr(&[38, 5, 196]).collect();
        assert_eq!(changes, vec![SgrChange::Fg256(196)]);
    }

    #[test]
    fn sgr_rgb_color() {
        let changes: Vec<_> = parse_sgr(&[38, 2, 100, 150, 200]).collect();
        assert_eq!(changes, vec![SgrChange::FgRgb(100, 150, 200)]);
    }

    #[test]
    fn sgr_bg_256_color() {
        let changes: Vec<_> = parse_sgr(&[48, 5, 21]).collect();
        assert_eq!(changes, vec![SgrChange::Bg256(21)]);
    }

    #[test]
    fn sgr_bg_rgb_color() {
        let changes: Vec<_> = parse_sgr(&[48, 2, 50, 100, 150]).collect();
        assert_eq!(changes, vec![SgrChange::BgRgb(50, 100, 150)]);
    }

    #[test]
    fn sgr_bright_colors() {
        let changes: Vec<_> = parse_sgr(&[91, 101]).collect();
        assert_eq!(
            changes,
            vec![SgrChange::FgBrightAnsi(1), SgrChange::BgBrightAnsi(1)]
        );
    }

    #[test]
    fn sgr_complex_sequence() {
        // Bold, red fg, blue bg, reset
        let changes: Vec<_> = parse_sgr(&[1, 31, 44, 0]).collect();
        assert_eq!(
            changes,
            vec![
                SgrChange::Bold(true),
                SgrChange::FgAnsi(1),
                SgrChange::BgAnsi(4),
                SgrChange::Reset,
            ]
        );
    }

    #[test]
    fn sgr_empty_treated_as_reset() {
        // Empty params should produce no changes (vte handles this at a higher level)
        let changes: Vec<_> = parse_sgr(&[]).collect();
        assert!(changes.is_empty());
    }

    #[test]
    fn sgr_defaults() {
        let changes: Vec<_> = parse_sgr(&[39, 49]).collect();
        assert_eq!(changes, vec![SgrChange::FgDefault, SgrChange::BgDefault]);
    }

    // ── DCS (Device Control String) tests ───────────────────────────────

    /// Extended handler that also records DCS events.
    #[derive(Default)]
    #[allow(clippy::type_complexity)]
    struct DcsTestHandler {
        printed: Vec<char>,
        hook_calls: Vec<(Vec<i64>, Vec<u8>, char)>,
        put_bytes: Vec<u8>,
        unhook_count: usize,
    }

    impl AnsiHandler for DcsTestHandler {
        fn print(&mut self, c: char) {
            self.printed.push(c);
        }
        fn execute(&mut self, _byte: u8) {}
        fn csi_dispatch(&mut self, _params: &[i64], _intermediates: &[u8], _c: char) {}
        fn osc_dispatch(&mut self, _params: &[&[u8]]) {}
        fn esc_dispatch(&mut self, _intermediates: &[u8], _c: char) {}
        fn hook(&mut self, params: &[i64], intermediates: &[u8], c: char) {
            self.hook_calls
                .push((params.to_vec(), intermediates.to_vec(), c));
        }
        fn put(&mut self, byte: u8) {
            self.put_bytes.push(byte);
        }
        fn unhook(&mut self) {
            self.unhook_count += 1;
        }
    }

    #[test]
    fn dcs_hook_put_unhook_roundtrip() {
        let mut parser = AnsiParser::new();
        let mut handler = DcsTestHandler::default();

        // DCS q (sixel): ESC P q <data> ESC backslash
        parser.parse(b"\x1bPq", &mut handler);
        assert_eq!(handler.hook_calls.len(), 1);
        assert_eq!(handler.hook_calls[0].2, 'q');

        // Send data bytes
        parser.parse(b"#0;2;0;0;0", &mut handler);
        assert!(!handler.put_bytes.is_empty());

        // End DCS with ST (ESC \)
        parser.parse(b"\x1b\\", &mut handler);
        assert_eq!(handler.unhook_count, 1);
    }

    #[test]
    fn dcs_with_params() {
        let mut parser = AnsiParser::new();
        let mut handler = DcsTestHandler::default();

        // DCS with params: ESC P 1;2 q
        parser.parse(b"\x1bP1;2q", &mut handler);
        assert_eq!(handler.hook_calls.len(), 1);
        assert_eq!(handler.hook_calls[0].0, vec![1, 2]);
        assert_eq!(handler.hook_calls[0].2, 'q');
    }

    #[test]
    fn dcs_hook_byte_quota_enforced_with_replay_trace() {
        let mut parser = AnsiParser::new();
        let mut handler = DcsTestHandler::default();
        let dcs_put_hook_calls = Rc::new(RefCell::new(0_usize));
        let dcs_put_hook_calls_for_hook = Rc::clone(&dcs_put_hook_calls);

        parser.set_hook_policy(HookPolicy {
            max_dcs_bytes_per_parse: 2,
            ..HookPolicy::default()
        });
        parser.register_dcs_hook(move |event| {
            if matches!(event, DcsHookEvent::Put { .. }) {
                *dcs_put_hook_calls_for_hook.borrow_mut() += 1;
            }
            HookDisposition::Continue
        });

        parser.parse(b"\x1bPqABCDE\x1b\\", &mut handler);

        assert_eq!(*dcs_put_hook_calls.borrow(), 2);
        assert_eq!(handler.put_bytes, b"ABCDE".to_vec());

        let trace = parser.drain_hook_trace();
        assert!(trace.iter().any(|event| {
            event.class == HookClass::Dcs
                && event.stage == HookTraceStage::PolicyRejected
                && event.reject_reason == Some(HookRejectReason::QuotaExceeded)
        }));
    }

    // ── ESC sequence variant tests ──────────────────────────────────────

    #[test]
    fn esc_restore_cursor() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC 8 - restore cursor
        parser.parse(b"\x1b8", &mut handler);

        let esc_calls = handler.esc_calls.borrow();
        assert_eq!(esc_calls.len(), 1);
        assert_eq!(esc_calls[0].1, '8');
    }

    #[test]
    fn esc_index_down() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC D - index (move down, scroll if needed)
        parser.parse(b"\x1bD", &mut handler);

        let esc_calls = handler.esc_calls.borrow();
        assert_eq!(esc_calls.len(), 1);
        assert_eq!(esc_calls[0].1, 'D');
        assert!(esc_calls[0].0.is_empty());
    }

    #[test]
    fn esc_reverse_index() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC M - reverse index (move up, scroll if needed)
        parser.parse(b"\x1bM", &mut handler);

        let esc_calls = handler.esc_calls.borrow();
        assert_eq!(esc_calls.len(), 1);
        assert_eq!(esc_calls[0].1, 'M');
    }

    #[test]
    fn esc_full_reset() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC c - full reset (RIS)
        parser.parse(b"\x1bc", &mut handler);

        let esc_calls = handler.esc_calls.borrow();
        assert_eq!(esc_calls.len(), 1);
        assert_eq!(esc_calls[0].1, 'c');
    }

    #[test]
    fn multiple_esc_sequences() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // Save cursor, then restore cursor
        parser.parse(b"\x1b7\x1b8", &mut handler);

        let esc_calls = handler.esc_calls.borrow();
        assert_eq!(esc_calls.len(), 2);
        assert_eq!(esc_calls[0].1, '7');
        assert_eq!(esc_calls[1].1, '8');
    }

    // ── CSI sequence variant tests ──────────────────────────────────────

    #[test]
    fn csi_cursor_down() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"\x1b[3B", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![3]);
        assert_eq!(csi_calls[0].2, 'B');
    }

    #[test]
    fn csi_cursor_forward() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"\x1b[7C", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![7]);
        assert_eq!(csi_calls[0].2, 'C');
    }

    #[test]
    fn csi_cursor_back() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"\x1b[2D", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![2]);
        assert_eq!(csi_calls[0].2, 'D');
    }

    #[test]
    fn csi_erase_display() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ 2 J - erase entire display
        parser.parse(b"\x1b[2J", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![2]);
        assert_eq!(csi_calls[0].2, 'J');
    }

    #[test]
    fn csi_erase_line() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ K - erase from cursor to end of line (default 0)
        parser.parse(b"\x1b[K", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].2, 'K');
    }

    #[test]
    fn csi_no_params_defaults() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ H - cursor home (no params = 1;1)
        parser.parse(b"\x1b[H", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].2, 'H');
        // vte delivers an empty or default param set
    }

    #[test]
    fn csi_alt_screen_enable() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ ? 1049 h - enable alt screen
        parser.parse(b"\x1b[?1049h", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![1049]);
        assert_eq!(csi_calls[0].1, vec![b'?']);
        assert_eq!(csi_calls[0].2, 'h');
    }

    #[test]
    fn csi_alt_screen_disable() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ ? 1049 l - disable alt screen
        parser.parse(b"\x1b[?1049l", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![1049]);
        assert_eq!(csi_calls[0].1, vec![b'?']);
        assert_eq!(csi_calls[0].2, 'l');
    }

    #[test]
    fn csi_bracketed_paste_mode() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ ? 2004 h - enable bracketed paste
        parser.parse(b"\x1b[?2004h", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);
        assert_eq!(csi_calls[0].0, vec![2004]);
        assert_eq!(csi_calls[0].1, vec![b'?']);
        assert_eq!(csi_calls[0].2, 'h');
    }

    #[test]
    fn multiple_csi_in_one_parse() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // Cursor up, then cursor down, then erase line
        parser.parse(b"\x1b[5A\x1b[3B\x1b[K", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 3);
        assert_eq!(csi_calls[0].2, 'A');
        assert_eq!(csi_calls[1].2, 'B');
        assert_eq!(csi_calls[2].2, 'K');
    }

    // ── SGR individual attribute tests ──────────────────────────────────

    #[test]
    fn sgr_dim() {
        let changes: Vec<_> = parse_sgr(&[2]).collect();
        assert_eq!(changes, vec![SgrChange::Dim(true)]);
    }

    #[test]
    fn sgr_underline() {
        let changes: Vec<_> = parse_sgr(&[4]).collect();
        assert_eq!(changes, vec![SgrChange::Underline(true)]);
    }

    #[test]
    fn sgr_blink() {
        let changes: Vec<_> = parse_sgr(&[5]).collect();
        assert_eq!(changes, vec![SgrChange::Blink(true)]);
    }

    #[test]
    fn sgr_reverse() {
        let changes: Vec<_> = parse_sgr(&[7]).collect();
        assert_eq!(changes, vec![SgrChange::Reverse(true)]);
    }

    #[test]
    fn sgr_hidden() {
        let changes: Vec<_> = parse_sgr(&[8]).collect();
        assert_eq!(changes, vec![SgrChange::Hidden(true)]);
    }

    #[test]
    fn sgr_strikethrough() {
        let changes: Vec<_> = parse_sgr(&[9]).collect();
        assert_eq!(changes, vec![SgrChange::Strikethrough(true)]);
    }

    #[test]
    fn sgr_normal_intensity_resets_bold() {
        // SGR 22 resets both bold and dim, but implementation returns Bold(false)
        let changes: Vec<_> = parse_sgr(&[22]).collect();
        assert_eq!(changes, vec![SgrChange::Bold(false)]);
    }

    #[test]
    fn sgr_no_italic() {
        let changes: Vec<_> = parse_sgr(&[23]).collect();
        assert_eq!(changes, vec![SgrChange::Italic(false)]);
    }

    #[test]
    fn sgr_no_underline() {
        let changes: Vec<_> = parse_sgr(&[24]).collect();
        assert_eq!(changes, vec![SgrChange::Underline(false)]);
    }

    #[test]
    fn sgr_no_blink() {
        let changes: Vec<_> = parse_sgr(&[25]).collect();
        assert_eq!(changes, vec![SgrChange::Blink(false)]);
    }

    #[test]
    fn sgr_no_reverse() {
        let changes: Vec<_> = parse_sgr(&[27]).collect();
        assert_eq!(changes, vec![SgrChange::Reverse(false)]);
    }

    #[test]
    fn sgr_no_hidden() {
        let changes: Vec<_> = parse_sgr(&[28]).collect();
        assert_eq!(changes, vec![SgrChange::Hidden(false)]);
    }

    #[test]
    fn sgr_no_strikethrough() {
        let changes: Vec<_> = parse_sgr(&[29]).collect();
        assert_eq!(changes, vec![SgrChange::Strikethrough(false)]);
    }

    // ── SGR all standard colors ─────────────────────────────────────────

    #[test]
    fn sgr_all_standard_fg_colors() {
        for code in 30..=37 {
            let changes: Vec<_> = parse_sgr(&[code]).collect();
            assert_eq!(
                changes,
                vec![SgrChange::FgAnsi((code - 30) as u8)],
                "failed for SGR {code}"
            );
        }
    }

    #[test]
    fn sgr_all_standard_bg_colors() {
        for code in 40..=47 {
            let changes: Vec<_> = parse_sgr(&[code]).collect();
            assert_eq!(
                changes,
                vec![SgrChange::BgAnsi((code - 40) as u8)],
                "failed for SGR {code}"
            );
        }
    }

    #[test]
    fn sgr_all_bright_fg_colors() {
        for code in 90..=97 {
            let changes: Vec<_> = parse_sgr(&[code]).collect();
            assert_eq!(
                changes,
                vec![SgrChange::FgBrightAnsi((code - 90) as u8)],
                "failed for SGR {code}"
            );
        }
    }

    #[test]
    fn sgr_all_bright_bg_colors() {
        for code in 100..=107 {
            let changes: Vec<_> = parse_sgr(&[code]).collect();
            assert_eq!(
                changes,
                vec![SgrChange::BgBrightAnsi((code - 100) as u8)],
                "failed for SGR {code}"
            );
        }
    }

    // ── SGR extended color edge cases ───────────────────────────────────

    #[test]
    fn sgr_extended_fg_unknown_mode_skipped() {
        // 38;9 is not a valid color mode (only 2 and 5 are)
        let changes: Vec<_> = parse_sgr(&[38, 9]).collect();
        assert!(
            changes.is_empty(),
            "unknown extended mode should be skipped"
        );
    }

    #[test]
    fn sgr_extended_bg_unknown_mode_skipped() {
        // 48;9 is not a valid bg color mode
        let changes: Vec<_> = parse_sgr(&[48, 9]).collect();
        assert!(
            changes.is_empty(),
            "unknown extended bg mode should be skipped"
        );
    }

    #[test]
    fn sgr_extended_fg_truncated_256() {
        // 38;5 without the color index - next_param returns None
        let changes: Vec<_> = parse_sgr(&[38, 5]).collect();
        assert!(
            changes.is_empty(),
            "truncated 256-color should yield nothing"
        );
    }

    #[test]
    fn sgr_extended_fg_truncated_rgb_partial() {
        // 38;2;100;150 missing the B component
        let changes: Vec<_> = parse_sgr(&[38, 2, 100, 150]).collect();
        assert!(changes.is_empty(), "truncated RGB should yield nothing");
    }

    #[test]
    fn sgr_extended_fg_truncated_mode_only() {
        // 38 with no mode byte at all
        let changes: Vec<_> = parse_sgr(&[38]).collect();
        assert!(changes.is_empty(), "38 alone should yield nothing");
    }

    #[test]
    fn sgr_extended_bg_truncated_256() {
        // 48;5 without color index
        let changes: Vec<_> = parse_sgr(&[48, 5]).collect();
        assert!(
            changes.is_empty(),
            "truncated bg 256-color should yield nothing"
        );
    }

    #[test]
    fn sgr_extended_bg_truncated_rgb_partial() {
        // 48;2;50;100 missing B component
        let changes: Vec<_> = parse_sgr(&[48, 2, 50, 100]).collect();
        assert!(changes.is_empty(), "truncated bg RGB should yield nothing");
    }

    #[test]
    fn sgr_extended_bg_truncated_mode_only() {
        // 48 with no mode byte
        let changes: Vec<_> = parse_sgr(&[48]).collect();
        assert!(changes.is_empty(), "48 alone should yield nothing");
    }

    #[test]
    fn sgr_unknown_params_skipped() {
        // 6, 10, 50, 99 are not recognized SGR codes
        let changes: Vec<_> = parse_sgr(&[6, 10, 50, 99]).collect();
        assert!(changes.is_empty(), "unknown SGR params should be skipped");
    }

    #[test]
    fn sgr_unknown_interspersed_with_valid() {
        // Unknown (6), bold (1), unknown (99), italic (3)
        let changes: Vec<_> = parse_sgr(&[6, 1, 99, 3]).collect();
        assert_eq!(
            changes,
            vec![SgrChange::Bold(true), SgrChange::Italic(true)]
        );
    }

    #[test]
    fn sgr_extended_fg_followed_by_valid() {
        // 38;5;196 then bold
        let changes: Vec<_> = parse_sgr(&[38, 5, 196, 1]).collect();
        assert_eq!(changes, vec![SgrChange::Fg256(196), SgrChange::Bold(true)]);
    }

    #[test]
    fn sgr_extended_bg_followed_by_valid() {
        // bg RGB then fg default
        let changes: Vec<_> = parse_sgr(&[48, 2, 10, 20, 30, 39]).collect();
        assert_eq!(
            changes,
            vec![SgrChange::BgRgb(10, 20, 30), SgrChange::FgDefault]
        );
    }

    // ── Parser edge cases ───────────────────────────────────────────────

    #[test]
    fn parse_empty_input() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"", &mut handler);

        assert!(handler.printed.borrow().is_empty());
        assert!(handler.executed.borrow().is_empty());
        assert!(handler.csi_calls.borrow().is_empty());
        assert!(handler.osc_calls.borrow().is_empty());
        assert!(handler.esc_calls.borrow().is_empty());
    }

    #[test]
    fn parse_only_control_codes() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"\n\r\t\x07\x08", &mut handler);

        assert!(handler.printed.borrow().is_empty());
        let executed: Vec<u8> = handler.executed.borrow().clone();
        assert_eq!(executed, vec![b'\n', b'\r', b'\t', 0x07, 0x08]);
    }

    #[test]
    fn parse_bell_control_code() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"\x07", &mut handler);

        let executed: Vec<u8> = handler.executed.borrow().clone();
        assert_eq!(executed, vec![0x07]);
    }

    #[test]
    fn parse_backspace_control_code() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.parse(b"\x08", &mut handler);

        let executed: Vec<u8> = handler.executed.borrow().clone();
        assert_eq!(executed, vec![0x08]);
    }

    #[test]
    fn parse_osc_with_st_terminator() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // OSC 2 ; title ST (ESC \)
        parser.parse(b"\x1b]2;Window Title\x1b\\", &mut handler);

        let osc_calls = handler.osc_calls.borrow();
        assert_eq!(osc_calls.len(), 1);
        assert_eq!(osc_calls[0][0], b"2");
        assert_eq!(osc_calls[0][1], b"Window Title");
    }

    #[test]
    fn parse_multiple_parse_calls_state_persists() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // First call: text
        parser.parse(b"AB", &mut handler);
        // Second call: more text
        parser.parse(b"CD", &mut handler);

        let printed: Vec<char> = handler.printed.borrow().clone();
        assert_eq!(printed, vec!['A', 'B', 'C', 'D']);
    }

    #[test]
    fn parse_sequence_split_across_calls() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // Split ESC [ 31 m across two calls
        parser.parse(b"\x1b[3", &mut handler);
        assert!(handler.csi_calls.borrow().is_empty());

        parser.parse(b"1m", &mut handler);
        assert_eq!(handler.csi_calls.borrow().len(), 1);
        assert_eq!(handler.csi_calls.borrow()[0].0, vec![31]);
        assert_eq!(handler.csi_calls.borrow()[0].2, 'm');
    }

    #[test]
    fn parser_default_impl() {
        let parser = AnsiParser::default();
        // Just verify it constructs without error
        let debug = format!("{parser:?}");
        assert!(debug.contains("AnsiParser"));
    }

    #[test]
    fn parser_debug_impl() {
        let parser = AnsiParser::new();
        let debug = format!("{parser:?}");
        assert!(debug.contains("AnsiParser"));
    }

    #[test]
    fn hook_trace_correlation_ids_are_monotonic() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        parser.register_esc_hook(|_event| HookDisposition::Continue);
        parser.parse(b"\x1b7\x1b8", &mut handler);

        let trace = parser.drain_hook_trace();
        assert!(!trace.is_empty());
        assert!(
            trace
                .windows(2)
                .all(|window| { window[0].correlation_id < window[1].correlation_id })
        );
        assert!(trace.iter().all(|event| event.ts_ms > 0));
    }

    #[test]
    fn reset_discards_partial_sequence() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // Start incomplete CSI
        parser.parse(b"\x1b[", &mut handler);

        // Reset discards it
        parser.reset();

        // Plain text should work normally
        parser.parse(b"Hello", &mut handler);
        let printed: Vec<char> = handler.printed.borrow().clone();
        assert_eq!(printed, vec!['H', 'e', 'l', 'l', 'o']);
        assert!(handler.csi_calls.borrow().is_empty());
    }

    // ── SGR constant value tests ────────────────────────────────────────

    #[test]
    fn sgr_constants_are_correct() {
        assert_eq!(sgr::RESET, 0);
        assert_eq!(sgr::BOLD, 1);
        assert_eq!(sgr::DIM, 2);
        assert_eq!(sgr::ITALIC, 3);
        assert_eq!(sgr::UNDERLINE, 4);
        assert_eq!(sgr::BLINK, 5);
        assert_eq!(sgr::REVERSE, 7);
        assert_eq!(sgr::HIDDEN, 8);
        assert_eq!(sgr::STRIKETHROUGH, 9);

        assert_eq!(sgr::NORMAL_INTENSITY, 22);
        assert_eq!(sgr::NO_ITALIC, 23);
        assert_eq!(sgr::NO_UNDERLINE, 24);
        assert_eq!(sgr::NO_BLINK, 25);
        assert_eq!(sgr::NO_REVERSE, 27);
        assert_eq!(sgr::NO_HIDDEN, 28);
        assert_eq!(sgr::NO_STRIKETHROUGH, 29);
    }

    #[test]
    fn sgr_fg_color_constants() {
        assert_eq!(sgr::FG_BLACK, 30);
        assert_eq!(sgr::FG_RED, 31);
        assert_eq!(sgr::FG_GREEN, 32);
        assert_eq!(sgr::FG_YELLOW, 33);
        assert_eq!(sgr::FG_BLUE, 34);
        assert_eq!(sgr::FG_MAGENTA, 35);
        assert_eq!(sgr::FG_CYAN, 36);
        assert_eq!(sgr::FG_WHITE, 37);
        assert_eq!(sgr::FG_EXTENDED, 38);
        assert_eq!(sgr::FG_DEFAULT, 39);
    }

    #[test]
    fn sgr_bg_color_constants() {
        assert_eq!(sgr::BG_BLACK, 40);
        assert_eq!(sgr::BG_RED, 41);
        assert_eq!(sgr::BG_GREEN, 42);
        assert_eq!(sgr::BG_YELLOW, 43);
        assert_eq!(sgr::BG_BLUE, 44);
        assert_eq!(sgr::BG_MAGENTA, 45);
        assert_eq!(sgr::BG_CYAN, 46);
        assert_eq!(sgr::BG_WHITE, 47);
        assert_eq!(sgr::BG_EXTENDED, 48);
        assert_eq!(sgr::BG_DEFAULT, 49);
    }

    #[test]
    fn sgr_bright_color_constants() {
        assert_eq!(sgr::FG_BRIGHT_BLACK, 90);
        assert_eq!(sgr::FG_BRIGHT_RED, 91);
        assert_eq!(sgr::FG_BRIGHT_GREEN, 92);
        assert_eq!(sgr::FG_BRIGHT_YELLOW, 93);
        assert_eq!(sgr::FG_BRIGHT_BLUE, 94);
        assert_eq!(sgr::FG_BRIGHT_MAGENTA, 95);
        assert_eq!(sgr::FG_BRIGHT_CYAN, 96);
        assert_eq!(sgr::FG_BRIGHT_WHITE, 97);

        assert_eq!(sgr::BG_BRIGHT_BLACK, 100);
        assert_eq!(sgr::BG_BRIGHT_RED, 101);
        assert_eq!(sgr::BG_BRIGHT_GREEN, 102);
        assert_eq!(sgr::BG_BRIGHT_YELLOW, 103);
        assert_eq!(sgr::BG_BRIGHT_BLUE, 104);
        assert_eq!(sgr::BG_BRIGHT_MAGENTA, 105);
        assert_eq!(sgr::BG_BRIGHT_CYAN, 106);
        assert_eq!(sgr::BG_BRIGHT_WHITE, 107);
    }

    #[test]
    fn sgr_color_mode_constants() {
        assert_eq!(sgr::COLOR_256, 5);
        assert_eq!(sgr::COLOR_RGB, 2);
    }

    // ── DEC mode constant tests ─────────────────────────────────────────

    #[test]
    fn dec_mode_constants_are_correct() {
        assert_eq!(dec_mode::CURSOR_VISIBLE, 25);
        assert_eq!(dec_mode::ALT_SCREEN, 1049);
        assert_eq!(dec_mode::ALT_SCREEN_NO_CLEAR, 1047);
        assert_eq!(dec_mode::SAVE_CURSOR, 1048);
        assert_eq!(dec_mode::MOUSE_TRACKING, 1000);
        assert_eq!(dec_mode::MOUSE_BUTTON, 1002);
        assert_eq!(dec_mode::MOUSE_ANY, 1003);
        assert_eq!(dec_mode::MOUSE_SGR, 1006);
        assert_eq!(dec_mode::FOCUS, 1004);
        assert_eq!(dec_mode::BRACKETED_PASTE, 2004);
    }

    // ── SgrChange trait coverage ────────────────────────────────────────

    #[test]
    fn sgr_change_debug_format() {
        let change = SgrChange::Bold(true);
        let debug = format!("{change:?}");
        assert!(debug.contains("Bold"));
    }

    #[test]
    fn sgr_change_clone_eq() {
        let a = SgrChange::FgRgb(100, 150, 200);
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn sgr_change_ne() {
        assert_ne!(SgrChange::Bold(true), SgrChange::Bold(false));
        assert_ne!(SgrChange::FgAnsi(1), SgrChange::FgAnsi(2));
        assert_ne!(SgrChange::FgDefault, SgrChange::BgDefault);
    }

    // ── Full roundtrip: parse ANSI then interpret SGR ───────────────────

    #[test]
    fn roundtrip_parse_then_sgr() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // Bold red text with reset
        parser.parse(b"\x1b[1;31mRed\x1b[0m", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 2);

        // Interpret the first SGR
        let sgr_changes: Vec<_> = parse_sgr(&csi_calls[0].0).collect();
        assert_eq!(
            sgr_changes,
            vec![SgrChange::Bold(true), SgrChange::FgAnsi(1)]
        );

        // Interpret the reset
        let reset_changes: Vec<_> = parse_sgr(&csi_calls[1].0).collect();
        assert_eq!(reset_changes, vec![SgrChange::Reset]);
    }

    #[test]
    fn roundtrip_256_color() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ 38;5;208 m - 256-color orange foreground
        parser.parse(b"\x1b[38;5;208m", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);

        let sgr_changes: Vec<_> = parse_sgr(&csi_calls[0].0).collect();
        assert_eq!(sgr_changes, vec![SgrChange::Fg256(208)]);
    }

    #[test]
    fn roundtrip_rgb_color() {
        let mut parser = AnsiParser::new();
        let mut handler = TestHandler::default();

        // ESC [ 48;2;255;128;0 m - RGB orange background
        parser.parse(b"\x1b[48;2;255;128;0m", &mut handler);

        let csi_calls = handler.csi_calls.borrow();
        assert_eq!(csi_calls.len(), 1);

        let sgr_changes: Vec<_> = parse_sgr(&csi_calls[0].0).collect();
        assert_eq!(sgr_changes, vec![SgrChange::BgRgb(255, 128, 0)]);
    }

    #[test]
    fn sgr_all_attribute_set_then_reset() {
        // Set every attribute, then reset each one
        let changes: Vec<_> =
            parse_sgr(&[1, 2, 3, 4, 5, 7, 8, 9, 22, 23, 24, 25, 27, 28, 29]).collect();
        assert_eq!(
            changes,
            vec![
                SgrChange::Bold(true),
                SgrChange::Dim(true),
                SgrChange::Italic(true),
                SgrChange::Underline(true),
                SgrChange::Blink(true),
                SgrChange::Reverse(true),
                SgrChange::Hidden(true),
                SgrChange::Strikethrough(true),
                SgrChange::Bold(false), // 22 resets bold
                SgrChange::Italic(false),
                SgrChange::Underline(false),
                SgrChange::Blink(false),
                SgrChange::Reverse(false),
                SgrChange::Hidden(false),
                SgrChange::Strikethrough(false),
            ]
        );
    }
}
