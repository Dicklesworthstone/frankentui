#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const REQUIRED_METHODS = [
  "createDecoration",
  "createMarker",
  "decorationsState",
  "drainMarkerDecorationJsonl",
  "dropDecoration",
  "dropMarker",
  "markersState",
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
  return `T${String(seq * timeStepMs).padStart(6, "0")}`;
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

function markerById(markersState, markerId) {
  return asArray(markersState?.markers).find((marker) => Number(marker?.id) === markerId);
}

function decorationById(decorationsState, decorationId) {
  return asArray(decorationsState).find(
    (decoration) => Number(decoration?.id) === decorationId,
  );
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const pkg = await loadPkg(args.pkgDir);
  const runId = args.runId || `frankenterm-markers-seed-${args.seed}`;
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
      type: "marker_contract_event",
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
  const methods = asArray(contract.methods);
  for (const method of REQUIRED_METHODS) {
    expect(methods.includes(method), errors, `apiContract.methods missing ${method}`);
  }

  term.resize(12, 6);
  emit("resize.initial", { cols: 12, rows: 6 });

  const initialMarkerId = Number(term.createMarker(0, 0));
  expect(initialMarkerId > 0, errors, "createMarker should return positive id");
  emit("marker.create.initial", { marker_id: initialMarkerId, line_idx: 0 });

  const burstLines = 900;
  const chunks = [];
  for (let i = 0; i < burstLines; i += 1) {
    chunks.push(`L${String(i).padStart(4, "0")} marker-compaction\n`);
  }
  const payload = Buffer.from(chunks.join(""), "utf8");
  term.feed(Uint8Array.from(payload));
  emit("feed.scrollback_burst", { bytes: payload.length, lines: burstLines });

  const markersAfterBurst = term.markersState();
  const initialMarker = markerById(markersAfterBurst, initialMarkerId);
  expect(Boolean(initialMarker), errors, "initial marker should exist after burst");
  expect(Boolean(initialMarker?.stale), errors, "initial marker should become stale after compaction");
  expect(
    String(initialMarker?.staleReason ?? "") === "compacted_out",
    errors,
    `expected compacted_out stale reason, got ${String(initialMarker?.staleReason ?? "")}`,
  );

  const viewport = term.viewportState();
  const totalLines = Number(viewport?.totalLines ?? 0);
  expect(totalLines > 4, errors, `expected totalLines > 4 after burst, got ${totalLines}`);

  const markerA = Number(term.createMarker(Math.max(0, totalLines - 2), 2));
  const markerB = Number(term.createMarker(Math.max(0, totalLines - 1), 6));
  expect(markerA > 0 && markerB > 0, errors, "fresh marker creation should return ids");

  const lineDecoration = Number(term.createDecoration("line", markerA, -1, 0, 0));
  const rangeDecoration = Number(term.createDecoration("range", markerA, markerB, 2, 8));
  expect(lineDecoration > 0 && rangeDecoration > 0, errors, "decoration creation should return ids");

  let decorations = term.decorationsState();
  const lineSnap = decorationById(decorations, lineDecoration);
  const rangeSnap = decorationById(decorations, rangeDecoration);
  expect(Boolean(lineSnap), errors, "line decoration should exist");
  expect(Boolean(rangeSnap), errors, "range decoration should exist");
  expect(!lineSnap?.stale, errors, "line decoration should be active");
  expect(!rangeSnap?.stale, errors, "range decoration should be active");
  expect(
    Number.isFinite(Number(lineSnap?.startOffset)) && Number.isFinite(Number(lineSnap?.endOffset)),
    errors,
    "line decoration should resolve to visible offsets",
  );

  term.resize(12, 4);
  emit("resize.reflow", { cols: 12, rows: 4 });
  decorations = term.decorationsState();
  const rangeAfterResize = decorationById(decorations, rangeDecoration);
  expect(Boolean(rangeAfterResize), errors, "range decoration should persist after resize");
  expect(!rangeAfterResize?.stale, errors, "range decoration should remain active after resize");

  term.dropMarker(markerA);
  decorations = term.decorationsState();
  const lineAfterDrop = decorationById(decorations, lineDecoration);
  const rangeAfterDrop = decorationById(decorations, rangeDecoration);
  expect(Boolean(lineAfterDrop?.stale), errors, "line decoration should stale after marker drop");
  expect(Boolean(rangeAfterDrop?.stale), errors, "range decoration should stale after marker drop");
  expect(
    String(lineAfterDrop?.staleReason ?? "") === "start_marker_missing",
    errors,
    `unexpected line stale reason: ${String(lineAfterDrop?.staleReason ?? "")}`,
  );

  const diagLines = Array.from(
    term.drainMarkerDecorationJsonl(runId, args.seed, deterministicTimestamp(seq + 1, args.timeStepMs)),
  );
  const diagnostics = diagLines.map((line) => JSON.parse(String(line)));
  emit("diagnostics.drain", { count: diagnostics.length });

  const diagSeqs = diagnostics.map((entry) => Number(entry.seq ?? -1));
  expect(monotonic(diagSeqs), errors, "marker/decoration diagnostic seq values must be monotonic");
  expect(diagnostics.length > 0, errors, "expected non-empty marker/decoration diagnostics");
  const hasInvalidated = diagnostics.some((entry) => String(entry.action ?? "") === "invalidated");
  expect(hasInvalidated, errors, "diagnostics should include invalidated events");

  const outcome = errors.length === 0 ? "pass" : "fail";
  const summary = {
    run_id: runId,
    seed: args.seed,
    deterministic: args.deterministic,
    outcome,
    errors,
    event_count: jsonlEvents.length,
    diagnostic_count: diagnostics.length,
    initial_marker_id: initialMarkerId,
    marker_ids: [markerA, markerB],
    decoration_ids: [lineDecoration, rangeDecoration],
  };

  fs.mkdirSync(path.dirname(path.resolve(args.jsonl)), { recursive: true });
  fs.writeFileSync(
    path.resolve(args.jsonl),
    `${jsonlEvents.map((event) => JSON.stringify(event)).join("\n")}\n`,
    "utf8",
  );
  if (args.summary) {
    fs.mkdirSync(path.dirname(path.resolve(args.summary)), { recursive: true });
    fs.writeFileSync(path.resolve(args.summary), `${JSON.stringify(summary, null, 2)}\n`, "utf8");
  }

  if (outcome !== "pass") {
    process.exitCode = 1;
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
