# WASM Showcase Runner Contract — bd-lff4p.12.1

Describes the in-tree runner API and its intended integration with:

1. The browser host (HTML+JS)
2. `FrankenTermWeb` (adjacent web terminal surface)
3. The showcase app runner compiled to WASM (`ShowcaseRunner`)

The runner and bounded Rust input queue are implemented here; the adjacent
`frankenterm-web` renderer is not vendored. Browser/renderer verification and
host queue bounds remain open under `bd-g00-root-epic-ewths.29.8`.

## Architecture

Two WASM objects cooperate; JS orchestrates the data flow:

```
Browser (JS host loop)
├── FrankenTermWeb (display surface)
│   ├── WebGPU renderer
│   ├── Input normalizer (DOM → JSON)
│   ├── Search / links / accessibility
│   └── Shadow cell buffer
│
└── ShowcaseRunner (app logic)
    ├── RunnerCore → StepProgram<AppModel>
    ├── Deterministic clock
    ├── Patch generation (Buffer → Diff → WebPatchRun)
    └── Log capture
```

Data flow per frame:
```
DOM events → term.input() → term.drainEncodedInputs() → runner.pushEncodedInput()
                                                              ↓
                                                        runner.step()
                                                              ↓
              term.render() ← term.applyPatchBatchFlat() ← runner.takeFlatPatches()
```

## 1. Inputs Contract

### Flow

1. Browser captures DOM keyboard/mouse/touch/paste/focus events.
2. JS calls `term.input(eventObj)` — FrankenTermWeb normalizes the event.
3. JS calls `term.drainEncodedInputs()` — returns `Array<string>` of JSON-encoded events.
4. JS forwards each string: `runner.pushEncodedInput(json)` — returns `true`
   only when admitted to the runner queue. Every `false` needs host handling.
5. Runner parses JSON → `ftui_core::event::Event` → `StepProgram::push_event()`.
   `false` covers malformed input, unsupported/no-mapping input, a stopped runner, and capacity
   rejection; the boolean does not distinguish these causes.

### Bounded Admission

`WebEventSource` admits at most **4096 pending events** and **1 MiB of retained
text allocation capacity** across paste, IME, and clipboard events. The byte
bound sums `String::capacity()`, including spare capacity; it does not bound
raw JSON, temporary parsing, or host/renderer queues. Accepted events remain
FIFO and are never evicted to admit later input.

The Rust entry points return `Result<(), WebBackendError>`:

```rust
// WebEventSource and StepProgram:
pub fn push_event(&mut self, event: Event) -> Result<(), WebBackendError>;
// StepProgram:
pub fn resize(&mut self, width: u16, height: u16) -> Result<(), WebBackendError>;
// SessionRecorder:
pub fn push_event(&mut self, ts_ns: u64, event: Event) -> Result<(), WebBackendError>;
pub fn resize(&mut self, ts_ns: u64, width: u16, height: u16)
    -> Result<(), WebBackendError>;
```

Overflow returns `WebBackendError::InputQueueFull { limit, event }` with
`WebInputLimit::Events` or `PayloadBytes`, leaving accepted input unchanged.
For live capacity retries, step, take and apply **every intermediate patch**,
then retry the same input before later inputs. Stepping without presenting
loses intermediate output batches. If an empty queue still rejects the input
(including a payload over 1 MiB), report permanent rejection explicitly.
Diagnostics must omit raw input and payload-bearing error `Debug` output.
Known display-only `touch`/`accessibility` inputs need no capacity retry;
pane touch handling uses separate APIs.

### JSON Input Schema

Accepted by the in-tree parser; adjacent encoder changes require compatibility
checks.

```json
{"kind":"key","phase":"down","code":"a","mods":0,"repeat":false}
{"kind":"key","phase":"up","code":"Enter","mods":4,"repeat":false}
{"kind":"mouse","phase":"down","button":0,"x":10,"y":5,"mods":0}
{"kind":"mouse","phase":"move","x":11,"y":5,"mods":0}
{"kind":"wheel","x":10,"y":5,"dx":0,"dy":-3,"mods":0}
{"kind":"paste","data":"hello world"}
{"kind":"focus","focused":true}
{"kind":"composition","phase":"end","data":"你好"}
{"kind":"touch","phase":"start","touches":[{"id":1,"x":5,"y":3}],"mods":0}
{"kind":"accessibility","screen_reader":true}
```

### JSON → Event Conversion

Implemented in `ftui-web/src/input_parser.rs` (no web-sys dependency):

```rust
pub fn parse_encoded_input_to_event(json: &str) -> Result<Option<Event>, InputParseError>
```

Mapping rules:
- `kind:"key" + phase:"down"` → `Event::Key` with Press, or Repeat when `repeat:true`
- `kind:"key" + phase:"up"` → `Event::Key` with `KeyEventKind::Release`
- `kind:"mouse" + phase:"down"` → `Event::Mouse` with `MouseEventKind::Down(button)`
- `kind:"mouse" + phase:"up"` → `Event::Mouse` with `MouseEventKind::Up(button)`
- `kind:"mouse" + phase:"move"` → `Event::Mouse` with `MouseEventKind::Moved`
- `kind:"mouse" + phase:"drag"` → `Event::Mouse` with `MouseEventKind::Drag(button)`
- `kind:"paste"` → `Event::Paste(PasteEvent { text })`
- `kind:"focus"` → `Event::Focus(focused)`
- `kind:"wheel"` → scroll-style mouse event: vertical `dy` takes precedence,
  then horizontal `dx`; both zero returns `Ok(None)` (no movement).
- `kind:"composition"` → one `Event::Ime` per input: `start` → Start,
  `update` → Update, `end`/`commit` → Commit, `cancel` → Cancel. Update and
  commit preserve the complete text, including empty commits; no character-key
  synthesis occurs. An unknown phase is a parse error.
- `kind:"accessibility"` → `Ok(None)` (display-only, no runner effect)
- `kind:"touch"` → `Ok(None)` (not mapped to terminal events yet)
- Unknown `kind` → `Ok(None)`; `pushEncodedInput` returns `false`, which the
  host must handle explicitly rather than treating it as accepted.

### Modifier Mapping

```
frankenterm-web Modifiers (u8)    →  ftui_core::event::Modifiers (bitflags)
SHIFT = 0b0001                       SHIFT
ALT   = 0b0010                       ALT
CTRL  = 0b0100                       CTRL
SUPER = 0b1000                       SUPER
```

### Key Code Mapping

```
frankenterm-web code string  →  ftui_core::event::KeyCode
"Enter"                          KeyCode::Enter
"Escape"                         KeyCode::Escape
"Backspace"                      KeyCode::Backspace
"Tab"                            KeyCode::Tab
"Delete"                         KeyCode::Delete
"Up"/"Down"/"Left"/"Right"       KeyCode::Up/Down/Left/Right
"Home"/"End"                     KeyCode::Home/End
"PageUp"/"PageDown"              KeyCode::PageUp/PageDown
"F1".."F24"                      KeyCode::F(1)..F(24)
single char "a"                  KeyCode::Char('a')
```

## 2. Resize Contract

The runner consumes **terminal cols/rows (cell-space) only**. Pixel dimensions and DPR are renderer concerns.

### Flow

1. Host detects container resize (ResizeObserver / window resize / DPR change).
2. Before changing renderer geometry, drain pending runner events and apply
   every patch at the existing geometry.
3. Host calls `term.fitToContainer(widthCss, heightCss, dpr)` → returns `{ cols, rows, ... }`.
4. Host checks `runner.resize(cols, rows)` for successful admission. A `false`
   leaves runner dimensions unchanged; stop the geometry transition explicitly.
5. If `fitToContainer` was not used, resize the renderer to the accepted
   dimensions before presenting the next runner frame.

### Lockstep Invariant

FrankenTermWeb and ShowcaseRunner **MUST** agree on `(cols, rows)` when applying
patches. If `fitToContainer` changes geometry before admission fails, halt or
restore the old renderer geometry before draining and retrying. Never apply
old-geometry patches to the resized renderer.

### Behavior

- `runner.resize(cols, rows) -> boolean` queues an `Event::Resize`, clamping
  each dimension to at least one cell.
  `true` means accepted; `false` means capacity rejection or a stopped runner, without changing
  runner dimensions or pane preview state.
- On the next `step()`, the resize event is processed, `prev_buffer` is invalidated, and a full repaint is emitted.
- First frame after resize is always a full repaint (single span covering `cols * rows` cells).
- A same-size resize signal also forces a repaint boundary (for host fit,
  DPR, or zoom transitions). Admission does not render until `step()`.

## 3. Patch Contract

### When to Read Patches

After initialization's first render and after every `runner.step()` that
returns `{ rendered: true }`, including steps used to drain input capacity.
Consume and apply each batch before another render can replace it.

### Format

`runner.takeFlatPatches()` returns `{ cells: Uint32Array, spans: Uint32Array }`:

**spans**: `[offset, len, offset, len, ...]`
- `offset`: linear cell index in row-major order (`y * cols + x`), `u32`.
- `len`: number of contiguous cells in this span, `u32`.

**cells**: `[bg, fg, glyph, attrs, bg, fg, glyph, attrs, ...]`
- Each cell is 4 consecutive `u32` values (16 bytes per cell).
- `bg`: packed RGBA (`0xRRGGBBAA`).
- `fg`: packed RGBA.
- `glyph`: Unicode codepoint. `0` = empty cell. `0x25A1` (□) = grapheme fallback.
- `attrs`: bits 0–7 = `StyleFlags` (bold/italic/underline/strikethrough/dim/inverse/hidden/blink), bits 8–31 = link_id.

### Display

Host calls:
```js
term.applyPatchBatchFlat(patches.spans, patches.cells);
term.render();
```

### Invariants

| Property | Guarantee |
|----------|-----------|
| First frame (after init) | Full repaint: single span `[0, cols*rows]` |
| First frame (after resize) | Full repaint: single span `[0, cols*rows]` |
| Empty batches | Valid (both arrays length 0); host should skip render |
| Span ordering | Ascending offset, non-overlapping |
| Cell encoding | Matches `frankenterm-web::renderer::CellData` layout |
| Existing implementation | `WebOutputs::flatten_patches_u32()` produces this format |

## 4. Determinism / Time

### Design

- Time is **never** read from the system clock inside WASM.
- All timing is host-driven through the `DeterministicClock`.
- Same inputs + same time sequence → identical frame checksums.

### Real-Time Mode

```js
function stepAndPresent() {
    const result = runner.step();
    if (result.rendered) {
        const patches = runner.takeFlatPatches();
        term.applyPatchBatchFlat(patches.spans, patches.cells);
        term.render();
    }
    return result;
}

runner.init();
const initial = runner.takeFlatPatches(); // Preserve init output before retries.
term.applyPatchBatchFlat(initial.spans, initial.cells);
term.render();

let lastTs = 0;
function frame(timestamp) {
    const dt = lastTs === 0 ? 16.0 : timestamp - lastTs;
    lastTs = timestamp;

    runner.advanceTime(dt);  // milliseconds, f64

    const inputs = term.drainEncodedInputs();
    for (const json of inputs) {
        let input;
        try { input = JSON.parse(json); }
        catch { throw new Error("Malformed input rejected; contents withheld"); }
        if (input?.kind === "touch" || input?.kind === "accessibility") continue;
        if (runner.pushEncodedInput(json)) continue;
        // Drain and apply every intermediate batch before one retry.
        if (!stepAndPresent().running || !runner.pushEncodedInput(json)) {
            throw new Error("Input rejected after draining; contents withheld");
        }
    }

    const result = stepAndPresent();
    if (result.running) requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
```

This example halts on permanent rejection; a host may instead show a redacted
status under an explicit rejection policy. Host drain/retry storage also needs
bounds. This example is not evidence of browser verification.

`advanceTime(dt_ms)` ignores non-finite/non-positive deltas and clamps accepted
deltas to a representable `Duration`.

### Fixed-Step Replay Mode

Use the canonical `ftui_web::session_record::replay`, which returns
`Result<ReplayResult, ReplayError>`. Example inside a function returning
`Result<(), &'static str>`:

```rust,ignore
let result = ftui_web::session_record::replay(MyModel::default(), &trace)
    .map_err(|_| "trace validation or replay failed; contents withheld")?;
if !result.ok() {
    return Err("replay frame checksum mismatch");
}
```

`SessionRecorder` records input/resize and changes its recording timestamp only
after successful admission. Replay validates the trace, advances time at tick
records, and steps only at frame boundaries; initialization supplies frame 0.
Admission failure returns `ReplayError::Backend`. Never insert unrecorded steps
to make replay fit capacity. Browser replay must fail on rejection and apply
every intermediate patch while preserving those recorded boundaries.

`setTime(ts_ns)` maps non-finite/non-positive values to zero and clamps positive
values to `u64` nanoseconds. JavaScript `number` precision still limits exact
large timestamps.

### Patch Checksum Algorithm

This patch-batch hash differs from the full-buffer checksums and checksum chain
used by `SessionRecorder`/`replay`. They must not be compared interchangeably.

FNV-1a 64-bit over the patch batch, matching `ftui-web::patch_batch_hash()`:
```
hash = FNV64_OFFSET_BASIS (0xcbf29ce484222325)
hash = fnv1a64(hash, patch_count as u64 LE bytes)
for each patch:
    hash = fnv1a64(hash, offset as u32 LE bytes)
    hash = fnv1a64(hash, cell_count as u64 LE bytes)
    for each cell:
        hash = fnv1a64(hash, bg LE, fg LE, glyph LE, attrs LE)
```

Formatted as `"fnv1a64:{hash:016x}"`.

## 5. Logging Contract

### Runtime Logs

- Model emits `Cmd::Log(text)` → captured by `WebPresenter`.
- Host reads: `runner.takeLogs()` → `Array<string>`.
- Logs are consumed (drained) on each call.

### Host-Side JSONL (for E2E/CI)

The runner provides raw data; the host formats JSONL lines:

```jsonl
{"event":"step","frame_idx":1,"ts_ns":16000000,"rendered":true,"events_processed":3}
{"event":"patch_stats","frame_idx":1,"dirty_cells":42,"patch_count":3,"bytes_uploaded":672}
{"event":"frame","frame_idx":1,"patch_hash":"fnv1a64:a1b2c3d4e5f6a7b8"}
```

### Data Accessors

| Method | Returns | Description |
|--------|---------|-------------|
| `runner.patchHash()` | `string \| null` | FNV-1a hash of last patch batch |
| `runner.patchStats()` | `{dirty_cells, patch_count, bytes_uploaded} \| null` | Patch upload accounting |
| `runner.frameIdx()` | `bigint` | Rendered frame count: 0 before init, 1 after its first render; `step().frame_idx` is a JS number |
| `runner.isRunning()` | `boolean` | False after model emits `Cmd::Quit` |

## 6. ShowcaseRunner wasm-bindgen API

```rust
// Core runner exports from ftui-showcase-wasm/src/wasm.rs (excerpt).
// Dependencies: ftui-web, ftui-demo-showcase, wasm-bindgen, js-sys

#[wasm_bindgen]
pub struct ShowcaseRunner {
    inner: RunnerCore,
}

#[wasm_bindgen]
impl ShowcaseRunner {
    /// Create a new runner with initial terminal dimensions.
    #[wasm_bindgen(constructor)]
    pub fn new(cols: u16, rows: u16) -> Self;

    /// Initialize the model and render the first frame. Call exactly once.
    pub fn init(&mut self);

    /// Advance deterministic clock by dt milliseconds (real-time mode).
    #[wasm_bindgen(js_name = advanceTime)]
    pub fn advance_time(&mut self, dt_ms: f64);

    /// Set deterministic clock to absolute nanoseconds (replay mode).
    #[wasm_bindgen(js_name = setTime)]
    pub fn set_time(&mut self, ts_ns: f64);

    /// Parse a JSON-encoded input and push to the event queue.
    /// Returns true only after admission. False includes unsupported,
    /// malformed, no-mapping, and over-capacity inputs.
    #[wasm_bindgen(js_name = pushEncodedInput)]
    pub fn push_encoded_input(&mut self, json: &str) -> bool;

    /// Queue a resize. False leaves dimensions unchanged on capacity rejection.
    pub fn resize(&mut self, cols: u16, rows: u16) -> bool;

    /// Process pending events and render if dirty.
    /// Returns { running, rendered, events_processed, frame_idx }.
    pub fn step(&mut self) -> JsValue;

    /// Take flat patch batch for GPU upload.
    /// Returns { cells: Uint32Array, spans: Uint32Array }.
    #[wasm_bindgen(js_name = takeFlatPatches)]
    pub fn take_flat_patches(&mut self) -> JsValue;

    /// Drain accumulated log lines. Returns Array<string>.
    #[wasm_bindgen(js_name = takeLogs)]
    pub fn take_logs(&mut self) -> js_sys::Array;

    /// FNV-1a hash of the last patch batch, or null.
    #[wasm_bindgen(js_name = patchHash)]
    pub fn patch_hash(&mut self) -> Option<String>;

    /// Patch upload stats: { dirty_cells, patch_count, bytes_uploaded }.
    #[wasm_bindgen(js_name = patchStats)]
    pub fn patch_stats(&self) -> JsValue;

    /// Rendered frame count (monotonic, zero before initialization).
    #[wasm_bindgen(js_name = frameIdx)]
    pub fn frame_idx(&self) -> u64;

    /// Whether the program is still running.
    #[wasm_bindgen(js_name = isRunning)]
    pub fn is_running(&self) -> bool;

    /// Currently a no-op; internal resources use Rust Drop cleanup.
    pub fn destroy(&mut self);
}
```

## 7. Implementation and Verification Scope

1. **ftui-web: `parse_encoded_input_to_event`**
   - Public function in `ftui-web/src/input_parser.rs`.
   - Parses frankenterm-web JSON input schema.
   - Converts to `ftui_core::event::Event`.
   - No web-sys/js-sys dependency. Uses `serde_json`.
   - `WebEventSource` enforces count and retained-text allocation bounds.

2. **Crate: `ftui-showcase-wasm`**
   - `ShowcaseRunner` wraps `RunnerCore`, which owns `StepProgram<AppModel>`.
   - wasm-bindgen exports per the API above.
   - Dependencies: ftui-web, ftui-demo-showcase, wasm-bindgen, js-sys.
   - Builds and verification use the repository's DSR native-host path and
     pinned Rust toolchain.

3. **Host HTML: `crates/ftui-showcase-wasm/frankentui_showcase_demo.html`**
   - Creates FrankenTermWeb + ShowcaseRunner.
   - requestAnimationFrame host loop.
   - ResizeObserver → fitToContainer → runner.resize.
   - DOM event listeners → term.input → drain → runner.pushEncodedInput.
   - Check admission; preserve FIFO, geometry, and intermediate patches on retry.
   - DOM/IME/clipboard, renderer bounds, accessibility, and lifecycle verification
     remain browser-host obligations under `bd-g00-root-epic-ewths.29.8`.

## 8. Invariants Summary

| Property | Guarantee |
|----------|-----------|
| Determinism | Same inputs + same time → identical patch hashes |
| No system time | DeterministicClock only; no Instant::now() in WASM |
| No threads | StepProgram runs synchronously; Cmd::Task executes inline |
| Lockstep geometry | Host checks resize admission and applies each patch at its matching geometry |
| Patch ordering | Spans in ascending offset order, non-overlapping |
| First frame | Always full repaint (single span, all cells) |
| Empty batches | Valid (length-0 arrays); host skips render |
| Accepted input | FIFO, never evicted to admit later events |
| Input bounds | 4096 pending events; 1 MiB retained text allocation capacity |
| Rejected input | Rust capacity errors return the event; stopped programs return `Unsupported`; JS returns false; host handles explicitly |
| Display-only input | Touch/accessibility have no mapping on the encoded-input path |
| Live capacity retry | Drain and apply every intermediate batch before retrying the same input |
| Replay admission | Fail on rejected recorded input; never add unrecorded steps |
| GraphemePool GC | Every 256 frames (automatic inside StepProgram) |
| Schema compat | Parser and trace schemas require explicit compatibility checks; patch hashes differ from replay buffer checksums |
