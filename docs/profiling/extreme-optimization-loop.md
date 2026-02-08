# Extreme Optimization Loop (FrankenTerm + ftui-web) — bd-lff4p.7

Purpose
- Make performance work repeatable, measurable, and regression-proof.
- Enforce the "single lever" rule: no behavior changes mixed into perf work.
- Require proofs (goldens + trace replay) so we can optimize aggressively without correctness drift.

Scope
- `frankenterm-core` (parser/grid/patch apply)
- `frankenterm-web` (WebGPU renderer + input)
- native backend (`ftui-tty` and/or successor): IO + present path

Definitions
- **Baseline**: the "before" measurement set, versioned by git SHA + run_id.
- **Opportunity matrix**: ranked candidate optimizations scored by impact/confidence/effort.
- **One lever**: exactly one optimization mechanism per PR/commit.
- **Isomorphism proof**: explicit argument that visible output and determinism are preserved.

---

## The Mandatory Loop (Every Perf Change)

1. BASELINE
   - Capture p50/p95/p99 + memory, keyed by `run_id` + git SHA.
2. PROFILE
   - CPU + allocations; (web) GPU timing when available.
3. OPPORTUNITY MATRIX
   - Score = Impact x Confidence / Effort.
   - Implement only Score >= 2.0.
4. IMPLEMENT (ONE LEVER)
   - Small, isolated change.
   - No refactors, no unrelated formatting, no behavior tweaks.
5. PROVE (ISOMORPHISM)
   - Argue: ordering/ties/FP/RNG unchanged, and any normalization is stable.
6. VERIFY (GOLDENS + GATES)
   - Trace replay + checksum gates.
   - If hashes differ: root-cause before blessing.
7. REPEAT
   - Re-profile; hotspots shift.

---

## Artifacts (Always Produce)

Required per optimization PR:
- `run_id`: stable slug for correlation.
- `baseline.json`: machine-readable before/after metrics.
- `profile.txt` (or equivalent): top hotspots (CPU + allocs; web GPU if available).
- `opportunity_matrix.md`: updated matrix with the chosen lever called out.
- `isomorphism_proof.md`: short proof note (template below).
- `trace_replay.log`: trace replay output for the curated corpus.
- `hash_registry` changes (if any), with an explanation.

If any artifact is missing, the PR is not "perf work", it's just guesswork.

---

## Subsystem Harnesses (Repeatable Commands)

These are the minimum viable harnesses. Each must be:
- deterministic (seeded; explicit time)
- machine-readable (JSON or JSONL)
- fast enough to run in CI for smoke and locally for iteration

### A) Core Engine (Parser/Grid/Patch Apply)

Target command shape (planned; not all harnesses exist yet as of 2026-02-08):
```bash
# Bench harness (criterion or custom)
cargo bench -p frankenterm-core

# Deterministic trace replay gate (current)
./scripts/e2e_test.sh --quick

# Replay a single trace directly (current)
FTUI_HARNESS_REPLAY_TRACE=path/to/trace.jsonl cargo run -p ftui-harness
```

Required metrics:
- parser throughput (MB/s) on a realistic byte corpus
- patch apply cost (ns/cell or us/frame)
- memory growth under long scrollback + resize storms

### B) Web Renderer (WebGPU)

Target command shape (planned; not implemented yet as of 2026-02-08):
```bash
# Browser harness (headless OK) emits JSON summary + JSONL detail
# ./scripts/web_bench.sh --case smoke --size 120x40 --dpr 2 --seed 0
```

Required metrics:
- frame time histogram (p50/p95/p99)
- time-to-first-frame, time-to-interactive
- steady-state memory (JS + WASM)
- patch statistics (bytes/frame, dirty spans, draw calls)

### C) Native Backend (TTY IO + Present)

Target command shape:
```bash
# PTY-backed harness emitting JSONL + summary JSON
./scripts/e2e_test.sh --quick
```

Required metrics:
- present path time breakdown (render vs present)
- bytes written per frame (and cursor move counts if available)
- syscall profile (e.g., `strace -c` for present loop)

---

## Opportunity Matrix Template

Score formula:
```
Score = Impact x Confidence / Effort
```

Rubric (1-5):
- Impact: expected p95 improvement magnitude + affected users/cases.
- Confidence: strength of evidence (profile + locality + minimal risk).
- Effort: code complexity + test updates + rollout risk.

Table:

| ID | Candidate | Impact | Confidence | Effort | Score | Evidence | One-Lever Plan |
| --- | --- | ---:| ---:| ---:| ---:| --- | --- |
| O1 | ... |  |  |  |  | flamegraph: ... | change: ... |

---

## Isomorphism Proof Template

Perf PRs MUST include a short proof note answering:

1. What is the single lever?
2. Why is output identical?
   - ordering and tie-breaks unchanged
   - RNG and seeds unchanged (or explicitly re-keyed and logged)
   - FP behavior unchanged (or normalization made explicit + stable)
3. Why is determinism preserved?
   - same inputs -> same checksum chain
4. What tests/gates prove it?
   - trace replay log path(s)
   - hash registry key(s)
   - any new unit/proptest/fuzz coverage

If hashes changed:
- provide root cause and a minimal repro (fixture or trace)
- then (and only then) bless with explanation

---

## Regression Threshold Policy Template

Define thresholds per harness:
- **CI smoke**: small corpus, low variance, hard fail on regressions.
- **Nightly/cron** (optional): larger corpus, statistical guard rails.

Minimum fields to record per run:
- git SHA, run_id, seed, size/DPR/profile
- p50/p95/p99, memory, bytes/frame (where applicable)
- corpus identifier (for trace replays)

Fail criteria example (p95):
- fail if `p95_after > p95_before * 1.10` (10% regression) for the same corpus.

---

## Notes

- "Perf improvements" without proofs/gates are not accepted.
- If you cannot get actionable profiling (e.g., kernel perf restrictions), record that as a blocker and switch to a different evidence source (instrumentation counters, alloc stats, byte counts).
