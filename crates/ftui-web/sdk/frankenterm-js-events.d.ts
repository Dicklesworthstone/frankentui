// AUTO-GENERATED — do not edit by hand.
// Source of truth: crates/ftui-web/src/sdk_event_model.rs
// (mirrors docs/spec/frankenterm-web-api.md). Regenerate with:
//   FTUI_SDK_DTS_BLESS=1 cargo test -p ftui-web \
//     --test frankenterm_js_sdk_contract_compat
//
// FrankenTermJS SDK event/error model (bd-2vr05.9.2).

export const EVENT_SCHEMA_VERSION = "1.0.0";

/** Canonical host-observable event classes. */
export type HostEventClass =
  | "attach.transition"
  | "input.accessibility"
  | "input.composition"
  | "input.composition_trace"
  | "input.focus"
  | "input.key"
  | "input.mouse"
  | "input.paste"
  | "input.touch"
  | "input.vt_bytes"
  | "input.wheel"
  | "terminal.progress"
  | "terminal.reply_bytes"
  | "ui.accessibility_announcement"
  | "ui.link_click";

/** A single host-observable event. */
export interface HostEvent {
  readonly type: HostEventClass;
  /** Globally monotonic sequence number. */
  readonly seq: number;
  /** Event-class-specific payload. */
  readonly payload?: unknown;
}

/** Stable SDK error codes. */
export type SdkErrorCode =
  | "attach.protocol_error"
  | "capability.unsupported"
  | "clipboard.disabled"
  | "clipboard.paste_too_large"
  | "input.parse"
  | "queue.overflow"
  | "terminal.progress.malformed"
  | "ui.link.blocked";

/** A typed SDK error. */
export interface SdkError {
  readonly code: SdkErrorCode;
  readonly message: string;
}

/** Bounded host-queue policy (drop-oldest). */
export interface EventBufferPolicy {
  readonly encodedInputsQueueMax: number;
  readonly encodedInputBytesQueueMax: number;
  readonly imeTraceQueueMax: number;
  readonly linkClickQueueMax: number;
  readonly accessibilityAnnouncementQueueMax: number;
  readonly attachTransitionQueueMax: number;
  readonly eventSubscriptionQueueDefaultMax: number;
  readonly eventSubscriptionQueueConfigurableMax: number;
  readonly eventSubscriptionRegistryMax: number;
}

export const EVENT_BUFFER_POLICY: EventBufferPolicy = {
  encodedInputsQueueMax: 4096,
  encodedInputBytesQueueMax: 4096,
  imeTraceQueueMax: 2048,
  linkClickQueueMax: 2048,
  accessibilityAnnouncementQueueMax: 64,
  attachTransitionQueueMax: 512,
  eventSubscriptionQueueDefaultMax: 512,
  eventSubscriptionQueueConfigurableMax: 8192,
  eventSubscriptionRegistryMax: 256,
};
