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
  'FocusChanged', 'FocusedStateChanged', 'LiveRegionAdded',
  'LiveContentChanged', 'LiveRegionChanged',
]);

function validId(value) {
  return value === null || (typeof value === 'string'
    && /^(0|[1-9][0-9]{0,19})$/.test(value)
    && BigInt(value) <= 18446744073709551615n);
}

function validText(value) {
  return typeof value === 'string' && value.length <= MAX_TEXT_CHARS * 2
    && Array.from(value).length <= MAX_TEXT_CHARS;
}

function validCount(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function decodeUpdate(json) {
  if (typeof json !== 'string' || json.length > MAX_JSON_CHARS) return null;
  let value;
  try { value = JSON.parse(json); } catch { return null; }
  if (!value || value.schema_version !== 1 || typeof value.enabled !== 'boolean'
      || !validId(value.frame_id) || !validId(value.focus_id)
      || !validCount(value.omitted_nodes) || !validCount(value.dropped_count)
      || !Array.isArray(value.lines) || value.lines.length > MAX_LINES
      || !value.lines.every(validText)
      || !Array.isArray(value.announcements)
      || value.announcements.length > MAX_ANNOUNCEMENTS
      || !value.announcements.every(item => item && validId(item.node_id)
        && (item.urgency === 'polite' || item.urgency === 'assertive')
        && reasons.has(item.reason) && validText(item.text))) return null;
  if (value.frame_id === null && (value.lines.length || value.announcements.length)) return null;
  return value;
}

/** Own one host-supplied proxy. All content is inserted with textContent. */
export class ShowcaseAccessibilityBridge {
  constructor(root) {
    if (!root?.ownerDocument) throw new TypeError('An accessibility proxy element is required');
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
    owners.set(root, this);

    // The old static proxy was itself live. Turn that off BEFORE replacing it,
    // otherwise the mirror and dedicated logs could both speak the same update.
    root.setAttribute('role', 'document');
    root.setAttribute('aria-live', 'off');
    root.setAttribute('aria-atomic', 'false');
    this.mirror = this.document.createElement('div');
    this.mirror.setAttribute('data-ftui-mirror', '');
    this.omitted = this.document.createElement('p');
    this.omitted.setAttribute('data-ftui-omitted', '');
    this.polite = this.makeLog('polite');
    this.assertive = this.makeLog('assertive');
    root.replaceChildren(this.mirror, this.omitted, this.polite, this.assertive);
    this.onVisibility = () => {
      if (this.document.hidden) this.clearSpeech();
      this.setLivePolicies();
    };
    this.document.addEventListener('visibilitychange', this.onVisibility);
    this.setLivePolicies();
  }

  makeLog(urgency) {
    const log = this.document.createElement('div');
    log.setAttribute('role', 'log');
    log.setAttribute('aria-label', `${urgency === 'assertive' ? 'Urgent' : 'Polite'} terminal announcements`);
    log.setAttribute('aria-live', urgency);
    log.setAttribute('aria-atomic', 'false');
    log.setAttribute('aria-relevant', 'additions');
    return log;
  }

  setLivePolicies() {
    const silent = !this.enabled || this.document.hidden || this.disposed;
    this.polite.setAttribute('aria-live', silent ? 'off' : 'polite');
    this.assertive.setAttribute('aria-live', silent ? 'off' : 'assertive');
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
      this.mirror.replaceChildren();
      this.omitted.textContent = '';
      this.root.removeAttribute('data-ftui-focus');
      return true;
    }
    if (value.frame_id === null) return false; // Collection enabled, no rendered frame yet.
    const frame = BigInt(value.frame_id);
    if (this.lastFrame !== null && frame <= this.lastFrame) return false;
    this.lastFrame = frame;
    this.enabled = true;
    this.setLivePolicies();
    this.root.setAttribute('data-ftui-frame', value.frame_id);
    if (value.focus_id === null) this.root.removeAttribute('data-ftui-focus');
    else this.root.setAttribute('data-ftui-focus', value.focus_id);

    // Preserve nodes and unchanged text so repeated frames do not reset a
    // screen reader's browse position or churn the browser accessibility tree.
    value.lines.forEach((text, index) => {
      let line = this.mirror.children[index];
      if (!line) {
        line = this.document.createElement('p');
        this.mirror.appendChild(line);
      }
      if (line.textContent !== text) line.textContent = text;
    });
    while (this.mirror.children.length > value.lines.length) this.mirror.lastChild.remove();
    const omitted = value.omitted_nodes ? `${value.omitted_nodes} additional items omitted.` : '';
    if (this.omitted.textContent !== omitted) this.omitted.textContent = omitted;
    this.root.setAttribute('data-ftui-policy-dropped', String(value.dropped_count));

    // Background-tab updates refresh the mirror but never queue stale speech
    // to be replayed when the user returns to the tab.
    if (!this.document.hidden) {
      this.pending.push(...value.announcements.filter(item => item.text.trim()));
      while (this.pending.length > MAX_PENDING) {
        const polite = this.pending.findIndex(item => item.urgency === 'polite');
        this.pending.splice(polite < 0 ? 0 : polite, 1);
        this.transportDropped += 1;
      }
      this.root.setAttribute('data-ftui-transport-dropped', String(this.transportDropped));
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
    if (this.disposed || !this.enabled || this.document.hidden
        || owners.get(this.root) !== this) {
      this.clearSpeech();
      return;
    }
    for (const item of this.pending.splice(0)) {
      const log = item.urgency === 'assertive' ? this.assertive : this.polite;
      const entry = this.document.createElement('p');
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
    this.document.removeEventListener('visibilitychange', this.onVisibility);
    if (owners.get(this.root) === this) {
      this.root.replaceChildren();
      for (const attr of ['data-ftui-frame', 'data-ftui-focus',
        'data-ftui-policy-dropped', 'data-ftui-transport-dropped']) this.root.removeAttribute(attr);
      owners.delete(this.root);
    }
  }
}

// No DOM access at module load: workers and native test imports remain usable.
// Only the showcase's existing reserved proxy is auto-bound; no body insertion.
export function attachShowcaseAccessibility() {
  const root = globalThis.document?.getElementById('a11y-proxy');
  return root ? new ShowcaseAccessibilityBridge(root) : null;
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
  for (const name of ['init', 'step', 'destroy', 'free',
    'setAccessibilityEnabled', 'takeAccessibilityUpdateJson']) {
    if (typeof Base?.prototype?.[name] !== 'function') {
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
        publishShowcaseAccessibility(this.#accessibilityBridge, super.takeAccessibilityUpdateJson());
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
