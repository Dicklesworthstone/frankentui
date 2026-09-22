// Text-only canvas accessibility bridge. Bundled with the verified WASM glue.
// No fabricated DOM controls/focus: the mirror is browseable, speech has one path.
// WAI-ARIA log semantics: https://www.w3.org/TR/wai-aria-1.2/#log
const MAX_LINES = 128;
const MAX_ANNOUNCEMENTS = 8;
const MAX_TEXT_CHARS = 240;
const MAX_PENDING = 32;
const MAX_HISTORY = 16;
const MAX_JSON_CHARS = 250_000;
const owners = new WeakMap();
const reasons = new Set([
  "FocusChanged",
  "FocusedStateChanged",
  "LiveRegionAdded",
  "LiveContentChanged",
  "LiveRegionChanged",
]);
let showcaseReviewSequence = 0;

function validId(value) {
  return (
    value === null ||
    (typeof value === "string" &&
      /^(0|[1-9][0-9]{0,19})$/.test(value) &&
      BigInt(value) <= 18446744073709551615n)
  );
}

function validText(value) {
  return (
    typeof value === "string" &&
    value.length <= MAX_TEXT_CHARS * 2 &&
    Array.from(value).length <= MAX_TEXT_CHARS
  );
}

function validCount(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function decodeUpdate(json) {
  if (typeof json !== "string" || json.length > MAX_JSON_CHARS) return null;
  let value;
  try {
    value = JSON.parse(json);
  } catch {
    return null;
  }
  if (
    !value ||
    value.schema_version !== 1 ||
    typeof value.enabled !== "boolean" ||
    !validId(value.frame_id) ||
    !validId(value.focus_id) ||
    !validCount(value.omitted_nodes) ||
    !validCount(value.dropped_count) ||
    !Array.isArray(value.lines) ||
    value.lines.length > MAX_LINES ||
    !value.lines.every(validText) ||
    !Array.isArray(value.announcements) ||
    value.announcements.length > MAX_ANNOUNCEMENTS ||
    !value.announcements.every(
      (item) =>
        item &&
        validId(item.node_id) &&
        (item.urgency === "polite" || item.urgency === "assertive") &&
        reasons.has(item.reason) &&
        validText(item.text),
    )
  )
    return null;
  if (value.frame_id === null && (value.lines.length || value.announcements.length)) return null;
  return value;
}

/**
 * An actual native text control for reviewing/copying the bounded snapshot.
 * It is not a set of pretend controls for the canvas widgets. The non-modal
 * panel keeps browser Tab, selection, scrolling and clipboard defaults; Escape
 * returns to the terminal. A documented chord escapes the canvas's Tab routing.
 * See https://www.w3.org/WAI/WCAG22/Understanding/no-keyboard-trap.html .
 */
class TerminalContentReview {
  constructor(bridge, terminal, keyboardProxy) {
    this.bridge = bridge;
    this.document = bridge.document;
    this.window = bridge.window;
    this.terminal = terminal;
    this.keyboardProxy = keyboardProxy?.ownerDocument === this.document ? keyboardProxy : null;
    this.opened = false;
    this.latest = null;
    this.returnTarget = null;
    this.ownedKeys = new Set();
    let id;
    do {
      id = `ftui-content-review-${++showcaseReviewSequence}`;
    } while (this.document.getElementById(id) || this.document.getElementById(`${id}-hint`));

    this.controls = this.document.createElement("div");
    this.controls.setAttribute("data-ftui-review-controls", "");
    this.controls.style.cssText =
      "position:fixed;top:0.5rem;right:0.5rem;z-index:20;font:14px system-ui;";
    this.openButton = this.document.createElement("button");
    this.openButton.type = "button";
    this.openButton.textContent = "Read terminal";
    this.openButton.disabled = true;
    this.openButton.setAttribute("aria-keyshortcuts", "Alt+Shift+R");
    this.openButton.setAttribute("aria-controls", id);
    this.openButton.setAttribute("aria-expanded", "false");
    this.openButton.title = "Read or copy terminal content (Alt+Shift+R)";
    this.hint = this.document.createElement("span");
    this.hint.id = `${id}-hint`;
    this.hint.hidden = true;
    this.hint.textContent =
      "Press Alt+Shift+R to read or copy terminal content. In content review, Escape returns to the terminal and Tab moves between browser controls.";
    this.openButton.setAttribute("aria-describedby", this.hint.id);
    this.controls.append(this.openButton, this.hint);

    this.panel = this.document.createElement("section");
    this.panel.id = id;
    this.panel.hidden = true;
    this.panel.setAttribute("data-ftui-review", "");
    this.panel.setAttribute("role", "region");
    this.panel.setAttribute("aria-label", "Terminal content review");
    this.panel.setAttribute("aria-live", "off");
    this.panel.style.cssText =
      "position:fixed;inset:12vh 2vw auto;max-height:80vh;overflow:auto;z-index:1000;padding:1rem;border:2px solid currentColor;background:Canvas;color:CanvasText;font:16px system-ui;box-sizing:border-box;";
    const heading = this.document.createElement("h2");
    heading.textContent = "Terminal content review";
    const help = this.document.createElement("p");
    help.id = `${id}-help`;
    help.textContent =
      "This read-only snapshot stays fixed while you read. Select and copy normally. Refresh to read newer content. Escape or Return to terminal closes this panel; Tab can leave it.";
    this.text = this.document.createElement("textarea");
    this.text.readOnly = true;
    this.text.spellcheck = false;
    this.text.wrap = "off";
    this.text.setAttribute("aria-label", "Terminal content snapshot");
    this.text.setAttribute("aria-describedby", help.id);
    this.text.setAttribute("data-ftui-review-text", "");
    this.text.style.cssText =
      "box-sizing:border-box;width:100%;height:45vh;font:16px monospace;background:Canvas;color:CanvasText;";
    this.status = this.document.createElement("p");
    this.status.setAttribute("data-ftui-review-status", "");
    // Status is deliberately not live: changing frames must not interrupt reading.
    this.status.setAttribute("aria-live", "off");
    this.refreshButton = this.document.createElement("button");
    this.refreshButton.type = "button";
    this.refreshButton.textContent = "Refresh snapshot";
    this.closeButton = this.document.createElement("button");
    this.closeButton.type = "button";
    this.closeButton.textContent = "Return to terminal";
    this.closeButton.style.marginInlineStart = "0.5rem";
    this.panel.append(heading, help, this.text, this.status, this.refreshButton, this.closeButton);

    this.openButton.addEventListener("click", () => this.open());
    this.refreshButton.addEventListener("click", () => this.refresh());
    this.closeButton.addEventListener("click", () => this.close(true));
    this.onInput = (event) => this.routeInput(event);
    this.onBlur = () => this.ownedKeys.clear();
    this.onFocus = (event) => {
      if (this.opened && this.isTerminal(event.target)) this.close(false);
    };
    this.inputEvents = [
      "keydown",
      "keyup",
      "beforeinput",
      "input",
      "paste",
      "copy",
      "cut",
      "compositionstart",
      "compositionupdate",
      "compositionend",
    ];
    // Installed during runner init, before the showcase's window input handlers.
    // Do not cancel browser defaults, apart from the explicit open/close keys.
    for (const type of this.inputEvents) this.window.addEventListener(type, this.onInput, true);
    this.window.addEventListener("blur", this.onBlur);
    this.window.addEventListener("focusin", this.onFocus, true);
    bridge.root.after(this.controls, this.panel);
    const describedBy = (terminal.getAttribute("aria-describedby") || "")
      .split(/\s+/)
      .filter(Boolean);
    terminal.setAttribute("aria-describedby", [...describedBy, this.hint.id].join(" "));
  }

  owns(target) {
    return (
      target instanceof this.window.Node &&
      (this.controls.contains(target) || this.panel.contains(target))
    );
  }

  isTerminal(target) {
    return (
      target === this.terminal || (this.keyboardProxy !== null && target === this.keyboardProxy)
    );
  }

  routeInput(event) {
    const key = event.code || String(event.key || "").toLowerCase();
    if (event.type === "keyup" && this.ownedKeys.delete(key)) {
      event.stopImmediatePropagation();
      return;
    }
    const owned = this.owns(event.target);
    const shortcut =
      event.type === "keydown" &&
      !event.isComposing &&
      event.altKey &&
      event.shiftKey &&
      !event.ctrlKey &&
      !event.metaKey &&
      (event.code === "KeyR" || String(event.key).toLowerCase() === "r") &&
      (owned || this.isTerminal(event.target));
    if (!owned && !shortcut) return;
    if (event.type === "keydown") {
      // Bound retained release identities even for synthetic/virtual keyboards.
      if (this.ownedKeys.size >= 64) this.ownedKeys.clear();
      this.ownedKeys.add(key);
    }
    event.stopImmediatePropagation();
    if (shortcut) {
      event.preventDefault();
      if (!event.repeat) this.open();
    } else if (
      event.type === "keydown" &&
      event.key === "Escape" &&
      !event.isComposing &&
      !event.ctrlKey &&
      !event.altKey &&
      !event.metaKey
    ) {
      event.preventDefault();
      this.close(true);
    }
  }

  update(value) {
    const omitted = value.omitted_nodes ? `\n${value.omitted_nodes} additional items omitted.` : "";
    this.latest = { frame: value.frame_id, text: value.lines.join("\n") + omitted };
    this.openButton.disabled = false;
    if (this.opened) this.updateStatus();
  }

  updateStatus() {
    const status =
      this.latest?.text === this.text.value
        ? "Snapshot is up to date."
        : "New terminal content is available. Refresh when ready.";
    if (this.status.textContent !== status) this.status.textContent = status;
  }

  open() {
    if (this.bridge.disposed || !this.bridge.enabled || !this.latest) return false;
    if (!this.opened) {
      const active = this.document.activeElement;
      this.returnTarget = this.isTerminal(active) ? active : this.terminal;
      this.opened = true;
      this.bridge.setLivePolicies();
      this.bridge.clearSpeech();
      this.panel.hidden = false;
      this.openButton.setAttribute("aria-expanded", "true");
      this.text.value = this.latest.text;
      this.text.setSelectionRange(0, 0);
      this.updateStatus();
    }
    this.text.focus({ preventScroll: true });
    return true;
  }

  refresh() {
    if (!this.opened || !this.latest) return false;
    if (this.text.value !== this.latest.text) {
      const { selectionStart, selectionEnd, selectionDirection, scrollTop, scrollLeft } = this.text;
      this.text.value = this.latest.text;
      this.text.setSelectionRange(selectionStart, selectionEnd, selectionDirection);
      this.text.scrollTop = scrollTop;
      this.text.scrollLeft = scrollLeft;
    }
    this.updateStatus();
    this.text.focus({ preventScroll: true });
    return true;
  }

  close(restoreFocus) {
    if (!this.opened) return;
    const ownedFocus = this.owns(this.document.activeElement);
    this.opened = false;
    this.panel.hidden = true;
    this.openButton.setAttribute("aria-expanded", "false");
    this.text.value = "";
    this.status.textContent = "";
    this.bridge.setLivePolicies();
    const target = this.returnTarget?.isConnected ? this.returnTarget : this.terminal;
    this.returnTarget = null;
    if (
      restoreFocus &&
      ownedFocus &&
      !this.document.hidden &&
      target?.isConnected &&
      !target.disabled
    )
      target.focus({ preventScroll: true });
  }

  clear() {
    // Disabling a focused native button blurs it to body. Move focus first so
    // disabling/replacing a runner does not strand the reader outside the TUI.
    const openerFocused = this.document.activeElement === this.openButton;
    this.close(true);
    this.latest = null;
    this.text.value = "";
    this.status.textContent = "";
    if (
      openerFocused &&
      !this.document.hidden &&
      this.terminal.isConnected &&
      !this.terminal.disabled
    )
      this.terminal.focus({ preventScroll: true });
    this.openButton.disabled = true;
  }

  dispose() {
    this.clear();
    for (const type of this.inputEvents) this.window.removeEventListener(type, this.onInput, true);
    this.window.removeEventListener("blur", this.onBlur);
    this.window.removeEventListener("focusin", this.onFocus, true);
    this.ownedKeys.clear();
    // Remove only our description token, retaining concurrent host additions.
    const ids = (this.terminal.getAttribute("aria-describedby") || "")
      .split(/\s+/)
      .filter((id) => id && id !== this.hint.id);
    if (ids.length) this.terminal.setAttribute("aria-describedby", ids.join(" "));
    else this.terminal.removeAttribute("aria-describedby");
    this.controls.remove();
    this.panel.remove();
  }
}

/** Own one host-supplied proxy. All content is inserted with textContent. */
export class ShowcaseAccessibilityBridge {
  constructor(root, { terminal = null, keyboardProxy = null } = {}) {
    if (!root?.ownerDocument) throw new TypeError("An accessibility proxy element is required");
    owners.get(root)?.dispose();
    this.root = root;
    this.document = root.ownerDocument;
    this.window = this.document.defaultView;
    this.disposed = false;
    this.enabled = true;
    this.lastFrame = null;
    this.pending = [];
    this.timer = null;
    this.transportDropped = 0;
    this.review = null;
    owners.set(root, this);

    // The old static proxy was itself live. Turn that off BEFORE replacing it,
    // otherwise the mirror and dedicated logs could both speak the same update.
    root.setAttribute("role", "document");
    root.setAttribute("aria-live", "off");
    root.setAttribute("aria-atomic", "false");
    this.mirror = this.document.createElement("div");
    this.mirror.setAttribute("data-ftui-mirror", "");
    this.omitted = this.document.createElement("p");
    this.omitted.setAttribute("data-ftui-omitted", "");
    this.polite = this.makeLog("polite");
    this.assertive = this.makeLog("assertive");
    root.replaceChildren(this.mirror, this.omitted, this.polite, this.assertive);
    this.onVisibility = () => {
      if (this.document.hidden) this.clearSpeech();
      this.setLivePolicies();
    };
    this.document.addEventListener("visibilitychange", this.onVisibility);
    if (
      terminal?.ownerDocument === this.document &&
      terminal.isConnected &&
      typeof terminal.focus === "function" &&
      !root.contains(terminal)
    ) {
      this.review = new TerminalContentReview(this, terminal, keyboardProxy);
    }
    this.setLivePolicies();
  }

  makeLog(urgency) {
    const log = this.document.createElement("div");
    log.setAttribute("role", "log");
    log.setAttribute(
      "aria-label",
      `${urgency === "assertive" ? "Urgent" : "Polite"} terminal announcements`,
    );
    log.setAttribute("aria-live", urgency);
    log.setAttribute("aria-atomic", "false");
    log.setAttribute("aria-relevant", "additions");
    return log;
  }

  setLivePolicies() {
    const silent = !this.enabled || this.document.hidden || this.disposed || this.review?.opened;
    this.polite.setAttribute("aria-live", silent ? "off" : "polite");
    this.assertive.setAttribute("aria-live", silent ? "off" : "assertive");
  }

  clearSpeech() {
    if (this.timer !== null) this.window.clearTimeout(this.timer);
    this.timer = null;
    this.pending.length = 0;
    this.polite.replaceChildren();
    this.assertive.replaceChildren();
  }

  /** Returns false for malformed, replayed, stale, or no-longer-owned updates. */
  update(json) {
    if (this.disposed || owners.get(this.root) !== this) return false;
    const value = decodeUpdate(json);
    if (!value) return false; // Never log the rejected payload or exception.
    if (!value.enabled) {
      const frame = value.frame_id === null ? null : BigInt(value.frame_id);
      if (this.lastFrame !== null && (frame === null || frame < this.lastFrame)) return false;
      if (frame !== null) this.lastFrame = frame;
      this.enabled = false;
      this.setLivePolicies();
      this.clearSpeech();
      this.review?.clear();
      this.mirror.replaceChildren();
      this.omitted.textContent = "";
      this.root.removeAttribute("data-ftui-focus");
      return true;
    }
    if (value.frame_id === null) return false; // Collection enabled, no rendered frame yet.
    const frame = BigInt(value.frame_id);
    if (this.lastFrame !== null && frame <= this.lastFrame) return false;
    this.lastFrame = frame;
    this.enabled = true;
    this.setLivePolicies();
    this.root.setAttribute("data-ftui-frame", value.frame_id);
    if (value.focus_id === null) this.root.removeAttribute("data-ftui-focus");
    else this.root.setAttribute("data-ftui-focus", value.focus_id);

    // Preserve nodes and unchanged text so repeated frames do not reset a
    // screen reader's browse position or churn the browser accessibility tree.
    value.lines.forEach((text, index) => {
      let line = this.mirror.children[index];
      if (!line) {
        line = this.document.createElement("p");
        this.mirror.appendChild(line);
      }
      if (line.textContent !== text) line.textContent = text;
    });
    while (this.mirror.children.length > value.lines.length) this.mirror.lastChild.remove();
    const omitted = value.omitted_nodes ? `${value.omitted_nodes} additional items omitted.` : "";
    if (this.omitted.textContent !== omitted) this.omitted.textContent = omitted;
    this.root.setAttribute("data-ftui-policy-dropped", String(value.dropped_count));
    this.review?.update(value);

    // Background-tab updates refresh the mirror but never queue stale speech
    // to be replayed when the user returns to the tab. Review likewise pauses
    // synthetic speech while the user reads a deliberately frozen snapshot.
    if (!this.document.hidden && !this.review?.opened) {
      this.pending.push(...value.announcements.filter((item) => item.text.trim()));
      while (this.pending.length > MAX_PENDING) {
        const polite = this.pending.findIndex((item) => item.urgency === "polite");
        this.pending.splice(polite < 0 ? 0 : polite, 1);
        this.transportDropped += 1;
      }
      this.root.setAttribute("data-ftui-transport-dropped", String(this.transportDropped));
      if (this.pending.length && this.timer === null) {
        // Give the initially empty live regions a task boundary before their
        // first additions. A later quiet render must not erase queued speech.
        this.timer = this.window.setTimeout(() => this.flush(), 0);
      }
    }
    return true;
  }

  flush() {
    this.timer = null;
    if (
      this.disposed ||
      !this.enabled ||
      this.document.hidden ||
      this.review?.opened ||
      owners.get(this.root) !== this
    ) {
      this.clearSpeech();
      return;
    }
    for (const item of this.pending.splice(0)) {
      const log = item.urgency === "assertive" ? this.assertive : this.polite;
      const entry = this.document.createElement("p");
      entry.textContent = item.text;
      // Append a distinct node even when two legitimate transitions have the
      // same text. Frame identity, not text equality, suppresses replay.
      while (log.children.length >= MAX_HISTORY) log.firstChild.remove();
      log.appendChild(entry);
    }
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.setLivePolicies();
    this.clearSpeech();
    this.review?.dispose();
    this.review = null;
    this.document.removeEventListener("visibilitychange", this.onVisibility);
    if (owners.get(this.root) === this) {
      this.root.replaceChildren();
      for (const attr of [
        "data-ftui-frame",
        "data-ftui-focus",
        "data-ftui-policy-dropped",
        "data-ftui-transport-dropped",
      ])
        this.root.removeAttribute(attr);
      owners.delete(this.root);
    }
  }
}

// No DOM access at module load: workers and native test imports remain usable.
// Only the showcase's existing reserved proxy is auto-bound; no body insertion.
export function attachShowcaseAccessibility() {
  const root = globalThis.document?.getElementById("a11y-proxy");
  return root
    ? new ShowcaseAccessibilityBridge(root, {
        terminal: root.ownerDocument.getElementById("terminal-canvas"),
        keyboardProxy: root.ownerDocument.getElementById("mobile-kb-proxy"),
      })
    : null;
}

export function publishShowcaseAccessibility(bridge, json) {
  return bridge instanceof ShowcaseAccessibilityBridge && bridge.update(json);
}

export function disposeShowcaseAccessibility(bridge) {
  if (bridge instanceof ShowcaseAccessibilityBridge) bridge.dispose();
}

/**
 * Adapt the generated wasm-bindgen class without editing its generated methods.
 * build-wasm.sh appends this import-free module to the verified runner glue and
 * replaces the live ShowcaseRunner export with this subclass. The manifest
 * therefore covers every executed bridge byte, even with the Blob-URL loader.
 */
export function withShowcaseAccessibility(Base) {
  for (const name of [
    "init",
    "step",
    "destroy",
    "free",
    "setAccessibilityEnabled",
    "takeAccessibilityUpdateJson",
  ]) {
    if (typeof Base?.prototype?.[name] !== "function") {
      throw new TypeError(`ShowcaseRunner is missing ${name}`);
    }
  }
  return class extends Base {
    #accessibilityBridge = null;
    #automaticAccessibility = true;
    #initializedAccessibility = false;

    #disposeAccessibility() {
      disposeShowcaseAccessibility(this.#accessibilityBridge);
      this.#accessibilityBridge = null;
    }

    #publishAccessibility() {
      if (!this.#accessibilityBridge) return;
      try {
        publishShowcaseAccessibility(
          this.#accessibilityBridge,
          super.takeAccessibilityUpdateJson(),
        );
      } catch {
        // A broken host DOM must not fail visual rendering or log private text.
        this.#disposeAccessibility();
      }
    }

    init() {
      if (this.#initializedAccessibility) return super.init();
      if (this.#automaticAccessibility) {
        try {
          this.#accessibilityBridge = attachShowcaseAccessibility();
          if (this.#accessibilityBridge) super.setAccessibilityEnabled(true);
        } catch {
          this.#disposeAccessibility();
        }
      }
      let result;
      try {
        result = super.init();
      } catch (error) {
        this.#disposeAccessibility();
        throw error;
      }
      this.#initializedAccessibility = true;
      // Drain BEFORE another step can replace the initial frame's speech.
      this.#publishAccessibility();
      return result;
    }

    step() {
      // RunnerCore permits step-before-init; preserve that contract while
      // delivering its initial frame before the first ordinary host step.
      if (!this.#initializedAccessibility) this.init();
      const result = super.step();
      if (result.rendered) this.#publishAccessibility();
      return result;
    }

    setAccessibilityEnabled(enabled) {
      // Explicit host configuration selects manual delivery, never two speech
      // paths at once. It also works before init to opt out of automatic DOM.
      this.#automaticAccessibility = false;
      this.#disposeAccessibility();
      return super.setAccessibilityEnabled(enabled);
    }

    destroy() {
      this.#automaticAccessibility = false;
      this.#disposeAccessibility();
      super.setAccessibilityEnabled(false);
      return super.destroy();
    }

    free() {
      this.#automaticAccessibility = false;
      this.#disposeAccessibility();
      return super.free();
    }
  };
}
