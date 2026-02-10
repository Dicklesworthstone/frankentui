# WASM Showcase Runner Contract — bd-lff4p.12.1

Defines the exact API/ABI contract between:
1. The browser host (HTML+JS)
2. `FrankenTermWeb` (web terminal surface, frankenterm-web crate)
3. The showcase app runner compiled to WASM (new `ShowcaseRunner` struct)

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
    ├── StepProgram<AppModel>
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
4. JS forwards each string: `runner.pushEncodedInput(json)` — returns `true` if accepted.
5. Runner parses JSON → `ftui_core::event::Event` → `StepProgram::push_event()`.

### JSON Input Schema

Defined by `frankenterm-web::input::InputEvent::to_json_string()`. Stable across versions.

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

New function in `ftui-web` (no web-sys dependency):

```rust
pub fn parse_encoded_input_to_event(json: &str) -> Result<Option<Event>, InputParseError>
```

Mapping rules:
- `kind:"key" + phase:"down"` → `Event::Key { code, modifiers, kind: Press }`
- `kind:"key" + phase:"up"` → `Event::Key { code, modifiers, kind: Release }`
- `kind:"mouse" + phase:"down"` → `Event::Mouse { kind: Press, x, y, button, modifiers }`
- `kind:"mouse" + phase:"up"` → `Event::Mouse { kind: Release, x, y, button, modifiers }`
- `kind:"mouse" + phase:"move"` → `Event::Mouse { kind: Move, x, y, modifiers }`
- `kind:"mouse" + phase:"drag"` → `Event::Mouse { kind: Drag, x, y, button, modifiers }`
- `kind:"paste"` → `Event::Paste(PasteEvent { text })`
- `kind:"focus"` → `Event::Focus(focused)`
- `kind:"wheel"` → mapped to scroll-style Mouse event (dy < 0 → ScrollUp, dy > 0 → ScrollDown)
- `kind:"composition" + phase:"end"` → synthesized `Event::Key` events for each char in `data`
- `kind:"accessibility"` → `Ok(None)` (display-only, no runner effect)
- `kind:"touch"` → `Ok(None)` (not mapped to terminal events yet)
- Unknown `kind` → `Ok(None)` (silently dropped)

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
2. Host calls `term.fitToContainer(widthCss, heightCss, dpr)` → returns `{ cols, rows, ... }`.
3. Host calls `runner.resize(cols, rows)`.
4. If `fitToContainer` was not used, host calls `term.resize(cols, rows)` separately.

### Lockstep Invariant

FrankenTermWeb and ShowcaseRunner **MUST** agree on `(cols, rows)` at all times. The host is responsible for calling both resize methods before the next `step()`.

### Behavior

- `runner.resize(cols, rows)` pushes `Event::Resize { width: cols, height: rows }` internally.
- On the next `step()`, the resize event is processed, `prev_buffer` is invalidated, and a full repaint is emitted.
- First frame after resize is always a full repaint (single span covering `cols * rows` cells).

## 3. Patch Contract

### When to Read Patches

Only after `runner.step()` returns `{ rendered: true }`.

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
term.applyPatchBatchFlat(patches.cells, patches.spans);
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
let lastTs = 0;
function frame(timestamp) {
    const dt = lastTs === 0 ? 16.0 : timestamp - lastTs;
    lastTs = timestamp;

    runner.advanceTime(dt);  // milliseconds, f64

    const inputs = term.drainEncodedInputs();
    for (const json of inputs) runner.pushEncodedInput(json);

    const result = runner.step();
    if (result.rendered) {
        const patches = runner.takeFlatPatches();
        term.applyPatchBatchFlat(patches.cells, patches.spans);
        term.render();
    }
    if (result.running) requestAnimationFrame(frame);
}
```

`advanceTime(dt_ms)` internally: `Duration::from_secs_f64(dt_ms / 1000.0)`.

### Fixed-Step Replay Mode

```js
for (const record of trace.records) {
    switch (record.event) {
        case "input":
            runner.setTime(record.ts_ns);
            runner.pushEncodedInput(JSON.stringify(record.payload));
            break;
        case "tick":
            runner.setTime(record.ts_ns);
            break;
        case "resize":
            runner.setTime(record.ts_ns);
            runner.resize(record.cols, record.rows);
            term.resize(record.cols, record.rows);
            break;
    }
    const result = runner.step();
    if (record.event === "frame") {
        console.assert(runner.patchHash() === record.patch_hash);
    }
}
```

`setTime(ts_ns)` internally: `Duration::from_nanos(ts_ns as u64)`.

### Checksum Algorithm

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
| `runner.frameIdx()` | `number` | Current frame index (monotonic, 0-based) |
| `runner.isRunning()` | `boolean` | False after model emits `Cmd::Quit` |

## 6. ShowcaseRunner wasm-bindgen API

```rust
// Crate: ftui-showcase-wasm (new)
// Dependencies: ftui-web, ftui-demo-showcase, wasm-bindgen, js-sys

#[wasm_bindgen]
pub struct ShowcaseRunner {
    inner: StepProgram<AppModel>,
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
    /// Returns true if accepted, false if unsupported/malformed.
    #[wasm_bindgen(js_name = pushEncodedInput)]
    pub fn push_encoded_input(&mut self, json: &str) -> bool;

    /// Resize the terminal (pushes Resize event, processed on next step).
    pub fn resize(&mut self, cols: u16, rows: u16);

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
    pub fn patch_hash(&self) -> Option<String>;

    /// Patch upload stats: { dirty_cells, patch_count, bytes_uploaded }.
    #[wasm_bindgen(js_name = patchStats)]
    pub fn patch_stats(&self) -> JsValue;

    /// Current frame index (monotonic, 0-based).
    #[wasm_bindgen(js_name = frameIdx)]
    pub fn frame_idx(&self) -> u64;

    /// Whether the program is still running.
    #[wasm_bindgen(js_name = isRunning)]
    pub fn is_running(&self) -> bool;

    /// Release internal resources.
    pub fn destroy(&mut self);
}
```

## 7. Implementation Checklist (for bd-lff4p.12.3)

1. **ftui-web: `parse_encoded_input_to_event`**
   - New public function in `ftui-web/src/input_parser.rs` (or similar).
   - Parses frankenterm-web JSON input schema.
   - Converts to `ftui_core::event::Event`.
   - No web-sys/js-sys dependency. Uses serde_json or hand-rolled parser.

2. **New crate: `ftui-showcase-wasm`**
   - `ShowcaseRunner` struct wrapping `StepProgram<AppModel>`.
   - wasm-bindgen exports per the API above.
   - Dependencies: ftui-web, ftui-demo-showcase, wasm-bindgen, js-sys.
   - Build: `wasm-pack build --target web`.

3. **Host HTML: `frankentui_showcase_demo.html`**
   - Creates FrankenTermWeb + ShowcaseRunner.
   - requestAnimationFrame host loop.
   - ResizeObserver → fitToContainer → runner.resize.
   - DOM event listeners → term.input → drain → runner.pushEncodedInput.

## 8. Invariants Summary

| Property | Guarantee |
|----------|-----------|
| Determinism | Same inputs + same time → identical patch hashes |
| No system time | DeterministicClock only; no Instant::now() in WASM |
| No threads | StepProgram runs synchronously; Cmd::Task executes inline |
| Lockstep geometry | Host calls resize on both FrankenTermWeb and ShowcaseRunner |
| Patch ordering | Spans in ascending offset order, non-overlapping |
| First frame | Always full repaint (single span, all cells) |
| Empty batches | Valid (length-0 arrays); host skips render |
| Input drop | Unknown kinds return false; not an error |
| GraphemePool GC | Every 256 frames (automatic inside StepProgram) |
| Schema compat | JSON input format matches golden-trace-v1 input records |
