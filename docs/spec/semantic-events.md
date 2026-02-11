# Semantic Events - High-Level Input Abstraction

This spec defines the semantic (gesture-level) event system for FrankenTUI.
It establishes the API surface, invariants, and test plan for converting raw
terminal events into consistent, high-level input signals.

For the browser SDK (`FrankenTermWeb`) host-facing event taxonomy and strict
ordering contract, see `docs/spec/frankenterm-web-api.md`.

---

## 1) Goals
- Provide consistent gesture detection across widgets.
- Ensure deterministic output given an identical input stream.
- Keep per-event processing O(1) with no allocations on the hot path.
- Preserve raw events for low-level consumers.

## 2) Non-Goals
- Full gesture customization UI (config only).
- Per-widget custom gesture pipelines.
- Touch-only gestures on terminals that do not report them.

---

## 3) Architecture Overview

Raw input flow:

```
TerminalSession -> Event (ftui-core) -> GestureRecognizer (ftui-runtime)
   -> SemanticEvent(s) -> Model update
```

Notes:
- `Event` remains the canonical raw input type (ftui-core).
- `SemanticEvent` is a separate stream derived from raw events.
- Runtime forwards **both** raw and semantic events to models.

---

## 4) Core Types (Proposed)

### 4.1 SemanticEvent (ftui-core)
```rust
pub enum SemanticEvent {
    // Mouse gestures
    Click { pos: Point, button: MouseButton },
    DoubleClick { pos: Point, button: MouseButton },
    TripleClick { pos: Point, button: MouseButton },
    LongPress { pos: Point, button: MouseButton, duration: Duration },

    // Drag gestures
    DragStart { pos: Point, button: MouseButton },
    DragMove { from: Point, to: Point, delta: (i16, i16) },
    DragEnd { from: Point, to: Point },
    DragCancel,

    // Keyboard gestures
    Chord { sequence: Vec<KeyEvent> },

    // Optional / future
    Swipe { direction: Direction, distance: u16, velocity: f32 },
}
```

### 4.2 Point (ftui-core)
```rust
pub struct Point { pub x: u16, pub y: u16 }
```

### 4.3 GestureConfig (ftui-runtime)
```rust
pub struct GestureConfig {
    pub double_click_timeout: Duration, // default 300ms
    pub long_press_threshold: Duration, // default 500ms
    pub drag_threshold: u16,            // default 3 cells
    pub chord_timeout: Duration,        // default 1000ms
    pub enable_swipe: bool,             // default false
}
```

### 4.4 GestureRecognizer (ftui-runtime)
```rust
pub struct GestureRecognizer { /* state */ }

impl GestureRecognizer {
    pub fn process(&mut self, event: Event) -> SmallVec<[SemanticEvent; 2]>;
    pub fn reset(&mut self);
    pub fn is_dragging(&self) -> bool;
}
```

---

## 5) Recognition Rules

### 5.1 Click / Double / Triple
- Mouse down + up within the same cell is a Click.
- Two clicks within `double_click_timeout` and same cell -> DoubleClick.
- Three clicks within timeout -> TripleClick.
- Any movement beyond drag threshold cancels click detection.

### 5.2 Long Press
- Mouse down held longer than `long_press_threshold` without movement.
- Emits LongPress once; subsequent release does not emit Click.

### 5.3 Drag
- Mouse down + movement >= `drag_threshold` -> DragStart.
- Subsequent moves emit DragMove with delta.
- Mouse up -> DragEnd.
- Resize or focus loss during drag -> DragCancel.

### 5.4 Chord
- Sequence of KeyEvents within `chord_timeout` produces a Chord.
- Sequence resets on timeout or on non-key event.

---

## 6) Invariants
I1. Determinism: semantic output is a pure function of event stream + config.
I2. No allocations on the hot path (SmallVec allowed, fixed capacity).
I3. No gesture emits both Click and Drag for the same interaction.
I4. Semantic events are ordered after the raw event that caused them.
I5. Cancel events (DragCancel) always precede any new drag start.

---

## 7) Capability Tiers
- Tier 0: semantic events disabled (raw only).
- Tier 1: click + drag only.
- Tier 2: click + drag + long press.
- Tier 3: full (click, drag, long press, chord, swipe).

Tier selection is deterministic based on runtime config and platform support.

---

## 8) Evidence Ledger (Explainability)
For each recognized gesture, log:
- input sequence summary (count, duration, positions)
- decision rule applied (thresholds)
- reason for emit or cancel
- if a probabilistic rule is used, log its score (log-likelihood or Bayes factor)

---

## 9) Failure Modes
- False positives: drag emitted when user intended click.
- Missed gestures: long-press or chord timeout too aggressive.
- Ordering glitches: semantic events emitted out of order vs raw input.
- Performance drift: recognizer adds noticeable latency under load.

---

## 10) Performance & Optimization Protocol
- Baseline p50/p95/p99 recognition latency with `hyperfine`.
- Profile CPU + allocations; identify top 5 hotspots.
- Opportunity matrix; implement only Score >= 2.0 (Impact x Confidence / Effort).
- Isomorphism proof: deterministic ordering + golden checksums.

---

## 11) Test Plan

### Unit Tests
- Double click timing boundaries.
- Long press threshold behavior.
- Drag threshold and cancel on resize/focus loss.
- Chord timeout boundaries.

### Property Tests
- Determinism across repeated runs with same event stream.
- No overlap of Click + Drag for the same interaction.

### Fuzz Tests
- Random event streams should not panic or violate invariants.

### E2E PTY
- Simulated mouse click/drag sequences with JSONL logs.
- Keyboard chord sequences and timeouts.

---

## 12) JSONL Logging Fields (Required)
- ts_ms
- raw_event
- semantic_event
- pos / delta
- thresholds (drag, double-click, long-press, chord)
- decision (emit, cancel, ignore)
- duration_ms

---

## 13) Integration Notes
- `SemanticEvent` lives in ftui-core for shared types.
- `GestureRecognizer` lives in ftui-runtime (stateful, time-based).
- Models can opt-in to semantic events or continue using raw events.
