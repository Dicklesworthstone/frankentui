//! First-party integration adapters for the FrankenTermJS SDK
//! (bd-2vr05.9.3).
//!
//! # Why this exists
//!
//! Replacing xterm.js in a real app is mostly a *lifecycle wiring* problem:
//! when to call `init`, when it is safe to `attachConnect`, how resize
//! observation feeds `resize`, and — the classic integration bug — what
//! happens when React StrictMode runs your effect twice. The SDK contract
//! (`docs/spec/frankenterm-web-api.md`) defines the method surface; this
//! module defines the **recommended wiring** as an executable, deterministic
//! model instead of prose:
//!
//! * [`AdapterLifecycle`] — a pure state machine encoding the legal call
//!   order for a host adapter (mount → attach → resize/input → detach →
//!   dispose), with per-host-kind semantics: the React adapter treats
//!   StrictMode-style repeated mounts/cleanups as idempotent no-ops, the
//!   vanilla adapter reports them as misuse.
//! * [`recommended_wiring`] — the ordered wiring steps for each adapter
//!   kind, each step naming the *real* stable contract method it calls.
//! * [`vanilla_example`] / [`react_example`] — canonical example sources.
//!   The committed files under `crates/ftui-web/sdk/examples/` must be
//!   byte-identical to these generators (lockstep-tested, exactly like the
//!   `.d.ts` in [`crate::sdk_event_model`]), so shipped examples can never
//!   drift from the modeled lifecycle.
//!
//! Like the rest of the durable SDK surface, this lives in-tree in `ftui-web`
//! while the `frankenterm-web` WASM packaging crate remains out-of-tree; the
//! examples reference only the **Stable Method Surface (`1.0.0`)** from the
//! canonical contract.
//!
//! # Determinism and logging
//!
//! Every applied action (and every rejected misuse) yields a JSONL line with
//! a monotone `seq` and the host-chosen `adapter_id` as the correlation key.
//! The model is timestamp-free so identical action sequences produce
//! byte-identical logs; wall-clock timestamps are added by the E2E harness
//! layer (`scripts/frankenterm_js_sdk_adapter_e2e.sh`) where determinism is
//! not required.

use core::fmt;

/// Schema version for adapter lifecycle events and wiring tables.
pub const ADAPTER_SCHEMA_VERSION: &str = "1.0.0";

/// Stable contract methods referenced by the adapter wiring (a subset of the
/// **Stable Method Surface (`1.0.0`)** in `docs/spec/frankenterm-web-api.md`).
pub const WIRING_CONTRACT_METHODS: [&str; 9] = [
    "apiContract",
    "init",
    "fitToContainer",
    "resize",
    "attachConnect",
    "input",
    "drainEventSubscriptionJsonl",
    "attachClose",
    "destroy",
];

// ============================================================================
// Adapter kinds
// ============================================================================

/// First-party adapter flavors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AdapterKind {
    /// Plain-DOM adapter: explicit create/dispose, misuse is an error.
    Vanilla,
    /// React adapter (also the Next.js wiring): effect-driven lifecycle where
    /// StrictMode intentionally double-invokes mount/cleanup, so repeated
    /// idempotent actions are deduplicated instead of rejected.
    React,
}

impl AdapterKind {
    /// Both adapter kinds, in stable order.
    pub const ALL: [AdapterKind; 2] = [AdapterKind::Vanilla, AdapterKind::React];

    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AdapterKind::Vanilla => "vanilla",
            AdapterKind::React => "react",
        }
    }
}

impl fmt::Display for AdapterKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Lifecycle phases and actions
// ============================================================================

/// Adapter lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AdapterPhase {
    /// Constructed; no DOM or engine resources yet.
    Created,
    /// Engine initialized into a container (`init` called).
    Mounted,
    /// Transport attached (`attachConnect` succeeded).
    Attached,
    /// Transport detached (`attachClose`), engine still mounted.
    Detached,
    /// Fully torn down (`destroy`); terminal state.
    Disposed,
}

impl AdapterPhase {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AdapterPhase::Created => "created",
            AdapterPhase::Mounted => "mounted",
            AdapterPhase::Attached => "attached",
            AdapterPhase::Detached => "detached",
            AdapterPhase::Disposed => "disposed",
        }
    }
}

/// Host action applied to the adapter (each maps to stable contract calls).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterAction {
    /// Initialize the engine into a container (`init`, then `fitToContainer`).
    Mount,
    /// Connect the transport (`attachConnect`).
    Attach,
    /// Propagate a size change (`resize(cols, rows)`).
    Resize {
        /// New column count.
        cols: u16,
        /// New row count.
        rows: u16,
    },
    /// Forward host input (`input`).
    Input {
        /// Number of host-encoded input bytes forwarded.
        bytes: u32,
    },
    /// Close the transport (`attachClose`).
    Detach,
    /// Tear everything down (`destroy`).
    Dispose,
}

impl AdapterAction {
    /// Stable wire label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            AdapterAction::Mount => "mount",
            AdapterAction::Attach => "attach",
            AdapterAction::Resize { .. } => "resize",
            AdapterAction::Input { .. } => "input",
            AdapterAction::Detach => "detach",
            AdapterAction::Dispose => "dispose",
        }
    }

    fn detail(&self) -> String {
        match self {
            AdapterAction::Resize { cols, rows } => format!("cols={cols} rows={rows}"),
            AdapterAction::Input { bytes } => format!("bytes={bytes}"),
            _ => String::new(),
        }
    }
}

// ============================================================================
// Outcomes, events, and misuse
// ============================================================================

/// How an accepted action was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterOutcome {
    /// The action performed its contract calls and advanced the phase.
    Applied,
    /// React-only: the action repeated an already-satisfied idempotent step
    /// (StrictMode double mount / double cleanup) and was deduplicated.
    StrictModeDeduped,
}

impl AdapterOutcome {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AdapterOutcome::Applied => "applied",
            AdapterOutcome::StrictModeDeduped => "strict_mode_deduped",
        }
    }
}

/// One accepted lifecycle transition (JSONL-serializable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterEvent {
    /// Monotone per-adapter sequence number (correlation within a run).
    pub seq: u64,
    /// Host-chosen correlation id.
    pub adapter_id: String,
    /// Adapter flavor.
    pub kind: AdapterKind,
    /// Action label.
    pub action: &'static str,
    /// Phase before the action.
    pub phase_before: AdapterPhase,
    /// Phase after the action.
    pub phase_after: AdapterPhase,
    /// Applied vs deduplicated.
    pub outcome: AdapterOutcome,
    /// Action-specific detail (`cols=..`, `bytes=..`), possibly empty.
    pub detail: String,
}

impl AdapterEvent {
    /// Deterministic single-line JSON (timestamp-free; see module docs).
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            "{{\"event\":\"adapter_transition\",\"schema\":\"{}\",\"seq\":{},\"adapter_id\":\"{}\",\"adapter\":\"{}\",\"action\":\"{}\",\"phase_before\":\"{}\",\"phase_after\":\"{}\",\"outcome\":\"{}\",\"detail\":\"{}\"}}",
            ADAPTER_SCHEMA_VERSION,
            self.seq,
            escape_json(&self.adapter_id),
            self.kind.as_str(),
            self.action,
            self.phase_before.as_str(),
            self.phase_after.as_str(),
            self.outcome.as_str(),
            escape_json(&self.detail),
        )
    }
}

/// A rejected action: the host called the adapter out of order.
///
/// This is the *adapter-layer* misuse taxonomy (lifecycle ordering), distinct
/// from the engine-level [`crate::sdk_event_model::SdkErrorKind`] taxonomy
/// (protocol/capability/input errors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterMisuse {
    /// Monotone per-adapter sequence number (shared with accepted events).
    pub seq: u64,
    /// Host-chosen correlation id.
    pub adapter_id: String,
    /// Adapter flavor.
    pub kind: AdapterKind,
    /// Stable dotted misuse code.
    pub code: &'static str,
    /// Action that was rejected.
    pub action: &'static str,
    /// Phase the adapter was in.
    pub phase: AdapterPhase,
    /// Human-readable explanation with the recommended fix.
    pub explanation: &'static str,
}

impl AdapterMisuse {
    /// All stable misuse codes (sorted), for contract tests.
    pub const CODES: [&'static str; 5] = [
        "adapter.already_attached",
        "adapter.disposed",
        "adapter.double_mount",
        "adapter.not_attached",
        "adapter.not_mounted",
    ];

    /// Deterministic single-line JSON for the error timeline.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            "{{\"event\":\"adapter_misuse\",\"schema\":\"{}\",\"seq\":{},\"adapter_id\":\"{}\",\"adapter\":\"{}\",\"code\":\"{}\",\"action\":\"{}\",\"phase\":\"{}\",\"explanation\":\"{}\"}}",
            ADAPTER_SCHEMA_VERSION,
            self.seq,
            escape_json(&self.adapter_id),
            self.kind.as_str(),
            self.code,
            self.action,
            self.phase.as_str(),
            escape_json(self.explanation),
        )
    }
}

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ============================================================================
// The lifecycle state machine
// ============================================================================

/// Deterministic adapter lifecycle model.
#[derive(Debug, Clone)]
pub struct AdapterLifecycle {
    kind: AdapterKind,
    adapter_id: String,
    phase: AdapterPhase,
    seq: u64,
}

impl AdapterLifecycle {
    /// Create a fresh adapter in [`AdapterPhase::Created`].
    #[must_use]
    pub fn new(kind: AdapterKind, adapter_id: impl Into<String>) -> Self {
        Self {
            kind,
            adapter_id: adapter_id.into(),
            phase: AdapterPhase::Created,
            seq: 0,
        }
    }

    /// Current phase.
    #[must_use]
    pub const fn phase(&self) -> AdapterPhase {
        self.phase
    }

    /// Adapter flavor.
    #[must_use]
    pub const fn kind(&self) -> AdapterKind {
        self.kind
    }

    /// Apply a host action, returning the transition event or the misuse.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterMisuse`] when the action is illegal in the current
    /// phase for this adapter kind. React deduplicates StrictMode-style
    /// repeats of idempotent steps (mount when mounted, detach when detached,
    /// dispose when disposed); vanilla reports them as misuse.
    pub fn apply(&mut self, action: &AdapterAction) -> Result<AdapterEvent, AdapterMisuse> {
        self.seq += 1;
        let phase_before = self.phase;
        let react = self.kind == AdapterKind::React;

        let decided: Result<(AdapterPhase, AdapterOutcome), (&'static str, &'static str)> =
            match (action, self.phase) {
                // ── Mount ────────────────────────────────────────────
                (AdapterAction::Mount, AdapterPhase::Created) => {
                    Ok((AdapterPhase::Mounted, AdapterOutcome::Applied))
                }
                // Detached still has an INITIALIZED engine (attachClose only
                // closed the transport), so a second `init` is a double
                // mount, not a re-mount: React dedups it (mount already
                // satisfied, phase unchanged), vanilla reports misuse.
                (AdapterAction::Mount, AdapterPhase::Mounted | AdapterPhase::Detached) if react => {
                    Ok((self.phase, AdapterOutcome::StrictModeDeduped))
                }
                (
                    AdapterAction::Mount,
                    AdapterPhase::Mounted | AdapterPhase::Attached | AdapterPhase::Detached,
                ) => Err((
                    "adapter.double_mount",
                    "init was already called for this container; vanilla hosts must dispose \
                     before re-mounting (React hosts get StrictMode dedup instead)",
                )),
                // ── Attach ───────────────────────────────────────────
                (AdapterAction::Attach, AdapterPhase::Mounted | AdapterPhase::Detached) => {
                    Ok((AdapterPhase::Attached, AdapterOutcome::Applied))
                }
                (AdapterAction::Attach, AdapterPhase::Attached) if react => {
                    Ok((AdapterPhase::Attached, AdapterOutcome::StrictModeDeduped))
                }
                (AdapterAction::Attach, AdapterPhase::Attached) => Err((
                    "adapter.already_attached",
                    "attachConnect was already called; call attachClose before reconnecting",
                )),
                (AdapterAction::Attach, AdapterPhase::Created) => Err((
                    "adapter.not_mounted",
                    "call init (mount) before attachConnect: the engine must exist before \
                     the transport",
                )),
                // ── Resize ───────────────────────────────────────────
                // Legal from Mounted onward: fitToContainer/resize commonly
                // run between init and attachConnect.
                (
                    AdapterAction::Resize { .. },
                    AdapterPhase::Mounted | AdapterPhase::Attached | AdapterPhase::Detached,
                ) => Ok((self.phase, AdapterOutcome::Applied)),
                (AdapterAction::Resize { .. }, AdapterPhase::Created) => Err((
                    "adapter.not_mounted",
                    "resize requires an initialized engine; call init first",
                )),
                // ── Input ────────────────────────────────────────────
                (AdapterAction::Input { .. }, AdapterPhase::Attached) => {
                    Ok((AdapterPhase::Attached, AdapterOutcome::Applied))
                }
                (
                    AdapterAction::Input { .. },
                    AdapterPhase::Created | AdapterPhase::Mounted | AdapterPhase::Detached,
                ) => Err((
                    "adapter.not_attached",
                    "input requires a connected transport; call attachConnect first",
                )),
                // ── Detach ───────────────────────────────────────────
                (AdapterAction::Detach, AdapterPhase::Attached) => {
                    Ok((AdapterPhase::Detached, AdapterOutcome::Applied))
                }
                (AdapterAction::Detach, AdapterPhase::Detached | AdapterPhase::Mounted)
                    if react =>
                {
                    Ok((self.phase, AdapterOutcome::StrictModeDeduped))
                }
                (
                    AdapterAction::Detach,
                    AdapterPhase::Created | AdapterPhase::Mounted | AdapterPhase::Detached,
                ) => Err((
                    "adapter.not_attached",
                    "attachClose requires a connected transport",
                )),
                // ── Dispose ──────────────────────────────────────────
                // Teardown must be reachable from every live phase.
                (AdapterAction::Dispose, AdapterPhase::Disposed) if react => {
                    Ok((AdapterPhase::Disposed, AdapterOutcome::StrictModeDeduped))
                }
                (AdapterAction::Dispose, AdapterPhase::Disposed) => Err((
                    "adapter.disposed",
                    "destroy was already called; create a new adapter instead of reusing \
                     a disposed one",
                )),
                (AdapterAction::Dispose, _) => {
                    Ok((AdapterPhase::Disposed, AdapterOutcome::Applied))
                }
                // ── Anything on a disposed adapter ───────────────────
                (_, AdapterPhase::Disposed) => Err((
                    "adapter.disposed",
                    "the adapter was destroyed; create a new adapter instead of reusing \
                     a disposed one",
                )),
            };

        match decided {
            Ok((phase_after, outcome)) => {
                self.phase = phase_after;
                Ok(AdapterEvent {
                    seq: self.seq,
                    adapter_id: self.adapter_id.clone(),
                    kind: self.kind,
                    action: action.label(),
                    phase_before,
                    phase_after,
                    outcome,
                    detail: action.detail(),
                })
            }
            Err((code, explanation)) => Err(AdapterMisuse {
                seq: self.seq,
                adapter_id: self.adapter_id.clone(),
                kind: self.kind,
                code,
                action: action.label(),
                phase: phase_before,
                explanation,
            }),
        }
    }
}

// ============================================================================
// Recommended wiring
// ============================================================================

/// One ordered wiring step for an adapter integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringStep {
    /// 1-based order.
    pub step: u8,
    /// Stable contract method (or `"-"` for host-side-only steps).
    pub method: &'static str,
    /// What the step does and why it sits at this position.
    pub note: &'static str,
}

/// The recommended, ordered wiring for each adapter kind, naming the real
/// stable contract methods. This is the executable form of the "framework
/// wiring" section in the migration guide.
#[must_use]
pub fn recommended_wiring(kind: AdapterKind) -> Vec<WiringStep> {
    let mut steps = Vec::new();
    let mut order = 0u8;
    let mut push = |method: &'static str, note: &'static str| {
        order += 1;
        steps.push(WiringStep {
            step: order,
            method,
            note,
        });
    };

    if kind == AdapterKind::React {
        push(
            "-",
            "mark the component `'use client'` and guard on `typeof window` so Next.js \
             never runs engine code during SSR",
        );
    }
    push(
        "apiContract",
        "pin the contract: verify apiLine and a `1.` apiVersion before any other call",
    );
    push(
        "init",
        "initialize the engine into the container element (the Mount action)",
    );
    push(
        "fitToContainer",
        "size the grid to the container before first paint; wire a ResizeObserver to \
         keep it sized (fitToContainer again, or resize(cols, rows) when the host \
         computes the grid itself)",
    );
    push(
        "attachConnect",
        "connect the transport only after init succeeded (the Attach action)",
    );
    push(
        "input",
        "forward host-encoded input; input is only legal while attached",
    );
    push(
        "drainEventSubscriptionJsonl",
        "drain typed events on the host's schedule (drain-driven, not push-driven)",
    );
    push(
        "attachClose",
        "teardown step 1: close the transport (the Detach action)",
    );
    push(
        "destroy",
        "teardown step 2: destroy the engine (the Dispose action); React cleanup runs \
         this in the effect destructor and StrictMode may run it twice — the adapter \
         dedups the repeat",
    );
    steps
}

// ============================================================================
// Canonical examples (lockstep with crates/ftui-web/sdk/examples/)
// ============================================================================

/// Canonical vanilla-DOM adapter example. The committed
/// `sdk/examples/frankenterm-adapter-vanilla.js` must equal this exactly.
#[must_use]
pub fn vanilla_example() -> String {
    VANILLA_EXAMPLE.to_string()
}

/// Canonical React/Next adapter example. The committed
/// `sdk/examples/frankenterm-adapter-react.tsx` must equal this exactly.
#[must_use]
pub fn react_example() -> String {
    REACT_EXAMPLE.to_string()
}

const VANILLA_EXAMPLE: &str = r#"// FrankenTermJS first-party vanilla adapter (bd-2vr05.9.3).
// Lifecycle contract: mount -> attach -> resize/input -> detach -> dispose.
// Generated in lockstep with ftui-web's sdk_adapter model; do not hand-edit.

export function createFrankenTermAdapter(FrankenTermWeb, container, transportUrl) {
  // Step 1: pin the contract before any other call.
  const contract = FrankenTermWeb.apiContract();
  if (contract.apiLine !== "frankenterm-js" || !String(contract.apiVersion).startsWith("1.")) {
    throw new Error(`unsupported FrankenTermWeb contract: ${contract.apiVersion}`);
  }

  // Step 2 (Mount): initialize the engine into the container.
  const term = FrankenTermWeb.init(container);

  // Step 3: size to the container now, then keep sizing on changes.
  term.fitToContainer();
  const resizeObserver = new ResizeObserver(() => term.fitToContainer());
  resizeObserver.observe(container);

  // Step 4 (Attach): connect the transport only after init succeeded.
  term.attachConnect(transportUrl);

  // Step 5: forward host input (legal only while attached).
  const onKeyDown = (domEvent) => term.input(domEvent);
  container.addEventListener("keydown", onKeyDown);

  // Step 6: drain typed events on the host's schedule (drain-driven).
  const drainTimer = setInterval(() => {
    for (const line of term.drainEventSubscriptionJsonl()) {
      handleTerminalEvent(JSON.parse(line));
    }
  }, 16);

  // Teardown order matters: detach the transport, then destroy the engine.
  // Vanilla hosts must call dispose() exactly once.
  return {
    term,
    dispose() {
      clearInterval(drainTimer);
      container.removeEventListener("keydown", onKeyDown);
      resizeObserver.disconnect();
      term.attachClose(); // Detach
      term.destroy(); // Dispose
    },
  };
}

function handleTerminalEvent(event) {
  // Route by the typed taxonomy from sdk/frankenterm-js-events.d.ts.
  console.debug("frankenterm event", event.type, event);
}
"#;

const REACT_EXAMPLE: &str = r#"// FrankenTermJS first-party React adapter — also the Next.js wiring
// (bd-2vr05.9.3). Lifecycle contract: mount -> attach -> resize/input ->
// detach -> dispose, driven from a single effect. React StrictMode runs
// setup -> cleanup -> setup in development; because the cleanup below fully
// tears down the engine, the second setup starts from a clean container.
// (The adapter model additionally dedups repeated idempotent steps for
// hosts that keep one adapter instance across effect runs.)
// Generated in lockstep with ftui-web's sdk_adapter model; do not hand-edit.
"use client";

import { useEffect, useRef } from "react";

export function FrankenTerm({ FrankenTermWeb, transportUrl, onEvent }) {
  const containerRef = useRef(null);

  useEffect(() => {
    // Next.js SSR guard: engine code is browser-only.
    if (typeof window === "undefined" || !containerRef.current) {
      return undefined;
    }
    const container = containerRef.current;

    // Step 1: pin the contract before any other call.
    const contract = FrankenTermWeb.apiContract();
    if (contract.apiLine !== "frankenterm-js" || !String(contract.apiVersion).startsWith("1.")) {
      throw new Error(`unsupported FrankenTermWeb contract: ${contract.apiVersion}`);
    }

    // Step 2 (Mount): initialize the engine into the container.
    const term = FrankenTermWeb.init(container);

    // Step 3: size to the container now, then keep sizing on changes.
    term.fitToContainer();
    const resizeObserver = new ResizeObserver(() => term.fitToContainer());
    resizeObserver.observe(container);

    // Step 4 (Attach): connect the transport only after init succeeded.
    term.attachConnect(transportUrl);

    // Step 5: forward host input (legal only while attached).
    const onKeyDown = (domEvent) => term.input(domEvent);
    container.addEventListener("keydown", onKeyDown);

    // Step 6: drain typed events on the host's schedule (drain-driven).
    const drainTimer = setInterval(() => {
      for (const line of term.drainEventSubscriptionJsonl()) {
        onEvent?.(JSON.parse(line));
      }
    }, 16);

    // Effect cleanup IS the teardown: detach, then destroy. StrictMode runs
    // cleanup between its two dev-mode setups, so each setup gets a fresh
    // engine; the adapter model dedups repeats defensively.
    return () => {
      clearInterval(drainTimer);
      container.removeEventListener("keydown", onKeyDown);
      resizeObserver.disconnect();
      term.attachClose(); // Detach
      term.destroy(); // Dispose
    };
  }, [FrankenTermWeb, transportUrl, onEvent]);

  return <div ref={containerRef} style={{ width: "100%", height: "100%" }} />;
}
"#;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn lifecycle(kind: AdapterKind) -> AdapterLifecycle {
        AdapterLifecycle::new(kind, "term-1")
    }

    #[test]
    fn happy_path_reaches_disposed_for_both_kinds() {
        for kind in AdapterKind::ALL {
            let mut adapter = lifecycle(kind);
            for action in [
                AdapterAction::Mount,
                AdapterAction::Resize { cols: 80, rows: 24 },
                AdapterAction::Attach,
                AdapterAction::Input { bytes: 3 },
                AdapterAction::Resize {
                    cols: 100,
                    rows: 30,
                },
                AdapterAction::Detach,
                AdapterAction::Dispose,
            ] {
                let event = adapter
                    .apply(&action)
                    .unwrap_or_else(|m| panic!("{kind}: {} rejected: {}", action.label(), m.code));
                assert_eq!(event.outcome, AdapterOutcome::Applied);
            }
            assert_eq!(adapter.phase(), AdapterPhase::Disposed);
        }
    }

    #[test]
    fn react_strict_mode_double_mount_and_double_cleanup_are_deduped() {
        let mut adapter = lifecycle(AdapterKind::React);
        assert_eq!(
            adapter.apply(&AdapterAction::Mount).expect("mount").outcome,
            AdapterOutcome::Applied
        );
        // StrictMode re-runs the effect body.
        assert_eq!(
            adapter
                .apply(&AdapterAction::Mount)
                .expect("strict remount")
                .outcome,
            AdapterOutcome::StrictModeDeduped
        );
        adapter.apply(&AdapterAction::Attach).expect("attach");
        assert_eq!(
            adapter
                .apply(&AdapterAction::Attach)
                .expect("strict reattach")
                .outcome,
            AdapterOutcome::StrictModeDeduped
        );
        adapter.apply(&AdapterAction::Detach).expect("detach");
        // StrictMode re-runs the cleanup.
        assert_eq!(
            adapter
                .apply(&AdapterAction::Detach)
                .expect("strict re-detach")
                .outcome,
            AdapterOutcome::StrictModeDeduped
        );
        adapter.apply(&AdapterAction::Dispose).expect("dispose");
        assert_eq!(
            adapter
                .apply(&AdapterAction::Dispose)
                .expect("strict re-dispose")
                .outcome,
            AdapterOutcome::StrictModeDeduped
        );
        assert_eq!(adapter.phase(), AdapterPhase::Disposed);
    }

    #[test]
    fn vanilla_rejects_the_same_repeats_react_dedups() {
        let mut adapter = lifecycle(AdapterKind::Vanilla);
        adapter.apply(&AdapterAction::Mount).expect("mount");
        let misuse = adapter
            .apply(&AdapterAction::Mount)
            .expect_err("double mount");
        assert_eq!(misuse.code, "adapter.double_mount");

        adapter.apply(&AdapterAction::Attach).expect("attach");
        let misuse = adapter
            .apply(&AdapterAction::Attach)
            .expect_err("double attach");
        assert_eq!(misuse.code, "adapter.already_attached");

        adapter.apply(&AdapterAction::Detach).expect("detach");
        adapter.apply(&AdapterAction::Dispose).expect("dispose");
        let misuse = adapter
            .apply(&AdapterAction::Dispose)
            .expect_err("double dispose");
        assert_eq!(misuse.code, "adapter.disposed");
    }

    #[test]
    fn ordering_violations_are_named_misuse() {
        // Attach before mount.
        let mut adapter = lifecycle(AdapterKind::Vanilla);
        assert_eq!(
            adapter
                .apply(&AdapterAction::Attach)
                .expect_err("no mount")
                .code,
            "adapter.not_mounted"
        );
        // Input before attach.
        adapter.apply(&AdapterAction::Mount).expect("mount");
        assert_eq!(
            adapter
                .apply(&AdapterAction::Input { bytes: 1 })
                .expect_err("no attach")
                .code,
            "adapter.not_attached"
        );
        // Resize before mount.
        let mut fresh = lifecycle(AdapterKind::React);
        assert_eq!(
            fresh
                .apply(&AdapterAction::Resize { cols: 1, rows: 1 })
                .expect_err("no mount")
                .code,
            "adapter.not_mounted"
        );
        // Anything after dispose (React dedups only dispose itself).
        let mut disposed = lifecycle(AdapterKind::React);
        disposed.apply(&AdapterAction::Mount).expect("mount");
        disposed.apply(&AdapterAction::Dispose).expect("dispose");
        assert_eq!(
            disposed
                .apply(&AdapterAction::Mount)
                .expect_err("mount after dispose")
                .code,
            "adapter.disposed"
        );
    }

    #[test]
    fn dispose_is_reachable_from_every_live_phase() {
        for kind in AdapterKind::ALL {
            // From Created.
            let mut a = lifecycle(kind);
            a.apply(&AdapterAction::Dispose)
                .expect("dispose from created");
            // From Mounted.
            let mut b = lifecycle(kind);
            b.apply(&AdapterAction::Mount).expect("mount");
            b.apply(&AdapterAction::Dispose)
                .expect("dispose from mounted");
            // From Attached.
            let mut c = lifecycle(kind);
            c.apply(&AdapterAction::Mount).expect("mount");
            c.apply(&AdapterAction::Attach).expect("attach");
            c.apply(&AdapterAction::Dispose)
                .expect("dispose from attached");
        }
    }

    #[test]
    fn mount_while_detached_is_a_double_mount_not_a_remount() {
        // Detached keeps the engine initialized; a second init must never be
        // silently accepted as a fresh mount.
        let mut vanilla = lifecycle(AdapterKind::Vanilla);
        vanilla.apply(&AdapterAction::Mount).expect("mount");
        vanilla.apply(&AdapterAction::Attach).expect("attach");
        vanilla.apply(&AdapterAction::Detach).expect("detach");
        let misuse = vanilla
            .apply(&AdapterAction::Mount)
            .expect_err("init on a detached-but-initialized engine");
        assert_eq!(misuse.code, "adapter.double_mount");
        assert_eq!(vanilla.phase(), AdapterPhase::Detached, "phase unchanged");

        let mut react = lifecycle(AdapterKind::React);
        react.apply(&AdapterAction::Mount).expect("mount");
        react.apply(&AdapterAction::Attach).expect("attach");
        react.apply(&AdapterAction::Detach).expect("detach");
        let event = react
            .apply(&AdapterAction::Mount)
            .expect("react dedups the repeat");
        assert_eq!(event.outcome, AdapterOutcome::StrictModeDeduped);
        assert_eq!(react.phase(), AdapterPhase::Detached, "dedup keeps phase");
    }

    #[test]
    fn remount_after_detach_supports_reconnect_flows() {
        let mut adapter = lifecycle(AdapterKind::Vanilla);
        adapter.apply(&AdapterAction::Mount).expect("mount");
        adapter.apply(&AdapterAction::Attach).expect("attach");
        adapter.apply(&AdapterAction::Detach).expect("detach");
        // Reconnect without re-initializing the engine.
        adapter.apply(&AdapterAction::Attach).expect("re-attach");
        assert_eq!(adapter.phase(), AdapterPhase::Attached);
    }

    #[test]
    fn jsonl_lines_are_deterministic_and_carry_correlation() {
        let run = || -> Vec<String> {
            let mut adapter = AdapterLifecycle::new(AdapterKind::React, "corr-7");
            let mut lines = Vec::new();
            for action in [
                AdapterAction::Mount,
                AdapterAction::Attach,
                AdapterAction::Input { bytes: 5 },
                AdapterAction::Dispose,
            ] {
                lines.push(adapter.apply(&action).expect("apply").to_jsonl());
            }
            lines.push(
                adapter
                    .apply(&AdapterAction::Attach)
                    .expect_err("attach after dispose")
                    .to_jsonl(),
            );
            lines
        };
        let a = run();
        let b = run();
        assert_eq!(a, b, "identical action sequences must log byte-identically");
        for line in &a {
            let parsed: serde_json::Value =
                serde_json::from_str(line).expect("adapter JSONL parses");
            assert_eq!(parsed["adapter_id"].as_str(), Some("corr-7"));
            assert_eq!(parsed["schema"].as_str(), Some(ADAPTER_SCHEMA_VERSION));
            assert!(parsed["seq"].as_u64().is_some());
        }
        // seq is monotone across accepted AND rejected actions.
        let seqs: Vec<u64> = a
            .iter()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).expect("parses")["seq"]
                    .as_u64()
                    .expect("seq")
            })
            .collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1));
    }

    #[test]
    fn misuse_codes_are_stable_sorted_and_exercised() {
        let mut sorted = AdapterMisuse::CODES;
        sorted.sort_unstable();
        assert_eq!(sorted, AdapterMisuse::CODES, "codes must stay sorted");
    }

    #[test]
    fn wiring_references_only_stable_contract_methods() {
        for kind in AdapterKind::ALL {
            let wiring = recommended_wiring(kind);
            assert!(wiring.len() >= 8, "wiring must cover the full lifecycle");
            for step in &wiring {
                assert!(
                    step.method == "-" || WIRING_CONTRACT_METHODS.contains(&step.method),
                    "wiring step {} references non-contract method `{}`",
                    step.step,
                    step.method
                );
            }
            // Order is 1..=n with no gaps.
            for (idx, step) in wiring.iter().enumerate() {
                assert_eq!(usize::from(step.step), idx + 1);
            }
            // Teardown order: attachClose strictly before destroy.
            let pos = |m: &str| wiring.iter().position(|s| s.method == m).expect(m);
            assert!(pos("init") < pos("attachConnect"));
            assert!(pos("attachConnect") < pos("attachClose"));
            assert!(pos("attachClose") < pos("destroy"));
        }
        // React wiring leads with the SSR guard.
        assert_eq!(recommended_wiring(AdapterKind::React)[0].method, "-");
    }

    #[test]
    fn examples_embed_every_wired_contract_method() {
        for (example, kind) in [
            (vanilla_example(), AdapterKind::Vanilla),
            (react_example(), AdapterKind::React),
        ] {
            for step in recommended_wiring(kind) {
                if step.method != "-" {
                    assert!(
                        example.contains(step.method),
                        "{kind} example is missing wired method `{}`",
                        step.method
                    );
                }
            }
        }
        // The React example must carry the Next.js markers.
        let react = react_example();
        assert!(react.contains("\"use client\""));
        assert!(react.contains("typeof window"));
        assert!(react.contains("StrictMode"));
    }

    #[test]
    fn committed_example_files_are_in_lockstep_with_the_generators() {
        let vanilla_committed = include_str!("../sdk/examples/frankenterm-adapter-vanilla.js");
        assert_eq!(
            vanilla_committed,
            vanilla_example(),
            "sdk/examples/frankenterm-adapter-vanilla.js drifted from sdk_adapter::vanilla_example()"
        );
        let react_committed = include_str!("../sdk/examples/frankenterm-adapter-react.tsx");
        assert_eq!(
            react_committed,
            react_example(),
            "sdk/examples/frankenterm-adapter-react.tsx drifted from sdk_adapter::react_example()"
        );
    }
}
