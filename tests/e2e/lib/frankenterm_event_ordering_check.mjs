#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const DEFAULT_REQUIRED_ORDERING_SNIPPETS = [
  "drainEncodedInputs() returns FIFO",
  "drainEncodedInputBytes() returns FIFO",
  "drainImeCompositionJsonl() returns FIFO",
  "drainReplyBytes() returns FIFO",
  "drainAttachTransitionsJsonl() returns transitions",
  "drainLinkClicks() and drainAccessibilityAnnouncements() return FIFO",
];

const REQUIRED_EVENT_TYPES = [
  "attach.transition",
  "input.accessibility",
  "input.composition",
  "input.composition_trace",
  "input.focus",
  "input.mouse",
  "input.paste",
  "input.vt_bytes",
  "input.wheel",
  "terminal.reply_bytes",
  "ui.accessibility_announcement",
  "ui.link_click",
];

function parseArgs(argv) {
  const out = {
    pkgDir: "",
    jsonl: "",
    summary: "",
    runId: "",
    seed: 0,
    deterministic: true,
    timeStepMs: 100,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    switch (arg) {
      case "--pkg-dir":
        out.pkgDir = argv[++i] ?? "";
        break;
      case "--jsonl":
        out.jsonl = argv[++i] ?? "";
        break;
      case "--summary":
        out.summary = argv[++i] ?? "";
        break;
      case "--run-id":
        out.runId = argv[++i] ?? "";
        break;
      case "--seed":
        out.seed = Number.parseInt(argv[++i] ?? "0", 10);
        break;
      case "--deterministic":
        out.deterministic = true;
        break;
      case "--nondeterministic":
        out.deterministic = false;
        break;
      case "--time-step-ms":
        out.timeStepMs = Number.parseInt(argv[++i] ?? "100", 10);
        break;
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (!out.pkgDir) {
    throw new Error("--pkg-dir is required");
  }
  if (!out.jsonl) {
    throw new Error("--jsonl is required");
  }
  if (!Number.isFinite(out.seed)) {
    throw new Error("--seed must be numeric");
  }
  if (!Number.isFinite(out.timeStepMs) || out.timeStepMs <= 0) {
    throw new Error("--time-step-ms must be > 0");
  }
  return out;
}

function isoNow() {
  return new Date().toISOString();
}

function deterministicTimestamp(seq, timeStepMs) {
  const t = seq * timeStepMs;
  return `T${String(t).padStart(6, "0")}`;
}

function toHex(bytesLike) {
  const u8 = bytesLike instanceof Uint8Array ? bytesLike : new Uint8Array(bytesLike);
  return Array.from(u8, (b) => b.toString(16).padStart(2, "0")).join("");
}

function expect(condition, errors, message) {
  if (!condition) {
    errors.push(message);
  }
}

function monotonic(values) {
  for (let i = 1; i < values.length; i += 1) {
    if (values[i - 1] > values[i]) {
      return false;
    }
  }
  return true;
}

function asArray(value) {
  return Array.isArray(value) ? value : [];
}

async function loadPkg(pkgDir) {
  const pkgPath = path.resolve(pkgDir, "frankenterm_web.js");
  if (!fs.existsSync(pkgPath)) {
    throw new Error(`wasm-pack package entry not found: ${pkgPath}`);
  }
  const url = new URL(`file://${pkgPath}`);
  return import(url.href);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const pkg = await loadPkg(args.pkgDir);
  const runId = args.runId || `frankenterm-event-ordering-seed-${args.seed}`;
  const correlationId = `corr-${runId}`;

  /** @type {Array<Record<string, unknown>>} */
  const jsonlEvents = [];
  /** @type {Array<string>} */
  const errors = [];
  let seq = 0;

  function emit(eventType, payload = {}) {
    seq += 1;
    const timestamp = args.deterministic
      ? deterministicTimestamp(seq, args.timeStepMs)
      : isoNow();
    jsonlEvents.push({
      schema_version: "e2e-jsonl-v1",
      type: "contract_event",
      event_type: eventType,
      timestamp,
      run_id: runId,
      seed: args.seed,
      seq,
      correlation_id: correlationId,
      ...payload,
    });
  }

  const term = new pkg.FrankenTermWeb();
  const contract = term.apiContract();
  const contractTypes = asArray(contract.eventTypes);
  const contractOrdering = asArray(contract.eventOrdering);

  emit("contract.snapshot", {
    event_schema_version: contract.eventSchemaVersion ?? "",
    event_types_count: contractTypes.length,
    ordering_rules_count: contractOrdering.length,
  });

  expect(
    contract.eventSchemaVersion === "1.0.0",
    errors,
    `unexpected eventSchemaVersion: ${String(contract.eventSchemaVersion)}`,
  );

  for (const requiredType of REQUIRED_EVENT_TYPES) {
    expect(
      contractTypes.includes(requiredType),
      errors,
      `apiContract.eventTypes missing required type: ${requiredType}`,
    );
  }
  expect(monotonic(contractTypes), errors, "apiContract.eventTypes must stay sorted");

  for (const snippet of DEFAULT_REQUIRED_ORDERING_SNIPPETS) {
    const found = contractOrdering.some((entry) => String(entry).includes(snippet));
    expect(found, errors, `apiContract.eventOrdering missing rule snippet: ${snippet}`);
  }

  // Mode transitions: attach state-machine transitions.
  term.attachConnect(0);
  term.attachTransportOpened(10);
  term.attachHandshakeAck("session-e2e", 20);
  term.attachTransportClosed(1000, true, "normal_close", 30);
  const attachLines = Array.from(term.drainAttachTransitionsJsonl(runId));
  const attachSeqs = [];
  for (const [idx, line] of attachLines.entries()) {
    const parsed = JSON.parse(String(line));
    const transitionSeq = Number(parsed.transition_seq ?? idx);
    attachSeqs.push(transitionSeq);
    emit("attach.transition", {
      drain_index: idx,
      transition_seq: transitionSeq,
      from_state: parsed.from_state ?? "",
      to_state: parsed.to_state ?? "",
      attach_event: parsed.attach_event ?? "",
    });
  }
  expect(attachSeqs.length >= 4, errors, "expected >=4 attach transitions in E2E fixture");
  expect(monotonic(attachSeqs), errors, "attach transition_seq must be monotonic");

  // Resize + feed mode transitions for deterministic reply bytes.
  term.resize(8, 4);
  term.feed(Buffer.from("\u001b[4;8H\u001b[6n", "utf8"));
  term.resize(5, 2);
  term.feed(Buffer.from("\u001b[6n", "utf8"));

  // Burst + composition edge ordering.
  term.input({ kind: "composition", phase: "update", data: "x" });
  const imeActiveSnapshot = term.imeState();
  expect(
    Boolean(imeActiveSnapshot.active),
    errors,
    `imeState.active should be true during composition, got ${JSON.stringify(imeActiveSnapshot)}`,
  );
  expect(
    String(imeActiveSnapshot.preedit ?? "") === "x",
    errors,
    `imeState.preedit should track latest preedit text, got ${JSON.stringify(imeActiveSnapshot)}`,
  );
  term.input({ kind: "key", phase: "down", key: "A", code: "KeyA", repeat: false, mods: 0 });
  term.input({ kind: "composition", phase: "end" });
  const imeInactiveSnapshot = term.imeState();
  expect(
    !Boolean(imeInactiveSnapshot.active),
    errors,
    `imeState.active should be false after composition end, got ${JSON.stringify(imeInactiveSnapshot)}`,
  );
  expect(
    imeInactiveSnapshot.preedit === null || imeInactiveSnapshot.preedit === undefined,
    errors,
    `imeState.preedit should be cleared after composition end, got ${JSON.stringify(imeInactiveSnapshot)}`,
  );
  for (let i = 0; i < 6; i += 1) {
    term.input({ kind: "paste", data: `burst-${i}` });
  }
  term.input({ kind: "focus", focused: true });
  term.input({ kind: "wheel", x: 0, y: 0, dx: 0, dy: 1, mods: 0 });
  term.input({ kind: "accessibility", announce: "screen-reader-ready" });

  // Link-click path.
  term.resize(1, 1);
  term.applyPatch({
    offset: 0,
    cells: [{ bg: 0, fg: 0, glyph: "A".codePointAt(0), attrs: (55 << 8) | 1 }],
  });
  term.input({ kind: "mouse", phase: "down", button: 0, x: 0, y: 0, mods: 0 });

  // Drain + structured logs.
  const encodedInputLines = Array.from(term.drainEncodedInputs());
  const encodedKinds = [];
  for (const [idx, encoded] of encodedInputLines.entries()) {
    const parsed = JSON.parse(String(encoded));
    const eventType = `input.${String(parsed.kind)}`;
    encodedKinds.push(String(parsed.kind));
    emit(eventType, {
      drain_index: idx,
      encoded_input: encoded,
    });
  }

  const encodedByteChunks = Array.from(term.drainEncodedInputBytes());
  for (const [idx, chunk] of encodedByteChunks.entries()) {
    emit("input.vt_bytes", {
      drain_index: idx,
      bytes_hex: toHex(chunk),
    });
  }

  const imeTraceTimestamp = args.deterministic ? "T999999" : isoNow();
  const imeTraceLines = Array.from(term.drainImeCompositionJsonl(runId, args.seed, imeTraceTimestamp));
  const imeTraceKinds = [];
  for (const [idx, line] of imeTraceLines.entries()) {
    const parsed = JSON.parse(String(line));
    imeTraceKinds.push(String(parsed.event_kind ?? ""));
    emit("input.composition_trace", {
      drain_index: idx,
      event_kind: parsed.event_kind ?? "",
      phase: parsed.phase ?? null,
      synthetic: Boolean(parsed.synthetic),
      active_after: Boolean(parsed.active_after),
      preedit_after: parsed.preedit_after ?? null,
    });
  }

  const replyChunks = Array.from(term.drainReplyBytes()).map((chunk) => toHex(chunk));
  for (const [idx, hex] of replyChunks.entries()) {
    emit("terminal.reply_bytes", {
      drain_index: idx,
      bytes_hex: hex,
    });
  }

  const linkClicks = Array.from(term.drainLinkClicks());
  for (const [idx, click] of linkClicks.entries()) {
    emit("ui.link_click", {
      drain_index: idx,
      x: Number(click.x ?? -1),
      y: Number(click.y ?? -1),
      link_id: Number(click.linkId ?? 0),
      open_allowed: Boolean(click.openAllowed),
      open_reason: click.openReason ?? null,
    });
  }

  const announcements = Array.from(term.drainAccessibilityAnnouncements());
  for (const [idx, text] of announcements.entries()) {
    emit("ui.accessibility_announcement", {
      drain_index: idx,
      text: String(text),
    });
  }

  const expectedKinds = [
    "composition",
    "composition",
    "composition",
    "paste",
    "paste",
    "paste",
    "paste",
    "paste",
    "paste",
    "focus",
    "wheel",
    "accessibility",
    "mouse",
  ];

  expect(
    JSON.stringify(encodedKinds) === JSON.stringify(expectedKinds),
    errors,
    `encoded input order mismatch: expected ${JSON.stringify(expectedKinds)} got ${JSON.stringify(encodedKinds)}`,
  );
  expect(
    encodedByteChunks.length >= 7,
    errors,
    `expected at least 7 VT byte chunks, got ${encodedByteChunks.length}`,
  );
  expect(
    imeTraceLines.length >= 4,
    errors,
    `expected at least 4 IME trace records, got ${imeTraceLines.length}`,
  );
  expect(
    imeTraceKinds.includes("drop_key"),
    errors,
    `expected IME trace to include drop_key event, got ${JSON.stringify(imeTraceKinds)}`,
  );
  expect(
    JSON.stringify(replyChunks) === JSON.stringify(["1b5b343b3852", "1b5b323b3552"]),
    errors,
    `reply byte order mismatch: ${JSON.stringify(replyChunks)}`,
  );
  expect(linkClicks.length === 1, errors, `expected 1 link click, got ${linkClicks.length}`);
  expect(
    announcements.length === 1 && String(announcements[0]) === "screen-reader-ready",
    errors,
    `expected one accessibility announcement, got ${JSON.stringify(announcements)}`,
  );

  const seqs = jsonlEvents.map((event) => Number(event.seq ?? 0));
  expect(monotonic(seqs), errors, "jsonl seq values must be monotonic");

  const eventTypesInLog = new Set(jsonlEvents.map((event) => String(event.event_type ?? "")));
  for (const required of REQUIRED_EVENT_TYPES) {
    expect(
      eventTypesInLog.has(required),
      errors,
      `E2E fixture log missing required event_type: ${required}`,
    );
  }

  fs.mkdirSync(path.dirname(path.resolve(args.jsonl)), { recursive: true });
  fs.writeFileSync(
    path.resolve(args.jsonl),
    `${jsonlEvents.map((event) => JSON.stringify(event)).join("\n")}\n`,
    "utf8",
  );

  const summary = {
    run_id: runId,
    seed: args.seed,
    deterministic: args.deterministic,
    event_schema_version: contract.eventSchemaVersion ?? "",
    event_count: jsonlEvents.length,
    attach_transition_count: attachLines.length,
    encoded_input_count: encodedInputLines.length,
    encoded_vt_chunk_count: encodedByteChunks.length,
    ime_trace_count: imeTraceLines.length,
    reply_chunk_count: replyChunks.length,
    link_click_count: linkClicks.length,
    accessibility_announcement_count: announcements.length,
    outcome: errors.length === 0 ? "pass" : "fail",
    errors,
  };

  if (args.summary) {
    fs.mkdirSync(path.dirname(path.resolve(args.summary)), { recursive: true });
    fs.writeFileSync(path.resolve(args.summary), `${JSON.stringify(summary, null, 2)}\n`, "utf8");
  }

  console.log(JSON.stringify(summary, null, 2));
  if (errors.length > 0) {
    process.exitCode = 1;
  }
}

main().catch((error) => {
  console.error(JSON.stringify({ outcome: "error", error: String(error) }, null, 2));
  process.exitCode = 1;
});
