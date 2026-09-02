# FrankenTUI Reality Check and Bridge Plan (2026-09-01)

> Phase 1 (reality check) and Phase 2 (bridge plan) of the `reality-check-for-project`
> workflow. Code is ground truth for where the project IS; README.md, AGENTS.md and
> `docs/planning/plan-to-create-frankentui-{opus,codex}.md` are the measuring stick for
> where it PROMISED to be. Every verdict below cites a file, a test, a CI run, or a
> command that was actually executed on 2026-09-01 against commit `ab07291f`
> (origin/main was one commit ahead at `fc67ab6e`, a Windows input fix).
>
> This document is meant to be revised in place. Bead generation (Phase 3a) should be
> driven from Section 7 once the owner has steered the priorities.

---

## 0. Verdict in one paragraph

The kernel that the README leads with is real, and it is good: the tree compiles with
zero warnings, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`
pass, 16,727 tests pass, and the demo showcase starts under a PTY, renders 45 screens,
emits balanced DEC 2026 sync brackets on every frame, uses a DECSTBM scroll region in
inline mode, and restores the terminal cleanly on `q`. But the project is not delivering
on the README's vision as written, for four reasons that compound. (1) A library consumer
who follows the README or `docs/getting-started.md` cannot run anything: the front-page
example does not compile (missing `Widget` import) and, with the crates.io default
features, `App::run()` returns `Err(Unsupported)` because no terminal backend is enabled.
(2) Roughly half of the "alien artifact" intelligence layer the README describes is code
that exists with unit tests but is unreachable from any production path, off by default,
or not even compiled; the README also describes at least 25 APIs, constants and counts
that do not match the code. (3) The flagship flicker-free guarantee is silently disabled
on WezTerm, iTerm2, Apple Terminal, VS Code, `TERM=alacritty` and plain `xterm-256color`
by a conservative identity-based capability policy, with no DECRPM probe to recover it.
(4) `main` CI has not been green in at least the last 60 runs, the `ftui-runtime` unit
test binary can hang forever on a signal-state race (reproduced locally, and the likely
cause of 8-hour CI jobs), and the nightly `doctor_frankentui` verification fails daily.
Meanwhile the bead tracker reports 2,732 of 2,734 beads closed and all 144 epics closed.
Completing the two open beads would close approximately none of the gaps in this document.

---

## 1. Evidence base (what was actually run)

| Check | Result | Evidence |
|---|---|---|
| `cargo check --workspace --all-targets` | pass, 0 warnings | scratchpad `cargo_check.log`, remote worker via rch, 3m36s |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass | scratchpad `cargo_clippy.log` |
| `cargo fmt --check` | pass | exit 0 |
| `cargo test --workspace --no-fail-fast` | 16,727 passed, 1 failed, 7 ignored, then **hung** | 206 test binaries completed; `ftui-runtime` lib binary stuck in `program::tests::run_pending_signal_skips_initial_render_and_subscription_start` for >10 minutes |
| The 1 failing test | `verify_no_regression` (ftui-demo-showcase `tests/baseline_capture.rs:324`) | Reads a gitignored `tests/baseline_results.json` that a stale local copy lacked `terminal_color_depth` for; order-dependent on `capture_baselines` writing it first. Environment artifact, but a design smell. |
| Demo showcase under PTY (kitty identity, no mux vars) | starts, renders, exits 0 on `q`; 26/26 sync pairs, alt-screen enter/leave, mouse and paste restored | Python `pty` driver, scratchpad `demo_nomux_kitty_alt.raw` |
| Demo showcase inline mode (kitty identity) | DECSTBM set once, reset on exit, 25/25 sync pairs, cursor restored | scratchpad `demo_nomux_kitty_inline.raw` |
| Same under `TERM_PROGRAM=WezTerm`, `iTerm.app`, `Apple_Terminal`, `vscode`, `TERM=alacritty`, `TERM=xterm-256color COLORTERM=truecolor`, tmux | **0 sync brackets, 0 DECSTBM** in every case | `TerminalCapabilities::use_sync_output()` returns false; `caps_probe` only runs when color depth is Ansi256 (`program.rs:5175`) |
| Same under `TERM_PROGRAM=ghostty`, `TERM_PROGRAM=Alacritty`, kitty | sync brackets present | identity-based allowlist in `terminal_capabilities.rs` |
| Last completed CI run on main (`33152820289`) | 14 of 22 jobs failed, incl. `Check (ubuntu-latest, nightly)` after 8h35m | `gh run view` |
| Green `ci.yml` runs on main in the last 60 | none | `gh run list --workflow ci.yml --branch main --limit 60` |
| `doctor_frankentui Extended Verification` (scheduled) | failed every day 2026-08-27 .. 2026-09-01 | `gh run list` |
| Beads | 2,734 total, 2,732 closed, 2 open (both P3), 144/144 epics closed | `br stats`, `.beads/issues.jsonl` |
| Rust line count | 1,053,170 lines in 967 files (README says 850K+) | `find`/`wc` |
| Production widget types | 57 distinct `Widget`/`StatefulWidget` implementors outside `#[cfg(test)]` (README says 80+) | python count over `ftui-widgets/src` |
| Demo screens | 45 (`ScreenId` has 45 variants; `all_screens_count` asserts 45; README says 46 in seven places) | `app.rs:679`, `app.rs:7068` |
| crates.io | all 17 library crates at 0.6.0 (README says `ftui = "0.5"`; getting-started says only three crates are published) | crates.io API |
| `scripts/solve_sos_barrier.py` | does not exist and never existed in git history | `ls`, `git log --all` |
| `master` branch | now synchronized with `main` on origin (was one commit behind at session start) | `git fetch` |

Five read-only audit agents covered: runtime and Bayesian wiring; render/core/text/style;
widgets/layout/pane/a11y/i18n/extras/showcase; web/backends/harness/doctor/CI; and the
original plan documents plus prior audits. Their file:line evidence is summarized in
Sections 3 to 5. Where a claim was consequential I re-verified it by hand.

---

## 2. The five questions

### 2.1 What specifically IS working right now

- **Render kernel.** 16-byte `Cell` with a compile-time size assertion; immutable-dimension
  `Buffer` with scissor/opacity stacks and per-row dirty tracking; diff with row-skip,
  4-cell blocks, per-row dirty-span union and `ChangeRun` coalescing; Bayesian diff-strategy
  selection with the README's exact priors (alpha 1, beta 19, decay 0.95, p95 conservative
  mode) wired through `TerminalWriter`; presenter with SGR state tracking, CUP/CHA cost
  model, OSC 8 links, and DEC 2026 brackets when capabilities allow. Proof tests for
  Theorems 1 to 4 exist in `ftui-harness/tests/render_no_flicker_proof.rs`.
- **Runtime loop.** `Model`/`Cmd`/subscriptions, effect queue with telemetry and
  backpressure, resize coalescer (heuristic regime detection by default), input fairness
  guard (Jain's index, 0.8 threshold, wired), PID budget controller in `ftui-render/src/budget.rs`
  wired by default through the load governor, evidence sink emitting `diff_decision`,
  `budget_decision`, `guardrail_snapshot`, `fairness_decision`, state persistence
  (`StateRegistry`, auto-load/save), input macro record/replay, headless `ProgramSimulator`.
- **Inline mode.** Scroll-region, overlay and hybrid strategies exist and are selected from
  capabilities; `write_log` sanitizes by default; one-writer rule enforced by
  `TerminalWriter`; RAII teardown restores raw mode, alt screen, mouse, paste and cursor
  (verified empirically, including panic hook installation).
- **Pane workspace.** `PaneTree`, operations, interaction timeline with undo/redo/replay,
  drag-resize machine, inertial throw, pressure snap, selection, intelligence modes, ghost
  preview, magnetic docking, terminal and web adapters; integrated in Dashboard, Widget
  Gallery and Layout Lab; dedicated E2E scripts.
- **Widgets.** 57 production widget types including Block, Paragraph, List, Table (with
  `TableTheme` consumed), TextInput, TextArea, Tabs, ProgressBar, Sparkline, Tree,
  CommandPalette (Bayesian scorer with the README's exact prior odds and an explainable
  ledger), Modal stack with focus integration, JsonView, FilePicker, VirtualizedList, Toast,
  Spinner, Scrollbar, and the animation system (spring, easing, stagger) used by modal/toast.
- **Demo showcase.** 45 screens, 419 insta snapshots plus 7 goldens, `BLESS=1` honored,
  Mermaid engine (about 43K lines), Doom/Quake easter eggs, text effects.
- **Quality gates that pass today.** check, clippy `-D warnings`, fmt, 16.7K unit and
  integration tests, fuzz targets exist, 74 E2E scripts exist.
- **Plan-doc items delivered.** One-writer rule, sanitize-by-default `write_log` and
  `LogSink`, PTY capture (feature), stdio capture (feature), render thread (feature),
  `view_string()` easy mode (`StringModel`, `App::string_model`), export to HTML/SVG and
  asciicast, terminal-model property tests, Unicode width corpus, input parser fuzzing,
  deterministic simulator and snapshots.

### 2.2 What is NOT working or not implemented

Grouped by the kind of gap. Full per-claim tables are in Sections 3 to 5.

**A. Consumer onboarding is broken (blocks the "Getting Started (Library Consumers)" promise).**

1. README "Minimal API Example" does not compile: `Paragraph::new(text).render(area, frame)`
   needs `use ftui_widgets::Widget` (`paragraph.rs:291` has no inherent `render`).
2. With default features, `App::run()` is a stub returning
   `Err(Unsupported, "enable crossterm-compat feature to use AppBuilder::run()")`
   (`program.rs:7115`). `ftui-runtime` has `default = []`; the `ftui` facade's default is
   `["runtime", "extras"]` and its `crossterm` feature is not default. Neither README nor
   `docs/getting-started.md` mentions any feature. The demo works only because
   `ftui-demo-showcase` enables `native-backend` and `crossterm-compat` itself.
3. The native `ftui-tty` backend (which `ftui-core` calls the preferred one, labelling
   crossterm "legacy") is not reachable through the `ftui` facade at all.
4. `docs/getting-started.md` claims only `ftui-core`, `ftui-layout`, `ftui-i18n` are on
   crates.io; README says all 17 are (17 are, at 0.6.0). README says `ftui = "0.5"`.
5. `cargo run -p ftui-harness --example minimal` (README "hello world") is a `wrap_text`
   debugging scratch that never opens a terminal. `FTUI_HARNESS_VIEW=dashboard cargo run -p
   ftui-demo-showcase` is ignored by the showcase (it uses `--screen=N` / `FTUI_DEMO_SCREEN`).
6. `view_string()` / `StringModel` (plan section 0.8.2) exists but is not re-exported from
   `ftui::` or the prelude and is absent from README and getting-started.

**B. The intelligence layer is largely dead, off by default, or mis-described.**

Dead means implemented and unit-tested but unreachable from `Program`, `Frame`,
`TerminalWriter`, any widget, or the demo:

| Module | Lines | Status | Evidence |
|---|---|---|---|
| `ftui-text/src/width_cache.rs` (`WidthCache`, `TinyLfuWidthCache`, `S3FifoWidthCache`) | 2,608 | DEAD; production width path is uncached `ftui_core::text_width` | no constructor outside docs/tests/benches |
| `ftui-core/src/gesture.rs` `GestureRecognizer` | 2,125 | DEAD; zero callers; defaults are 3 cells / 300 ms, README says 2 / 500 | agent grep |
| `ftui-core/src/hover_stabilizer.rs` (CUSUM) | 1,061 | demo-only (`mouse_playground`); Table hover is a plain compare | `table.rs:608` |
| `ftui-core/src/keybinding.rs` | 1,913 | Only an Esc-Esc `SequenceDetector`; no priorities, chords, conflict detection, or load/save | `keybinding.rs:308-416` |
| `ftui-core/src/caps_probe.rs` Bayesian log-BF ledger | part | built only by the `terminal_capabilities` demo screen; production `probe_capabilities_unix` sets plain booleans; no cache (S3-FIFO claim false) | `caps_probe.rs:158-200, 1153` |
| `ftui-render/src/diff.rs` summed-area table | part | SAT is computed but never queried; tile skip uses a boolean grid and only engages at 12,000+ cells | `diff.rs:763-805, 1010-1026` |
| `ftui-render/src/roaring_bitmap.rs` | | DEAD | zero non-test callers |
| `ftui-text` bidi / shaping / normalization / tier_budget | 3,200+ | feature-gated wrappers over `unicode-bidi`, `rustybuzz`, `unicode-normalization`; not in the Paragraph/wrap path; `shaping` feature enabled by no crate | `Cargo.toml:14-20` |
| `ftui-runtime/src/eprocess_throttle.rs` (GRAPA) | | DEAD; budget.rs has its own separate e-process | agent 1 item 9 |
| `alpha_investing.rs`, `rough_path.rs`, `flat_combine.rs`, `lens.rs`, `conformal_stages.rs`, `resize_sla.rs`, `diff_evidence.rs`, `telemetry_schema.rs` (constants never referenced) | | UNREFERENCED (only in-file tests) | agent 1 item 23 |
| `allocation_budget.rs`, `flake_detector.rs`, `degradation_cascade.rs`, `conformal_frame_guard.rs`, `conformal_alert.rs`, `sos_barrier.rs`, `cost_model.rs`, `ivm.rs`, `slo.rs`, `policy_config.rs`, `validation_pipeline.rs` | ~15K | TEST-ONLY | agent 1 items 9-15 |
| `timeline_aggregator.rs` (990), `countmin_sketch.rs` (1,022) | 2,012 | NOT COMPILED: no `pub mod` in `lib.rs` (the README's PAC-Bayes claim points here) | `lib.rs` |
| `ftui-layout/src/egraph.rs` | 1,733 | DEAD; not on `Flex::split`/`Grid::split` | no callers of `solve_layout` in egraph |
| `ftui-layout/src/cache.rs` `S3FifoLayoutCache` | | DEAD (`CoherenceCache` is live) | `table.rs:380` |
| `ftui-widgets/src/height_predictor.rs` | 1,079 | DEAD; not used by `VirtualizedList`; VOI remeasurement does not exist | agent 3 item 4 |
| `ftui-widgets/src/fenwick.rs` | 851 | opt-in mode nobody opts into; `virtualized_search` and `log_search` do not use `VirtualizedList` | `virtualized.rs:122,164` |
| `command_palette::ConformalRanker` | | DEAD (exported, unused; `rank_confidence.rs` is a 2-line "superseded" stub) | `scorer.rs:973` |
| `hint_ranker.rs` | 846 | demo-only (`command_palette_lab`); not used by Help/StatusLine as README implies | agent 3 item 3 |
| `ftui-a11y` tree | 2,019 | 9 widgets implement `Accessible` but nothing calls `accessibility_nodes()`; `A11yTreeBuilder::new` has no non-test caller; `accessibility_panel` shows theme toggles, not the tree | verified by grep |
| `DecisionCard`, `DriftVisualization`, `CachedWidget`, `ErrorBoundary<W>`, `TimeTravel` (harness) | | implemented and tested, no production or demo consumer | agent 3 section G |
| Approximately 30 of 63 declared `ftui-runtime` modules (about 25K lines) | | not reachable from any production path | agent 1 item 23 |

Off by default or a silent fallback:

- BOCPD: `CoalescerConfig::default().enable_bocpd = false` (`resize_coalescer.rs:202`); the
  default regime detector is a 10/5 events-per-second heuristic. The README presents BOCPD
  as how resize coalescing works.
- Conformal frame-time predictor: `conformal_config: None` by default (`program.rs:3008`);
  only `ftui-harness/src/main.rs` and tests enable it. The showcase does not.
- VOI sampling: used only for `inline_auto` height remeasure; defaults in `VoiConfig` are
  alpha 1 / beta 1 / max 250 ms / min 0 / cost 0.01, not the README's 1 / 9 / 1000 / 100 / 0.08
  (those are `InlineAutoRemeasureConfig` values).
- Queueing scheduler (SRPT/Smith/aging): used only with the `EffectQueue` backend; default
  lanes spawn a thread per task.
- Asupersync lane: `RuntimeLane::resolve()` maps Asupersync to Structured unconditionally
  (`program.rs:2734-2742`); `RolloutPolicy::Shadow` is a startup log line, not a shadow run.
  The `asupersync-executor` feature is real but reachable only via explicit backend selection.
- SOS barrier: `scripts/solve_sos_barrier.py` never existed; `sos_barrier_coeffs.rs` holds
  hand-typed round constants under a header saying "Auto-generated ... 2026-03-05"; the
  evaluator is not used for frame admissibility.
- Guardrails: `check_frame(memory_bytes, 0)` hardcodes queue depth 0 (`program.rs:6117`)
  even though `queue_telemetry().in_flight` is available (open bead bd-1za0z).

**C. README APIs, constants and counts that do not match the code.**

| README says | Code has |
|---|---|
| `frame.render_widget(w, area)`, `frame.render_stateful_widget(...)`, `frame.area()` | none of these; pattern is `widget.render(area, frame)`; `frame.width()`/`height()` |
| `Layout::horizontal([Constraint::Percentage(30), ...]).split(frame.area())` | `Flex::horizontal().constraints(..).split(area)`; `Percentage(f32)`; no `Layout` type in ftui-layout |
| `focus_manager.register("input1", FocusNode::new()); set_next(..)` | `FocusId = u64`; `graph.insert(FocusNode::new(id, bounds))`, `connect(from, NavDirection, to)`; `focus_next/prev` exist |
| `modal_stack.push(ConfirmDialog::new("Delete file?"))` | `push(Box<dyn StackModal>)`; `Dialog::confirm(title, msg)`; `ConfirmDialog` is a form widget in ftui-extras |
| `frame.link_registry().register(url)`; `cell.link_id = id` | `Frame::register_link/with_links/set_links`; `Cell::with_link(u32)`, `cell.link_id()` (24-bit packed in `CellAttrs(u32)`) |
| Cell layout content 4 + fg 4 + bg 4 + attrs 2 + link 2 | content 4 + fg 4 + bg 4 + `CellAttrs(u32)` = 8 flag bits + 24-bit link id |
| `GraphemeId` width in bits [31:25], 16M slots, width 0-127 | width 4 bits [30:27], generation 11 bits, slot 16 bits: 64K slots, width 0-15 |
| `TimeTravel::new(); record(frame); seek(i); current()` | `new(capacity)`, `record(&Buffer, FrameMetadata)`, `get(idx)`, `rewind(steps)`; `seek` is on `TimeTravelInspector` |
| `Stylesheet::new(); sheet.register(..); sheet.get(..).unwrap_or_default()` | `StyleSheet::define/get -> Option<Style>/get_or_default/compose`; no widget consumer |
| `TableTheme::modern().with_stripe_period(2).with_header_style(..).with_selection_style(..)` | presets `aurora` ... `terminal_classic`; no `modern`, no such builders; striping is a fixed `row_alt` style; no per-column truncation/alignment; no CUSUM hover |
| 9 border styles | `BorderType` has 5 (Square, Ascii, Rounded, Double, Heavy) |
| `Cmd::perform(future, mapper)`, `Cmd::SetClipboard/GetClipboard`, `tick_every`, `file_watcher` | `Cmd::task/task_with_spec/task_named`; no clipboard variants (only inbound `Event::Clipboard`); `Every` subscription; no FS watcher |
| `frame.checksum()`, `MacroPlayer::next() -> (event, delay)`, `simulator.send_event` | `ftui_harness::golden::compute_buffer_checksum`; `MacroPlayer::step/replay_all/replay_with_timing`; `sim.inject_events` |
| `PersistenceConfig::new().with_auto_save(true).with_backend(FileBackend::new(..))`, `MemoryBackend` | `PersistenceConfig::with_registry(Arc<StateRegistry>).auto_load(bool).auto_save(bool)`; `FileStorage` (feature `state-persistence`), `MemoryStorage` |
| `field_lens!` macro | no `macro_rules!` in `lens.rs`; only `compose` |
| `slo.yaml` with `objectives / budget_us / window_seconds / error_budget_pct` | hand-rolled parser for `regression_threshold`, `noise_tolerance`, `safe_mode_*`, `metrics: {metric_type, max_value, max_ratio, safe_mode_trigger}`; safe mode never enters `Program` |
| Evidence events `resize_decision`, `conformal_gate`, `degradation_event`, `queue_select`, `voi_sample` | `decision`/`decision_evidence`/`regime_transition`, folded into `budget_decision`, `effect_queue_select`, and `voi_*` never written |
| Degradation ladder Full, SimpleBorders, NoColors, TextOnly | Full, SimpleBorders, NoStyling, EssentialOnly, Skeleton, SkipFrame |
| Editor: undo coalescing, paragraph movement | `push_undo` pushes every op; no coalescing; no paragraph movement |
| Input "history"; Textarea "syntax hooks"; Progress "indeterminate"; JsonView "collapse/expand"; Sparkline "min/max markers" | none of these exist (`TextInput` doc points to `undo::HistoryManager`; `ProgressBar` has no indeterminate mode) |
| "Plus" widget names `Cached`, `DragHandle`, `Inspector`, `NotificationQueue`, `ValidationError` | `CachedWidget`, no `DragHandle` (`DragPreview`/`Draggable`/`DropTarget`), `InspectorOverlay`, `NotificationStack`, `ValidationErrorDisplay` |
| 46 screens, 11 categories, screens `3d_data` and `quake` | 45 screens, 6 categories, no `3d_data` screen, `quake_easter_egg` |
| VFX list credited to ftui-extras | only Metaballs and Plasma are library code; the rest live in the demo's `visual_effects.rs` |
| Command palette BF word-boundary about 2.0, position proportional to 1/pos, length proportional to 1/len | `1 + 0.3 * boundaries`, `1 + 0.5/(pos+1)`, `1 + 0.2 * (qlen/tlen)`; tag 3.0 and gap penalty match |
| i18n: number/date formatting, LTR/RTL via ftui-text bidi, demo in EN/FR/DE/JA/AR | string catalog + plural rules only; no formatting; no bidi integration; demo has en/es/fr/ru/ar/ja |
| Benchmarks `diff/identical_100x50 1.2 µs`, `sparse 8.3 µs`, `dense 45 µs` | no 100x50 or `dense` bench; checked-in 2026-02-03 results are 80x24/120x40/200x60 (identical 1.81 µs) |
| `prop_diff_soundness`, `counterexample_dirty_soundness` | do not exist; nearest are in `proptest_diff_invariants.rs` |
| Architecture diagram "TerminalSession (crossterm)" (also AGENTS.md) | crossterm is optional and labelled legacy; native `ftui-tty` is the stated preference; neither is default for consumers |
| "Hybrid" inline strategy is default with runtime DECSTBM-reliability fallback | selection is static from capabilities; Hybrid and ScrollRegion are handled identically in the writer; mux detection is the only fallback |

**D. Terminal-compatibility policy silently defeats the flicker-free promise.**

Measured with the real binary under a PTY, `q` sent after 2 seconds:

| Identity | sync brackets | DECSTBM (inline) |
|---|---|---|
| kitty (`TERM=xterm-kitty` + `KITTY_WINDOW_ID`) | yes (26/26) | yes |
| `TERM_PROGRAM=ghostty` | yes | yes |
| `TERM_PROGRAM=Alacritty` | yes | yes |
| `TERM=alacritty` (what Alacritty actually sets) | **no** | **no** |
| `TERM_PROGRAM=WezTerm` (with or without `WEZTERM_PANE`), `TERM=wezterm` | **no** (treated as a multiplexer) | **no** |
| `TERM_PROGRAM=iTerm.app` (+ `LC_TERMINAL=iTerm2`) | **no** | **no** |
| `TERM_PROGRAM=Apple_Terminal`, `vscode` | no | no |
| `TERM=xterm-256color COLORTERM=truecolor` | **no** | **no** |
| tmux | no (correct) | no (correct) |

`caps_probe::probe_capabilities` (which can ask DECRPM `?2026$p`) runs only when the color
depth resolved to Ansi256 (`program.rs:5175`), so truecolor terminals are never probed. The
README's "Guarantee: No partial frames ever visible" and "Theorem 1" are conditional on an
allowlist that excludes most terminals people use, including the terminal this repository's
owner appears to develop in (this session runs under WezTerm).

**E. CI, test health, and process.**

- `main` CI has had no green `ci.yml` run in at least the last 60; the last completed push
  run failed 14 of 22 jobs including the basic `Check` matrix (one job ran 8h35m before
  failing, consistent with a hang). The newest push run has been queued for over five hours.
  Root causes per job are in Section 5 (agent 4).
- Reproducible hang: `ftui-runtime` lib tests block forever in
  `run_pending_signal_skips_initial_render_and_subscription_start`. Root cause in code:
  `record_pending_termination_signal` writes a process-global atomic; only two tests take
  `with_test_signal_serialization`, while every headless test constructor
  (`headless_program_with_resolved_config`, `program.rs:11274`) and production teardown
  (`program.rs:5490`) call `clear_termination_signal()` unconditionally. Any parallel test
  clears the pending signal between `record` and the first `observed_termination_signal()`
  check, and `run()` then blocks in the headless event loop with nothing to wake it. No
  per-test timeout exists in CI, so this becomes a multi-hour job.
- `verify_no_regression` depends on a gitignored file that another test in the same binary
  writes: order-dependent and stale-file-sensitive.
- `doctor_frankentui Extended Verification` fails on every scheduled run since at least
  2026-08-27.
- `tests/baseline.json` is consumed by `scripts/perf_regression_gate.sh`, which no workflow
  invokes; the `benchmarks` job runs `bench_budget.sh --quick` only on pushes to main with
  loosened 1.5x envelopes; startup/first-frame/shutdown budgets are skipped as
  "non_criterion_baseline". README performance numbers are not backed by any checked-in
  artifact.
- Prior "reality-gap" epic bd-i80el (2026-04-09) restored green gates and doc truth for one
  day; nothing kept them true. `docs/reports/deep-codebase-review-final.md` declares
  "Release Ready" with no evidence links; `docs/risk-register.md` says all risks mitigated
  in its summary while its detail rows still say "Planned"/"Designed"; `docs/main-todo-bead-map.md`
  is unchecked despite closed beads; ADR-004/005/006/008/010 are still PROPOSED.

**F. Plan-document Definition-of-Done items never delivered.**

- The primary real-world target (a Claude Code / Codex-style agent harness session powered by
  ftui) has no in-tree consumer beyond `ftui-harness`, and no bead names one.
- `write_raw()` / semi-trusted SGR passthrough (ADR-006's opt-in half): not started.
- Adversarial escape-injection PTY tests: unit-level only.
- Perf budgets at 120x40 / 200x60, input parse+dispatch under 100 µs, bytes-emitted
  O(changes), wrap 200 lines under 2 ms, allocations per frame: no gate enforces any of them.
- SIMD chapter: `ftui-simd` is a 17-line doc-only crate that nothing depends on (yet it is
  published on crates.io at 0.6.0).
- SSH extra: not started. Windows native backend: deferred (plan-only). SIGTSTP/SIGCONT:
  open bead bd-d4dtr; `kill -TSTP` leaves the shell in raw mode.
- "Inline never clears full screen" invariant has no named test.
- The `tests/` workspace directory that AGENTS.md says holds cross-component integration
  tests contains no Rust files (shell E2E scripts and fixtures only).

### 2.3 What is blocking us

1. **No truth mechanism between README and code.** Claims were written from plans and bead
   titles, then never checked against the code; nothing fails when they drift. This is the
   root cause of Section 2.2.C and most of 2.2.B.
2. **No "reachable from production" definition of done.** Hundreds of beads were closed on
   "module + unit tests exist". Wiring into `Program`/`Frame`/widgets was treated as
   optional, so the intelligence layer accreted as parallel, unused implementations
   (three width caches, two e-process controllers, two degradation ladders, two evidence
   ledgers, two CUSUM allocation monitors).
3. **Red CI normalized.** With no green run in 60 attempts, failures carry no signal; the
   hang has survived since March because nothing distinguishes it from runner starvation.
4. **Feature-flag defaults optimized for the demo, not the consumer.** The showcase enables
   everything; the published facade enables nothing that can open a terminal.
5. **Conservative-by-identity capability policy with no probing on the common path.**
6. **Bead count as the progress metric.** 99.9% closure with the front-page example broken
   is the "bead completion illusion" this workflow exists to catch.

### 2.4 Would implementing all open and in-progress beads close the gap?

No. There are two open beads and zero in progress. bd-d4dtr (SIGTSTP/SIGCONT) and
bd-1za0z (guardrail sensor semantics, resize telemetry classification, queue depth wiring)
are real but narrow P3 items. Closing both leaves every item in 2.2.A, 2.2.C, 2.2.D, 2.2.E
and 2.2.F untouched and fixes one line of 2.2.B (queue depth). Coverage of the vision by
the tracker is effectively zero.

### 2.5 Vision goals not covered by ANY bead (NO_BEAD)

- Working `App::run()` under the facade's default features; documented backend selection.
- README/getting-started examples that compile and run (a doc-test mechanism).
- Truthful README claims ledger (counts, APIs, constants, algorithms actually on the path).
- Wiring or quarantining of every dead module in 2.2.B (no bead covers width cache, a11y
  tree construction, height predictor, e-graph, hint ranker, SAT, gesture recognizer,
  keybinding system, e-process/alpha-investing monitors, SLO safe mode, IVM, lenses, ...).
- Capability probing for DEC 2026 / DECSTBM on truecolor terminals; WezTerm, iTerm2,
  Alacritty (`TERM=alacritty`), VS Code, xterm handling; compat matrix assertions in CI.
- Fixing the signal-state test race and adding per-test timeouts; green-main policy.
- `doctor_frankentui` nightly failures (see Section 5 for scope decision).
- `write_raw()` opt-in; adversarial PTY injection tests; SSH extra decision.
- Perf gates: 120x40 / 200x60 present budgets, input latency, bytes emitted, wrap, allocs;
  running `perf_regression_gate.sh` in CI; backing README numbers with artifacts.
- A real agent-harness consumer app (the plan's primary target).
- Keybinding system as described (priorities, chords, conflict detection, serialization).
- Editor undo coalescing, paragraph movement, outbound clipboard commands, FS-watch
  subscription, `tick_every` convenience.
- ADR finalization, risk register and execution tracker refresh, `ftui-simd` decision.
- Widget feature claims: indeterminate progress, JsonView folding, Textarea syntax hook,
  Input history, Sparkline markers, border styles.
- i18n formatting and bidi integration (or retracting the claims).

---

## 3. Vision checklist (README + AGENTS.md)

Status legend: WORKING, PARTIAL, DEAD (exists, unreachable in production), OPT-IN (off by
default), WRONG_API (exists, README shape wrong), NOT_STARTED, UNPROVEN.

| # | Goal | Source | Status | Evidence |
|---|---|---|---|---|
| 1 | Inline mode with scrollback preservation and stable chrome | README TL;DR | WORKING (identity-gated) | DECSTBM + sync under kitty/ghostty; overlay elsewhere |
| 2 | Deterministic Buffer -> Diff -> Presenter -> ANSI | README | WORKING | diff.rs, presenter.rs, proofs in harness |
| 3 | One-writer rule | README, plan 0.9.1 | WORKING | TerminalWriter; docs/one-writer-rule.md |
| 4 | RAII cleanup even on panic | README | WORKING (gap: SIGTSTP) | terminal_session.rs:1178,1197; ftui-tty RawModeGuard; empirical |
| 5 | Composable crates, add only what you need | README | PARTIAL | facade defaults cannot open a terminal |
| 6 | 80+ widgets | README | PARTIAL | 57 production types |
| 7 | Pane workspaces with drag/dock/snap/throw/undo | README | WORKING | pane.rs, layout_lab.rs |
| 8 | Web/WASM backend, runs in browser | README | see Section 5 | agent 4 |
| 9 | Bayesian diff strategy | README | WORKING | diff_strategy.rs wired in terminal_writer |
| 10 | BOCPD resize coalescing | README | OPT-IN (off) | resize_coalescer.rs:202 |
| 11 | VOI sampling for expensive ops | README | PARTIAL (inline_auto only) | program.rs:6266 |
| 12 | E-process / GRAPA anytime-valid monitors | README | PARTIAL: budget.rs has its own; eprocess_throttle DEAD | agent 1 item 9 |
| 13 | Conformal frame-time gating (Mondrian) | README | OPT-IN (None by default) | program.rs:3008 |
| 14 | Multi-stage conformal monitors | README | DEAD (`conformal_stages` unreferenced) | agent 1 |
| 15 | CUSUM allocation + hover | README | DEAD / demo-only | alloc_budget doc-ref only; hover in mouse_playground |
| 16 | Alpha-investing FDR across monitors | README | UNREFERENCED | alpha_investing.rs |
| 17 | Flake detector for E2E timing | README | DEAD | only proptest file |
| 18 | Rough-path signatures | README | UNREFERENCED | rough_path.rs |
| 19 | SOS barrier certificates (SDP-solved) | README | DEAD + provenance false | no script; hand-typed coeffs |
| 20 | S3-FIFO cache for caps + width | README | DEAD | width_cache.rs, cache.rs |
| 21 | W-TinyLFU width cache + PAC-Bayes CMS | README | DEAD / NOT COMPILED | width_cache.rs; countmin_sketch.rs orphan |
| 22 | Flat combining | README | UNREFERENCED | flat_combine.rs |
| 23 | Bidirectional lenses `field_lens!` | README | WRONG_API / DEAD | lens.rs |
| 24 | IVM DAG | README | DEAD | ivm.rs |
| 25 | SLO schema + safe mode | README | WRONG_API / DEAD | slo.rs |
| 26 | State persistence | README | WORKING (API names wrong) | state_persistence.rs, program.rs:3224 |
| 27 | Input macro record/playback | README | WORKING (player API wrong) | input_macro.rs |
| 28 | Headless simulator | README | WORKING (`checksum` name wrong) | simulator.rs |
| 29 | Frame arena in hot path | README | WORKING (light use) | frame.rs:470; only input.rs + dashboard use it |
| 30 | Grapheme pool with width bits | README | WORKING (bit layout wrong in README) | cell.rs:34-48 |
| 31 | Synchronized output every frame | README | WORKING (identity-gated) | Section 2.2.D |
| 32 | Elm architecture Model/Cmd/Subscriptions | README | WORKING (`perform`, `tick_every`, `file_watcher` missing) | program.rs |
| 33 | Zero unsafe | README, AGENTS | WORKING | 20/20 crates forbid; ftui-core `cfg_attr(not(test))` |
| 34 | Formal proof sketches Theorems 1-4 | README | WORKING (file names differ) | harness render_no_flicker_proof.rs |
| 35 | Property tests, snapshots, benches | README | WORKING; bench numbers unbacked | proptest files; 419 snaps |
| 36 | Resize coalescing regimes | README | WORKING (delays 16/40 ms not 200/20) | resize_coalescer.rs:194 |
| 37 | Budget degradation PID | README | WORKING (level names wrong) | budget.rs |
| 38 | Input fairness guard | README | WORKING | input_fairness.rs, program.rs:5676 |
| 39 | Table theming engine | README | PARTIAL / WRONG_API | table_theme.rs |
| 40 | Stylesheet | README | WRONG_API / no consumer | stylesheet.rs |
| 41 | Widget composition helpers `render_widget`, `Layout` | README | WRONG_API | frame.rs, ftui-layout lib.rs |
| 42 | Hyperlinks | README | WORKING (API wrong) | link_registry.rs, presenter OSC 8 |
| 43 | Focus management | README | WORKING (API wrong) | focus/manager.rs |
| 44 | Modal system | README | WORKING (API wrong) | modal/stack.rs |
| 45 | Time-travel debugging | README | DEAD (no consumer), API wrong | time_travel.rs |
| 46 | Accessibility tree, live regions | README | DEAD (never built at runtime) | ftui-a11y; no callers |
| 47 | i18n formatting/bidi/5 languages | README | PARTIAL (catalog + plurals only) | ftui-i18n |
| 48 | Queueing scheduler SRPT/Smith/aging | README | OPT-IN | program.rs:3884 |
| 49 | Inline strategies A/B/C auto-selected | README | WORKING (Hybrid == ScrollRegion) | inline_mode.rs:93-107 |
| 50 | Color system profiles + WCAG | README | WORKING | color.rs, ansi.rs |
| 51 | Evidence sink categories | README | PARTIAL (names differ; `voi_sample` never written) | agent 1 item 6 |
| 52 | Runtime lanes + rollout + shadow-run | README | PARTIAL (Asupersync falls back; Shadow is a label) | program.rs:2734, 4909 |
| 53 | Effect queue telemetry + backpressure | README | WORKING | effect_system.rs |
| 54 | Telemetry schema targets | README | PARTIAL (constants unused; literals match) | telemetry_schema.rs |
| 55 | E-graph layout optimizer before solver | README | DEAD | egraph.rs |
| 56 | Rope text engine | README | WORKING (ropey wrapper) | rope.rs, textarea |
| 57 | Editor core features | README | PARTIAL | editor.rs |
| 58 | Degradation cascade module | README | DEAD (real controller is budget.rs) | degradation_cascade.rs |
| 59 | Cost models (cache / M-G-1 / batching) | README | DEAD | cost_model.rs |
| 60 | Gesture recognizer | README | DEAD | gesture.rs |
| 61 | Input parser (CSI/SS3/DCS/OSC/APC, kitty, paste, mouse) | README | WORKING (APC/SOS/PM as Alt introducers; no 1016 pixel mouse) | input_parser.rs |
| 62 | Keybinding system | README | NOT_STARTED as described | keybinding.rs |
| 63 | Animation system | README | WORKING | animation/ |
| 64 | Bayesian capability detection | README | DEAD in production (demo builds ledger) | caps_probe.rs |
| 65 | 46 demo screens, gallery table | README | WRONG (45; names) | app.rs |
| 66 | crates.io: all 17 libraries | README | WORKING (getting-started contradicts) | crates.io |
| 67 | Windows support | README FAQ | PARTIAL | docs/WINDOWS.md; Section 5 |
| 68 | doctor_frankentui verification stack | README, AGENTS | see Section 5 | daily CI failure |
| 69 | Cross-component tests in workspace `tests/` | AGENTS | WRONG (no .rs files) | tests/ |
| 70 | Mandatory gates green (check/clippy/fmt/tests) | AGENTS | PARTIAL locally, RED in CI | Section 1 |
| 71 | `master` synchronized with `main` | AGENTS | WORKING (after fc67ab6e) | git |

Plan-document goals (agent 5's 46-item checklist) are folded into Sections 2.2.F and 7;
the NO_BEAD list is in 2.5.

---

## 4. Bead landscape

- 2,734 beads; 2,732 closed; 2 open (P3); 0 in progress; 144 of 144 epics closed.
- Creation: Jan 168, Feb 2,415, Mar 102, Apr 8, Jun 28, Jul 12, Aug 1. Closure: Feb 2,278,
  Mar 173, Apr 7, May 37, Jun 166, Jul 67, Aug 4. Commits: Feb 2,368, then 236 / 165 / 51 /
  229 / 122 / 36.
- 190 closed beads have a null close reason; 663 say only "done".
- Silent scope cuts recorded only in closing notes (curated): Windows native backend
  "defer implementation" (bd-lff4p.4.9), Windows Terminal "deferred" (bd-1xo), FRP "NOT
  implemented" (bd-16pal), Aho-Corasick "deferred" (bd-12o8.8), CI E2E gate
  "environmentally unmeetable" (bd-1dccp), layout-solver integration "can be follow-up"
  (bd-2dow.5), "App builder compiles (even if not implemented)" (bd-10i.2.7), nine
  FrankenTermJS features each with an "Out of Scope" block (bd-2vr05.*), SIGTSTP split to
  the still-open bd-d4dtr.
- The last months of swarm activity were FrankenTermJS/xterm parity and pane workspace
  polish; the inline `write_log` path was still being bug-fixed on 2026-08-22.
- The April reality-gap epic bd-i80el closed the same day it was opened, with three
  children (green gates, getting-started fix, README/AGENTS truth). All three regressed.

---

## 5. Web/WASM, backends, harness, doctor_frankentui, CI root causes

### 5.1 CI root causes (run 33152820289 on ab07291f; identical failing steps on the two prior runs)

22 job instances: 8 green (Benchmarks, Feature Combinations, MSRV, Docs rustdoc+examples,
Perf Rollout Gates, WASM Build Check, Pane Perf Replay Artifacts), 13 failed, 1 cancelled.
No job is `continue-on-error`, so red is the steady state. No green `ci.yml` run on main
exists in the last 40 (all failures since 2026-07-08).

| Job | Root cause | Class |
|---|---|---|
| Check ubuntu nightly | runner disk exhausted during all-features tests (`No space left on device`) | infra, triggered by the 1M-line all-features footprint |
| Check ubuntu stable | `perf_corpus_1000_under_budget` wall-clock assertion (`scorer.rs:3508`, p95 5794 µs > 5000 µs) | code: timing test on shared runner |
| Check macos stable | four wall-clock tests in ftui-runtime (`subscription.rs:1768` 188 ms > 100 ms; `every_respects_interval`) | code: timing tests |
| Check macos nightly | **hang** in `run_invokes_on_shutdown_before_returning_signal_error` and `run_pending_signal_skips_initial_render_and_subscription_start`, killed at the 6 h limit | code: signal-state race (Section 2.2.E) |
| Check windows stable | clippy `-D warnings`: 19 dead-code errors in `ftui-tty` (unix-only items not cfg-gated) | code |
| Coverage | disk exhausted | infra |
| FrankenTerm Conformance/WS gates | python `websockets` never installed by the workflow (`ws_client.py:46`) | workflow config |
| Widget API E2E | `scripts/widget_api_e2e.sh:114` exports `FTUI_HARNESS_SEED=0` then runs `cargo test --workspace --lib`; `determinism.rs:518` reads it, seed 0 != 99 | code: env leak into unit tests |
| Documentation | rustdoc `-D warnings`: unresolved link `ReceiptVerdict` + redundant link targets in `receipt_verifier_panel.rs` | code |
| Golden Trace Gates | `frankenterm_js_parser_hooks_compat` test exit 101 (output only in /tmp) | code |
| Demo Showcase | `demo_showcase_e2e.sh` sets `E2E_SEED=0`; DeterminismLab reads it (`determinism.rs:53`, default 7) so the blessed snapshot `Seed: 7` mismatches | code: env leak into snapshots |
| Fuzz Build Check | `fuzz/Cargo.toml` inherits `[lints] workspace = true` while excluded from the workspace | code: manifest |
| PTY E2E ubuntu / macos | 42/166 failures: `rg` not installed on runners plus real assertion failures (cleanup x4, keybind x3, voi_marker x4, rtl_locale x4, mouse SGR, paste; vsearch, inline_story, bidi on macOS) | code, mixed |
| doctor_frankentui Verification | "Install VHS (pinned)" dies in 0.5 s: `find ... \| head` under `set -euo pipefail` returns 1 on unreadable /tmp dirs (`ci.yml:1147`); 68 of 79 steps skipped | workflow script |
| doctor_frankentui Extended Verification (scheduled) | `sudo install /tmp/vhs` but the tarball extracts to `/tmp/vhs_0.10.0_Linux_x86_64/vhs` (`doctor_frankentui_extended.yml:85`); 30 of 30 runs red since 2026-08-06 | workflow script |
| release.yml (last two runs) | `ftui-simd@0.6.0 already exists on crates.io` (publish loop not idempotent) | workflow |

Other CI facts: the `wasm` job only `cargo check`s core crates for wasm32 and never builds
`ftui-web` or `ftui-showcase-wasm`; the `msrv` job installs floating `nightly` and runs
`cargo check` (not an MSRV check); CI jobs use floating `nightly` despite the dated pin in
`rust-toolchain.toml`; `scripts/e2e_test.sh` and `scripts/pane_e2e.sh` are invoked by no
workflow; `tests/e2e/lib/pty.sh` is a real Python-`pty` driver. The newest push run
(`fc67ab6e`) has been queued for over five hours. Commit `fc67ab6e` (#95) is an issue filed
and fixed by the owner, not an outside PR; the only merged PRs in history are dependabot.

Locally, the isolated re-run of the hanging test could not be completed because the
remote build queue was occupied by the full-suite run; the static root cause in 2.2.E and
the macOS CI log are the evidence.

### 5.2 Web/WASM

| Claim | Status | Evidence |
|---|---|---|
| ftui-web renders in the browser | PARTIAL: a host-driven patch producer with no DOM/canvas code; `lib.rs:8` "intentionally does not bind to wasm-bindgen yet"; implements the ftui-backend traits | 12.3K lines, 9 integration tests |
| Pointer/touch parity, `PaneSemanticInputEvent` translation | WORKING | `pane_pointer_capture.rs` (1,684 lines), `pane_web_e2e.rs`, `pane_cross_host_parity.rs` |
| DPR/zoom handling | NOT_STARTED (one comment in `step_program.rs:352`) | |
| ftui-showcase-wasm | `ShowcaseRunner` exports match `docs/spec/wasm-showcase-runner-contract.md`; `#[wasm_bindgen]` under `cfg(target_arch = "wasm32")`; never built for wasm32 in CI | UNPROVEN |
| "Can it run in a browser? Yes." | Not from this repo alone: `frankentui_showcase_demo.html` imports an out-of-tree `pkg/FrankenTerm.js` bundle and an unbuilt `pkg/ftui_showcase_wasm.js`; `build-wasm.sh` needs `wasm-pack` and `FRANKENTERM_WEB_CRATE_DIR` | PARTIAL |
| `frankenterm-core` dependency | crates.io 0.2.0, resolves; scripts/frankenterm_js_*.sh run in-tree tests (four are in CI) | WORKING |

### 5.3 Backend crates and harness

- `ftui-backend`: the event side of `Program` really goes through `BackendEventSource`;
  the presenter side writes straight to `W: Write`, and `BackendPresenter` is implemented
  only by ftui-web. Half a seam.
- `ftui-tty`: real, Unix-only, opt-in via `native-backend`; fails Windows clippy because
  unix-only helpers are not cfg-gated. `docs/WINDOWS.md` says "Validated (2026-02-03)"
  while every Windows CI job since is red; native Windows backend is deferred.
- `ftui-harness`: README's `ShadowRun`, `RolloutScorecard`, `RolloutEvidenceBundle`
  snippets are exact; the harness binary reads all nine `FTUI_HARNESS_*` variables;
  examples `counter`, `layout`, `minimal` (a `wrap_text` scratch), `modal`, `streaming`
  exist. There is no ratatui shadow comparison; `shadow_run` compares one model across two
  runtimes.

### 5.4 doctor_frankentui (192K lines, 128 source files, the largest crate)

- Self-description: "operator-facing workflow crate for capture, certification, replay,
  suite execution, and migration planning". The planning doc proposed 6 subcommands; there
  are 31, all routed to real handlers (no stubs).
- About 47% of the crate is tests (2,418 `#[test]` in src, 268 in tests/). Only 7 of 128
  source files import any `ftui_*` crate.
- The verification core (capture, suite, report, doctor, import; roughly 12K lines) is
  coherent. The remaining ~170K lines are three unrelated products behind one binary: a
  TSX/React-to-FrankenTUI migration compiler (`tsx_parser`, `translation_planner`,
  `code_emission`, `mapping_atlas`), an "alien-graveyard" research-governance and evidence
  framework (`graveyard_*`, `alien_kernel_tests`, `portfolio_scheduler`,
  `reverse_round_governance`, `galaxy_brain_cards`, `guarantee_layer`, `paper_verification`,
  `cegis_synthesis`, `concolic_differential`, `abstract_interpretation`), and nightly/stress/
  rollout gate machinery. Live MCP seeding was never smoke-tested per its own parity doc.
- Neither of its CI workflows has ever executed a doctor gate (both die at VHS install).

---

## 6. Gap categories (for bead typing)

| Category | Items |
|---|---|
| Vision gap (no bead) | everything in 2.5 |
| Implementation gap | keybinding system; editor coalescing/clipboard/paragraph; widget feature claims; i18n formatting/bidi; write_raw; FS-watch subscription; SIGTSTP; queue depth wiring |
| Wiring gap (code exists, not on path) | width cache; a11y tree; height predictor + Fenwick; hint ranker; conformal predictor and stages; BOCPD default; SAT query; caps ledger; e-process/alpha-investing/flake monitors; SLO safe mode; e-graph; IVM; lenses; SOS barrier; timeline aggregator/CMS (uncompiled) |
| Proof gap | perf budgets (present sizes, input latency, bytes, wrap, allocs); README bench numbers; inline-never-clears invariant; adversarial injection PTY tests; sync-bracket coverage per emulator |
| Integration gap | facade default backend; README/getting-started examples; harness minimal example; showcase env var docs |
| Design gap | process-global signal state (test race); duplicated controllers (two e-process, two degradation ladders, two evidence ledgers, two CUSUM alloc monitors, two terminal-session stacks); identity-only capability policy |
| Doc gap | every row of 2.2.C; AGENTS.md architecture/backends/tests dir; risk register; execution tracker; ADR statuses; changelog of scope cuts |

---

## 7. Bridge plan

Ordering principle: first make the truth mechanisms exist (WS0, WS1, WS2), because every
later workstream is otherwise unverifiable; then wire or quarantine the intelligence layer
(WS3), fix the terminal policy (WS4), deliver the missing features (WS5, WS6), decide
doctor_frankentui and web scope (WS7), and lock the process (WS8). Every task lists its
acceptance evidence; a bead closes only when that evidence exists in CI.

### WS0. Truth mechanism: README and docs cannot drift from code again

- **WS0.1 README as doc-test.** Add `#![doc = include_str!("../../README.md")]` to the
  `ftui` facade (or a dedicated `readme_doctests` crate) so every ```` ```rust ```` block
  compiles under `cargo test --doc`. Mark intentionally non-compiling blocks
  ```` ```rust,ignore ```` or ```` ```text ````. Acceptance: `cargo test -p ftui --doc` fails
  on any README snippet that does not compile; the Minimal API Example passes.
- **WS0.2 Claims ledger.** `docs/claims-ledger.md`: one row per quantitative or algorithmic
  README claim (counts, byte layouts, constants, "used by X", "default Y") mapping to a
  test name or a `cargo` command that proves it. A CI job greps the README for the tracked
  numerals/identifiers and fails when a claim has no ledger row. Acceptance: 100% of the
  rows in Section 2.2.C are either fixed in code or corrected in README, and each has a
  ledger row.
- **WS0.3 README rewrite pass.** Correct every row of 2.2.C that WS3/WS5 will not fix in
  code (counts: 45 screens, 6 categories, 57+ widgets, 5 border styles; layouts:
  `CellAttrs`, `GraphemeId`; names: `StyleSheet`, `CachedWidget`, `InspectorOverlay`,
  `NotificationStack`, `ValidationErrorDisplay`, `TextInput`, `TextArea`, `ProgressBar`;
  bench numbers replaced by the checked-in 2026-02-03 artifact or regenerated ones; VFX
  attribution; evidence event names; degradation level names; VOI defaults; resize delays;
  `ftui = "0.6"`; harness env vars vs showcase flags; SOS provenance). Add an honest
  "Experimental modules" section for whatever WS3 quarantines. Acceptance: WS0.2 ledger
  complete; README doc-tests green.
- **WS0.4 AGENTS.md truth.** Backend statement (native `ftui-tty` on Unix, crossterm
  optional), architecture diagram, workspace `tests/` description, toolchain pin note,
  `doctor_frankentui` verification commands that actually pass. Acceptance: a fresh agent
  following AGENTS.md verbatim gets green gates.
- **WS0.5 Secondary docs.** `docs/getting-started.md` (features, crates.io statement,
  working example that is itself doc-tested), `docs/risk-register.md` (summary matches
  rows), `docs/main-todo-bead-map.md` (regenerate from beads or delete with permission),
  ADR-004/005/006/008/010 status decisions, `docs/reports/deep-codebase-review-final.md`
  annotated as superseded by this document.

### WS1. Consumer onboarding: `ftui = "0.6"` must run a program

- **WS1.1 Default backend.** Make `ftui` default features open a terminal: on Unix
  `native-backend`, elsewhere `crossterm-compat`; `AppBuilder::run()` picks native on Unix
  when compiled and crossterm otherwise, and only returns `Unsupported` when no backend was
  compiled (with the exact feature to enable in the message). Expose `run_native`/`run_crossterm`
  for explicit choice. Acceptance: a scratch crate with `ftui = { path = ... }` and no
  features runs the README example under a PTY, renders "Ticks:", exits on `q`, restores
  the terminal (E2E script `scripts/consumer_smoke_e2e.sh` with logged escape-sequence
  counts).
- **WS1.2 Fix the example.** Add `use ftui_widgets::Widget` (or make `Paragraph` reachable
  through the facade prelude with the trait), keep the example identical in README,
  getting-started and a real `crates/ftui/examples/minimal_inline.rs`. Acceptance: WS0.1
  doc-test plus `cargo run -p ftui --example minimal_inline` in the E2E script.
- **WS1.3 Easy mode.** Re-export `StringModel`/`StringModelAdapter` and `App::string_model`
  from `ftui::` and the prelude; document `view_string()` as the 30-second path. Acceptance:
  doc-tested example.
- **WS1.4 Harness examples.** Replace `ftui-harness/examples/minimal.rs` with a real hello
  world; make `streaming` the "logs above, chrome below" example; document
  `FTUI_HARNESS_*` variables only where they are read; document `--screen=N` /
  `FTUI_DEMO_SCREEN` for the showcase. Acceptance: examples run in
  `scripts/consumer_smoke_e2e.sh`.
- **WS1.5 Publish consistency.** Release checklist verifies README version string, crates.io
  versions, and that `ftui-simd` is either given content or unpublished/yanked (owner
  decision). `ftui-demo-showcase` 0.1.1 on crates.io: yank or mark deprecated (owner
  decision).

### WS2. Green main and a test suite that cannot hang

- **WS2.1 Signal-state race.** Move pending-termination state off the process-global atomic
  into the `Program`/session (or a per-`Program` handle registered with the signal thread),
  remove `clear_termination_signal()` from `headless_program_with_resolved_config`, and make
  `with_test_signal_serialization` unnecessary. Acceptance: `cargo test -p ftui-runtime`
  passes 20 consecutive runs with `--test-threads=16`; a regression test asserts that two
  concurrent headless programs with independent pending signals both terminate.
- **WS2.2 Per-test timeouts.** Adopt `cargo-nextest` with a 120 s per-test slow-timeout and
  terminate-after policy in CI; keep `cargo test` working locally. Acceptance: a deliberately
  hanging test fails CI in under 3 minutes.
- **WS2.3 Baseline test determinism.** Make `verify_no_regression` self-contained: capture
  and verify in one test, or skip cleanly when provenance is missing and log why; never
  read a file another test writes. Acceptance: passes on a clean clone and with a stale
  local file.
- **WS2.4 CI triage to green.** One task per row of Section 5.1: (a) stop
  `widget_api_e2e.sh` and `demo_showcase_e2e.sh` from exporting seed variables into
  `cargo test` (scope env to the PTY runs, or make the readers ignore it under
  `cfg(test)`); (b) convert wall-clock assertions (`scorer.rs:3508`, `subscription.rs:1768`,
  `every_respects_interval`) to virtual-clock or `#[ignore]`-on-CI perf tests fed by the
  perf gate instead; (c) cfg-gate unix-only items in `ftui-tty` so Windows clippy is clean;
  (d) fix rustdoc intra-doc links in `receipt_verifier_panel.rs`; (e) give `fuzz/` its own
  `[workspace]` or lints table; (f) install `rg` and python `websockets` in the workflows
  (or remove the dependencies); (g) fix both VHS install steps (find/pipefail; extraction
  path); (h) surface the `parser_hooks` test output and fix it; (i) triage the 42 PTY E2E
  failures into real bugs (cleanup, keybind, VOI markers, RTL locale, mouse SGR, paste) with
  one bead each; (j) pin CI to the `rust-toolchain.toml` nightly instead of floating
  `nightly`; (k) make `release.yml` skip already-published versions; (l) split the
  all-features test job so disk exhaustion and hangs cannot take the matrix down; (m) run
  `scripts/e2e_test.sh` and `scripts/pane_e2e.sh` in CI or delete the claim that they are
  gates; (n) build `ftui-web` and `ftui-showcase-wasm` for wasm32 in the `wasm` job; (o)
  make the `msrv` job a real MSRV check or rename it. Acceptance: three consecutive green
  `ci.yml` runs on main; `doctor_frankentui Extended Verification` green or demoted per WS7.
- **WS2.5 Green-main policy.** Beads may not close while main is red; the "Landing the
  Plane" checklist in AGENTS.md gains "link the green run".

### WS3. Wire or quarantine the intelligence layer

Decision rule for each dead module: WIRE if it delivers measurable user value on the
production path with a benchmark or behavior test proving it; QUARANTINE behind an
`experimental-*` feature with README moved to an "Experimental" section if the value is
unproven; DELETE only with explicit owner permission (AGENTS.md rule 1).

Recommended WIRE (each with before/after benchmark or behavior test and an evidence event):

- **WS3.1 Width cache.** Route `ftui_core::text_width` grapheme lookups through one cache
  (pick TinyLFU or S3-FIFO by `cache_bench.rs`; delete the losers only with permission).
  Acceptance: wrap/measure bench shows the win; README claim matches the chosen policy.
- **WS3.2 Accessibility tree.** Build the a11y tree during `view()` (Frame collects
  `Accessible::accessibility_nodes()` from widgets that opt in), expose it from `Program`,
  emit live-region announcements as evidence, and make `accessibility_panel` render the
  real tree. Acceptance: snapshot of the tree for the Dashboard; diff events on focus move.
- **WS3.3 VirtualizedList.** Default variable-height mode to Fenwick; wire
  `height_predictor` + VOI remeasure; use `VirtualizedList` in `virtualized_search` and
  `log_search`. Acceptance: scroll-jump metric test; evidence `voi_sample` written.
- **WS3.4 Conformal frame guard on by default** with safe defaults and the `budget_decision`
  evidence; keep `conformal_stages` per-stage monitors or quarantine them.
- **WS3.5 BOCPD default-on** after a resize-storm differential test proves parity or
  improvement over the heuristic (`tests/e2e/lib/resize_storm_differential.py` exists).
- **WS3.6 Hint ranker** feeding `Help`/`StatusLine` hints; hysteresis proven by a
  no-flicker test.
- **WS3.7 SAT query** in tile skip (or remove the SAT); lower `min_cells_for_tiles` if the
  bench supports it.
- **WS3.8 Capability ledger** used by `probe_capabilities_unix` (log-BF combination with
  evidence output), which is also the substrate for WS4.
- **WS3.9 One controller each.** Unify `eprocess_throttle` with `budget.rs`'s e-process,
  `degradation_cascade` with `BudgetController`, `diff_evidence` with `terminal_writer`'s
  ledger, `allocation_budget` with `alloc_budget`, and pick one of the two terminal-session
  stacks as canonical. Acceptance: no duplicate implementations; README describes the one
  that runs.
- **WS3.10 Queue depth** fed from `queue_telemetry().in_flight` (closes bd-1za0z item 3).
- **WS3.11 Orphans.** Compile-or-remove `timeline_aggregator.rs` and `countmin_sketch.rs`
  (wire the aggregator into `action_timeline`, or delete with permission).

Recommended QUARANTINE (feature `experimental-alien`): `rough_path`, `flat_combine`, `lens`,
`ivm`, `cost_model`, `sos_barrier` (after deleting the false "auto-generated" header and
either adding the solver script or documenting the constants as hand-chosen),
`alpha_investing`, `flake_detector` (unless WS6 wires it into perf gates), `slo` (unless
wired to safe mode), `egraph`, `S3FifoLayoutCache`, `roaring_bitmap`, `tier_budget`,
`gesture` (unless WS5 wires it into widgets), `ConformalRanker`, `DecisionCard`,
`DriftVisualization`, `CachedWidget`, `ErrorBoundary<W>`, `TimeTravel`.

- **WS3.12 Dead-code gate.** A CI script lists every `pub mod` in each crate and requires
  at least one non-test reference outside its own file or an `experimental-*` gate;
  fails on new orphans.

### WS4. Terminal compatibility policy that keeps the flicker-free promise

- **WS4.1 Probe on the common path.** Run the DECRPM `?2026$p` and DECSTBM checks for every
  interactive session (bounded timeout, already implemented for Ansi256), not only when
  color depth is Ansi256. Enable sync output on a positive reply regardless of identity.
- **WS4.2 Identity table.** Recognize `TERM=alacritty`, `LC_TERMINAL=iTerm2`, VS Code,
  Apple Terminal, and modern xterm; treat WezTerm as a multiplexer only with mux-domain
  evidence (`WEZTERM_UNIX_SOCKET` plus a mux pane), otherwise as a modern terminal.
- **WS4.3 Overrides.** `FTUI_SYNC_OUTPUT`, `FTUI_SCROLL_REGION` env overrides with evidence
  logging of the decision and its reason.
- **WS4.4 Compat matrix in CI.** Extend `emulator_compat_matrix.yml` to assert, per
  emulator identity, the counts of sync pairs and DECSTBM sets on a 2-second showcase run
  (the Python PTY driver from this audit is the template). Publish the matrix in
  `docs/compat-matrix.md` and link it from the README's Synchronized Output section, which
  must state the guarantee's preconditions.

### WS5. Deliver the documented widget, input, and text features

- **WS5.1 Keybinding system** as described: priority levels, chord sequences with timeout,
  context activation, conflict detection, serde load/save; wire showcase and
  `pane_keymap` through it. Tests: chord timing, shadowing report, round-trip.
- **WS5.2 Gesture recognizer** wired into `Draggable`/pane drag and Table click handling,
  defaults documented as implemented (3 cells / 300 ms) or changed to the README's.
- **WS5.3 Editor:** undo coalescing (typing burst = one step), paragraph movement,
  outbound `Cmd::SetClipboard/GetClipboard` via OSC 52 with a PTY test.
- **WS5.4 Subscriptions:** `tick_every` convenience and an FS watcher subscription
  (`notify` crate, feature-gated), with a demo screen use.
- **WS5.5 Widgets:** indeterminate `ProgressBar`, `JsonView` fold/unfold, `TextArea` syntax
  hook, `TextInput` history, `Sparkline` min/max markers, border styles up to the documented
  count or README corrected, `TableTheme` builders (`with_stripe_period`, header, selection)
  and per-column truncation/alignment, `StyleSheet` consumed by at least Block/Table.
- **WS5.6 Convenience API:** `Frame::render_widget/render_stateful_widget/area` and a
  `Layout` alias over `Flex`, so the README's idioms are real (or the README adopts the
  existing idioms; pick one in WS0.3).
- **WS5.7 i18n:** either add locale number/date formatting and bidi integration, or
  retract; align demo languages with the docs.
- **WS5.8 Input parser:** SGR-pixels (1016) and DCS/APC payload handling, or retract.

### WS6. Plan-document Definition of Done

- **WS6.1 Agent-harness reference app.** Ship a real inline "agent shell" binary
  (streaming child-process logs above, stable status/input chrome below, links, resize,
  crash-safe teardown) as the getting-started tutorial target and the flagship inline demo;
  drive it in `scripts/e2e_test.sh` with a log-spam scenario and assert scrollback
  integrity. Owner decision: which real tool to dogfood it in.
- **WS6.2 `write_raw()`** and SGR-only semi-trusted mode per ADR-006, with adversarial
  injection PTY tests (ESC/CSI/OSC/DCS/APC payloads in log lines) and the "inline never
  clears full screen" invariant test.
- **WS6.3 Perf gates.** Run `scripts/perf_regression_gate.sh` in CI against
  `tests/baseline.json`; add present budgets at 120x40 and 200x60, input parse+dispatch
  latency, bytes-emitted-per-scene, wrap-200-lines, allocations-per-frame (counting
  allocator behind a feature); regenerate README numbers from the artifact.
- **WS6.4 Signals and platforms.** SIGTSTP/SIGCONT (bd-d4dtr); Windows decision (native
  backend or documented crossterm-only with CI proof); SSH extra dropped from the plan or
  scheduled.
- **WS6.5 ADRs and trackers.** Accept or supersede ADR-004/005/006/008/010; regenerate the
  execution tracker from beads; refresh the risk register.
- **WS6.6 `ftui-simd`.** Give it real safe SIMD paths with benches, or unpublish.

### WS7. Scope decisions the owner must make (see Section 5 for evidence)

- doctor_frankentui: keep the ~12K-line verification core (capture, suite, report, doctor,
  import) as the product the README describes and green its gates; then decide separately
  for the TSX migration compiler, the alien-graveyard governance framework, and the
  nightly/stress machinery: split each to its own repo, feature-gate it, or delete it (with
  permission). Whatever stays must have a CI job that actually executes it.
- ftui-web / ftui-showcase-wasm: define what "runs in a browser" means without the
  out-of-tree FrankenTermWeb bundle. Minimum: build both crates for wasm32 in CI, ship a
  minimal in-tree JS host that drives `ShowcaseRunner` and renders the flat patches to a
  `<pre>`/canvas so the claim is testable, implement or retract DPR/zoom, and rewrite the
  README web sections to say "patch producer for a host" until a renderer exists.
- Asupersync lane: implement the executor behind lane selection, or remove the lane and the
  Shadow policy from README until it exists.

### WS8. Process guardrails

- Definition of done for any bead touching a README claim: reachable from production path,
  evidence event or test named in the close reason, ledger row updated.
- Close reasons must name the CI run or test; empty/"done" close reasons rejected by a
  `br` pre-close hook or review script.
- Reality check cadence: re-run this document's Section 1 commands monthly; diff the
  claims ledger.
- Version bump checklist includes README doc-tests, compat matrix, and consumer smoke E2E.

---

## 8. Immediate next step

Phase 3a: convert Section 7 into beads with the frozen template, after the owner steers on
WS7 decisions and on the wire/quarantine/delete split in WS3. Suggested first five beads by
leverage: WS1.1 default backend, WS2.1 signal race, WS0.1 README doc-tests, WS4.1 probing,
WS2.4 CI to green.
