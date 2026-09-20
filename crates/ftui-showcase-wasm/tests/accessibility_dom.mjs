// Real-DOM cases, loaded without network by accessibility_dom.py.
export function browserCases(api) {
  const results = [];
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const tick = () => new Promise(resolve => setTimeout(resolve, 10));
  const speech = (text, urgency = 'polite', node_id = '1') => ({
    text, urgency, node_id, reason: 'FocusedStateChanged',
  });
  const update = (frame, overrides = {}) => JSON.stringify({
    schema_version: 1, enabled: true, frame_id: String(frame), focus_id: '1',
    lines: ['textInput: Email. focused'], omitted_nodes: 0,
    announcements: [], dropped_count: 0, ...overrides,
  });
  async function run(name, body) {
    const root = document.createElement('div');
    root.id = 'a11y-proxy';
    root.setAttribute('aria-live', 'polite');
    root.setAttribute('aria-atomic', 'true');
    root.textContent = 'Static placeholder';
    document.body.appendChild(root);
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => false });
    const bridge = api.attachShowcaseAccessibility();
    try {
      await body(bridge, root);
      results.push({ name, passed: true });
    } catch (error) {
      results.push({ name, passed: false, error: String(error) });
    } finally {
      api.disposeShowcaseAccessibility(bridge);
      root.remove();
    }
  }
  return (async () => {
    await run('static proxy becomes separate mirror and primed live logs', async (bridge, root) => {
      check(bridge !== null, 'existing proxy was not attached');
      check(root.getAttribute('role') === 'document', 'not a browseable document');
      check(root.getAttribute('aria-live') === 'off', 'mirror ancestor is still live');
      check(bridge.polite.getAttribute('aria-live') === 'polite', 'polite policy');
      check(bridge.assertive.getAttribute('aria-live') === 'assertive', 'assertive policy');
      check(bridge.polite.getAttribute('aria-relevant') === 'additions', 'removals would speak');
      check(bridge.polite.getAttribute('aria-atomic') === 'false', 'history would repeat');
      check(!root.textContent.includes('Static placeholder'), 'placeholder survived');
    });
    await run('text is literal, Unicode is preserved, and DOM focus never moves', async (bridge, root) => {
      const control = document.getElementById('focus-target');
      control.focus();
      const text = '<img src=x onerror="window.injected=true"> é界🦀';
      check(bridge.update(update(0, { lines: [text], announcements: [speech(text)] })), 'update refused');
      await tick();
      check(root.querySelector('img') === null, 'untrusted markup became HTML');
      check(bridge.mirror.textContent === text, 'literal mirror text changed');
      check(bridge.polite.textContent === text, 'literal announcement changed');
      check(document.activeElement === control, 'bridge stole focus');
      check(!window.injected, 'script ran');
    });
    await run('first-frame speech survives a quiet follow-up before flushing', async bridge => {
      bridge.update(update(0, { announcements: [speech('required')] }));
      bridge.update(update(1));
      check(bridge.pending.length === 1, 'quiet frame erased queued speech');
      await tick();
      check(bridge.polite.children.length === 1, 'initial speech was lost or repeated');
      check(bridge.polite.textContent === 'required', 'wrong initial speech');
    });
    await run('duplicate and stale frame deliveries do not speak twice', async bridge => {
      const payload = update(2, { announcements: [speech('disabled')] });
      check(bridge.update(payload), 'new frame refused');
      check(!bridge.update(payload), 'same frame replay accepted');
      check(!bridge.update(update(1, { announcements: [speech('enabled')] })), 'stale accepted');
      await tick();
      check(bridge.polite.children.length === 1, 'duplicate DOM speech');
    });
    await run('equal text from different nodes and later transitions is not deduplicated', async bridge => {
      bridge.update(update(0, { announcements: [speech('selected', 'polite', '1'), speech('selected', 'polite', '2')] }));
      await tick();
      bridge.update(update(1, { announcements: [speech('selected')] }));
      await tick();
      check(bridge.polite.children.length === 3, 'legitimate equal text was swallowed');
    });
    await run('unchanged mirror nodes and text are not rewritten', async bridge => {
      bridge.update(update(0));
      const line = bridge.mirror.firstChild;
      const observer = new MutationObserver(() => {});
      observer.observe(bridge.mirror, { childList: true, subtree: true, characterData: true });
      bridge.update(update(1));
      check(bridge.mirror.firstChild === line, 'mirror node replaced');
      check(observer.takeRecords().length === 0, 'unchanged mirror mutated');
      observer.disconnect();
    });
    await run('64-bit frame ordering does not round adjacent IDs', async bridge => {
      check(bridge.update(update('9007199254740992')), 'first large frame');
      check(bridge.update(update('9007199254740993')), 'adjacent large frame was rounded');
      check(!bridge.update(update('9007199254740992')), 'older large frame accepted');
      check(!bridge.update(update('18446744073709551616')), 'out-of-range frame accepted');
    });
    await run('malformed and oversized packets leave DOM and pending speech unchanged', async (bridge, root) => {
      bridge.update(update(0));
      const html = root.innerHTML;
      for (const payload of [
        '{broken', update(1, { schema_version: 2 }), update(1, { frame_id: 1 }),
        update(1, { lines: Array(129).fill('x') }), update(1, { lines: ['x'.repeat(241)] }),
        update(1, { announcements: Array(9).fill(speech('x')) }),
        update(1, { announcements: [speech('x', 'invalid')] }),
        update(1, { announcements: [{ ...speech('x'), reason: 'unknown' }] }),
        update(1, { dropped_count: -1 }), update(1, { focus_id: 9007199254740992 }),
        ' '.repeat(250001),
      ]) check(!bridge.update(payload), 'invalid packet accepted');
      check(root.innerHTML === html, 'invalid data modified DOM');
      check(bridge.pending.length === 0, 'invalid data queued speech');
    });
    await run('all pending and live history remain bounded, preserving urgent speech first', async bridge => {
      bridge.update(update(0, { announcements: [speech('urgent', 'assertive')] }));
      for (let frame = 1; frame <= 50; frame += 1) {
        bridge.update(update(frame, { announcements: [speech(`polite ${frame}`)] }));
      }
      check(bridge.pending.length === 32, 'unbounded pending speech');
      check(bridge.transportDropped === 19, 'transport drop accounting');
      await tick();
      check(bridge.assertive.textContent === 'urgent', 'urgent message dropped first');
      check(bridge.polite.children.length === 16, 'unbounded polite history');
    });
    await run('hidden tabs clear queued speech and never replay background updates', async bridge => {
      bridge.update(update(0, { announcements: [speech('before hidden')] }));
      let hidden = true;
      Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden });
      document.dispatchEvent(new Event('visibilitychange'));
      bridge.update(update(1, { lines: ['background mirror'], announcements: [speech('background')] }));
      await tick();
      check(bridge.pending.length === 0 && bridge.polite.children.length === 0, 'hidden speech queued');
      check(bridge.mirror.textContent === 'background mirror', 'hidden mirror not updated');
      hidden = false;
      document.dispatchEvent(new Event('visibilitychange'));
      await tick();
      check(bridge.polite.children.length === 0, 'background speech replayed');
      bridge.update(update(2, { announcements: [speech('foreground')] }));
      await tick();
      check(bridge.polite.textContent === 'foreground', 'foreground speech not restored');
    });
    await run('disable clears content, rejects stale disable, and permits a fresh enabled frame', async (bridge, root) => {
      bridge.update(update(2, { announcements: [speech('private')] }));
      check(!bridge.update(update(1, { enabled: false })), 'stale disable accepted');
      check(bridge.update(update(2, { enabled: false })), 'same-frame explicit disable refused');
      await tick();
      check(root.textContent === '', 'disabled content retained');
      check(!bridge.update(update(2, { announcements: [speech('stale private')] })), 'disabled frame replayed');
      check(bridge.update(update(3, { announcements: [speech('fresh')] })), 'fresh enable refused');
      await tick();
      check(bridge.polite.textContent === 'fresh', 'reenabled speech wrong');
    });
    await run('replacing the owner blocks stale runners and stale disposal', async (old, root) => {
      old.update(update(0, { announcements: [speech('old')] }));
      const fresh = api.attachShowcaseAccessibility();
      check(!old.update(update(99, { lines: ['stale'] })), 'old runner still owns proxy');
      fresh.update(update(0, { lines: ['fresh'], announcements: [speech('new')] }));
      old.dispose();
      await tick();
      check(fresh.mirror.textContent === 'fresh', 'old dispose cleared new mirror');
      check(fresh.polite.textContent === 'new', 'old pending speech escaped');
      fresh.dispose();
      check(root.textContent === '', 'fresh disposal retained content');
    });
    await run('disposal cancels pending tasks and is idempotent', async (bridge, root) => {
      bridge.update(update(0, { announcements: [speech('private')] }));
      bridge.dispose();
      bridge.dispose();
      await tick();
      check(root.textContent === '', 'late flush after disposal');
      check(!bridge.update(update(1)), 'disposed runner accepted content');
    });
    // The generated WASM class is the seam here: use its public contract to
    // exercise the production adapter without claiming this compiles WASM.
    class RunnerContract {
      enabled = false;
      initialized = false;
      frame = 0;
      drains = 0;
      speech = [];
      renders = true;
      freed = false;
      destroyed = false;
      init() {
        if (!this.initialized) {
          this.initialized = true;
          this.speech = this.enabled ? [speech('initial focus')] : [];
        }
        return 'initialized';
      }
      step() {
        if (this.renders) { this.frame += 1; this.speech = []; }
        return { running: true, rendered: this.renders, frame_idx: this.frame + 1 };
      }
      setAccessibilityEnabled(enabled) { this.enabled = enabled; }
      takeAccessibilityUpdateJson() {
        this.drains += 1;
        const packet = update(this.frame, { enabled: this.enabled, announcements: this.speech });
        this.speech = [];
        return packet;
      }
      destroy() { this.destroyed = true; return 'destroyed'; }
      free() { this.freed = true; return 'freed'; }
    }
    await run('packaged runner enables before init and publishes the initial frame once', async (_, root) => {
      const Runner = api.withShowcaseAccessibility(RunnerContract);
      const runner = new Runner();
      try {
        check(runner.init() === 'initialized', 'init return value changed');
        check(runner.enabled, 'automatic collection was not enabled');
        check(runner.drains === 1, 'init was not drained');
        runner.init();
        check(runner.drains === 1, 'idempotent init drained twice');
        runner.renders = false;
        check(!runner.step().rendered && runner.drains === 1, 'idle step drained');
        await tick();
        check(root.querySelector('[role="log"][aria-live="polite"]').textContent === 'initial focus', 'no first-frame DOM speech');
      } finally { runner.free(); }
    });
    await run('step-before-init preserves first speech across the immediate quiet render', async (_, root) => {
      const runner = new (api.withShowcaseAccessibility(RunnerContract))();
      try {
        check(runner.step().rendered, 'step contract changed');
        check(runner.drains === 2, 'init and rendered step must each be drained');
        await tick();
        check(root.querySelector('[role="log"][aria-live="polite"]').textContent === 'initial focus', 'initial frame was overwritten');
      } finally { runner.free(); }
    });
    await run('explicit runner configuration chooses manual delivery without double speech', async (_, root) => {
      const runner = new (api.withShowcaseAccessibility(RunnerContract))();
      try {
        runner.setAccessibilityEnabled(true);
        runner.init();
        check(runner.drains === 0, 'manual host lost its drain');
        const packet = JSON.parse(runner.takeAccessibilityUpdateJson());
        check(packet.announcements.length === 1, 'manual first speech missing');
        await tick();
        check(!root.textContent.includes('initial focus'), 'manual and DOM paths both spoke');
      } finally { runner.free(); }
    });
    await run('runner destroy and free cancel DOM tasks and retain underlying cleanup', async (_, root) => {
      const Runner = api.withShowcaseAccessibility(RunnerContract);
      const destroyed = new Runner();
      destroyed.init();
      check(destroyed.destroy() === 'destroyed', 'underlying destroy did not run');
      check(!destroyed.enabled, 'destroy retained collection');
      await tick();
      check(root.textContent === '', 'destroy left a late announcement');
      destroyed.free();
      const freed = new Runner();
      freed.init();
      check(freed.free() === 'freed' && freed.freed, 'underlying free did not run');
      await tick();
      check(root.textContent === '', 'free left a late announcement');
    });
    await run('runner without a reserved DOM proxy remains default-off', async (_, root) => {
      root.remove();
      const count = document.body.children.length;
      const runner = new (api.withShowcaseAccessibility(RunnerContract))();
      try {
        runner.init();
        runner.step();
        check(!runner.enabled && runner.drains === 0, 'non-showcase host was opted in');
        check(document.body.children.length === count, 'adapter inserted unsolicited DOM');
      } finally { runner.free(); }
    });
    check(api.attachShowcaseAccessibility() === null, 'missing proxy should not create a body node');
    return results;
  })();
}

// Review controls are native HTML, not fabricated counterparts of TUI widgets.
export async function browserReviewCases(api) {
  const results = [];
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const tick = () => new Promise(resolve => setTimeout(resolve, 10));
  const speech = text => ({ node_id: '1', urgency: 'polite', reason: 'FocusedStateChanged', text });
  const packet = (frame, overrides = {}) => JSON.stringify({
    schema_version: 1, enabled: true, frame_id: String(frame), focus_id: '1',
    lines: ['textInput: Email. focused', 'checkbox: Agree. not checked'],
    omitted_nodes: 0, announcements: [], dropped_count: 0, ...overrides,
  });
  async function run(name, body) {
    const fixture = document.createElement('div');
    const canvas = document.createElement('canvas');
    canvas.id = 'terminal-canvas';
    canvas.tabIndex = 0;
    canvas.setAttribute('aria-describedby', 'existing-help');
    const root = document.createElement('div');
    root.id = 'a11y-proxy';
    root.style.cssText = 'position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0,0,0,0)';
    const keyboardProxy = document.createElement('textarea');
    keyboardProxy.id = 'mobile-kb-proxy';
    const outside = document.createElement('input');
    outside.setAttribute('aria-label', 'Outside input');
    fixture.append(canvas, root, keyboardProxy, outside);
    document.body.append(fixture);
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => false });
    const bridge = api.attachShowcaseAccessibility();
    const context = { bridge, review: bridge.review, root, canvas, keyboardProxy, outside, fixture };
    try {
      await body(context);
      results.push({ name, passed: true });
    } catch (error) {
      results.push({ name, passed: false, error: String(error) });
    } finally {
      api.disposeShowcaseAccessibility(bridge);
      fixture.remove();
    }
  }

  await run('review is discoverable outside the clipped proxy and waits for a snapshot', async ({ bridge, review, root, canvas }) => {
    check(review !== null, 'terminal review not attached');
    check(!root.contains(review.controls) && !root.contains(review.panel), 'controls inherit proxy clipping');
    check(review.panel.hidden && review.openButton.disabled, 'review offered before a frame');
    check(!review.open(), 'opened before a snapshot');
    check(review.openButton.getAttribute('aria-keyshortcuts') === 'Alt+Shift+R', 'shortcut missing');
    check(review.openButton.getAttribute('aria-controls') === review.panel.id, 'broken controls relation');
    check(canvas.getAttribute('aria-describedby').includes(review.hint.id), 'terminal exit hint missing');
    check(canvas.getAttribute('aria-describedby').includes('existing-help'), 'host description replaced');
    bridge.update(packet(0));
    check(!review.openButton.disabled, 'first frame did not enable review');
    check(review.panel.hidden && document.activeElement !== review.text, 'frame stole focus');
  });

  await run('review opens a literal bounded snapshot and announces its native readonly semantics', async ({ bridge, review, canvas, root }) => {
    const literal = '<img src=x onerror="window.reviewInjected=true"> é界🦀';
    bridge.update(packet(0, { lines: [literal], omitted_nodes: 7 }));
    canvas.focus();
    review.openButton.click();
    check(document.activeElement === review.text, 'review did not focus the native text control');
    check(review.text.readOnly && review.text.tagName === 'TEXTAREA', 'not native readonly text');
    check(review.text.value === `${literal}\n7 additional items omitted.`, 'snapshot changed or truncation hidden');
    check(review.openButton.getAttribute('aria-expanded') === 'true', 'open state not exposed');
    check(!review.panel.hasAttribute('aria-modal'), 'review incorrectly traps the page as a modal');
    check(!review.panel.querySelector('img') && !root.querySelector('img'), 'markup parsed');
    check(!window.reviewInjected, 'untrusted script ran');
    review.closeButton.click();
    check(document.activeElement === canvas && review.panel.hidden, 'return failed');
    check(review.text.value === '', 'closed snapshot retained');
  });

  await run('review freezes selection and content until an explicit refresh', async ({ bridge, review, canvas }) => {
    bridge.update(packet(0, { lines: ['original snapshot'] }));
    canvas.focus();
    review.open();
    review.text.setSelectionRange(2, 6, 'backward');
    bridge.update(packet(1, { lines: ['replacement snapshot'] }));
    check(review.text.value === 'original snapshot', 'background frame overwrote reading');
    check(review.text.selectionStart === 2 && review.text.selectionEnd === 6, 'selection reset');
    check(review.status.textContent.includes('New terminal content'), 'new content not discoverable');
    review.refreshButton.click();
    check(review.text.value === 'replacement snapshot', 'refresh not applied');
    check(review.text.selectionStart === 2 && review.text.selectionEnd === 6, 'refresh discarded selection');
    check(review.text.selectionDirection === 'backward', 'selection direction discarded');
    check(document.activeElement === review.text, 'refresh not returned to reading');
    check(review.status.textContent === 'Snapshot is up to date.', 'stale freshness status');
    review.refresh();
    check(review.text.selectionStart === 2 && review.text.selectionEnd === 6, 'unchanged refresh reset selection');
  });

  await run('review suppresses concurrent speech without replaying it on return', async ({ bridge, review, canvas }) => {
    bridge.update(packet(0, { announcements: [speech('before review')] }));
    canvas.focus();
    review.open();
    check(bridge.pending.length === 0, 'opening did not cancel pending speech');
    check(bridge.polite.getAttribute('aria-live') === 'off', 'speech competes with reading');
    bridge.update(packet(1, { announcements: [speech('while reviewing')] }));
    await tick();
    check(bridge.pending.length === 0 && bridge.polite.children.length === 0, 'reading queued speech');
    review.close(true);
    await tick();
    check(bridge.polite.getAttribute('aria-live') === 'polite', 'speech policy not restored');
    check(bridge.polite.children.length === 0, 'reading backlog replayed');
    bridge.update(packet(2, { announcements: [speech('after review')] }));
    await tick();
    check(bridge.polite.textContent === 'after review', 'new foreground transition was lost');
  });

  await run('review isolates input from host handlers without canceling native defaults', async ({ bridge, review, canvas, outside }) => {
    bridge.update(packet(0));
    canvas.focus();
    const seen = [];
    const host = event => seen.push(event.type);
    const types = ['keydown', 'keyup', 'beforeinput', 'input', 'copy', 'cut', 'paste',
      'compositionstart', 'compositionupdate', 'compositionend'];
    for (const type of types) window.addEventListener(type, host, true);
    try {
      const open = new KeyboardEvent('keydown', {
        key: 'R', code: 'KeyR', altKey: true, shiftKey: true, bubbles: true, cancelable: true,
      });
      canvas.dispatchEvent(open);
      check(open.defaultPrevented && review.opened, 'shortcut not captured');
      for (const type of types) {
        const event = type.startsWith('key')
          ? new KeyboardEvent(type, { key: 'a', code: 'KeyA', bubbles: true, cancelable: true })
          : new Event(type, { bubbles: true, cancelable: true });
        review.text.dispatchEvent(event);
        check(!event.defaultPrevented, `${type} browser default canceled`);
      }
      check(seen.length === 0, 'review leaked keys or clipboard/IME events to host');
      outside.dispatchEvent(new KeyboardEvent('keydown', { key: 'z', code: 'KeyZ', bubbles: true }));
      check(seen.length === 1, 'unrelated host input was swallowed');
    } finally {
      for (const type of types) window.removeEventListener(type, host, true);
    }
  });

  await run('Escape returns focus and consumes its release on the terminal', async ({ bridge, review, canvas }) => {
    bridge.update(packet(0));
    canvas.focus();
    review.open();
    const seen = [];
    const host = event => seen.push(event.type);
    window.addEventListener('keydown', host, true);
    window.addEventListener('keyup', host, true);
    try {
      review.text.dispatchEvent(new KeyboardEvent('keydown', {
        key: 'Escape', code: 'Escape', bubbles: true, cancelable: true,
      }));
      check(review.panel.hidden && document.activeElement === canvas, 'Escape did not return to canvas');
      canvas.dispatchEvent(new KeyboardEvent('keyup', { key: 'Escape', code: 'Escape', bubbles: true }));
      check(seen.length === 0, 'Escape release reached terminal');
      canvas.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', code: 'KeyA', bubbles: true }));
      check(seen.length === 1, 'normal terminal input not restored');
    } finally {
      window.removeEventListener('keydown', host, true);
      window.removeEventListener('keyup', host, true);
    }
  });

  await run('review respects IME, repeat, unrelated input and mobile focus ownership', async ({ bridge, review, keyboardProxy, outside, canvas }) => {
    bridge.update(packet(0));
    const chord = extra => new KeyboardEvent('keydown', {
      key: 'R', code: 'KeyR', altKey: true, shiftKey: true, bubbles: true, cancelable: true, ...extra,
    });
    outside.focus();
    outside.dispatchEvent(chord());
    check(!review.opened, 'shortcut hijacked unrelated control');
    canvas.focus();
    canvas.dispatchEvent(chord({ isComposing: true }));
    canvas.dispatchEvent(chord({ repeat: true }));
    check(!review.opened, 'IME/repeat opened review');
    keyboardProxy.focus();
    keyboardProxy.dispatchEvent(chord());
    check(review.opened && document.activeElement === review.text, 'mobile input cannot enter review');
    review.close(true);
    check(document.activeElement === keyboardProxy, 'mobile focus not restored');
    canvas.focus();
    review.open();
    outside.focus();
    review.close(true);
    check(document.activeElement === outside, 'close stole unrelated focus');
  });

  await run('disable erases review copies and reenable starts with a fresh snapshot', async ({ bridge, review, canvas }) => {
    bridge.update(packet(0, { lines: ['private original'] }));
    canvas.focus();
    review.open();
    bridge.update(packet(0, { enabled: false, lines: [], announcements: [] }));
    check(review.panel.hidden && review.text.value === '' && review.latest === null, 'disable retained private snapshot');
    check(review.openButton.disabled && document.activeElement === canvas, 'disable left focus hidden');
    check(!review.open(), 'disabled review reopened');
    bridge.update(packet(1, { lines: ['fresh'] }));
    check(review.open(), 'fresh frame not reviewable');
    check(review.text.value === 'fresh', 'stale disabled content revived');
  });

  await run('replacing a runner removes old review controls, listeners and descriptions', async ({ bridge: old, review, canvas, root }) => {
    old.update(packet(0));
    canvas.focus();
    review.open();
    const oldHint = review.hint.id;
    canvas.setAttribute('aria-describedby', `${canvas.getAttribute('aria-describedby')} concurrent-help`);
    const fresh = api.attachShowcaseAccessibility();
    try {
      check(document.activeElement === canvas, 'owner replacement lost focus');
      check(!review.panel.isConnected && !review.controls.isConnected, 'old controls survived');
      check(document.querySelectorAll('[data-ftui-review]').length === 1, 'two review panels');
      check(!canvas.getAttribute('aria-describedby').split(/\s+/).includes(oldHint), 'old hint survived');
      check(canvas.getAttribute('aria-describedby').includes('concurrent-help'), 'host hint lost');
      old.dispose();
      fresh.update(packet(0, { lines: ['new owner'] }));
      fresh.review.open();
      check(fresh.review.text.value === 'new owner', 'old disposal damaged new owner');
    } finally { fresh.dispose(); }
    check(root.textContent === '', 'disposed mirror retained');
    check(canvas.getAttribute('aria-describedby') === 'existing-help concurrent-help', 'description cleanup changed host tokens');
  });

  await run('disposal returns owned focus without stealing unrelated focus and cancels all UI', async ({ bridge, review, canvas, outside }) => {
    bridge.update(packet(0));
    canvas.focus();
    review.open();
    bridge.dispose();
    check(document.activeElement === canvas, 'dispose lost owned text focus');
    check(!review.panel.isConnected && !review.controls.isConnected, 'review UI retained');
    check(review.latest === null && review.text.value === '', 'disposed review retains content');
    outside.focus();
    bridge.dispose();
    check(document.activeElement === outside, 'idempotent dispose stole focus');
    const event = new KeyboardEvent('keydown', {
      key: 'R', code: 'KeyR', altKey: true, shiftKey: true, bubbles: true, cancelable: true,
    });
    canvas.dispatchEvent(event);
    check(!event.defaultPrevented, 'disposed keyboard listener survived');
  });

  await run('returning directly to canvas closes review without replay or hidden focus', async ({ bridge, review, canvas }) => {
    bridge.update(packet(0));
    canvas.focus();
    review.open();
    bridge.update(packet(1, { announcements: [speech('during review')] }));
    canvas.focus();
    check(!review.opened && review.panel.hidden, 'direct return left review active');
    check(document.activeElement === canvas, 'direct return changed focus again');
    check(review.text.value === '', 'direct return retained frozen content');
    await tick();
    check(bridge.polite.children.length === 0, 'direct return replayed suppressed speech');
  });

  await run('disabling or removing the focused review opener does not strand focus on body', async ({ bridge, review, canvas }) => {
    bridge.update(packet(0));
    review.openButton.focus();
    bridge.update(packet(0, { enabled: false, lines: [] }));
    check(document.activeElement === canvas, 'disabling the opener stranded focus');
    bridge.update(packet(1));
    review.openButton.focus();
    bridge.dispose();
    check(document.activeElement === canvas, 'removing the opener stranded focus');
  });

  return results;
}
