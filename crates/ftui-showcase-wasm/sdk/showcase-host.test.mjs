import assert from "node:assert/strict";
import test from "node:test";
import {
  MODIFIERS,
  STYLE_FLAGS,
  applyPatchesToGrid,
  computeGeometry,
  createGridState,
  decodeFlatPatches,
  encodeKeyEvent,
  encodeModifiers,
  mountShowcase,
} from "./showcase-host.js";

function logCase(name, status, detail = {}) {
  console.log(
    JSON.stringify({
      suite: "showcase-host",
      case: name,
      status,
      detail,
    }),
  );
}

function createFakeDom({
  clientWidth = 800,
  clientHeight = 480,
  cellW = 10,
  cellH = 20,
  dpr = 1,
  scale = 1,
} = {}) {
  const winListeners = new Map();
  const containerListeners = new Map();
  const state = { clientWidth, clientHeight, cellW, cellH, dpr, scale };

  function makeElement(tag) {
    const el = {
      tagName: tag.toUpperCase(),
      className: "",
      textContent: "",
      tabIndex: -1,
      style: {},
      attributes: new Map(),
      children: [],
      setAttribute(k, v) {
        this.attributes.set(k, String(v));
      },
      appendChild(child) {
        this.children.push(child);
        return child;
      },
      replaceChildren(...nodes) {
        this.children = [...nodes];
      },
      remove() {},
      getBoundingClientRect() {
        if (this.className === "ftui-cell-probe") {
          return { width: state.cellW, height: state.cellH, left: 0, top: 0 };
        }
        return { width: state.clientWidth, height: state.clientHeight, left: 0, top: 0 };
      },
    };
    return el;
  }

  const win = {
    get devicePixelRatio() {
      return state.dpr;
    },
    visualViewport: {
      get scale() {
        return state.scale;
      },
      addEventListener() {},
      removeEventListener() {},
    },
    addEventListener(type, fn) {
      if (!winListeners.has(type)) winListeners.set(type, new Set());
      winListeners.get(type).add(fn);
    },
    removeEventListener(type, fn) {
      winListeners.get(type)?.delete(fn);
    },
    dispatchEvent(event) {
      for (const fn of winListeners.get(event.type) || []) {
        fn(event);
      }
    },
  };

  const doc = {
    defaultView: win,
    createElement(tag) {
      return makeElement(tag);
    },
    createTextNode(text) {
      return { nodeType: 3, textContent: text };
    },
  };

  const container = {
    ownerDocument: doc,
    children: [],
    get clientWidth() {
      return state.clientWidth;
    },
    get clientHeight() {
      return state.clientHeight;
    },
    appendChild(child) {
      this.children.push(child);
      return child;
    },
    replaceChildren(...nodes) {
      this.children = [...nodes];
    },
    addEventListener(type, fn) {
      if (!containerListeners.has(type)) containerListeners.set(type, new Set());
      containerListeners.get(type).add(fn);
    },
    removeEventListener(type, fn) {
      containerListeners.get(type)?.delete(fn);
    },
  };

  return { state, win, doc, container };
}

class ContractMockRunner {
  constructor(cols, rows) {
    this.cols = cols;
    this.rows = rows;
    this.frame = 0n;
    this.running = true;
    this.screenIndex = 0;
    this.screens = ["FrankenTUI Dashboard", "Shakespeare Explorer"];
    this.dirty = true;
    this.lastStats = null;
    this.resizeCalls = [];
  }
  init() {
    this.dirty = true;
    this.#renderFull();
  }
  advanceTime(_dt) {
    this.dirty = true;
  }
  pushEncodedInput(json) {
    const parsed = JSON.parse(json);
    if (parsed.kind === "key" && parsed.phase === "down" && parsed.code === "Tab") {
      this.screenIndex = (this.screenIndex + 1) % this.screens.length;
      this.dirty = true;
    }
    return true;
  }
  resize(cols, rows) {
    this.cols = cols;
    this.rows = rows;
    this.resizeCalls.push({ cols, rows });
    this.dirty = true;
    return true;
  }
  step() {
    if (!this.dirty) {
      return { running: this.running, rendered: false, frame_idx: Number(this.frame) };
    }
    this.#renderFull();
    return { running: this.running, rendered: true, frame_idx: Number(this.frame) };
  }
  #renderFull() {
    const total = this.cols * this.rows;
    const cells = new Uint32Array(total * 4);
    const title = this.screens[this.screenIndex];
    for (let i = 0; i < total; i++) {
      const ch = i < title.length ? title.codePointAt(i) : 0x20;
      cells[i * 4] = 0x000000ff;
      cells[i * 4 + 1] = 0xffffffff;
      cells[i * 4 + 2] = ch;
      cells[i * 4 + 3] = i < title.length ? STYLE_FLAGS.BOLD : 0;
    }
    this.pendingPatches = {
      spans: new Uint32Array([0, total]),
      cells,
    };
    this.lastStats = {
      dirty_cells: total,
      patch_count: 1,
      bytes_uploaded: total * 16,
    };
    this.frame += 1n;
    this.dirty = false;
  }
  takeFlatPatches() {
    return (
      this.pendingPatches || {
        spans: new Uint32Array([]),
        cells: new Uint32Array([]),
      }
    );
  }
  patchStats() {
    return this.lastStats;
  }
  takeLogs() {
    return [];
  }
  frameIdx() {
    return this.frame;
  }
  destroy() {
    this.running = false;
  }
}

test("decodes_cells_and_spans", () => {
  const grid = createGridState(3, 1);
  const batch = {
    spans: new Uint32Array([0, 3]),
    cells: new Uint32Array([
      0x000000ff, 0xffffffff, 0x48, STYLE_FLAGS.BOLD,
      0x000000ff, 0xffffffff, 0x2e, 0,
      0x000000ff, 0xffffffff, 0x2e, STYLE_FLAGS.UNDERLINE,
    ]),
  };
  const decoded = decodeFlatPatches(batch, 3);
  assert.equal(decoded.length, 1);
  assert.equal(decoded[0].offset, 0);
  assert.equal(decoded[0].len, 3);

  const updated = applyPatchesToGrid(grid, batch);
  assert.equal(updated, 3);
  assert.equal(grid.toText(), "H..");
  assert.equal(grid.cells[0].bgCss, "rgb(0, 0, 0)");
  assert.equal(grid.cells[0].fgCss, "rgb(255, 255, 255)");
  assert.deepEqual(grid.cells[0].classes, ["ftui-bold"]);
  assert.deepEqual(grid.cells[2].classes, ["ftui-underline"]);
  logCase("decodes_cells_and_spans", "pass", {
    text: grid.toText(),
    bg: grid.cells[0].bgCss,
    fg: grid.cells[0].fgCss,
  });
});

test("encodes_key_event_per_contract", () => {
  const plainA = encodeKeyEvent({ key: "a", ctrlKey: false, repeat: false }, "down");
  assert.equal(
    plainA,
    '{"kind":"key","phase":"down","code":"a","mods":0,"repeat":false}',
  );

  // Bit assignment per docs/spec/wasm-showcase-runner-contract.md Section 1:
  // SHIFT = 1, ALT = 2, CTRL = 4, SUPER/META = 8.
  assert.equal(
    encodeModifiers({ shiftKey: true, altKey: false, ctrlKey: true, metaKey: false }),
    MODIFIERS.SHIFT | MODIFIERS.CTRL,
  );
  const ctrlEnterUp = JSON.parse(
    encodeKeyEvent({ key: "Enter", ctrlKey: true, shiftKey: false, repeat: false }, "up"),
  );
  assert.deepEqual(ctrlEnterUp, {
    kind: "key",
    phase: "up",
    code: "Enter",
    mods: 4,
    repeat: false,
  });
  const arrowUp = JSON.parse(encodeKeyEvent({ key: "ArrowUp" }, "down"));
  assert.equal(arrowUp.code, "Up");
  logCase("encodes_key_event_per_contract", "pass", { plainA: JSON.parse(plainA) });
});

test("computeGeometry_is_dpr_invariant", () => {
  const dpr1 = computeGeometry({
    clientWidth: 800,
    clientHeight: 480,
    cellW: 10,
    cellH: 20,
    dpr: 1,
  });
  const dpr2 = computeGeometry({
    clientWidth: 800,
    clientHeight: 480,
    cellW: 10,
    cellH: 20,
    dpr: 2,
  });
  assert.equal(dpr1.cols, 80);
  assert.equal(dpr1.rows, 24);
  assert.equal(dpr2.cols, 80);
  assert.equal(dpr2.rows, 24);

  const zero = computeGeometry({
    clientWidth: 0,
    clientHeight: 0,
    cellW: 10,
    cellH: 20,
    dpr: 1,
  });
  assert.equal(zero.cols, 1);
  assert.equal(zero.rows, 1);
  logCase("computeGeometry_is_dpr_invariant", "pass", {
    cols: dpr1.cols,
    rows: dpr1.rows,
    zeroCols: zero.cols,
    zeroRows: zero.rows,
  });
});

test("empty_batch_is_noop", () => {
  const grid = createGridState(4, 2);
  const before = grid.toText();
  const updated = applyPatchesToGrid(grid, {
    spans: new Uint32Array([]),
    cells: new Uint32Array([]),
  });
  assert.equal(updated, 0);
  assert.equal(grid.toText(), before);
  logCase("empty_batch_is_noop", "pass", { updated });
});

test("page_loads_and_renders_dashboard_and_keyboard_switches_screen", async () => {
  const { win, container } = createFakeDom({
    clientWidth: 800,
    clientHeight: 480,
    cellW: 10,
    cellH: 20,
    dpr: 1,
  });
  const host = await mountShowcase({
    container,
    RunnerClass: ContractMockRunner,
  });
  for (let i = 0; i < 60; i++) {
    host.stepOnce(16.0);
  }
  assert.ok(host.frames() >= 60);
  assert.ok(host.text().includes("FrankenTUI Dashboard"));

  const beforeText = host.text();
  win.dispatchEvent({ type: "keydown", key: "Tab", code: "Tab", repeat: false });
  for (let i = 0; i < 5; i++) {
    host.stepOnce(16.0);
  }
  assert.notEqual(host.text(), beforeText);
  assert.ok(host.text().includes("Shakespeare Explorer"));
  host.destroy();
  logCase("page_loads_and_renders_dashboard_and_keyboard_switches_screen", "pass", {
    frames: host.frames(),
  });
});

test("dpr_1_and_2_render_identical_grid_text_and_zoom_resize_repaints", async () => {
  const dom1 = createFakeDom({ clientWidth: 800, clientHeight: 480, cellW: 10, cellH: 20, dpr: 1 });
  const dom2 = createFakeDom({ clientWidth: 800, clientHeight: 480, cellW: 10, cellH: 20, dpr: 2 });
  const host1 = await mountShowcase({ container: dom1.container, RunnerClass: ContractMockRunner });
  const host2 = await mountShowcase({ container: dom2.container, RunnerClass: ContractMockRunner });

  assert.equal(host1.geometry().cols, 80);
  assert.equal(host1.geometry().rows, 24);
  assert.equal(host2.geometry().cols, 80);
  assert.equal(host2.geometry().rows, 24);
  assert.equal(host1.text(), host2.text());

  // Viewport resize changes cols/rows and triggers a full-span repaint.
  dom1.state.clientWidth = 1200;
  dom1.state.clientHeight = 800;
  dom1.win.dispatchEvent({ type: "resize" });
  host1.stepOnce(16.0);
  assert.equal(host1.geometry().cols, 120);
  assert.equal(host1.geometry().rows, 40);
  assert.equal(host1.stats().dirty_cells, 120 * 40);

  // CSS zoom changes measured cell dimensions and records a dpr_zoom event.
  const eventsBefore = host1.events.length;
  dom1.state.cellW = 15;
  dom1.state.cellH = 30;
  dom1.win.dispatchEvent({ type: "resize" });
  host1.stepOnce(16.0);
  assert.ok(host1.events.length > eventsBefore);
  assert.equal(host1.events.at(-1).event, "dpr_zoom");
  assert.equal(host1.events.at(-1).cell_w, 15);
  assert.equal(host1.geometry().cols, 80);

  host1.destroy();
  host2.destroy();
  logCase("dpr_1_and_2_render_identical_grid_text_and_zoom_resize_repaints", "pass", {
    dprZoomEvents: host1.events.length,
  });
});

