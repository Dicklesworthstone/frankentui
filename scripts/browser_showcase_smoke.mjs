// Run inside a native DSR job against build-wasm.sh's site directory.
// Direct CDP avoids browser-automation wrappers' implicit temporary cleanup.
// Node 22+; Chrome must already be running with a retained profile/CDP port.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { resolve, extname } from "node:path";

const [siteArg, outputArg, endpoint] = process.argv.slice(2);
if (!siteArg || !outputArg || !endpoint) throw new Error("usage: node browser_showcase_smoke.mjs SITE NEW_OUTPUT CDP_URL");
const site = resolve(siteArg);
const output = resolve(outputArg);
await mkdir(output);
await mkdir(`${output}/downloads`);
const observations = { cases: [], console: [], pageErrors: [], browserLog: [], injections: [], downloads: [] };
let fault = null;
const incompatibleApi = Buffer.from('\nconst originalContract = FrankenTermWeb.prototype.apiContract;\nFrankenTermWeb.prototype.apiContract = function () { return {...originalContract.call(this), apiVersion:"9.0.0"}; };\n');
const server = createServer(async (req, res) => {
  try {
    const name = decodeURIComponent(new URL(req.url, "http://localhost").pathname);
    const path = resolve(site, name === "/" ? "index.html" : `.${name}`);
    if (!path.startsWith(`${site}/`)) { res.writeHead(403).end(); return; }
    if (fault === "missing" && name === "/pkg/FrankenTerm.js") {
      observations.injections.push(fault); res.writeHead(404).end(); return;
    }
    let body = await readFile(path);
    if (fault === "corrupt" && name === "/pkg/ftui_showcase_wasm.js") {
      // Still-valid JavaScript: rejection must be integrity, not syntax.
      body = Buffer.concat([body, Buffer.from("\n// planted stale glue\n")]);
      observations.injections.push(fault);
    }
    if (fault === "revision" && name === "/pkg/manifest.json") {
      const manifest = JSON.parse(body);
      manifest.renderer.revision = "0".repeat(40);
      body = Buffer.from(JSON.stringify(manifest));
      observations.injections.push(fault);
    }
    if (fault === "abi" && name === "/pkg/manifest.json") {
      const manifest = JSON.parse(body);
      const changed = Buffer.concat([await readFile(`${site}/pkg/FrankenTerm.js`), incompatibleApi]);
      manifest.files["FrankenTerm.js"] = createHash("sha256").update(changed).digest("hex");
      body = Buffer.from(JSON.stringify(manifest));
    }
    if (fault === "abi" && name === "/pkg/FrankenTerm.js") {
      body = Buffer.concat([body, incompatibleApi]);
      observations.injections.push(fault);
    }
    const mime = { ".html": "text/html", ".js": "text/javascript", ".wasm": "application/wasm", ".json": "application/json", ".woff2": "font/woff2" };
    res.writeHead(200, { "Content-Type": mime[extname(path)] || "text/plain", "Cache-Control": "no-store" });
    res.end(body);
  } catch (error) {
    if (error.code !== "ENOENT") console.error(error);
    res.writeHead(404).end();
  }
});
await new Promise((done) => server.listen(0, "127.0.0.1", done));
const url = `http://127.0.0.1:${server.address().port}/?jsonl=1`;
let socket;
const pending = new Map();
let nextId = 1;
let sessionId;
function send(method, params = {}, session = sessionId) {
  const id = nextId++;
  return new Promise((done, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 30000);
    pending.set(id, { done, reject, timer });
    socket.send(JSON.stringify({ id, method, params, ...(session ? { sessionId: session } : {}) }));
  });
}
async function evaluate(expression) {
  const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true, timeout: 25000 });
  if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
  return result.result.value;
}
async function waitFor(check, name, timeout = 30000) {
  const until = Date.now() + timeout;
  while (Date.now() < until) {
    if (await check()) return;
    await new Promise((done) => setTimeout(done, 100));
  }
  throw new Error(`Timed out: ${name}`);
}
const errorText = 'document.getElementById("error-overlay")?.textContent || ""';
const statusText = 'document.getElementById("status")?.textContent || ""';
const linkExpr = 'document.querySelector(\'a[download="frankentui-unprocessed-input.jsonl"]\')';
const records = (start = 0) => observations.console.slice(start).flatMap((text) => { try { return [JSON.parse(text)]; } catch { return []; } });
async function freshScenario(name) {
  const start = observations.console.length;
  await send("Page.navigate", { url: `${url}&scenario=${name}` });
  await waitFor(() => records(start).some((row) => row.event === "frame"), `fresh ${name} frame`);
  assert.equal(await evaluate(errorText), "");
  return observations.console.length;
}
async function recoveredInput(name) {
  await waitFor(() => evaluate(`Boolean(${linkExpr})`), name);
  const payload = await evaluate(`(async () => (await fetch(${linkExpr}.href)).text())()`);
  return payload.trim().split("\n").map((line) => JSON.parse(line));
}
async function screenshot(name) {
  const shot = await send("Page.captureScreenshot", { format: "png" });
  await writeFile(`${output}/${name}.png`, Buffer.from(shot.data, "base64"), { flag: "wx" });
  // Inspect compositor output, not WebGPU canvas readback after presentation
  // (which can legitimately be transparent). Exclude the DOM status/link strip.
  const colors = await evaluate(`(async () => {
    const image = new Image(); image.src = "data:image/png;base64,${shot.data}"; await image.decode();
    const canvas = document.createElement("canvas"); canvas.width = image.width; canvas.height = image.height;
    const ctx = canvas.getContext("2d"); ctx.drawImage(image, 0, 0);
    const data = ctx.getImageData(20, 20, canvas.width - 40, canvas.height - 80).data;
    const colors = new Set();
    for (let i = 0; i < data.length; i += 16) colors.add(Array.from(data.slice(i, i + 3)).join(","));
    return colors.size;
  })()`);
  observations.cases.push({ screenshot: name, interiorColors: colors });
  assert.ok(colors >= 16, `${name}: actual showcase pixels must be visible, got ${colors} colors`);
}
try {
  const response = await fetch(`${endpoint}/json/version`, { headers: { "User-Agent": "OpenAI File Downloader, XaiImageApiFetch/1.0" } });
  assert.ok(response.ok);
  observations.browser = await response.json();
  socket = new WebSocket(observations.browser.webSocketDebuggerUrl);
  await new Promise((done, reject) => { socket.addEventListener("open", done, { once: true }); socket.addEventListener("error", reject, { once: true }); });
  socket.addEventListener("message", ({ data }) => {
    const event = JSON.parse(data);
    if (event.id) {
      const call = pending.get(event.id);
      if (!call) return;
      pending.delete(event.id); clearTimeout(call.timer);
      if (event.error) call.reject(new Error(JSON.stringify(event.error))); else call.done(event.result);
    } else if (event.method === "Runtime.consoleAPICalled") {
      const text = event.params.args.map((arg) => arg.value ?? arg.description).join(" ");
      observations.console.push(text); console.log(text);
    } else if (event.method === "Runtime.exceptionThrown") {
      observations.pageErrors.push(event.params.exceptionDetails);
    } else if (event.method === "Log.entryAdded") observations.browserLog.push(event.params.entry);
    else if (event.method.startsWith("Browser.download")) observations.downloads.push(event);
  });
  observations.gpu = (await send("SystemInfo.getInfo", {}, null)).gpu;
  const target = await send("Target.createTarget", { url: "about:blank" });
  sessionId = (await send("Target.attachToTarget", { targetId: target.targetId, flatten: true })).sessionId;
  await send("Runtime.enable"); await send("Page.enable"); await send("Log.enable");
  await send("Browser.setDownloadBehavior", { behavior: "allow", downloadPath: `${output}/downloads`, eventsEnabled: true }, null);
  await send("Emulation.setDeviceMetricsOverride", { width: 960, height: 640, deviceScaleFactor: 1, mobile: false });
  await send("Page.navigate", { url });
  await waitFor(async () => records().some((row) => row.event === "init") || Boolean(await evaluate(errorText)), "real startup", 120000);
  assert.equal(await evaluate(errorText), "", "real package must initialize");
  await waitFor(() => records().some((row) => row.event === "frame"), "first presented frame");
  await waitFor(() => evaluate('getComputedStyle(document.getElementById("loading-overlay")).opacity === "0"'), "loading overlay hidden");
  observations.renderer = records().find((row) => row.event === "init");
  observations.display = await evaluate(`(() => {
    const canvas = document.getElementById("terminal-canvas");
    const copy = document.createElement("canvas"); copy.width = canvas.width; copy.height = canvas.height;
    const ctx = copy.getContext("2d"); ctx.drawImage(canvas, 0, 0);
    const data = ctx.getImageData(0, 0, copy.width, copy.height).data;
    const colors = new Set(); let opaque = 0;
    for (let i = 0; i < data.length; i += 32) { colors.add(Array.from(data.slice(i, i + 4)).join(",")); if (data[i + 3]) opaque++; }
    return { width: canvas.width, height: canvas.height, colors: colors.size, opaque,
      firstColors: [...colors].slice(0, 8), background: getComputedStyle(document.body).backgroundColor,
      forcedColors: matchMedia("(forced-colors: active)").matches,
      topElement: document.elementFromPoint(innerWidth / 2, innerHeight / 2)?.id,
      canvasPng: copy.toDataURL("image/png") };
  })()`);
  await writeFile(`${output}/canvas.png`, Buffer.from(observations.display.canvasPng.split(",")[1], "base64"), { flag: "wx" });
  delete observations.display.canvasPng;
  console.log(JSON.stringify({ display: observations.display }));
  await screenshot("running");
  const frameIdx = Math.max(...records().filter((row) => row.event === "frame").map((row) => row.frame_idx));
  await send("Emulation.setDeviceMetricsOverride", { width: 800, height: 500, deviceScaleFactor: 1, mobile: false });
  await waitFor(() => records().some((row) => row.event === "resize" && row.changed), "resize admission");
  await waitFor(() => {
    const rows = records();
    const resize = rows.findIndex((row) => row.event === "resize" && row.changed);
    return resize >= 0 && rows.slice(resize + 1).some((row) => row.event === "frame" && row.frame_idx > frameIdx);
  }, "resized presented frame");
  assert.equal(await evaluate(errorText), "");
  await screenshot("resized");
  observations.cases.push("real-package-start-and-resize");

  // Synthetic DOM input drives the real renderer encoder and Rust model.
  await evaluate(`(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "q", code: "KeyQ", bubbles: true }));
    const data = new DataTransfer(); data.setData("text", "retained 你好 👩‍💻 {tail}");
    window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
  })()`);
  await waitFor(() => evaluate(`Boolean(${linkExpr})`), "recovery link");
  const payload = await evaluate(`(async () => (await fetch(${linkExpr}.href)).text())()`);
  const tail = payload.trim().split("\n").map((line) => JSON.parse(line));
  assert.ok(tail.some((row) => row.event === "input" && row.data?.kind === "paste" && row.data.text === "retained 你好 👩‍💻 {tail}"), "exact paste survives quit");
  assert.match(await evaluate(statusText), /stopped/);
  const href = await evaluate(`${linkExpr}.href`);
  await evaluate(`${linkExpr}.focus()`);
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: "+", code: "Equal", modifiers: 2 });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: "+", code: "Equal", modifiers: 2 });
  assert.equal(await evaluate(`${linkExpr}.href`), href, "zoom preserves recovery link");
  await evaluate(`${linkExpr}.focus()`);
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r" });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
  await waitFor(() => observations.downloads.some((event) => event.params.state === "completed"), "keyboard download");
  const saved = await readFile(`${output}/downloads/frankentui-unprocessed-input.jsonl`);
  assert.equal(saved.toString(), payload);
  await screenshot("recovery");
  observations.cases.push("quit-tail-browser-keyboard-download");
  assert.deepEqual(observations.pageErrors, [], "positive flow has no uncaught errors");

  // One browser task delays RAF consumption. The real producer must not evict
  // the leading quit while accepting a burst of more than 4096 input events.
  const burstStart = await freshScenario("count-burst");
  await evaluate(`(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "q", code: "KeyQ", bubbles: true }));
    for (let i = 0; i < 4097; i++) {
      const data = new DataTransfer(); data.setData("text", "burst-" + i + " 你好 👩‍💻");
      window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
    }
  })()`);
  const burstTail = await recoveredInput("quit survives producer count overflow");
  assert.deepEqual(burstTail.map((row) => row.data.text), Array.from({ length: 4095 }, (_, i) => `burst-${i} 你好 👩‍💻`), "every accepted paste survives in FIFO order");
  const burstAdmission = records(burstStart).filter((row) => row.event === "input_admission");
  assert.equal(burstAdmission.length, 4098);
  assert.deepEqual(burstAdmission.map((row) => row.sequence), Array.from({ length: 4098 }, (_, i) => burstAdmission[0].sequence + i));
  assert.ok(burstAdmission.slice(0, 4096).every((row) => row.outcome === "accepted" && row.normalized.length === 1 && row.normalized[0].outcome === "queued"));
  assert.deepEqual(burstAdmission.slice(4096).map((row) => row.outcome), ["rejected", "rejected"]);
  assert.deepEqual(burstAdmission[4096].normalized, [{ index: 0, outcome: "rejected" }]);
  assert.equal(burstAdmission[4097].reason, "stopped");
  assert.ok(burstAdmission.slice(0, 4097).every((row) => row.vt_chunks_discarded === 1 && row.ime_records_discarded === 0), "local VT copies drain on each call");
  assert.equal(records(burstStart).find((row) => row.event === "step" && !row.running).events_processed, 1, "only q is processed; paste tail is recovered");
  observations.cases.push("producer-count-burst-exact-quit-tail");

  // The producer can synthesize a start before an update. With one queue slot
  // left, that prefix is accepted and must survive the rejected primary.
  const partialStart = await freshScenario("partial-composition");
  await evaluate(`(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "q", code: "KeyQ", bubbles: true }));
    for (let i = 0; i < 4094; i++) {
      const data = new DataTransfer(); data.setData("text", "partial-" + i);
      window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
    }
    document.getElementById("terminal-canvas").dispatchEvent(new CompositionEvent("compositionupdate", { data: "unaccepted 你好", bubbles: true }));
  })()`);
  const partialTail = await recoveredInput("recover synthetic composition prefix");
  assert.deepEqual(partialTail.slice(0, -1).map((row) => row.data.text), Array.from({ length: 4094 }, (_, i) => `partial-${i}`));
  assert.deepEqual(partialTail.at(-1).data, { kind: "ime", phase: "start", text: "" });
  const partial = records(partialStart).find((row) => row.event === "input_admission" && row.kind === "composition");
  assert.equal(partial.outcome, "partial");
  assert.deepEqual(partial.normalized, [{ index: 0, outcome: "queued" }, { index: 1, outcome: "rejected" }]);
  assert.equal(partial.ime_records_discarded, 2);
  observations.cases.push("composition-rewrite-partial-admission");

  // Exact UTF-8 byte boundary: reject both a huge ASCII string and a shorter
  // multibyte string before normalization, but admit one complete 768 KiB paste.
  const byteStart = await freshScenario("byte-boundary");
  await evaluate(`(() => {
    const paste = text => { const data = new DataTransfer(); data.setData("text", text);
      window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true })); };
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "q", code: "KeyQ", bubbles: true }));
    paste("x".repeat(768 * 1024 + 1));
    paste("界".repeat(768 * 1024 / 3 + 1));
    const full = "你".repeat((768 * 1024 - 6) / 3) + "FULL01";
    paste(full); paste(full); paste("after-stop");
  })()`);
  const byteTail = await recoveredInput("recover full boundary paste");
  assert.equal(byteTail.length, 1);
  assert.equal(byteTail[0].data.text, "你".repeat((768 * 1024 - 6) / 3) + "FULL01");
  assert.equal(Buffer.byteLength(byteTail[0].data.text), 768 * 1024);
  const byteAdmission = records(byteStart).filter((row) => row.event === "input_admission");
  assert.deepEqual(byteAdmission.map((row) => row.outcome), ["accepted", "rejected", "rejected", "accepted", "rejected", "rejected"]);
  assert.ok(byteAdmission.slice(1, 3).every((row) => row.reason === "text_too_large" && row.normalized.length === 0 && row.vt_chunks_discarded === 0));
  assert.equal(byteAdmission[3].normalized[0].outcome, "queued");
  assert.equal(byteAdmission[4].normalized[0].outcome, "rejected");
  observations.cases.push("producer-utf8-byte-boundary-and-full-paste");

  // Exercise successful capacity drain/retry too, with an active consumer.
  const retryStart = await freshScenario("byte-retry");
  await evaluate(`(() => {
    const paste = text => { const data = new DataTransfer(); data.setData("text", text);
      window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true })); };
    for (let i = 0; i < 6; i++) paste(String(i) + "好".repeat(128 * 1024));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "q", code: "KeyQ", bubbles: true }));
    paste("retry-tail 👩‍💻");
  })()`);
  const retryTail = await recoveredInput("recover after successful capacity retries");
  assert.deepEqual(retryTail.map((row) => row.data.text), ["retry-tail 👩‍💻"]);
  const retryAdmission = records(retryStart).filter((row) => row.event === "input_admission");
  assert.equal(retryAdmission.length, 8);
  assert.ok(retryAdmission.every((row) => row.outcome === "accepted" && row.normalized[0].outcome === "queued"));
  const retrySteps = records(retryStart).filter((row) => row.event === "step");
  assert.deepEqual(retrySteps.map((row) => row.events_processed), [2, 2, 3]);
  assert.deepEqual(retrySteps.map((row) => row.rendered), [true, true, false]);
  const retryRecords = records(retryStart);
  for (const step of retrySteps.filter((row) => row.rendered)) {
    const start = retryRecords.findIndex((row) => row.event === "step" && row.frame_idx === step.frame_idx);
    const remaining = retryRecords.slice(start + 1);
    const frame = remaining.findIndex((row) => row.event === "frame" && row.frame_idx === step.frame_idx);
    const next = remaining.findIndex((row) => row.event === "step" || row.event === "input_admission");
    assert.ok(frame >= 0 && (next < 0 || frame < next), "each intermediate frame is presented before the retry completes");
  }
  await screenshot("byte-retry");
  observations.cases.push("producer-byte-capacity-drain-retry");

  for (const phase of ["update", "end"]) {
    const imeStart = await freshScenario(`oversized-ime-${phase}`);
    await evaluate(`(() => {
      const canvas = document.getElementById("terminal-canvas");
      const composition = (phase, data = "") => canvas.dispatchEvent(new CompositionEvent("composition" + phase, { data, bubbles: true }));
      composition("start"); composition("update", "stale-preedit");
      composition(${JSON.stringify(phase)}, "界".repeat(768 * 1024 / 3 + 1));
      if (${JSON.stringify(phase)} === "update") composition("end");
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "q", code: "KeyQ", bubbles: true }));
      const data = new DataTransfer(); data.setData("text", "ime-${phase}-tail");
      window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
    })()`);
    const imeTail = await recoveredInput(`ordinary key works after oversized IME ${phase}`);
    assert.deepEqual(imeTail.map((row) => row.data.text), [`ime-${phase}-tail`]);
    const imeAdmission = records(imeStart).filter((row) => row.event === "input_admission");
    const rejection = imeAdmission.find((row) => row.reason === "text_too_large");
    assert.equal(rejection.outcome, "rejected");
    assert.deepEqual(rejection.normalized, []);
    assert.deepEqual(rejection.cancellation.normalized, [{ index: 0, outcome: "queued" }]);
    assert.equal(rejection.cancellation.ime_records_discarded, 1);
    if (phase === "update") assert.ok(imeAdmission.some((row) => row.reason === "composition_cancelled" && row.normalized.length === 0), "failed session's trailing end is rejected");
    assert.ok(imeAdmission.some((row) => row.kind === "key" && row.normalized[0]?.outcome === "queued"), "q is not suppressed by stale composition state");
    assert.equal(records(imeStart).find((row) => row.event === "step" && !row.running).events_processed, 4, "start/update/cancel/q only; stale preedit is never committed");
    observations.cases.push(`cancel-oversized-composition-${phase}`);
  }
  assert.deepEqual(observations.pageErrors, [], "all positive admission journeys have no uncaught errors");

  // The actual screen command must reach the host through AppModel and the
  // generated WASM API. Synthetic DOM events use the existing renderer encoder.
  const screenLogStart = await freshScenario("screen-command-logs");
  await evaluate(`(() => {
    const key = (key, code, extra = {}) => window.dispatchEvent(new KeyboardEvent("keydown", { key, code, bubbles: true, ...extra }));
    // Use the real palette: numeric registry positions vary with features.
    key("k", "KeyK", { ctrlKey: true });
    for (const ch of "determinism") key(ch, "Key" + ch.toUpperCase());
    key("Enter", "Enter"); key("c", "KeyC");
  })()`);
  const screenLogs = (start) => records(start).filter((row) => row.event === "log" && row.text.startsWith("[determinism]"));
  const checksumPattern = /^\[determinism\] checksum \((Full|DirtyRows|FullRedraw)\) live = 0x[0-9a-f]{16}$/;
  await waitFor(() => records(screenLogStart).some((row) => row.event === "frame" && row.events_processed === 14), "screen navigation and log input processed");
  await screenshot("determinism-log");
  await waitFor(() => screenLogs(screenLogStart).length > 0, "actual screen checksum log");
  assert.equal(screenLogs(screenLogStart).length, 1);
  assert.equal(screenLogs(screenLogStart)[0].text.match(checksumPattern)?.[1], "DirtyRows");
  const quitLogStart = observations.console.length;
  await evaluate(`(() => {
    const key = (key, code, extra = {}) => window.dispatchEvent(new KeyboardEvent("keydown", { key, code, bubbles: true, ...extra }));
    for (const number of ["1", "2", "3"]) { key(number, "Digit" + number); key("c", "KeyC"); }
    key("c", "KeyC", { ctrlKey: true });
  })()`);
  await waitFor(() => records(quitLogStart).some((row) => row.event === "quit"), "screen logs followed by quit");
  const quitRows = records(quitLogStart);
  const stoppedIndex = quitRows.findIndex((row) => row.event === "step" && !row.running);
  assert.ok(stoppedIndex >= 0);
  const stoppedStep = quitRows[stoppedIndex];
  assert.equal(stoppedStep.rendered, false);
  assert.equal(stoppedStep.events_processed, 7);
  assert.ok(!quitRows.slice(stoppedIndex).some((row) => row.event === "frame"), "quit logs must not require a frame");
  const finalLogs = screenLogs(quitLogStart);
  assert.deepEqual(finalLogs.map((row) => row.text.match(checksumPattern)?.[1]), ["Full", "DirtyRows", "FullRedraw"]);
  assert.ok(finalLogs.every((row) => row.frame_idx === stoppedStep.frame_idx));
  assert.equal(screenLogs(screenLogStart).length, 4, "every requested log arrives exactly once");
  assert.match(await evaluate(statusText), /stopped/);
  assert.equal(await evaluate(errorText), "");
  assert.deepEqual(observations.pageErrors, [], "screen log journey has no uncaught errors");
  observations.cases.push("real-screen-log-and-nonrendering-quit-delivery");

  // Rejection must remain visible with logging OFF, after RAF has stopped,
  // without replacing the recovery anchor or appending unaccepted input to it.
  await send("Page.navigate", { url: `${url.replace("jsonl=1", "jsonl=0")}&scenario=stopped-notice` });
  await waitFor(() => evaluate('location.search.includes("jsonl=0") && document.getElementById("loading-overlay") && document.getElementById("status") && getComputedStyle(document.getElementById("loading-overlay")).opacity === "0" && document.getElementById("status").textContent.includes("panes")'), "quiet showcase startup");
  const quietStart = observations.console.length;
  await evaluate(`(() => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "q", code: "KeyQ", bubbles: true }));
    const data = new DataTransfer(); data.setData("text", "quiet accepted tail");
    window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
  })()`);
  const quietTail = await recoveredInput("quiet recovery link");
  const quietHref = await evaluate(`${linkExpr}.href`);
  assert.deepEqual(quietTail.map((row) => row.data.text), ["quiet accepted tail"]);
  await evaluate(`(() => {
    const data = new DataTransfer(); data.setData("text", "quiet rejected canary");
    window.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
  })()`);
  assert.equal(await evaluate('document.getElementById("input-rejection-count")?.textContent.trim() || ""'), "— 1 input rejected");
  assert.equal(await evaluate(`${linkExpr}.href`), quietHref);
  assert.deepEqual(await recoveredInput("quiet recovery unchanged"), quietTail);
  assert.ok(!records(quietStart).some((row) => row.event === "input_admission"));
  assert.ok(!observations.console.some((line) => line.includes("quiet rejected canary")));
  assert.deepEqual(observations.pageErrors, [], "quiet admission journey has no uncaught errors");
  observations.cases.push("visible-stopped-rejection-with-logging-off");

  for (const name of ["missing", "corrupt", "revision", "abi"]) {
    fault = name;
    const logStart = observations.browserLog.length;
    await send("Page.navigate", { url });
    await waitFor(() => evaluate(errorText), `reject ${name}`);
    if (name === "abi") assert.match(await evaluate(errorText), /Unsupported first-party renderer API contract/);
    else assert.match(await evaluate(errorText), /Failed to load the browser packages/);
    assert.ok(observations.injections.includes(name), "fault reached the actual loader");
    if (name === "revision") assert.match(await evaluate(errorText), /Unsupported renderer source revision/);
    if (name === "corrupt") assert.ok(observations.browserLog.slice(logStart).some((row) => /integrity|digest/i.test(row.text)), "valid modified JS must fail byte integrity");
    if (name === "missing") assert.ok(observations.browserLog.slice(logStart).some((row) => /404/.test(row.text)), "missing package must return 404");
    assert.equal(await evaluate(statusText), "Error");
    observations.cases.push(`reject-${name}`);
  }
  console.log(JSON.stringify({ browser: observations.browser.Browser, cases: observations.cases }));
} finally {
  await writeFile(`${output}/observations.json`, JSON.stringify(observations, null, 2), { flag: "wx" });
  if (socket) socket.close();
  for (const call of pending.values()) { clearTimeout(call.timer); call.reject(new Error("CDP closed")); }
  server.closeAllConnections();
  await new Promise((done) => server.close(done));
}
