// In-tree DOM <pre> text host for ftui-showcase-wasm.
//
// Implements the host-side patch decoding, input encoding, and DPR/zoom
// resize contracts defined in docs/spec/wasm-showcase-runner-contract.md:
// - Section 1: Inputs Contract (JSON input schema, modifier bitmask, key codes)
// - Section 2: Resize Contract (cell-space cols/rows, DPR/zoom repaint boundary)
// - Section 3: Patch Contract (spans [offset, len], cells [bg, fg, glyph, attrs])
// - Section 4: Determinism / Time (advanceTime / setTime)

/**
 * StyleFlags bit positions matching ftui_render::cell::StyleFlags and
 * docs/spec/wasm-showcase-runner-contract.md Section 3 (attrs bits 0-7).
 */
export const STYLE_FLAGS = Object.freeze({
  BOLD: 1 << 0,
  DIM: 1 << 1,
  ITALIC: 1 << 2,
  UNDERLINE: 1 << 3,
  BLINK: 1 << 4,
  INVERSE: 1 << 5,
  HIDDEN: 1 << 6,
  STRIKETHROUGH: 1 << 7,
});

export const STYLE_CLASSES = Object.freeze([
  [STYLE_FLAGS.BOLD, "ftui-bold"],
  [STYLE_FLAGS.DIM, "ftui-dim"],
  [STYLE_FLAGS.ITALIC, "ftui-italic"],
  [STYLE_FLAGS.UNDERLINE, "ftui-underline"],
  [STYLE_FLAGS.BLINK, "ftui-blink"],
  [STYLE_FLAGS.INVERSE, "ftui-inverse"],
  [STYLE_FLAGS.HIDDEN, "ftui-hidden"],
  [STYLE_FLAGS.STRIKETHROUGH, "ftui-strikethrough"],
]);

/**
 * Modifier bitmask matching docs/spec/wasm-showcase-runner-contract.md Section 1:
 * SHIFT = 0b0001, ALT = 0b0010, CTRL = 0b0100, SUPER/META = 0b1000.
 */
export const MODIFIERS = Object.freeze({
  SHIFT: 0b0001,
  ALT: 0b0010,
  CTRL: 0b0100,
  SUPER: 0b1000,
});

/**
 * Convert packed u32 RGBA (0xRRGGBBAA) to a CSS color string.
 */
export function rgbaToCss(packed) {
  const u = packed >>> 0;
  const r = (u >>> 24) & 0xff;
  const g = (u >>> 16) & 0xff;
  const b = (u >>> 8) & 0xff;
  const a = u & 0xff;
  if (a === 0) return "transparent";
  if (a === 255) return `rgb(${r}, ${g}, ${b})`;
  return `rgba(${r}, ${g}, ${b}, ${(a / 255).toFixed(3)})`;
}

/**
 * Compute terminal cell geometry from CSS container and cell dimensions.
 * DPR does not alter CSS pixel counts, so equal CSS viewports yield identical
 * cols/rows across DPR=1 and DPR=2 while recording DPR/scale for repaint boundaries.
 */
export function computeGeometry({
  clientWidth = 0,
  clientHeight = 0,
  cellW = 8,
  cellH = 16,
  dpr = 1,
  scale = 1,
} = {}) {
  const safeCellW = Number.isFinite(cellW) && cellW > 0 ? cellW : 8;
  const safeCellH = Number.isFinite(cellH) && cellH > 0 ? cellH : 16;
  const safeW = Number.isFinite(clientWidth) && clientWidth > 0 ? clientWidth : 0;
  const safeH = Number.isFinite(clientHeight) && clientHeight > 0 ? clientHeight : 0;
  const cols = Math.max(1, Math.min(0xffff, Math.floor(safeW / safeCellW) || 1));
  const rows = Math.max(1, Math.min(0xffff, Math.floor(safeH / safeCellH) || 1));
  return {
    cols,
    rows,
    cellWidthCss: safeCellW,
    cellHeightCss: safeCellH,
    dpr: Number.isFinite(dpr) && dpr > 0 ? dpr : 1,
    scale: Number.isFinite(scale) && scale > 0 ? scale : 1,
  };
}

/**
 * Measure a single monospace 'M' probe span inside `container`.
 */
export function measureCell(container) {
  const doc = container?.ownerDocument;
  if (!doc || typeof doc.createElement !== "function") {
    return { cellW: 8, cellH: 16 };
  }
  const probe = doc.createElement("span");
  probe.className = "ftui-cell-probe";
  probe.textContent = "M";
  container.appendChild(probe);
  const rect = typeof probe.getBoundingClientRect === "function"
    ? probe.getBoundingClientRect()
    : { width: 8, height: 16 };
  probe.remove();
  return {
    cellW: rect.width > 0 ? rect.width : 8,
    cellH: rect.height > 0 ? rect.height : 16,
  };
}

/**
 * Create an in-memory (and optionally DOM-backed) grid state for `cols x rows`.
 */
export function createGridState(cols, rows, doc = null) {
  const safeCols = Math.max(1, cols | 0);
  const safeRows = Math.max(1, rows | 0);
  const total = safeCols * safeRows;
  const cells = Array.from({ length: total }, () => ({
    char: " ",
    bg: 0x000000ff,
    fg: 0xffffffff,
    bgCss: "rgb(0, 0, 0)",
    fgCss: "rgb(255, 255, 255)",
    attrs: 0,
    styleFlags: 0,
    linkId: 0,
    classes: [],
  }));
  let pre = null;
  const domSpans = new Array(total).fill(null);
  if (doc && typeof doc.createElement === "function") {
    pre = doc.createElement("pre");
    pre.className = "ftui-grid";
    pre.tabIndex = 0;
    pre.setAttribute("role", "application");
    pre.setAttribute("aria-label", "FrankenTUI terminal grid");
    for (let r = 0; r < safeRows; r++) {
      for (let c = 0; c < safeCols; c++) {
        const idx = r * safeCols + c;
        const span = doc.createElement("span");
        span.textContent = " ";
        domSpans[idx] = span;
        pre.appendChild(span);
      }
      if (r + 1 < safeRows) {
        pre.appendChild(doc.createTextNode("\n"));
      }
    }
  }
  return {
    cols: safeCols,
    rows: safeRows,
    cells,
    pre,
    domSpans,
    toText() {
      const lines = [];
      for (let r = 0; r < safeRows; r++) {
        let line = "";
        for (let c = 0; c < safeCols; c++) {
          line += cells[r * safeCols + c].char;
        }
        lines.push(line);
      }
      return lines.join("\n");
    },
  };
}

/**
 * Decode a flat patch batch `{ spans, cells }` without mutating DOM state.
 */
export function decodeFlatPatches(batch, cols = 80) {
  const spans = batch?.spans;
  const cells = batch?.cells;
  if (!spans || !cells || spans.length === 0) {
    return [];
  }
  const safeCols = Math.max(1, cols | 0);
  const decodedSpans = [];
  let cellWordCursor = 0;
  for (let i = 0; i + 1 < spans.length; i += 2) {
    const offset = spans[i] >>> 0;
    const len = spans[i + 1] >>> 0;
    const spanCells = [];
    for (let j = 0; j < len && cellWordCursor + 3 < cells.length; j++) {
      const bg = cells[cellWordCursor] >>> 0;
      const fg = cells[cellWordCursor + 1] >>> 0;
      const glyph = cells[cellWordCursor + 2] >>> 0;
      const attrs = cells[cellWordCursor + 3] >>> 0;
      cellWordCursor += 4;
      const index = offset + j;
      const cp = glyph > 0 && glyph <= 0x10ffff ? glyph : 0x20;
      let char = " ";
      try {
        char = String.fromCodePoint(cp);
      } catch {
        char = "\u25a1";
      }
      const styleFlags = attrs & 0xff;
      const linkId = attrs >>> 8;
      spanCells.push({
        index,
        row: Math.floor(index / safeCols),
        col: index % safeCols,
        bg,
        fg,
        bgCss: rgbaToCss(bg),
        fgCss: rgbaToCss(fg),
        glyph,
        char,
        attrs,
        styleFlags,
        linkId,
      });
    }
    decodedSpans.push({ offset, len, cells: spanCells });
  }
  return decodedSpans;
}

/**
 * Apply `{ spans, cells }` flat patches to a grid state created by `createGridState`.
 * Empty batches are a no-op and do not throw.
 */
export function applyPatchesToGrid(gridState, batch) {
  if (!gridState || !batch || !batch.spans || batch.spans.length === 0) {
    return 0;
  }
  const decoded = decodeFlatPatches(batch, gridState.cols);
  let updated = 0;
  for (const span of decoded) {
    for (const cell of span.cells) {
      if (cell.index < 0 || cell.index >= gridState.cells.length) continue;
      const target = gridState.cells[cell.index];
      target.char = cell.char;
      target.bg = cell.bg;
      target.fg = cell.fg;
      target.bgCss = cell.bgCss;
      target.fgCss = cell.fgCss;
      target.attrs = cell.attrs;
      target.styleFlags = cell.styleFlags;
      target.linkId = cell.linkId;
      const classes = [];
      for (const [bit, cls] of STYLE_CLASSES) {
        if ((cell.styleFlags & bit) !== 0) {
          classes.push(cls);
        }
      }
      target.classes = classes;
      const domSpan = gridState.domSpans[cell.index];
      if (domSpan) {
        domSpan.textContent = cell.char;
        domSpan.style.backgroundColor = cell.bgCss;
        domSpan.style.color = cell.fgCss;
        domSpan.className = classes.join(" ");
      }
      updated++;
    }
  }
  return updated;
}

/**
 * Encode DOM modifier flags into the contract's u8 bitmask.
 */
export function encodeModifiers(event = {}) {
  let mods = 0;
  if (event.shiftKey) mods |= MODIFIERS.SHIFT;
  if (event.altKey) mods |= MODIFIERS.ALT;
  if (event.ctrlKey) mods |= MODIFIERS.CTRL;
  if (event.metaKey) mods |= MODIFIERS.SUPER;
  return mods;
}

const SPECIAL_KEY_MAP = Object.freeze({
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Enter: "Enter",
  Escape: "Escape",
  Backspace: "Backspace",
  Tab: "Tab",
  Delete: "Delete",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
});

/**
 * Normalize a DOM KeyboardEvent into the contract's JSON input string.
 */
export function encodeKeyEvent(event, phase = "down") {
  const rawKey = typeof event?.key === "string" ? event.key : "";
  const rawCode = typeof event?.code === "string" ? event.code : "";
  let code = SPECIAL_KEY_MAP[rawKey] || SPECIAL_KEY_MAP[rawCode] || "";
  if (!code) {
    if (/^F([1-9]|1[0-9]|2[0-4])$/.test(rawKey)) {
      code = rawKey;
    } else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(rawCode)) {
      code = rawCode;
    } else if (Array.from(rawKey).length === 1) {
      code = rawKey;
    } else {
      code = rawCode || rawKey;
    }
  }
  return JSON.stringify({
    kind: "key",
    phase: phase === "up" ? "up" : "down",
    code,
    mods: encodeModifiers(event),
    repeat: Boolean(event?.repeat),
  });
}

/**
 * Normalize a DOM MouseEvent into the contract's JSON input string.
 */
export function encodeMouseEvent(
  event,
  phase = "down",
  cellMetrics = { cellW: 8, cellH: 16 },
  rect = { left: 0, top: 0 },
) {
  const cellW = cellMetrics?.cellW > 0 ? cellMetrics.cellW : 8;
  const cellH = cellMetrics?.cellH > 0 ? cellMetrics.cellH : 16;
  const x = Math.max(0, Math.floor(((event?.clientX ?? 0) - (rect?.left ?? 0)) / cellW));
  const y = Math.max(0, Math.floor(((event?.clientY ?? 0) - (rect?.top ?? 0)) / cellH));
  const payload = {
    kind: "mouse",
    phase,
    x,
    y,
    mods: encodeModifiers(event),
  };
  if (phase !== "move") {
    payload.button = typeof event?.button === "number" ? event.button : 0;
  }
  return JSON.stringify(payload);
}

/**
 * Normalize a DOM WheelEvent into the contract's JSON input string.
 */
export function encodeWheelEvent(
  event,
  cellMetrics = { cellW: 8, cellH: 16 },
  rect = { left: 0, top: 0 },
) {
  const cellW = cellMetrics?.cellW > 0 ? cellMetrics.cellW : 8;
  const cellH = cellMetrics?.cellH > 0 ? cellMetrics.cellH : 16;
  const x = Math.max(0, Math.floor(((event?.clientX ?? 0) - (rect?.left ?? 0)) / cellW));
  const y = Math.max(0, Math.floor(((event?.clientY ?? 0) - (rect?.top ?? 0)) / cellH));
  const dx = Math.trunc(Math.sign(event?.deltaX ?? 0));
  const dy = Math.trunc(Math.sign(event?.deltaY ?? 0));
  return JSON.stringify({
    kind: "wheel",
    x,
    y,
    dx,
    dy,
    mods: encodeModifiers(event),
  });
}

export function encodePasteEvent(data) {
  return JSON.stringify({ kind: "paste", data: String(data ?? "") });
}

export function encodeFocusEvent(focused) {
  return JSON.stringify({ kind: "focus", focused: Boolean(focused) });
}

/**
 * Mount the in-tree DOM <pre> showcase host inside `container`.
 */
export async function mountShowcase({
  container,
  wasmUrl = "../../pkg/ftui_showcase_wasm_bg.wasm",
  runnerModuleUrl = "../../pkg/ftui_showcase_wasm.js",
  cols: explicitCols = 0,
  rows: explicitRows = 0,
  frames: maxFrames = 0,
  RunnerClass = null,
} = {}) {
  if (!container) {
    throw new TypeError("mountShowcase requires a container element");
  }
  const doc = container.ownerDocument;
  const win = doc?.defaultView || globalThis;

  let ShowcaseRunner = RunnerClass;
  if (!ShowcaseRunner) {
    const mod = await import(runnerModuleUrl);
    if (typeof mod.default === "function") {
      await mod.default({ module_or_path: wasmUrl });
    }
    ShowcaseRunner = mod.ShowcaseRunner;
  }
  if (typeof ShowcaseRunner !== "function") {
    throw new Error("ShowcaseRunner constructor not found");
  }

  let cellMetrics = measureCell(container);
  const currentDpr = () => Number(win.devicePixelRatio) || 1;
  const currentScale = () => Number(win.visualViewport?.scale) || 1;

  function readGeometry() {
    const computed = computeGeometry({
      clientWidth: container.clientWidth || 80 * cellMetrics.cellW,
      clientHeight: container.clientHeight || 24 * cellMetrics.cellH,
      cellW: cellMetrics.cellW,
      cellH: cellMetrics.cellH,
      dpr: currentDpr(),
      scale: currentScale(),
    });
    return {
      ...computed,
      cols: explicitCols > 0 ? explicitCols : computed.cols,
      rows: explicitRows > 0 ? explicitRows : computed.rows,
    };
  }

  let geometry = readGeometry();
  let gridState = createGridState(geometry.cols, geometry.rows, doc);
  if (gridState.pre) {
    container.replaceChildren(gridState.pre);
  }

  const runner = new ShowcaseRunner(geometry.cols, geometry.rows);
  runner.init();
  applyPatchesToGrid(gridState, runner.takeFlatPatches());

  const events = [];
  function recordDprZoom(geo) {
    const entry = {
      event: "dpr_zoom",
      dpr: geo.dpr,
      scale: geo.scale,
      cols: geo.cols,
      rows: geo.rows,
      cell_w: geo.cellWidthCss,
      cell_h: geo.cellHeightCss,
    };
    events.push(entry);
    if (typeof console?.debug === "function") {
      console.debug("ftui.web.host", JSON.stringify(entry));
    }
  }

  function recomputeMetrics() {
    cellMetrics = measureCell(container);
    const next = readGeometry();
    const changed =
      next.cols !== geometry.cols ||
      next.rows !== geometry.rows ||
      next.cellWidthCss !== geometry.cellWidthCss ||
      next.cellHeightCss !== geometry.cellHeightCss ||
      next.dpr !== geometry.dpr ||
      next.scale !== geometry.scale;
    if (!changed) return false;
    const sizeChanged = next.cols !== geometry.cols || next.rows !== geometry.rows;
    geometry = next;
    if (sizeChanged) {
      gridState = createGridState(geometry.cols, geometry.rows, doc);
      if (gridState.pre) {
        container.replaceChildren(gridState.pre);
      }
    }
    runner.resize(geometry.cols, geometry.rows);
    recordDprZoom(geometry);
    return true;
  }

  let resizeQueued = false;
  function scheduleRecompute() {
    if (resizeQueued) return;
    resizeQueued = true;
    const schedule = typeof win.requestAnimationFrame === "function"
      ? win.requestAnimationFrame.bind(win)
      : (fn) => setTimeout(fn, 0);
    schedule(() => {
      resizeQueued = false;
      recomputeMetrics();
    });
  }

  let resizeObserver = null;
  if (typeof win.ResizeObserver === "function") {
    resizeObserver = new win.ResizeObserver(() => scheduleRecompute());
    resizeObserver.observe(container);
  }
  const onWinResize = () => recomputeMetrics();
  win.addEventListener?.("resize", onWinResize);
  win.visualViewport?.addEventListener?.("resize", onWinResize);
  win.visualViewport?.addEventListener?.("scroll", onWinResize);

  const onKeyDown = (e) => {
    runner.pushEncodedInput(encodeKeyEvent(e, "down"));
  };
  const onKeyUp = (e) => {
    runner.pushEncodedInput(encodeKeyEvent(e, "up"));
  };
  const onMouseDown = (e) => {
    const rect = gridState.pre?.getBoundingClientRect?.() || { left: 0, top: 0 };
    runner.pushEncodedInput(encodeMouseEvent(e, "down", cellMetrics, rect));
  };
  const onMouseUp = (e) => {
    const rect = gridState.pre?.getBoundingClientRect?.() || { left: 0, top: 0 };
    runner.pushEncodedInput(encodeMouseEvent(e, "up", cellMetrics, rect));
  };
  const onMouseMove = (e) => {
    const rect = gridState.pre?.getBoundingClientRect?.() || { left: 0, top: 0 };
    const phase = e.buttons ? "drag" : "move";
    runner.pushEncodedInput(encodeMouseEvent(e, phase, cellMetrics, rect));
  };
  const onWheel = (e) => {
    const rect = gridState.pre?.getBoundingClientRect?.() || { left: 0, top: 0 };
    runner.pushEncodedInput(encodeWheelEvent(e, cellMetrics, rect));
  };
  const onPaste = (e) => {
    const text = e.clipboardData?.getData?.("text") ?? "";
    if (text) runner.pushEncodedInput(encodePasteEvent(text));
  };
  const onFocus = () => runner.pushEncodedInput(encodeFocusEvent(true));
  const onBlur = () => runner.pushEncodedInput(encodeFocusEvent(false));

  win.addEventListener?.("keydown", onKeyDown);
  win.addEventListener?.("keyup", onKeyUp);
  container.addEventListener?.("mousedown", onMouseDown);
  container.addEventListener?.("mouseup", onMouseUp);
  container.addEventListener?.("mousemove", onMouseMove);
  container.addEventListener?.("wheel", onWheel, { passive: true });
  win.addEventListener?.("paste", onPaste);
  win.addEventListener?.("focus", onFocus);
  win.addEventListener?.("blur", onBlur);

  let lastTs = 0;
  let rafId = null;
  function stepOnce(dtMs = 16.0) {
    runner.advanceTime(dtMs);
    const s = runner.step();
    if (s.rendered) {
      applyPatchesToGrid(gridState, runner.takeFlatPatches());
    }
    return s;
  }

  function frameLoop(ts) {
    const dt = lastTs === 0 ? 16.0 : Math.max(0, ts - lastTs);
    lastTs = ts;
    const s = stepOnce(dt);
    const frameCount = Number(runner.frameIdx());
    if (s.running && (maxFrames <= 0 || frameCount < maxFrames)) {
      if (typeof win.requestAnimationFrame === "function") {
        rafId = win.requestAnimationFrame(frameLoop);
      }
    }
  }
  if (typeof win.requestAnimationFrame === "function") {
    rafId = win.requestAnimationFrame(frameLoop);
  }

  const handle = {
    runner,
    events,
    frames: () => Number(runner.frameIdx()),
    text: () => gridState.toText(),
    stats: () => runner.patchStats(),
    logs: () => Array.from(runner.takeLogs()),
    geometry: () => ({ ...geometry }),
    recomputeMetrics,
    stepOnce,
    destroy() {
      if (rafId !== null && typeof win.cancelAnimationFrame === "function") {
        win.cancelAnimationFrame(rafId);
      }
      resizeObserver?.disconnect?.();
      win.removeEventListener?.("resize", onWinResize);
      win.visualViewport?.removeEventListener?.("resize", onWinResize);
      win.visualViewport?.removeEventListener?.("scroll", onWinResize);
      win.removeEventListener?.("keydown", onKeyDown);
      win.removeEventListener?.("keyup", onKeyUp);
      container.removeEventListener?.("mousedown", onMouseDown);
      container.removeEventListener?.("mouseup", onMouseUp);
      container.removeEventListener?.("mousemove", onMouseMove);
      container.removeEventListener?.("wheel", onWheel);
      win.removeEventListener?.("paste", onPaste);
      win.removeEventListener?.("focus", onFocus);
      win.removeEventListener?.("blur", onBlur);
      runner.destroy();
    },
  };
  win.__ftuiShowcase = handle;
  return handle;
}
