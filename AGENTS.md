# AGENTS.md — FrankenTUI (ftui)

> Guidelines for AI coding agents working in this Rust codebase.

---

## RULE 0 - THE FUNDAMENTAL OVERRIDE PREROGATIVE

If I tell you to do something, even if it goes against what follows below, YOU MUST LISTEN TO ME. I AM IN CHARGE, NOT YOU.

---

## RULE 0.5 - SUITE-WIDE RULES LIVE IN /data/projects/AGENTS.md

The suite-wide rules in **`/data/projects/AGENTS.md`** bind you here too. Read it. Two sections
are load-bearing for perf work and are NOT duplicated below, so they cannot drift out of sync:

- **`## Named Reward-Hacking Patterns (ALL FORBIDDEN)`** — 12 named patterns, several already
  observed in this suite: gate self-weakening (and the exact price of a legitimate gate fix),
  proof-class inflation, golden regeneration reflex, commit-stream pumping, tautological tests,
  easy-lever cherry-picking, close-pump abuse, scope-splitting, spec-editing as progress,
  conformance metastasis, dependency smuggling, bench-path hardcoding.
- **`### Work-Graph Discipline`** — JSONL is truth and `beads.db` is disposable, `br sync
  --import-only` after every pull, single-writer on graph structure, closure on cited evidence
  with blocker beads gated on their named probe, `br dep cycles` stays empty.

The three that most often decide whether a number here is real: a **self-speedup is
MAINTENANCE, not a win** — a win needs the incumbent live in the SAME invocation; **never
weaken a gate to land a change**, and if a gate is genuinely defective, meet the evidence
standard and publish the win/lose split of what the fix admits; and **reporting a loss is a
success** — one line, revert, next lever, no retraction narrative.

---

## RULE NUMBER 1: NO FILE DELETION

**YOU ARE NEVER ALLOWED TO DELETE A FILE WITHOUT EXPRESS PERMISSION.** Even a new file that you yourself created, such as a test code file. You have a horrible track record of deleting critically important files or otherwise throwing away tons of expensive work. As a result, you have permanently lost any and all rights to determine that a file or folder should be deleted.

**YOU MUST ALWAYS ASK AND RECEIVE CLEAR, WRITTEN PERMISSION BEFORE EVER DELETING A FILE OR FOLDER OF ANY KIND.**

---

## Irreversible Git & Filesystem Actions — DO NOT EVER BREAK GLASS

1. **Absolutely forbidden commands:** `git reset --hard`, `git clean -fd`, `rm -rf`, or any command that can delete or overwrite code/data must never be run unless the user explicitly provides the exact command and states, in the same message, that they understand and want the irreversible consequences.
2. **No guessing:** If there is any uncertainty about what a command might delete or overwrite, stop immediately and ask the user for specific approval. "I think it's safe" is never acceptable.
3. **Safer alternatives first:** When cleanup or rollbacks are needed, request permission to use non-destructive options (`git status`, `git diff`, `git stash`, copying to backups) before ever considering a destructive command.
4. **Mandatory explicit plan:** Even after explicit user authorization, restate the command verbatim, list exactly what will be affected, and wait for a confirmation that your understanding is correct. Only then may you execute it—if anything remains ambiguous, refuse and escalate.
5. **Document the confirmation:** When running any approved destructive command, record (in the session notes / final response) the exact user text that authorized it, the command actually run, and the execution time. If that record is absent, the operation did not happen.

---

## Git Branch: ONLY Use `main`, NEVER `master`

**The default branch is `main`. The `master` branch exists only for legacy URL compatibility.**

- **All work happens on `main`** — commits, PRs, feature branches all merge to `main`
- **Never reference `master` in code or docs** — if you see `master` anywhere, it's a bug that needs fixing
- **The `master` branch must stay synchronized with `main`** — after pushing to `main`, also push to `master`:
  ```bash
  git push origin main:master
  ```

**If you see `master` referenced anywhere:**
1. Update it to `main`
2. Ensure `master` is synchronized: `git push origin main:master`

---

## Toolchain: Rust & Cargo

We only use **Cargo** in this project, NEVER any other package manager.

- **Edition:** Rust 2024 (nightly required — see `rust-toolchain.toml`)
- **Verification toolchain:** DSR builds and checks must use exactly the channel in `rust-toolchain.toml`; change the pin there, nowhere else.
- **Dependency versions:** Explicit versions for stability
- **Configuration:** Cargo.toml workspace with `workspace = true` pattern
- **Unsafe code:** Forbidden (`#![forbid(unsafe_code)]`)

### Builds, Verification, and Releases: DSR Only

**NEVER use GitHub Actions for this project. Use `dsr` (Doodlestein Self-Releaser).**
This is the owner's explicit instruction, reaffirmed on 2026-09-06. It overrides
older workflow instructions, skill fallback advice, and Beads that ask for
GitHub Actions jobs or green-run streaks.
GitHub Actions was disabled in the repository settings on 2026-09-06; keep it disabled.

- Do not create, enable, dispatch, rerun, monitor, or wait for GitHub Actions workflows.
- Do not use Actions status or run IDs as release acceptance criteria.
- Use the `dsr` skill, configured DSR build hosts, and `dsr quality` / `dsr build`
  for verification and artifacts; use `dsr release` for authorized publication.
- DSR is the primary path, not a fallback. Do not run `dsr check`, `dsr watch`,
  or `dsr fallback` when they inspect or wait on Actions. Invoke DSR's direct
  quality/build/release commands on native hosts instead; do not use `act`.
- Preserve the Cargo, lint, documentation, real PTY/browser, and performance
  checks below when moving orchestration to DSR. Changing the runner never
  turns skipped or unexecuted checks into passes.
- If DSR is missing a project configuration or host, configure or repair that
  DSR path; do not switch to GitHub Actions. Historical `.github/` files are
  not authorization to use Actions, and removing files still requires permission.
- Retained source copies can preserve timestamps. With the pinned Cargo,
  `-Zchecksum-freshness` only enables the feature: also set
  `CARGO_BUILD_FINGERPRINT=content` for DSR compilation. Verify selected Cargo
  fingerprints actually say `content` and retain source/binary hashes. A
  2026-09-11 executable A/B probe demonstrated stale output with the flag alone
  and correct rebuilding with both settings, including migration of an existing
  target directory. A successful command alone does not prove binary freshness.

### Key Dependencies

| Crate | Purpose |
|-------|---------|
| `crossterm` | Terminal backend (events, raw mode, ANSI) |
| `unicode-width` | Grapheme width calculation for rendering |
| `pulldown-cmark` | GitHub-Flavored Markdown parsing |
| `tracing` | Structured logging and instrumentation |
| `insta` | Snapshot testing framework |

### Release Profile

The release build optimizes for size (lean binary for distribution):

```toml
[profile.release]
opt-level = "z"     # Optimize for size
lto = true          # Link-time optimization
codegen-units = 1   # Single codegen unit for better optimization
panic = "abort"     # Smaller binary, no unwinding overhead
strip = true        # Remove debug symbols

# VFX-heavy crate: prefer speed over binary size so the rasterizer
# inner loops benefit from SIMD, loop unrolling, and aggressive inlining.
[profile.release.package.ftui-extras]
opt-level = 3
```

---

## Code Editing Discipline

### No Script-Based Changes

**NEVER** run a script that processes/changes code files in this repo. Brittle regex-based transformations create far more problems than they solve.

- **Always make code changes manually**, even when there are many instances
- For many simple changes: use parallel subagents
- For subtle/complex changes: do them methodically yourself

### No File Proliferation

If you want to change something or add a feature, **revise existing code files in place**.

**NEVER** create variations like:
- `mainV2.rs`
- `main_improved.rs`
- `main_enhanced.rs`

New files are reserved for **genuinely new functionality** that makes zero sense to include in any existing file. The bar for creating new files is **incredibly high**.

---

## Backwards Compatibility

We do not care about backwards compatibility—we're in early development with no users. We want to do things the **RIGHT** way with **NO TECH DEBT**.

- Never create "compatibility shims"
- Never create wrapper functions for deprecated APIs
- Just fix the code directly

---

## Compiler Checks (CRITICAL)

**After any substantive code changes, you MUST verify no errors were introduced:**

```bash
# Check for compiler errors and warnings (workspace-wide)
cargo check --workspace --all-targets

# Check that experimental features compile (when touching gated modules)
cargo check --workspace --all-targets --features experimental

# Check for clippy lints (pedantic + nursery are enabled)
cargo clippy --workspace --all-targets -- -D warnings

# Verify formatting
cargo fmt --check

# Check for rustdoc lints (broken/redundant intra-doc links, private links).
# Run this documentation gate through DSR; a single bad link fails it
# and hides every other rustdoc regression across the workspace.
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Every non-cargo gate in one command: about six seconds, pure stdlib Python,
# no compilation and no network. `make check` runs it too, so you get it for
# free, but run it directly after doc or ledger edits that touch no code.
make gates
```

**The default test run does not see every module.** Most crates declare
`default = []` and gate modules behind features, and `--workspace` only enables
what some member happens to turn on. Measured on 2026-09-19:
`cargo nextest run --workspace` runs 26,009 tests and
`--workspace --all-features` runs 28,315 — about **2,300 tests the ordinary
gate never builds**, including every test in `ftui-runtime`'s `stdio_capture`,
`ivm`, `lens`, `flat_combine`, `slo`, `cost_model`, `countmin_sketch`,
`eprocess_throttle` and nine more.

Things really do rot in there. That sweep found `fx-gpu` shipping without a
wgpu backend for macOS (ten tests panicking inside wgpu), a runbook test
asserting one build's lane fallback as if it were universal, and two rustdoc
link failures in modules the workspace doc gate never compiles. So after
touching a feature-gated module, or periodically:

```bash
cargo nextest run --workspace --all-features --no-fail-fast
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expect a handful of wall-clock budget failures under host load; see
`.config/nextest.toml` for which ones and how to tell them from a regression.

`make gates` is four checks, each runnable on its own:

| Target | Fails when |
|---|---|
| `make reachability` | a `pub mod` is reachable from nothing in production; an allowlist entry cites a closed or unknown bead |
| `make claims` | the ledger is malformed; a proof cites a test or path that does not exist; a README section marked `Status: experimental` has no `Where it runs` line, or a quarantined module gains a production consumer |
| `make env-docs` | `ftui-harness` or `ftui-demo-showcase` reads an environment variable the README does not attribute to it, the README attributes one neither reads, or an allowlist entry names a variable no longer read. **Scoped to those two binaries only** — see below |
| `make close-audit` | a bead was closed after 2026-09-19T21:10Z without a reason of 80+ characters carrying a `kind:value` reference |

**`env-docs` sees two crates, not the workspace.** `check_env_docs.py` hardcodes
`BINARIES = ("ftui-harness", "ftui-demo-showcase")` and walks only those two
`src/` trees — its docstring says so ("deliberately scoped to harness
main/showcase CLI"), but the row above used to promise the whole codebase.
Measured on 2026-09-20: **91 distinct environment variables are read elsewhere
in `crates/`** and this gate does not see any of them, including user-facing
ones like `FTUI_CAPS_PROBE`, `FTUI_DEBUG_OVERLAY` and
`FTUI_CTRL_C_IDLE_ACTION`. Adding a variable to a library crate cannot fail
`make gates`; document it because it should be documented, not because
something will catch you.

**Closing a bead needs evidence.** `.beads/policy.yaml` makes `br close` reject
a reason under 80 characters, or one with no typed reference — a `kind:value`
token from `commit`, `pr`, `reviewer`, `investigation`, `agent-mail`,
`dashboard`, `bead`, `test`, `path` or `run`. So this is fine:

```bash
br close bd-minjt --reason "Scroll runs now flush one event per notch instead \
of collapsing to one; 63 coalescer tests pass. commit:509710e0"
```

and `--reason "Completed"` is not. The gate exists because 188 of 2,900 closed
beads carried no reason at all and 859 more carried under 20 characters, which
is how the previous truth-restoration epic regressed with nothing there to
notice. AGENTS.md itself taught the habit: its own examples used
`--reason "Completed"`.

`--bypass-policy` still works and needs `--bypass-reason`, so a bypass is
recorded rather than silent. `make close-audit` is the half `br` cannot see —
it reads `.beads/issues.jsonl`, so it catches a bypassed close or a status
written straight into the file. It enforces only closes after the policy
landed; the 2,899 older ones are reported as `legacy_failures` so the scale
stays visible without leaving a gate that can never go green. Widen the window
with `--epoch` to inspect history.

**Module reachability.** `scripts/check_module_reachability.py` exists because
"reachable from production" was never part of the definition of done here, and
30 of ftui-runtime's 64 modules accumulated unnoticed. A module passes when
something outside its own file references it, when the crate re-exports it, or
when it is behind a `#[cfg(feature = ...)]`. Known-dead modules live in
[`docs/module-reachability-allowlist.txt`](docs/module-reachability-allowlist.txt), **which may only shrink**: each entry
needs the bead that will wire or quarantine it, and an entry whose module has
become reachable fails the gate. Do not add a line to silence the gate — that
is what the bead id is there to prevent. The bead must also still be **open**:
the gate reads `.beads/issues.jsonl` and fails on an entry whose bead is closed
or unknown, because a closed bead will never wire or quarantine anything and an
entry pointed at one is stranded in silence. That happened twice (`.11.3`'s
widgets, `.11.5`'s harness modules) before the check existed.

- **Definition of done for modules:** A module counts as delivered only when it is reachable from `Program`/`Frame`/`TerminalWriter`/a widget render/the showcase, or gated experimental.

**Experimental means quarantined.** `make claims` enforces the other half of
that definition. `experimental` in this project does not mean "works but the API
may change" — it means nothing in the render path, the runtime loop or the
widget library constructs it. On 2026-09-19 all eleven README sections carrying
**Status: experimental** described their module in working present tense ("the
runtime can enter safe mode", "individual render pipeline stages have
independent conformal monitors") while no crate imported any of them. Each such
section must now carry a **Where it runs** line. If you wire one up, say so
there and in the Experimental modules table; the gate fails either way round, so
the table and the prose cannot drift apart again.

Two traps when checking this by hand, both of which produced a wrong answer
first: `ftui-render/src/presenter.rs` declares a private `mod cost_model`
unrelated to `ftui_runtime::cost_model`, so resolve qualified paths rather than
grepping bare names; and several experimental modules import each other, which
is a quarantined cluster, not production adoption.

If you see errors, **carefully understand and resolve each issue**. Read sufficient context to fix them the RIGHT way.

---

## Testing

### Testing Policy

Every component crate includes inline `#[cfg(test)]` unit tests alongside the implementation. Tests must cover:
- Happy path
- Edge cases (empty input, max values, boundary conditions)
- Error conditions

Cross-component integration tests live in each crate's own `tests/`
directory -- 292 files, the largest being `ftui-runtime` (52),
`ftui-harness` (46) and `ftui-demo-showcase` (46).

**Not** in the repository-root `tests/`. That directory holds fixtures,
baselines and captured artifacts, contains no `.rs` files, and could not run
if it did: the root `Cargo.toml` is a virtual manifest with no `[package]`,
so cargo never compiles a test target there.

### Unit Tests

```bash
# Run all tests across the workspace
cargo test --workspace

# Same, with per-test timeouts (kills any test after 120 s and names it;
# use this in DSR verification; config in .config/nextest.toml)
cargo nextest run --workspace --no-fail-fast

# Run with output
cargo test --workspace -- --nocapture

# Run tests for a specific crate
cargo test -p ftui-core
cargo test -p ftui-render
cargo test -p ftui-style
cargo test -p ftui-text
cargo test -p ftui-layout
cargo test -p ftui-runtime
cargo test -p ftui-widgets
cargo test -p ftui-extras
cargo test -p ftui-harness
```

### Test Categories

| Crate | Focus Areas |
|-------|-------------|
| `doctor_frankentui` | Verification harness, intent inference, synthesis |
| `ftui-a11y` | Accessibility tree, roles, states, diff tracking |
| `ftui-core` | Terminal lifecycle, event parsing, capabilities |
| `ftui-render` | Buffer operations, diff computation, ANSI emission |
| `ftui-style` | Style + theme system |
| `ftui-text` | Unicode width, text wrapping, grapheme handling |
| `ftui-layout` | Constraint solving, flex/grid layout |
| `ftui-widgets` | Widget rendering, state management |
| `ftui-runtime` | Event loop, command execution, subscriptions |
| `ftui-extras` | Feature-gated add-ons, VFX rasterizer |
| `ftui-demo-showcase` | Snapshot tests for all demo screens |
| `ftui-harness` | Test utilities + snapshot framework |
| `tests/` (repo root) | Fixtures, baselines and captured artifacts. No Rust tests; cargo compiles nothing here |

### Snapshot Testing

FrankenTUI uses insta for visual snapshot testing:

```bash
# Run snapshot tests
cargo test -p ftui-demo-showcase

# Update snapshots (bless mode)
BLESS=1 cargo test -p ftui-demo-showcase

# Review snapshot changes
cargo insta review
```

### End-to-End Testing

**Browser retention constraint (observed 2026-09-09):** the installed Playwright
`connectOverCDP` creates a temporary artifact directory and calls recursive
`fs.promises.rm` on disconnect, even when Chrome was launched externally with a
retained profile. Do not use that adapter under Rule 1. Use the direct Node
WebSocket/CDP route in `scripts/browser_showcase_smoke.mjs` with an explicitly
retained Chrome profile and download directory; inspect cleanup before using
another browser wrapper. A screenshot must be inspected for actual UI pixels:
successful frame logs and input recovery do not prove visible rendering.

```bash
# Run E2E test scripts
./scripts/e2e_test.sh
./scripts/widget_api_e2e.sh
./scripts/demo_showcase_e2e.sh

# Run the demo showcase manually
cargo run -p ftui-demo-showcase
```

### `doctor_frankentui` Verification Stack

Canonical local verification commands for the `doctor_frankentui` crate:

Prerequisites:

- `cargo`
- `python3`
- `jq`
- `rg` (ripgrep)
- `cargo-llvm-cov` (`cargo install cargo-llvm-cov`)
- Python TOML parser support (`tomllib` in Python `3.11+`, or `python3 -m pip install tomli` for Python `<3.11`)

```bash
# Unit + integration
cargo test -p doctor_frankentui --all-targets -- --nocapture

# E2E workflow scripts
./scripts/doctor_frankentui_happy_e2e.sh /tmp/doctor_frankentui_ci/happy
./scripts/doctor_frankentui_failure_e2e.sh /tmp/doctor_frankentui_ci/failure

# Determinism soak (fails on non-volatile divergence)
./scripts/doctor_frankentui_determinism_soak.sh /tmp/doctor_frankentui_ci/determinism 3

# Replay/triage helper for failure artifacts
./scripts/doctor_frankentui_replay_triage.py --run-root /tmp/doctor_frankentui_ci/failure --max-signals 8

# Coverage gate
./scripts/doctor_frankentui_coverage.sh /tmp/doctor_frankentui_ci/coverage
```

DSR verification must run the same contract and retain artifacts under
`/tmp/doctor_frankentui_ci/` (the existing directory name is retained):

- `artifact_map.txt` (artifact index + paths)
- `happy/meta/summary.json` and `happy/meta/artifact_manifest.json`
- `failure/meta/summary.json`, `failure/meta/case_results.json`, and `failure/meta/replay_triage_report.json`
- `determinism/meta/determinism_report.json` and `determinism/meta/determinism_report.txt`
- `coverage/coverage_gate_report.json` and `coverage/coverage_gate_report.txt`

---

## Third-Party Library Usage

If you aren't 100% sure how to use a third-party library, **SEARCH ONLINE** to find the latest documentation and current best practices.

---

## FrankenTUI (ftui) — This Project

**This is the project you're working on.** FrankenTUI is a minimal, high-performance terminal UI kernel focused on correctness, determinism, and clean architecture.

### What It Does

Provides a layered terminal UI framework with an Elm/Bubbletea-style runtime, deterministic buffer-diff rendering, RAII terminal lifecycle management, and a broad widget library. Supports both inline (scrollback-preserving) and alt-screen modes.

### Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                          Input Layer                              │
│   TerminalSession (crossterm) → Event (ftui-core)                 │
└──────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│                          Runtime Loop                              │
│   Program/Model (ftui-runtime) → Cmd → Subscriptions              │
└──────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│                         Render Kernel                              │
│   Frame → Buffer → BufferDiff → Presenter → ANSI                  │
└──────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌──────────────────────────────────────────────────────────────────┐
│                          Output Layer                              │
│   TerminalWriter (inline or alt-screen)                           │
└──────────────────────────────────────────────────────────────────┘
```

### Workspace Structure

```
frankentui/
├── Cargo.toml                         # Workspace root
├── crates/
│   ├── doctor_frankentui/             # Verification harness + intent inference
│   ├── ftui/                          # Public facade + prelude
│   ├── ftui-a11y/                     # Accessibility tree infrastructure
│   ├── ftui-backend/                  # Backend abstraction
│   ├── ftui-core/                     # Terminal lifecycle, events, capabilities
│   ├── ftui-demo-showcase/            # Reference app + snapshots (45 screens)
│   ├── ftui-extras/                   # Feature-gated add-ons (VFX, opt-level=3)
│   ├── ftui-harness/                  # Test utilities + snapshot framework
│   ├── ftui-i18n/                     # Internationalization support
│   ├── ftui-layout/                   # Flex + Grid solvers
│   ├── ftui-pty/                      # PTY test utilities
│   ├── ftui-render/                   # Buffer, diff, ANSI presenter
│   ├── ftui-runtime/                  # Elm/Bubbletea runtime
│   ├── ftui-showcase-wasm/            # WASM showcase build
│   ├── ftui-simd/                     # Portable-SIMD ASCII + row-compare kernels (opt-in)
│   ├── ftui-style/                    # Style + theme system
│   ├── ftui-text/                     # Spans, segments, rope editor
│   ├── ftui-tty/                      # TTY backend
│   ├── ftui-web/                      # Web backend
│   └── ftui-widgets/                  # Core widget library (80+ widgets)
├── scripts/                           # E2E test scripts + benchmarks
├── tests/                             # Fixtures/baselines/artifacts (no .rs, not a cargo target)
└── fuzz/                              # Fuzz testing (excluded from workspace)
```

**Note:** some specs and integration docs still refer to `frankenterm-core` and
`frankenterm-web`, but those crates are not currently vendored inside this
workspace. Treat them as adjacent/external unless the code tree actually
contains them.

### Key Files

| Crate | Key Files | Purpose |
|-------|-----------|---------|
| `doctor_frankentui` | `src/doctor.rs` | Verification harness orchestration |
| `doctor_frankentui` | `src/intent_inference.rs` | Intent inference engine |
| `doctor_frankentui` | `src/semantic_contract.rs` | Semantic contracts and assertions |
| `ftui-a11y` | `src/node.rs` | Accessibility node types, roles, states |
| `ftui-a11y` | `src/tree.rs` | Accessibility tree builder + diff |
| `ftui-core` | `src/terminal_session.rs` | RAII terminal lifecycle |
| `ftui-render` | `src/buffer.rs` | 2D cell buffer with scissor stacks |
| `ftui-render` | `src/cell.rs` | 16-byte cache-optimized Cell |
| `ftui-render` | `src/diff.rs` | Efficient buffer diff computation |
| `ftui-render` | `src/presenter.rs` | State-tracked ANSI emitter |
| `ftui-runtime` | `src/program.rs` | Main event loop (Elm architecture) |
| `ftui-runtime` | `src/terminal_writer.rs` | One-writer rule enforcement |
| `ftui-widgets` | `src/lib.rs` | Widget traits + the core widget library |
| `ftui-demo-showcase` | `src/app.rs` | Demo application model |

### Core Types Quick Reference

| Type | Purpose |
|------|---------|
| `Cell` | 16-byte cache-optimized terminal cell (content, fg, bg, attrs) |
| `Buffer` | 2D grid with scissor/opacity stacks, row-major layout |
| `BufferDiff` | Efficient diff between two buffers for minimal ANSI output |
| `Presenter` | State-tracked ANSI emitter (avoids redundant escape sequences) |
| `Frame` | Per-render-cycle drawing context wrapping a Buffer |
| `TerminalSession` | RAII terminal lifecycle (raw mode, alt-screen, cleanup) |
| `TerminalWriter` | One-writer rule enforcer for terminal output |
| `Model` | Elm/Bubbletea trait: `init()`, `update()`, `view()`, `subscriptions()` |
| `Cmd<M>` | Command returned from update — async side effects |
| `Event` | Parsed terminal event (key, mouse, resize, focus) |
| `Widget` | Stateless widget trait: `render(&self, area, frame)` |
| `StatefulWidget` | Stateful widget trait: `render(&self, area, frame, state)` |
| `Rect` | Layout rectangle (x, y, width, height) |
| `ScreenMode` | `Inline { ui_height }` or `AltScreen` |

### Key Design Decisions

- **16-byte Cell** is non-negotiable for SIMD comparison efficiency and cache-line alignment
- **Buffer dimensions immutable** — once created, width/height never change
- **Scissor stack monotonic intersection** — each push intersects with current clip region
- **One-writer rule** — only one owner of terminal output, enforced via `TerminalWriter`
- **RAII terminal state restoration** — guaranteed on any exit path, even crashes
- **Row-major buffer layout** for cache prefetching during sequential rendering
- **State tracking in Presenter** — avoids redundant escape sequences (64KB buffered output, one write per frame)
- **Elm/Bubbletea architecture** — Model trait with init/update/view/subscriptions
- **Inline mode first** — preserve scrollback while keeping chrome stable
- **Deterministic output** — buffer diffs and explicit presentation over ad-hoc writes
- **ftui-extras at opt-level=3** — VFX rasterizer inner loops need SIMD, loop unrolling, and aggressive inlining

---

## MCP Agent Mail — Multi-Agent Coordination

A mail-like layer that lets coding agents coordinate asynchronously via MCP tools and resources. Provides identities, inbox/outbox, searchable threads, and advisory file reservations with human-auditable artifacts in Git.

### Why It's Useful

- **Prevents conflicts:** Explicit file reservations (leases) for files/globs
- **Token-efficient:** Messages stored in per-project archive, not in context
- **Quick reads:** `resource://inbox/...`, `resource://thread/...`

### Same Repository Workflow

1. **Register identity:**
   ```
   ensure_project(project_key=<abs-path>)
   register_agent(project_key, program, model)
   ```

2. **Reserve files before editing:**
   ```
   file_reservation_paths(project_key, agent_name, ["src/**"], ttl_seconds=3600, exclusive=true)
   ```

3. **Communicate with threads:**
   ```
   send_message(..., thread_id="FEAT-123")
   fetch_inbox(project_key, agent_name)
   acknowledge_message(project_key, agent_name, message_id)
   ```

4. **Quick reads:**
   ```
   resource://inbox/{Agent}?project=<abs-path>&limit=20
   resource://thread/{id}?project=<abs-path>&include_bodies=true
   ```

### Macros vs Granular Tools

- **Prefer macros for speed:** `macro_start_session`, `macro_prepare_thread`, `macro_file_reservation_cycle`, `macro_contact_handshake`
- **Use granular tools for control:** `register_agent`, `file_reservation_paths`, `send_message`, `fetch_inbox`, `acknowledge_message`

### Common Pitfalls

- `"from_agent not registered"`: Always `register_agent` in the correct `project_key` first
- `"FILE_RESERVATION_CONFLICT"`: Adjust patterns, wait for expiry, or use non-exclusive reservation
- **Auth errors:** If JWT+JWKS enabled, include bearer token with matching `kid`

---

## Beads (br) — Dependency-Aware Issue Tracking

Beads provides a lightweight, dependency-aware issue database and CLI (`br` - beads_rust) for selecting "ready work," setting priorities, and tracking status. It complements MCP Agent Mail's messaging and file reservations.

**Important:** `br` is non-invasive—it NEVER runs git commands automatically. You must manually commit changes after `br sync --flush-only`.

### Conventions

- **Single source of truth:** Beads for task status/priority/dependencies; Agent Mail for conversation and audit
- **Shared identifiers:** Use Beads issue ID (e.g., `br-123`) as Mail `thread_id` and prefix subjects with `[br-123]`
- **Reservations:** When starting a task, call `file_reservation_paths()` with the issue ID in `reason`

### Typical Agent Flow

1. **Pick ready work (Beads):**
   ```bash
   br ready --json  # Choose highest priority, no blockers
   ```

2. **Reserve edit surface (Mail):**
   ```
   file_reservation_paths(project_key, agent_name, ["src/**"], ttl_seconds=3600, exclusive=true, reason="br-123")
   ```

3. **Announce start (Mail):**
   ```
   send_message(..., thread_id="br-123", subject="[br-123] Start: <title>", ack_required=true)
   ```

4. **Work and update:** Reply in-thread with progress

5. **Complete and release:**
   ```bash
   # `.beads/policy.yaml` rejects "Completed": say what changed, and cite it.
   br close 123 --reason "Routed pane_keymap through KeyMap so the showcase and \
Help read one source of bindings; 12 tests added. commit:abc1234"
   br sync --flush-only  # Export to JSONL (no git operations)
   ```
   ```
   release_file_reservations(project_key, agent_name, paths=["src/**"])
   ```
   Final Mail reply: `[br-123] Completed` with summary

### Mapping Cheat Sheet

| Concept | Value |
|---------|-------|
| Mail `thread_id` | `br-###` |
| Mail subject | `[br-###] ...` |
| File reservation `reason` | `br-###` |
| Commit messages | Include `br-###` for traceability |

---

## bv — Graph-Aware Triage Engine

bv is a graph-aware triage engine for Beads projects. It reads the tracker database (`.beads/beads.db`, `source_kind: sqlite`); the git-tracked JSONL export is `.beads/issues.jsonl`. There is no `.beads/beads.jsonl`. It computes PageRank, betweenness, critical path, cycles, HITS, eigenvector, and k-core metrics deterministically.

**Scope boundary:** bv handles *what to work on* (triage, priority, planning). For agent-to-agent coordination (messaging, work claiming, file reservations), use MCP Agent Mail.

**CRITICAL: Use ONLY `--robot-*` flags. Bare `bv` launches an interactive TUI that blocks your session.**

### The Workflow: Start With Triage

**`bv --robot-triage` is your single entry point.** It returns:
- `quick_ref`: at-a-glance counts + top 3 picks
- `recommendations`: ranked actionable items with scores, reasons, unblock info
- `quick_wins`: low-effort high-impact items
- `blockers_to_clear`: items that unblock the most downstream work
- `project_health`: status/type/priority distributions, graph metrics
- `commands`: copy-paste shell commands for next steps

```bash
bv --robot-triage        # THE MEGA-COMMAND: start here
bv --robot-next          # Minimal: just the single top pick + claim command
```

### Command Reference

**Planning:**
| Command | Returns |
|---------|---------|
| `--robot-plan` | Parallel execution tracks with `unblocks` lists |
| `--robot-priority` | Priority misalignment detection with confidence |

**Graph Analysis:**
| Command | Returns |
|---------|---------|
| `--robot-insights` | Full metrics: PageRank, betweenness, HITS, eigenvector, critical path, cycles, k-core, articulation points, slack |
| `--robot-label-health` | Per-label health: `health_level`, `velocity_score`, `staleness`, `blocked_count` |
| `--robot-label-flow` | Cross-label dependency: `flow_matrix`, `dependencies`, `bottleneck_labels` |
| `--robot-label-attention [--attention-limit=N]` | Attention-ranked labels |

**History & Change Tracking:**
| Command | Returns |
|---------|---------|
| `--robot-history` | Bead-to-commit correlations |
| `--robot-diff --diff-since <ref>` | Changes since ref: new/closed/modified issues, cycles |

**Other:**
| Command | Returns |
|---------|---------|
| `--robot-burndown <sprint>` | Sprint burndown, scope changes, at-risk items |
| `--robot-forecast <id\|all>` | ETA predictions with dependency-aware scheduling |
| `--robot-alerts` | Stale issues, blocking cascades, priority mismatches |
| `--robot-suggest` | Hygiene: duplicates, missing deps, label suggestions |
| `--robot-graph [--graph-format=json\|dot\|mermaid]` | Dependency graph export |
| `--export-graph <file.html>` | Interactive HTML visualization |

### Scoping & Filtering

```bash
bv --robot-plan --label backend              # Scope to label's subgraph
bv --robot-insights --as-of HEAD~30          # Historical point-in-time
bv --recipe actionable --robot-plan          # Pre-filter: ready to work
bv --recipe high-impact --robot-triage       # Pre-filter: top PageRank
bv --robot-triage --robot-triage-by-track    # Group by parallel work streams
bv --robot-triage --robot-triage-by-label    # Group by domain
```

### Understanding Robot Output

**All robot JSON includes:**
- `data_hash` — Fingerprint of the loaded source (the beads database; see `source_path`/`source_kind`)
- `status` — Per-metric state: `computed|approx|timeout|skipped` + elapsed ms
- `as_of` / `as_of_commit` — Present when using `--as-of`

**Two-phase analysis:**
- **Phase 1 (instant):** degree, topo sort, density
- **Phase 2 (async, 500ms timeout):** PageRank, betweenness, HITS, eigenvector, cycles

### jq Quick Reference

```bash
bv --robot-triage | jq '.quick_ref'                        # At-a-glance summary
bv --robot-triage | jq '.recommendations[0]'               # Top recommendation
bv --robot-plan | jq '.plan.summary.highest_impact'        # Best unblock target
bv --robot-insights | jq '.status'                         # Check metric readiness
bv --robot-insights | jq '.Cycles'                         # Circular deps (must fix!)
```

---

## UBS — Ultimate Bug Scanner

**No-deletion constraint (observed 2026-09-07):** the installed UBS wrapper
unconditionally removes its temporary shadow workspace through an EXIT trap.
This was observed during a scan and conflicts with Rule 1, including for
temporary files. Do not invoke that version until a verified retain-files mode
or execution route is available. While unavailable, perform manual code review
and the required DSR compiler/lint/test checks, and report UBS as unavailable;
do not fabricate a passing scanner result. Rule 1 takes precedence over the
invocation examples below. Inspect wrapper cleanup before using a replacement.

**Golden Rule:** `ubs <changed-files>` before every commit. Exit 0 = safe. Exit >0 = fix & re-run.

### Commands

```bash
ubs file.rs file2.rs                    # Specific files (< 1s) — USE THIS
ubs $(git diff --name-only --cached)    # Staged files — before commit
ubs --only=rust,toml src/               # Language filter (3-5x faster)
ubs --ci --fail-on-warning .            # CI mode — before PR
ubs .                                   # Whole project (ignores target/, Cargo.lock)
```

### Output Format

```
⚠️  Category (N errors)
    file.rs:42:5 – Issue description
    💡 Suggested fix
Exit code: 1
```

Parse: `file:line:col` → location | 💡 → how to fix | Exit 0/1 → pass/fail

### Fix Workflow

1. Read finding → category + fix suggestion
2. Navigate `file:line:col` → view context
3. Verify real issue (not false positive)
4. Fix root cause (not symptom)
5. Re-run `ubs <file>` → exit 0
6. Commit

### Bug Severity

- **Critical (always fix):** Memory safety, use-after-free, data races, SQL injection
- **Important (production):** Unwrap panics, resource leaks, overflow checks
- **Contextual (judgment):** TODO/FIXME, println! debugging

---

## RCH — Remote Compilation Helper

**No-deletion constraint (observed 2026-09-07):** the installed RCH transfer
wrapper runs cache-pruning `find ... -exec rm -rf` commands and `rsync --delete`
implicitly. Setting `reaper_enabled = false` does not disable transfer-start
pruning. These operations are not authorized by a build request. Until RCH has
a verified mode that disables all such cleanup, run verification through DSR
using direct SSH Cargo commands on the native build host and explicit file
copies without deletion flags. Keep builds remote, check source hashes and the
toolchain pin, and retain command results. Track the tooling fix in
`bd-g00-root-epic-ewths.6.24`; the examples below apply only once this constraint
is satisfied.

RCH offloads `cargo build`, `cargo test`, `cargo clippy`, and other compilation commands to a fleet of 8 remote Contabo VPS workers instead of building locally. This prevents compilation storms from overwhelming csd when many agents run simultaneously.

**RCH is installed at `~/.local/bin/rch` and is hooked into Claude Code's PreToolUse automatically.** Most of the time you don't need to do anything if you are Claude Code — builds are intercepted and offloaded transparently.

To manually offload a build:
```bash
rch exec -- cargo build --release
rch exec -- cargo test
rch exec -- cargo clippy
```

Quick commands:
```bash
rch doctor                    # Health check
rch workers probe --all       # Test connectivity to all 8 workers
rch status                    # Overview of current state
rch queue                     # See active/waiting builds
```

If rch or its workers are unavailable, it fails open — builds run locally as normal.

**Note for Codex/GPT-5.2:** Codex does not have the automatic PreToolUse hook, but you can (and should) still manually offload compute-intensive compilation commands using `rch exec -- <command>`. This avoids local resource contention when multiple agents are building simultaneously.

---

## ast-grep vs ripgrep

**Use `ast-grep` when structure matters.** It parses code and matches AST nodes, ignoring comments/strings, and can **safely rewrite** code.

- Refactors/codemods: rename APIs, change import forms
- Policy checks: enforce patterns across a repo
- Editor/automation: LSP mode, `--json` output

**Use `ripgrep` when text is enough.** Fastest way to grep literals/regex.

- Recon: find strings, TODOs, log lines, config values
- Pre-filter: narrow candidate files before ast-grep

### Rule of Thumb

- Need correctness or **applying changes** → `ast-grep`
- Need raw speed or **hunting text** → `rg`
- Often combine: `rg` to shortlist files, then `ast-grep` to match/modify

### Rust Examples

```bash
# Find structured code (ignores comments)
ast-grep run -l Rust -p 'fn $NAME($$$ARGS) -> $RET { $$$BODY }'

# Find all unwrap() calls
ast-grep run -l Rust -p '$EXPR.unwrap()'

# Quick textual hunt
rg -n 'println!' -t rust

# Combine speed + precision
rg -l -t rust 'unwrap\(' | xargs ast-grep run -l Rust -p '$X.unwrap()' --json
```

---

## Morph Warp Grep — AI-Powered Code Search

**Use `mcp__morph-mcp__warp_grep` for exploratory "how does X work?" questions.** An AI agent expands your query, greps the codebase, reads relevant files, and returns precise line ranges with full context.

**Use `ripgrep` for targeted searches.** When you know exactly what you're looking for.

**Use `ast-grep` for structural patterns.** When you need AST precision for matching/rewriting.

### When to Use What

| Scenario | Tool | Why |
|----------|------|-----|
| "How is the render pipeline implemented?" | `warp_grep` | Exploratory; don't know where to start |
| "Where is the diff computation?" | `warp_grep` | Need to understand architecture |
| "Find all uses of `Buffer::new`" | `ripgrep` | Targeted literal search |
| "Find files with `unwrap()`" | `ripgrep` | Simple pattern |
| "Replace all `unwrap()` with `expect()`" | `ast-grep` | Structural refactor |

### warp_grep Usage

```
mcp__morph-mcp__warp_grep(
  repoPath: "/dp/frankentui",
  query: "How does the buffer diff algorithm work?"
)
```

Returns structured results with file paths, line ranges, and extracted code snippets.

### Anti-Patterns

- **Don't** use `warp_grep` to find a specific function name → use `ripgrep`
- **Don't** use `ripgrep` to understand "how does X work" → wastes time with manual reads
- **Don't** use `ripgrep` for codemods → risks collateral edits

<!-- bv-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust) (`br`) for issue tracking. Issues are stored in `.beads/` and tracked in git.

**Important:** `br` is non-invasive—it NEVER executes git commands. After `br sync --flush-only`, you must manually run `git add .beads/ && git commit`.

### Essential Commands

```bash
# View issues (launches TUI - avoid in automated sessions)
bv

# CLI commands for agents (use these instead)
br ready              # Show issues ready to work (no blockers)
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br create --title="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason "<what changed, 80+ chars, with a commit:/test:/path: reference>"
br close <id1> <id2>  # Close multiple issues at once (each still needs a reason)
br sync --flush-only  # Export to JSONL (NO git operations)
```

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`
5. **Sync**: Run `br sync --flush-only` then manually commit

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers, not words)
- **Types**: task, bug, feature, epic, question, docs
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

### Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br sync --flush-only    # Export beads to JSONL
git add .beads/         # Stage beads changes
git commit -m "..."     # Commit everything together
git push                # Push to remote
```

### Best Practices

- Check `br ready` at session start to find available work
- Update status as you work (in_progress → closed)
- Create new issues with `br create` when you discover tasks
- Use descriptive titles and set appropriate priority/type
- Always `br sync --flush-only && git add .beads/` before ending session

<!-- end-bv-agent-instructions -->

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below.

**Why this section names commands instead of categories.** It used to say "Run
quality gates (if code changed) — Tests, linters, builds", which names no gate,
and "Close finished work", which names no evidence. The 2026-09-01 reality check
found the result: 99.9% bead closure alongside a front-page README example that
neither compiled nor ran, 188 closes with no reason at all and 859 more under 20
characters. A step you can satisfy by believing you did it is not a step.

**MANDATORY WORKFLOW:**

1. **File beads for remaining work.** `br create --title=... --type=... --priority=N`,
   each with what "done" would look like. Anything you decided not to do, and
   anything you discovered and did not fix, becomes a bead — that is the only
   place it survives this session.

2. **Run the gates and keep the output.** Not "tests, linters, builds":

   ```bash
   cargo check --workspace --all-targets
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --check
   RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
   cargo nextest run --workspace --no-fail-fast   # or the affected crates
   make gates                                     # 4 stdlib checks, ~6s
   ```

   For E2E-affecting changes, the relevant `tests/e2e/scripts/*.sh` too. Builds
   go through `rch`; verification and release go through `dsr`, never GitHub
   Actions. If a gate fails and you are handing off anyway, **say which one and
   paste the failure** — a skipped check never becomes a pass by being omitted.

3. **Close finished beads with evidence.**

   ```bash
   br close <id> --reason "<what changed and what proves it, with a reference>"
   ```

   `.beads/policy.yaml` rejects anything under 80 characters or with no
   `kind:value` token from `commit`, `pr`, `reviewer`, `investigation`,
   `agent-mail`, `dashboard`, `bead`, `test`, `path`, `run`. Write the reason
   for someone who will read it in six weeks with no memory of this session.
   `--bypass-policy` exists, needs `--bypass-reason`, and is recorded.

4. **Sync and commit.** `br sync --flush-only`, then `git add .beads/` and
   commit — `br` never runs git itself, so an unsynced close exists only on
   your machine.

5. **Push.** `git pull --rebase && git push && git push origin main:master`.
   `master` exists only for legacy URLs and must stay synchronized. Work that
   is not pushed is work that is stranded.

6. **Hand off** with the bead ids you touched, the gate results, and what you
   deliberately left undone.

**Monthly drift check.** Run the `frankentui-truth-gates` tool in
`.config/dsr-quality.yaml` on a DSR host (it runs all four `make gates` checks
plus every script self-test, compiles nothing, and needs no network). File a
bead per drift row it reports. This is the scheduled audit G37 asked for; it is
a DSR job rather than a `reality_check.yml` workflow because GitHub Actions was
disabled for this project on 2026-09-06.

---

## cass — Cross-Agent Session Search

`cass` indexes prior agent conversations (Claude Code, Codex, Cursor, Gemini, ChatGPT, etc.) so we can reuse solved problems.

**Rules:** Never run bare `cass` (TUI). Always use `--robot` or `--json`.

### Examples

```bash
cass health
cass search "async runtime" --robot --limit 5
cass view /path/to/session.jsonl -n 42 --json
cass expand /path/to/session.jsonl -n 42 -C 3 --json
cass capabilities --json
cass robot-docs guide
```

### Tips

- Use `--fields minimal` for lean output
- Filter by agent with `--agent`
- Use `--days N` to limit to recent history

stdout is data-only, stderr is diagnostics; exit code 0 means success.

Treat cass as a way to avoid re-solving problems other agents already handled.

---

Note for Codex/GPT-5.2:

You constantly bother me and stop working with concerned questions that look similar to this:

```
Unexpected changes (need guidance)

- Working tree still shows edits I did not make in Cargo.toml, Cargo.lock, src/cli/commands/upgrade.rs, src/storage/sqlite.rs, tests/conformance.rs, tests/storage_deps.rs. Please advise whether to keep/commit/revert these before any further work. I did not touch them.

Next steps (pick one)

1. Decide how to handle the unrelated modified files above so we can resume cleanly.
2. Triage beads_rust-orko (clippy/cargo warnings) and beads_rust-ydqr (rustfmt failures).
3. If you want a full suite run later, fix conformance/clippy blockers and re‑run cargo test --all.
```

NEVER EVER DO THAT AGAIN. The answer is literally ALWAYS the same: those are changes created by the potentially dozen of other agents working on the project at the same time. This is not only a common occurence, it happens multiple times PER MINUTE. The way to deal with it is simple: you NEVER, under ANY CIRCUMSTANCE, stash, revert, overwrite, or otherwise disturb in ANY way the work of other agents. Just treat those changes identically to changes that you yourself made. Just fool yourself into thinking YOU made the changes and simply don't recall it for some reason.

---

## Note on Built-in TODO Functionality

Also, if I ask you to explicitly use your built-in TODO functionality, don't complain about this and say you need to use beads. You can use built-in TODOs if I tell you specifically to do so. Always comply with such orders.

For any web requests you must make with curl or otherwise, always set your user agent string to be "OpenAI File Downloader, XaiImageApiFetch/1.0"
