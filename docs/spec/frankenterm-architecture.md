# FrankenTerm + ftui-web: North Star Architecture

> bd-lff4p.6 — Definitive system design for the full custom terminal stack.

Status: DRAFT
Authors: ChartreuseStream (claude-code / opus-4.6)
Date: 2026-02-08

---

## 1. Vision

Replace **both** Crossterm (native) and xterm.js (web) with a first-party, Rust-first terminal engine so we control correctness, latency, and UX end-to-end.

Three deployment targets from one codebase:

| Target | Engine | Renderer | Transport |
|--------|--------|----------|-----------|
| **Native terminal** | ftui-tty (replaces Crossterm) | ANSI Presenter (existing) | Direct stdout |
| **Web (client-side)** | frankenterm-core → frankenterm-web | WebGPU glyph atlas | N/A (in-browser) |
| **Web (remote)** | frankenterm-core → PTY bridge | WebGPU glyph atlas | WebSocket |

---

## 2. Crate Map

### 2.1 New Crates

```
frankenterm-core          Terminal data model (grid, scrollback, cursor, modes, VT parser)
frankenterm-web           WebGPU renderer + web input (wasm-bindgen, glyph atlas)
ftui-tty                  Unix-first native backend (raw mode, input, output, signals)
ftui-web                  WASM-compiled ftui runtime (step-based Program runner)
ftui-remote               WebSocket PTY bridge + terminal reply engine
```

### 2.2 Existing Crates (unchanged core)

```
ftui-core                 Geometry, events, capabilities (REMOVE crossterm dep)
ftui-render               Buffer, Diff, Presenter, Frame (host-agnostic core stays pure)
ftui-layout               Constraint solvers (already host-agnostic)
ftui-text                 Spans, wrapping, bidi (already host-agnostic)
ftui-style                Color, theme tokens (already host-agnostic)
ftui-widgets              Widget rendering (already host-agnostic)
ftui-runtime              Model/Cmd/Subscription (backend-abstracted via trait)
ftui-extras               Feature-gated add-ons (diagram, canvas, VFX, etc.)
ftui                      Public facade (re-exports; gains feature gates for web/native)
```

### 2.3 Dependency Graph

```
                    ┌─────────────────────────────────────────────┐
                    │            HOST-AGNOSTIC CORE               │
                    │                                             │
                    │  ftui-core  ftui-render  ftui-layout        │
                    │  ftui-text  ftui-style   ftui-widgets       │
                    │  ftui-extras             ftui-runtime       │
                    └────────┬───────────┬───────────┬────────────┘
                             │           │           │
               ┌─────────────┘           │           └──────────────┐
               ▼                         ▼                          ▼
        ┌─────────────┐          ┌──────────────┐          ┌──────────────┐
        │  ftui-tty   │          │  ftui-web    │          │ ftui-remote  │
        │  (native)   │          │  (wasm)      │          │ (websocket)  │
        └─────────────┘          └──────┬───────┘          └──────┬───────┘
               │                        │                         │
               │                        ▼                         ▼
               │                ┌──────────────┐          ┌──────────────┐
               │                │frankenterm   │          │frankenterm   │
               │                │  -web        │          │  -core       │
               │                │(WebGPU+input)│          │(VT engine)   │
               │                └──────────────┘          └──────────────┘
               ▼
          Native Terminal
```

---

## 3. Backend Trait: Replacing Crossterm

### 3.1 Current State

Crossterm is used in exactly **two** source files in ftui-core:

- `terminal_session.rs` — raw mode, alt-screen, mouse, paste, focus, cleanup
- `event.rs` — `From<crossterm::event::*>` conversions for Event/KeyEvent/MouseEvent

The runtime (`ftui-runtime`) accesses crossterm **only** through `TerminalSession`'s
`poll_event()` and `read_event()` methods. This is an excellent abstraction boundary.

### 3.2 New Backend Trait

```rust
// ftui-core/src/backend.rs (new)

/// Terminal backend abstraction.
///
/// Implementations provide platform-specific terminal I/O.
/// The runtime interacts exclusively through this trait.
pub trait TerminalBackend: Send {
    /// Poll for input events with timeout.
    /// Returns true if an event is available.
    fn poll_event(&self, timeout: Duration) -> io::Result<bool>;

    /// Read the next terminal event (non-blocking if poll returned true).
    fn read_event(&mut self) -> io::Result<Option<Event>>;

    /// Query current terminal size (columns, rows).
    fn size(&self) -> io::Result<(u16, u16)>;

    /// Write raw bytes to the terminal output.
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;

    /// Flush buffered output.
    fn flush(&mut self) -> io::Result<()>;

    /// Enter raw mode with the given options.
    fn enter_raw_mode(&mut self, opts: &SessionOptions) -> io::Result<()>;

    /// Exit raw mode and restore terminal state.
    fn exit_raw_mode(&mut self) -> io::Result<()>;
}
```

### 3.3 Backend Implementations

| Implementation | Crate | Target |
|---------------|-------|--------|
| `CrosstermBackend` | ftui-core (temporary, behind feature gate) | Migration period |
| `NativeBackend` | ftui-tty | Unix (Linux + macOS) |
| `WasmBackend` | ftui-web | wasm32-unknown-unknown |
| `NullBackend` | ftui-core | Testing / headless |

### 3.4 Migration Path

1. **Phase 1**: Extract `TerminalBackend` trait; wrap existing crossterm code as `CrosstermBackend`
2. **Phase 2**: Build `NativeBackend` in ftui-tty (Unix-first)
3. **Phase 3**: Build `WasmBackend` in ftui-web
4. **Phase 4**: Remove crossterm dependency (default to ftui-tty on Unix)

Crossterm stays as a feature-gated fallback during transition. No breaking changes to user-facing API.

---

## 4. Data Flows

### 4.1 Native Path (ftui-tty)

```
User Input
  │
  ▼
ftui-tty: read raw bytes from stdin (poll + non-blocking read)
  │
  ▼
ftui-core: InputParser converts byte sequences → Event
  │                                                    ┌──────────────┐
  ▼                                                    │ Subscriptions│
ftui-runtime: Model.update(Event) → (Model', Cmd)  ◄──┤ (tick, IO,   │
  │                                                    │  file watch) │
  ▼                                                    └──────────────┘
ftui-runtime: Model.view(&Frame)
  │
  ▼
ftui-render: Buffer (back) ← widgets write cells
  │
  ▼
ftui-render: BufferDiff::compute(front, back) → ChangeRuns
  │
  ▼
ftui-render: Presenter.emit(diff) → ANSI byte stream
  │
  ▼
ftui-tty: write ANSI bytes to stdout (buffered, sync-bracketed)
  │
  ▼
Terminal Display
```

### 4.2 Web Client-Side Path (ftui-web + frankenterm-web)

```
DOM Events (keyboard, mouse, touch, resize)
  │
  ▼
frankenterm-web: JS input capture → encode as Event
  │
  ▼
ftui-web: WasmApp.input(Event) — step-based, no blocking poll
  │
  ▼
ftui-runtime: Model.update(Event) → (Model', Cmd)
  │
  ▼
ftui-runtime: Model.view(&Frame)
  │
  ▼
ftui-render: Buffer → BufferDiff → patch list (NOT ANSI — raw cell diffs)
  │
  ▼
frankenterm-web: apply patch to GPU cell buffer
  │
  ▼
frankenterm-web: WebGPU render pass (glyph atlas + instanced quads)
  │
  ▼
Canvas / Browser Display
```

Key difference: **no ANSI emission on the web path**. The Presenter is bypassed; raw cell diffs
feed directly into the WebGPU renderer.

### 4.3 Web Remote Path (ftui-remote + frankenterm-core)

```
Server Side:                              Client Side:
┌─────────────────┐                       ┌─────────────────────┐
│  Application    │                       │  frankenterm-web    │
│  (runs on host) │                       │  (runs in browser)  │
│       │         │                       │       ▲             │
│       ▼         │                       │       │             │
│  PTY (spawned   │  WebSocket (binary)   │  frankenterm-core   │
│   subprocess)   │ ◄──────────────────►  │  (VT parser, grid)  │
│       │         │  ● ANSI stream →      │       │             │
│       ▼         │  ● input events ←     │       ▼             │
│  ftui-remote    │  ● resize signals ←   │  WebGPU renderer   │
│  (bridge)       │  ● terminal queries   │                     │
└─────────────────┘                       └─────────────────────┘
```

---

## 5. Crate Specifications

### 5.1 frankenterm-core

**Purpose**: Terminal data model and VT/ANSI parser engine.

**Owns**:
- Grid (cells, lines, wide-char handling, dirty tracking)
- Scrollback buffer (ring buffer, configurable depth)
- Cursor (position, style, visibility, save/restore stack)
- Terminal modes (origin, autowrap, insert/replace, DEC private modes)
- VT/ANSI state machine parser (ground, escape, CSI, OSC, DCS, APC states)
- Selection model (block/linear, copy extraction)
- Hyperlink registry (OSC 8)
- Incremental patch API (dirty spans → diff runs for renderer)

**Does NOT own**:
- Rendering (no WebGPU, no ANSI emission)
- IO (no sockets, no file handles)
- Platform-specific code

**Key types**:

```rust
pub struct TerminalGrid {
    width: u16,
    height: u16,
    cells: Vec<Cell>,          // Reuse ftui-render Cell (16 bytes)
    dirty_rows: BitVec,
    scrollback: ScrollbackBuffer,
    cursor: CursorState,
    modes: TerminalModes,
}

pub struct VtParser {
    state: ParserState,        // DEC-compatible state machine
    params: ParamBuffer,
    intermediates: [u8; 4],
    osc_buffer: Vec<u8>,
}

pub struct TerminalPatch {
    changed_rows: Vec<(u16, Vec<CellRun>)>,
    cursor_move: Option<(u16, u16)>,
    scroll_delta: i32,
}
```

**Invariants**:
- Grid dimensions immutable after creation (resize = new grid + reflow)
- Parser state machine is total (every byte produces a valid transition)
- Cursor always within grid bounds (clamped, never panics)
- Cell type is identical to ftui-render Cell (16 bytes, no conversion)

### 5.2 frankenterm-web

**Purpose**: WebGPU terminal renderer + web input stack, compiled to WASM.

**Owns**:
- Glyph rasterization + atlas cache (monospace-first, SDF or bitmap)
- WebGPU render pipeline (instanced quads, one draw call per frame)
- Cursor blinking + selection highlight rendering
- OSC 8 hyperlink hover/click detection
- DOM input capture (keyboard, mouse, wheel, touch, clipboard, IME)
- DPI/zoom awareness and fit-to-container
- Accessibility layer (DOM mirror for screen readers, high-contrast mode)

**Key API** (exported via wasm-bindgen):

```typescript
// TypeScript-facing API
interface FrankenTermWeb {
  init(canvas: HTMLCanvasElement, options?: TermOptions): Promise<void>;
  resize(cols: number, rows: number): void;
  input(event: InputEvent): void;     // Keyboard/mouse from DOM
  feed(data: Uint8Array): void;       // ANSI byte stream (remote mode)
  applyPatch(patch: CellPatch): void; // Cell diff (client-side ftui mode)
  render(): void;                     // Request frame
  destroy(): void;
}
```

**Invariants**:
- No partial frames visible (double-buffered WebGPU presentation)
- Glyph atlas never exceeds configured memory budget (LRU eviction)
- Input events never dropped (queued if renderer is busy)

### 5.3 ftui-tty

**Purpose**: Unix-first native terminal backend replacing Crossterm.

**Owns**:
- Raw mode enter/exit (termios manipulation)
- Terminal feature toggles (alt-screen, mouse, bracketed paste, focus, kitty keyboard)
- Input byte reader (poll-based, non-blocking)
- Resize detection (SIGWINCH handler)
- Terminal size queries (ioctl TIOCGWINSZ)
- Output buffering and sync bracket emission
- RAII cleanup (Drop-based, panic-safe)

**Does NOT own**:
- Event parsing (delegates to ftui-core InputParser)
- ANSI emission (delegates to ftui-render Presenter)

**Key types**:

```rust
pub struct NativeBackend {
    original_termios: Termios,
    features: EnabledFeatures,  // Bitflags for cleanup tracking
    input_fd: RawFd,            // stdin
    output: BufWriter<Stdout>,
}

impl TerminalBackend for NativeBackend { ... }
impl Drop for NativeBackend { ... }     // Guaranteed cleanup
```

**Invariants**:
- Terminal state always restored on Drop (raw mode, cursor, alt-screen)
- No leaked file descriptors
- Signal handlers installed/removed atomically with raw mode

### 5.4 ftui-web

**Purpose**: Compile ftui runtime to WASM with a step-based event loop.

**Owns**:
- `WasmApp` wrapper (no blocking poll, no threads)
- Step-based Program runner (JS calls `tick()` via requestAnimationFrame)
- Event translation (DOM → ftui Event)
- Buffer → patch extraction (bypass ANSI Presenter, emit raw cell diffs)
- Time source abstraction (performance.now() on web, Instant on native)

**Key API**:

```rust
// Compiled to WASM
#[wasm_bindgen]
pub struct WasmApp {
    model: Box<dyn AnyModel>,
    buffer_front: Buffer,
    buffer_back: Buffer,
}

#[wasm_bindgen]
impl WasmApp {
    pub fn new(width: u16, height: u16) -> Self;
    pub fn input(&mut self, event_json: &str);
    pub fn tick(&mut self, dt_ms: f64) -> JsValue;  // Returns cell patches
    pub fn resize(&mut self, width: u16, height: u16);
    pub fn render(&self) -> JsValue;                 // Full buffer snapshot
}
```

**Invariants**:
- No blocking calls (no `std::thread`, no `poll()`, no `sleep()`)
- Deterministic given same event sequence + time steps
- Memory budget capped (no unbounded allocations per frame)

### 5.5 ftui-remote

**Purpose**: WebSocket PTY bridge for remote terminal sessions.

**Owns**:
- PTY spawn + management (fork/exec, resize, signal forwarding)
- WebSocket server (binary frames, configurable compression)
- Terminal query/reply engine (DSR, DA1/DA2, DEC queries)
- Session lifecycle (auth, connect, disconnect, reconnect)
- Backpressure policy (flow control when client is slow)

**Non-negotiable**:
- Explicit threat model (origin checks, auth tokens, rate limits)
- No accidental command execution (PTY only started with explicit user action)
- Encrypted transport (WSS only in production)

---

## 6. Determinism Strategy

### 6.1 Principles

- **Seeded demos**: Fixed RNG seed + explicit time source → reproducible output
- **Record/replay**: Capture input events + timestamps; replay produces identical frames
- **Golden traces**: Frame checksums stored in version-controlled registry

### 6.2 Time Source Abstraction

```rust
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn elapsed_since(&self, earlier: Instant) -> Duration;
}

pub struct SystemClock;         // Real time (native default)
pub struct SteppedClock {       // Deterministic (test/replay)
    current: AtomicU64,         // Nanoseconds since epoch
}
pub struct JsClock;             // performance.now() (WASM)
```

All timers, animations, and schedulers take `&dyn Clock` instead of calling
`Instant::now()` directly.

### 6.3 Golden Trace Format

Golden traces are stored as JSONL bundles (`trace.jsonl` + optional sidecar payloads).
The canonical spec lives in:
- `docs/spec/frankenterm-golden-trace-format.md`

Minimal example (JSONL; one object per line):

```jsonl
{"schema_version":"golden-trace-v1","event":"trace_header","run_id":"trace-2026-02-08-abc123","git_sha":"<git sha>","seed":42,"env":{"target":"native","os":"linux","term":"xterm-256color"},"profile":"modern"}
{"schema_version":"golden-trace-v1","event":"resize","ts_ns":0,"cols":120,"rows":40}
{"schema_version":"golden-trace-v1","event":"input","ts_ns":16000000,"kind":"key","code":"a","mods":0}
{"schema_version":"golden-trace-v1","event":"frame","frame_idx":0,"ts_ns":16000000,"hash_algo":"sha256","frame_hash":"a1b2c3...","cells_changed":42}
{"schema_version":"golden-trace-v1","event":"tick","ts_ns":32000000}
{"schema_version":"golden-trace-v1","event":"frame","frame_idx":1,"ts_ns":32000000,"hash_algo":"sha256","frame_hash":"d4e5f6...","cells_changed":7}
{"schema_version":"golden-trace-v1","event":"trace_summary","total_frames":2,"final_checksum_chain":"..."}
```

---

## 7. Safety Constraints

### 7.1 Rust Safety

- `#![forbid(unsafe_code)]` in all new crates (frankenterm-core, ftui-tty, ftui-web, ftui-remote)
- Dependencies may use unsafe internally (wgpu, wasm-bindgen, nix/rustix)
- No `transmute`, no raw pointer arithmetic in our code

### 7.2 Panic Discipline

- Backends must not panic on I/O errors (return `Result`)
- Grid operations use saturating arithmetic for coordinates
- Parser state machine is total (invalid bytes → no-op or error recovery, never panic)
- WASM: panics become JS exceptions via wasm-bindgen (acceptable but log clearly)

### 7.3 Terminal State Restoration

- RAII via `Drop` on backend types (raw mode, cursor, alt-screen)
- Signal handlers (SIGINT, SIGTERM) trigger orderly cleanup before exit
- `panic = "abort"` in release profile means Drop won't run on panic →
  register `atexit` handler as belt-and-suspenders for native backend

### 7.4 Memory Safety

- No unbounded allocations in the render hot path
- Scrollback buffer has configurable max depth (default 10,000 lines)
- Glyph atlas has LRU eviction with configurable memory budget
- WebSocket bridge enforces per-message size limits

---

## 8. Performance Budgets

### 8.1 Native (ftui-tty)

| Metric | Budget | Measurement |
|--------|--------|-------------|
| Keystroke-to-frame | < 8ms (p95) | PTY harness + timestamp logging |
| Full-frame diff (120x40) | < 50us | `cargo bench -p ftui-render` |
| Identical-frame skip | < 2us | Dirty-row fast path |
| Raw mode enter/exit | < 1ms | Benchmark in ftui-tty |
| Syscalls per frame | <= 2 (1 read + 1 writev) | `strace -c` profiling |

### 8.2 Web (frankenterm-web)

| Metric | Budget | Measurement |
|--------|--------|-------------|
| Steady-state frame time (120x40) | < 16ms (60fps) | Browser performance.measure() |
| Glyph atlas initialization | < 100ms | First-frame timing |
| WASM module load + init | < 200ms | Navigation timing |
| Memory (steady state) | < 50MB | performance.memory |
| Input-to-frame latency | < 16ms (one frame) | requestAnimationFrame timing |

### 8.3 Remote (ftui-remote)

| Metric | Budget | Measurement |
|--------|--------|-------------|
| Keystroke-to-photon (LAN) | < 50ms (p95) | Round-trip timestamp in JSONL |
| Keystroke-to-photon (WAN) | < 150ms (p95) | Same, with simulated latency |
| WebSocket frame overhead | < 4 bytes/cell | Binary frame size measurement |
| Reconnect time | < 2s | Session resumption benchmark |

---

## 9. Invariants

### 9.1 Universal (all targets)

1. **Cell identity**: ftui-render `Cell` (16 bytes) is the canonical cell type everywhere.
   No conversion between native/web cell representations.
2. **Buffer correctness**: `BufferDiff::compute(old, new)` produces exactly the set of
   changed cells. This holds regardless of backend.
3. **Model purity**: `Model::update()` and `Model::view()` never perform I/O.
   Side effects go through `Cmd`.
4. **Deterministic replay**: Given identical (events, clock, seed), output is byte-identical.

### 9.2 Native-specific

5. **One-writer rule**: Only `TerminalWriter` may write to stdout while active.
6. **Terminal restoration**: On any exit path (return, panic, signal), terminal state
   is restored to pre-session values.
7. **Signal safety**: Signal handlers perform only async-signal-safe operations.

### 9.3 Web-specific

8. **No blocking**: WASM execution never blocks the main thread.
9. **Frame atomicity**: No partial frames rendered (WebGPU double-buffering).
10. **Memory bounded**: Steady-state allocations constant per frame (no per-frame Vec growth).

---

## 10. Failure Modes and Recovery

| Failure | Detection | Recovery | Fallback |
|---------|-----------|----------|----------|
| **Terminal init fails** (native) | `enter_raw_mode()` returns Err | Return error to caller; core remains usable | N/A (app cannot start) |
| **WebGPU unavailable** (web) | Feature detection at init | Fall back to Canvas 2D renderer | Reduced visual quality |
| **PTY spawn fails** (remote) | `fork/exec` returns error | Report to client; offer retry | N/A |
| **WebSocket disconnect** | Connection close event | Auto-reconnect with exponential backoff | Show disconnected state |
| **Parser invalid sequence** | State machine catches | Silently consume; log in debug | No visual corruption |
| **Memory budget exceeded** | Allocation tracking | Evict caches (glyphs, scrollback) | Reduced scrollback/quality |
| **Frame budget exceeded** | Frame-time monitoring | Degrade to simpler rendering tier | TextOnly degradation level |
| **Resize storm** | BOCPD regime detection | Coalesce aggressively | Continuous reflow with budget |

---

## 11. Rollback Strategy

### 11.1 Crossterm Fallback

During the transition period:

```toml
# ftui-core/Cargo.toml
[features]
default = ["backend-native"]
backend-native = ["dep:ftui-tty"]
backend-crossterm = ["dep:crossterm"]  # Legacy fallback
backend-wasm = []                      # Excludes all native deps
```

If ftui-tty proves unreliable, users can switch back to crossterm by changing the
feature flag. The `TerminalBackend` trait ensures both implementations are interchangeable.

### 11.2 Web Fallback

If WebGPU is unavailable:
1. Try Canvas 2D with pre-rendered glyph sprites
2. If Canvas 2D fails, render server-side and stream as images (last resort)

### 11.3 Remote Fallback

If WebSocket upgrade fails:
1. Try HTTP long-polling (degraded latency)
2. Fall back to SSH-based connection (requires user config)

---

## 12. Implementation Order

### Phase 1: Foundation (unblocks everything)

1. **Extract `TerminalBackend` trait** in ftui-core (bd-lff4p.3.2)
2. **Wrap crossterm as `CrosstermBackend`** behind feature gate
3. **Add `NullBackend`** for headless testing
4. **Verify all existing tests pass** with the trait indirection

### Phase 2: Native Backend (ftui-tty)

1. **Create ftui-tty crate** (bd-lff4p.4.1)
2. **Implement Unix raw mode** via rustix/nix (bd-lff4p.4.2)
3. **Implement input reader** with poll + byte parsing (bd-lff4p.4.3)
4. **Implement SIGWINCH** handling (bd-lff4p.4.4)
5. **Integrate with ftui-runtime** (bd-lff4p.4.5)
6. **Remove crossterm dependency** (bd-lff4p.4.6)
7. **PTY + panic cleanup tests** (bd-lff4p.4.7)

### Phase 3: Terminal Engine (frankenterm-core)

1. **Create frankenterm-core crate** (bd-lff4p.1.2)
2. **Implement grid + cursor + modes** (bd-lff4p.1.3)
3. **Implement scrollback + reflow** (bd-lff4p.1.4)
4. **Implement VT/ANSI parser** (bd-lff4p.1.6)
5. **Implement selection model** (bd-lff4p.1.5)
6. **Implement hyperlink registry** (bd-lff4p.1.7)
7. **Implement incremental patch API** (bd-lff4p.1.8)

### Phase 4: Web Stack

1. **Create ftui-web crate** (bd-lff4p.3.1)
2. **Make core crates wasm32-friendly** (bd-lff4p.3.3)
3. **Implement WASM step-based runner** (bd-lff4p.3.4)
4. **Create frankenterm-web** (bd-lff4p.2.2)
5. **WebGPU renderer** (bd-lff4p.2.3 → 2.5)
6. **Web input capture** (bd-lff4p.2.7)
7. **Integrate ftui-render Buffer → patch feed** (bd-lff4p.3.5)

### Phase 5: Remote + Polish

1. **WebSocket protocol spec** (bd-lff4p.10.1)
2. **PTY bridge server** (bd-lff4p.10.4)
3. **Browser input encoder** (bd-lff4p.10.2)
4. **Golden trace system** (bd-lff4p.5.1, 5.2)
5. **Demo showcase in browser** (bd-lff4p.3.6)

---

## 13. Key Design Decisions

### 13.1 Why reuse ftui-render Cell in frankenterm-core?

The 16-byte Cell is already cache-optimized and battle-tested. Using it in the terminal
engine means **zero conversion cost** when feeding cell diffs to the WebGPU renderer or
when the ftui runtime patches the terminal grid. One cell type, everywhere.

### 13.2 Why not just compile the ANSI Presenter to WASM?

The Presenter emits escape sequences that a terminal interprets. On the web, there is no
terminal to interpret them — we'd need to parse them right back into cells. Instead, we
bypass the Presenter entirely on the web path and pass raw cell diffs to the GPU renderer.
This eliminates an unnecessary encode/decode round-trip.

### 13.3 Why a separate frankenterm-core instead of extending ftui-render?

ftui-render is a **rendering kernel** (Buffer + Diff + Presenter). frankenterm-core is a
**terminal emulator** (parser + grid + scrollback + modes). They share the Cell type but
have different responsibilities. Keeping them separate means ftui-render stays lean for
apps that don't need terminal emulation (the common case for native TUI apps).

### 13.4 Why Unix-first for ftui-tty?

Windows terminal support has historically different semantics (ConPTY, legacy console API).
Starting with Unix lets us nail correctness and performance on the primary target. Windows
support can be added later as a separate backend implementation behind the same trait.

---

## 14. References

- ADR-003: Terminal Backend Selection (Crossterm as v1)
- ADR-005: One-Writer Rule Enforcement
- ADR-007: SDK Modularization
- docs/spec/embedded-core.md: Host-agnostic boundary map
- docs/spec/ffi-crate-layout.md: FFI crate structure
- docs/spec/state-machines.md: Terminal + rendering pipeline state machines
- bd-lff4p: Parent epic (FrankenTerm.WASM)
