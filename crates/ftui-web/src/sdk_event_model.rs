//! Typed host-event and error model for the FrankenTermJS SDK, with a
//! TypeScript-definition generator kept in lockstep (bd-2vr05.9.2).
//!
//! # Why this exists
//!
//! The FrankenTermJS browser SDK promises host integrators a *stable, typed*
//! event and error surface (see [`docs/spec/frankenterm-web-api.md`] — the
//! canonical contract). xterm.js integrators reach for `term.onData`,
//! `term.onKey`, typed event payloads, and predictable error shapes; the
//! FrankenTermJS replacement must offer the same with a single, versioned
//! taxonomy.
//!
//! The original `apiContract()` constants from `bd-2vr05.9.1` shipped inside the
//! `frankenterm-web` WASM-packaging crate, which is **transient** and not
//! vendored in this checkout. This module is therefore the **durable, in-tree
//! source of truth** for the event/error model: pure Rust enums that mirror the
//! documented taxonomy, plus a deterministic [`typescript_definitions`]
//! generator so the runtime payloads and the shipped `.d.ts` can never drift.
//!
//! ```text
//!  HostEventClass / SdkErrorKind (Rust, single source of truth)
//!        │
//!        ├── as_str()/code()  ──> runtime JSONL payloads (host-observable)
//!        └── typescript_definitions() ──> sdk/frankenterm-js-events.d.ts (golden)
//! ```
//!
//! ## Determinism
//!
//! Everything here is pure: the taxonomy order, the generated `.d.ts`, and the
//! buffer-policy snapshot are byte-for-byte reproducible for a fixed crate
//! version. The conformance harness
//! (`crates/ftui-web/tests/frankenterm_js_sdk_contract_compat.rs`) asserts the
//! Rust model and the committed `.d.ts` stay in lockstep.

use core::fmt;

/// Event-schema version of the host-observable taxonomy. Mirrors
/// `eventSchemaVersion` in the web API contract.
pub const EVENT_SCHEMA_VERSION: &str = "1.0.0";

/// Canonical, host-observable event classes (`eventSchemaVersion = 1.0.0`).
///
/// The wire strings and their order match the **Host Event Taxonomy** section of
/// `docs/spec/frankenterm-web-api.md` exactly. The list is monotonically sorted
/// by wire string, which the contract guarantees for `apiContract().eventTypes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostEventClass {
    /// `attach.transition` — websocket attach state-machine transitions.
    AttachTransition,
    /// `input.accessibility` — assistive-technology originated input.
    InputAccessibility,
    /// `input.composition` — IME composition lifecycle.
    InputComposition,
    /// `input.composition_trace` — IME rewrite/diagnostic trace.
    InputCompositionTrace,
    /// `input.focus` — focus gained/lost.
    InputFocus,
    /// `input.key` — key press/release.
    InputKey,
    /// `input.mouse` — mouse button/motion.
    InputMouse,
    /// `input.paste` — bracketed paste payload.
    InputPaste,
    /// `input.touch` — touch input.
    InputTouch,
    /// `input.vt_bytes` — raw VT byte chunk forwarded to the PTY.
    InputVtBytes,
    /// `input.wheel` — wheel/scroll input.
    InputWheel,
    /// `terminal.progress` — OSC 9;4 progress signal.
    TerminalProgress,
    /// `terminal.reply_bytes` — terminal-generated reply bytes.
    TerminalReplyBytes,
    /// `ui.accessibility_announcement` — live-region announcement for the host.
    UiAccessibilityAnnouncement,
    /// `ui.link_click` — host-actionable hyperlink activation.
    UiLinkClick,
}

impl HostEventClass {
    /// All event classes in canonical (sorted) order.
    pub const ALL: [HostEventClass; 15] = [
        HostEventClass::AttachTransition,
        HostEventClass::InputAccessibility,
        HostEventClass::InputComposition,
        HostEventClass::InputCompositionTrace,
        HostEventClass::InputFocus,
        HostEventClass::InputKey,
        HostEventClass::InputMouse,
        HostEventClass::InputPaste,
        HostEventClass::InputTouch,
        HostEventClass::InputVtBytes,
        HostEventClass::InputWheel,
        HostEventClass::TerminalProgress,
        HostEventClass::TerminalReplyBytes,
        HostEventClass::UiAccessibilityAnnouncement,
        HostEventClass::UiLinkClick,
    ];

    /// Canonical dotted wire string (stable for the `1.x` schema line).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            HostEventClass::AttachTransition => "attach.transition",
            HostEventClass::InputAccessibility => "input.accessibility",
            HostEventClass::InputComposition => "input.composition",
            HostEventClass::InputCompositionTrace => "input.composition_trace",
            HostEventClass::InputFocus => "input.focus",
            HostEventClass::InputKey => "input.key",
            HostEventClass::InputMouse => "input.mouse",
            HostEventClass::InputPaste => "input.paste",
            HostEventClass::InputTouch => "input.touch",
            HostEventClass::InputVtBytes => "input.vt_bytes",
            HostEventClass::InputWheel => "input.wheel",
            HostEventClass::TerminalProgress => "terminal.progress",
            HostEventClass::TerminalReplyBytes => "terminal.reply_bytes",
            HostEventClass::UiAccessibilityAnnouncement => "ui.accessibility_announcement",
            HostEventClass::UiLinkClick => "ui.link_click",
        }
    }

    /// Namespace prefix (`attach` / `input` / `terminal` / `ui`).
    #[must_use]
    pub fn namespace(self) -> &'static str {
        let s = self.as_str();
        match s.split_once('.') {
            Some((ns, _)) => ns,
            None => s,
        }
    }

    /// Parse a wire string back to its class (inverse of [`Self::as_str`]).
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == value)
    }
}

impl fmt::Display for HostEventClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Typed SDK error taxonomy.
///
/// Consolidates the contract's documented rejection/policy surfaces (malformed
/// progress signals, blocked links, clipboard bounds, capability gating,
/// bounded-queue drops, input parsing, and attach/protocol faults) into a
/// stable, host-consumable set of error codes. Each variant maps to a single
/// dotted [`Self::code`] that is stable for the `1.x` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SdkErrorKind {
    /// `attach.protocol_error` — websocket attach/lifecycle protocol fault.
    AttachProtocol,
    /// `capability.unsupported` — requested feature/renderer not advertised.
    CapabilityUnsupported,
    /// `clipboard.disabled` — copy/paste disabled by host clipboard policy.
    ClipboardDisabled,
    /// `clipboard.paste_too_large` — paste payload exceeds `maxPasteBytes`.
    ClipboardPasteTooLarge,
    /// `input.parse` — malformed host-encoded input payload.
    InputParse,
    /// `queue.overflow` — a bounded host-drained queue dropped its oldest entry.
    QueueOverflow,
    /// `terminal.progress.malformed` — malformed OSC 9;4 progress payload.
    TerminalProgressMalformed,
    /// `ui.link.blocked` — hyperlink activation denied by link-open policy.
    UiLinkBlocked,
}

impl SdkErrorKind {
    /// All error kinds in canonical (sorted-by-code) order.
    pub const ALL: [SdkErrorKind; 8] = [
        SdkErrorKind::AttachProtocol,
        SdkErrorKind::CapabilityUnsupported,
        SdkErrorKind::ClipboardDisabled,
        SdkErrorKind::ClipboardPasteTooLarge,
        SdkErrorKind::InputParse,
        SdkErrorKind::QueueOverflow,
        SdkErrorKind::TerminalProgressMalformed,
        SdkErrorKind::UiLinkBlocked,
    ];

    /// Stable dotted error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            SdkErrorKind::AttachProtocol => "attach.protocol_error",
            SdkErrorKind::CapabilityUnsupported => "capability.unsupported",
            SdkErrorKind::ClipboardDisabled => "clipboard.disabled",
            SdkErrorKind::ClipboardPasteTooLarge => "clipboard.paste_too_large",
            SdkErrorKind::InputParse => "input.parse",
            SdkErrorKind::QueueOverflow => "queue.overflow",
            SdkErrorKind::TerminalProgressMalformed => "terminal.progress.malformed",
            SdkErrorKind::UiLinkBlocked => "ui.link.blocked",
        }
    }

    /// Stable, human-readable summary of the error condition.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            SdkErrorKind::AttachProtocol => "websocket attach protocol error",
            SdkErrorKind::CapabilityUnsupported => {
                "requested capability is not advertised by the engine"
            }
            SdkErrorKind::ClipboardDisabled => "clipboard access disabled by host policy",
            SdkErrorKind::ClipboardPasteTooLarge => {
                "paste payload exceeds the maximum allowed size"
            }
            SdkErrorKind::InputParse => "host-encoded input payload could not be parsed",
            SdkErrorKind::QueueOverflow => "a bounded host queue dropped its oldest entry",
            SdkErrorKind::TerminalProgressMalformed => "malformed OSC 9;4 progress payload",
            SdkErrorKind::UiLinkBlocked => "hyperlink activation blocked by link-open policy",
        }
    }

    /// Parse a wire code back to its kind (inverse of [`Self::code`]).
    #[must_use]
    pub fn from_code(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.code() == value)
    }
}

impl fmt::Display for SdkErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Bounded host-drained queue policy (drop-oldest), mirroring the
/// **Bounded Buffering Contract** in `docs/spec/frankenterm-web-api.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventBufferPolicy {
    /// `encoded_inputs_queue_max`.
    pub encoded_inputs_queue_max: u32,
    /// `encoded_input_bytes_queue_max`.
    pub encoded_input_bytes_queue_max: u32,
    /// `ime_trace_queue_max`.
    pub ime_trace_queue_max: u32,
    /// `link_click_queue_max`.
    pub link_click_queue_max: u32,
    /// `accessibility_announcement_queue_max`.
    pub accessibility_announcement_queue_max: u32,
    /// `attach_transition_queue_max`.
    pub attach_transition_queue_max: u32,
    /// `event_subscription_queue_default_max`.
    pub event_subscription_queue_default_max: u32,
    /// `event_subscription_queue_configurable_max` (upper bound for the above).
    pub event_subscription_queue_configurable_max: u32,
    /// `event_subscription_registry_max`.
    pub event_subscription_registry_max: u32,
}

impl EventBufferPolicy {
    /// The documented default policy (`eventSchemaVersion = 1.0.0`).
    pub const DEFAULT: EventBufferPolicy = EventBufferPolicy {
        encoded_inputs_queue_max: 4096,
        encoded_input_bytes_queue_max: 4096,
        ime_trace_queue_max: 2048,
        link_click_queue_max: 2048,
        accessibility_announcement_queue_max: 64,
        attach_transition_queue_max: 512,
        event_subscription_queue_default_max: 512,
        event_subscription_queue_configurable_max: 8192,
        event_subscription_registry_max: 256,
    };

    /// Stable `(wire_name, value)` entries in declaration order, for
    /// serialization and contract validation.
    #[must_use]
    pub fn entries(self) -> [(&'static str, u32); 9] {
        [
            ("encoded_inputs_queue_max", self.encoded_inputs_queue_max),
            (
                "encoded_input_bytes_queue_max",
                self.encoded_input_bytes_queue_max,
            ),
            ("ime_trace_queue_max", self.ime_trace_queue_max),
            ("link_click_queue_max", self.link_click_queue_max),
            (
                "accessibility_announcement_queue_max",
                self.accessibility_announcement_queue_max,
            ),
            (
                "attach_transition_queue_max",
                self.attach_transition_queue_max,
            ),
            (
                "event_subscription_queue_default_max",
                self.event_subscription_queue_default_max,
            ),
            (
                "event_subscription_queue_configurable_max",
                self.event_subscription_queue_configurable_max,
            ),
            (
                "event_subscription_registry_max",
                self.event_subscription_registry_max,
            ),
        ]
    }
}

/// Convert a snake_case wire name to lowerCamelCase for the TypeScript surface.
fn to_lower_camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = false;
    for ch in snake.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Generate the TypeScript definition file for the SDK event/error model.
///
/// Output is deterministic and is the single source for the committed
/// `sdk/frankenterm-js-events.d.ts`. The conformance harness asserts the
/// committed file matches this output exactly, so the runtime model and the
/// shipped types can never drift.
#[must_use]
pub fn typescript_definitions() -> String {
    let mut out = String::new();
    out.push_str(
        "// AUTO-GENERATED — do not edit by hand.\n\
         // Source of truth: crates/ftui-web/src/sdk_event_model.rs\n\
         // (mirrors docs/spec/frankenterm-web-api.md). Regenerate with:\n\
         //   FTUI_SDK_DTS_BLESS=1 cargo test -p ftui-web \\\n\
         //     --test frankenterm_js_sdk_contract_compat\n\
         //\n\
         // FrankenTermJS SDK event/error model (bd-2vr05.9.2).\n\n",
    );

    out.push_str(&format!(
        "export const EVENT_SCHEMA_VERSION = \"{EVENT_SCHEMA_VERSION}\";\n\n"
    ));

    out.push_str("/** Canonical host-observable event classes. */\n");
    out.push_str("export type HostEventClass =\n");
    for (idx, class) in HostEventClass::ALL.iter().enumerate() {
        let last = idx + 1 == HostEventClass::ALL.len();
        let term = if last { ";" } else { "" };
        out.push_str(&format!("  | \"{}\"{term}\n", class.as_str()));
    }
    out.push('\n');

    out.push_str("/** A single host-observable event. */\n");
    out.push_str("export interface HostEvent {\n");
    out.push_str("  readonly type: HostEventClass;\n");
    out.push_str("  /** Globally monotonic sequence number. */\n");
    out.push_str("  readonly seq: number;\n");
    out.push_str("  /** Event-class-specific payload. */\n");
    out.push_str("  readonly payload?: unknown;\n");
    out.push_str("}\n\n");

    out.push_str("/** Stable SDK error codes. */\n");
    out.push_str("export type SdkErrorCode =\n");
    for (idx, kind) in SdkErrorKind::ALL.iter().enumerate() {
        let last = idx + 1 == SdkErrorKind::ALL.len();
        let term = if last { ";" } else { "" };
        out.push_str(&format!("  | \"{}\"{term}\n", kind.code()));
    }
    out.push('\n');

    out.push_str("/** A typed SDK error. */\n");
    out.push_str("export interface SdkError {\n");
    out.push_str("  readonly code: SdkErrorCode;\n");
    out.push_str("  readonly message: string;\n");
    out.push_str("}\n\n");

    out.push_str("/** Bounded host-queue policy (drop-oldest). */\n");
    out.push_str("export interface EventBufferPolicy {\n");
    for (name, _) in EventBufferPolicy::DEFAULT.entries() {
        out.push_str(&format!("  readonly {}: number;\n", to_lower_camel(name)));
    }
    out.push_str("}\n\n");

    out.push_str("export const EVENT_BUFFER_POLICY: EventBufferPolicy = {\n");
    for (name, value) in EventBufferPolicy::DEFAULT.entries() {
        out.push_str(&format!("  {}: {value},\n", to_lower_camel(name)));
    }
    out.push_str("};\n");

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxonomy_is_unique_sorted_and_round_trips() {
        let wires: Vec<&str> = HostEventClass::ALL.iter().map(|c| c.as_str()).collect();
        let mut sorted = wires.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(wires, sorted, "taxonomy must be unique and sorted");
        for class in HostEventClass::ALL {
            assert_eq!(HostEventClass::from_wire(class.as_str()), Some(class));
        }
        assert_eq!(HostEventClass::from_wire("nope"), None);
    }

    #[test]
    fn error_codes_are_unique_sorted_and_round_trip() {
        let codes: Vec<&str> = SdkErrorKind::ALL.iter().map(|k| k.code()).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(codes, sorted, "error codes must be unique and sorted");
        for kind in SdkErrorKind::ALL {
            assert_eq!(SdkErrorKind::from_code(kind.code()), Some(kind));
            assert!(!kind.summary().is_empty());
        }
    }

    #[test]
    fn typescript_definitions_are_deterministic_and_complete() {
        let a = typescript_definitions();
        let b = typescript_definitions();
        assert_eq!(a, b, "generator must be deterministic");
        for class in HostEventClass::ALL {
            assert!(
                a.contains(&format!("\"{}\"", class.as_str())),
                "d.ts missing event class {}",
                class.as_str()
            );
        }
        for kind in SdkErrorKind::ALL {
            assert!(
                a.contains(&format!("\"{}\"", kind.code())),
                "d.ts missing error code {}",
                kind.code()
            );
        }
    }

    #[test]
    fn lower_camel_conversion() {
        assert_eq!(
            to_lower_camel("encoded_inputs_queue_max"),
            "encodedInputsQueueMax"
        );
        assert_eq!(to_lower_camel("seq"), "seq");
    }
}
