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

## 7. Bridge plan (Phase 2)

**Reality check date:** 2026-09-01
**Gap count:** 7 critical, 24 major, 11 minor (42 resolution blocks, several of them clusters)
**Existing bead coverage:** 2 open beads touch 2 of the 42 blocks (bd-d4dtr covers G32 in full; bd-1za0z covers the telemetry half of G20 and the classification half of G12). Every other block is NO_BEAD.
**Estimated work:** 3 XL, 12 L, 18 M, 9 S resolution blocks. With the parallelism in the dependency graph (Section 7.5), the critical tier is roughly two focused swarm-weeks; the major tier is where most of the calendar goes.
**Plan-space passes done on this section:** completeness (every non-WORKING row of Section 3 and every letter of Section 2.2 maps to a block; V29 and V48 were the two misses found and are now in G25 and G24), optimality (G28 feeds G05; G13 merges four duplicate pairs in one block; widgets are exercised rather than quarantined), and test coverage (every block names a unit test, a bench where speed is claimed, and an E2E scenario; G42 is the final integration bead).

### 7.0 Conventions and decision policy

- **Status arrow.** Every block is written as `[current status] -> WORKING` where WORKING means: reachable from the production path (`Program`, `Frame`, `TerminalWriter`, a widget's `render`, or the showcase), covered by a named test, and where relevant proven by an E2E script that logs what it observed.
- **Code-first unless the claim is not worth the code.** For each README mismatch the block states one of: **CODE** (change the code to match the promise) or **DOC** (retract or correct the promise). The rule: CODE when the promised behavior is user-visible value and the change is at most M; DOC when the promise was decorative (bit layouts, illustrative numbers, nicer names for the same thing).
- **Quarantine before delete.** Dead modules move behind an `experimental` feature so the README can be truthful immediately; deletion needs explicit owner permission (AGENTS.md rule 1) and is listed as a separate decision in each block.
- **Every block carries three kinds of proof.** A unit or property test, a benchmark where speed is the claim, and an E2E scenario under a real PTY with structured logging (`tests/e2e/lib/pty.sh` plus JSONL via `tests/e2e/lib/validate_jsonl.py`). The Python identity driver used for Section 2.2.D becomes `scripts/pty_identity_matrix.py` and is reused by several blocks.
- **Would open beads close it?** Stated per block. Only G32 (bd-d4dtr) is fully covered.
- **Vision goals served** refer to Section 3 row numbers (V1..V71) and Section 2.2 letters (A..F).
- **Complexity:** S (under a day for one agent), M (1-3 days), L (a week), XL (multi-week or needs an owner decision first).

### 7.1 Critical gaps (block the core value proposition)

#### G01: Library consumers cannot run a program — PARTIAL -> WORKING

**Current state:** `ftui` facade `default = ["runtime", "extras"]` (`crates/ftui/Cargo.toml`); `crossterm` feature is opt-in; nothing enables `ftui-runtime/native-backend`. `AppBuilder::run()` under `#[cfg(not(feature = "crossterm-compat"))]` returns `Err(Unsupported)` (`crates/ftui-runtime/src/program.rs:7124`); `run_native()` exists only with `native-backend` on unix (`:7107`). `Program::new`/`with_config` are `crossterm-compat`-gated (`:4803-4814`); `with_native_backend` is `native-backend`-gated (`:5127-5160`). The showcase works because its own `default` enables both backends (`crates/ftui-demo-showcase/Cargo.toml`).
**Target state:** `ftui = "0.6"` with default features opens a terminal on Linux, macOS and Windows. `App::new(m).screen_mode(..).run()` selects the native backend on unix and crossterm elsewhere, and only fails with an `Unsupported` error naming the feature to enable when neither backend was compiled. Explicit `run_native()` / `run_crossterm()` remain for callers who care.
**Success criteria:**
- [ ] `crates/ftui/tests/default_backend.rs`: compiles `App::new(..)` and asserts `cfg!(any(feature = "native-backend", feature = "crossterm"))` under defaults, and that `AppBuilder::run` is not the stub (a `const BACKEND: &str` exposed by the runtime reports `"native"`/`"crossterm"`/`"none"`).
- [ ] `scripts/consumer_smoke_e2e.sh`: creates a temporary crate under `/data/projects/tmp-consumer-<pid>` (rch refuses paths outside `/data/projects`) depending on the facade by path with default features, copies the README Minimal API Example verbatim, builds it, runs it under `tests/e2e/lib/pty.sh` for 2 s, sends `q`, and logs JSONL with counts of `1049h/l`, `2026h/l`, `?25l/h`, DECSTBM, plus exit code 0 and the text `Ticks:` in the canonicalized screen. Runs in CI (G04).
- [ ] The same script with `--no-default-features --features runtime` asserts the error message names `native-backend`/`crossterm`.
**Implementation plan:**
1. `crates/ftui-runtime/src/program.rs`: replace the two `run` variants with one `pub fn run(self) -> io::Result<()>` that dispatches `#[cfg(all(feature = "native-backend", unix))]` to `Program::with_native_backend`, else `#[cfg(feature = "crossterm-compat")]` to `Program::with_config`, else returns `io::ErrorKind::Unsupported` with text "no terminal backend compiled: enable `native-backend` (unix) or `crossterm-compat`". Add `run_crossterm()` gated on `crossterm-compat`; keep `run_native()`.
2. Same file: add `Program::open(model, config)` with the same dispatch so non-builder users get one constructor; keep `new`/`with_config`/`with_native_backend`.
3. `crates/ftui/Cargo.toml`: `default = ["runtime", "extras", "backend"]`, `backend = ["native-backend", "crossterm"]`, `native-backend = ["runtime", "ftui-runtime/native-backend"]`. Because `ftui-tty` is unix-only inside, G04.3 must first make it compile (empty) on Windows.
4. `crates/ftui/src/lib.rs`: re-export `Program::open`; prelude gains `Widget` and `StatefulWidget` so README examples that call `.render(area, frame)` work with `use ftui::prelude::*`.
5. Add `crates/ftui/examples/minimal_inline.rs` containing the README example verbatim (it is the doc-tested source of truth for G02).
6. Write `scripts/consumer_smoke_e2e.sh` and add it to `ci.yml` job `e2e-widget-api` or a new `consumer-smoke` job.
7. README "Installation", "Quick Start", "Minimal API Example", and `docs/getting-started.md`: state the default backends and the `--no-default-features` slim path.
**Dependencies:** G04.3 (ftui-tty must compile on Windows) for the Windows leg; G02 for the doc-test side.
**Complexity:** M
**Vision goals served:** V5, V32, A.1-A.3; plan-doc 0.8.1 canonical entrypoint.
**Would open beads close it?** No.

#### G02: README and getting-started examples are unverified and do not compile — NOT_STARTED -> WORKING

**Current state:** README "Minimal API Example" (README.md:139-190) lacks `use ftui_widgets::Widget`; `Paragraph` has no inherent `render` (`crates/ftui-widgets/src/paragraph.rs:291`). About twenty `rust` fenced blocks in README and `docs/getting-started.md` are never compiled. Several are fragments that cannot compile in isolation (evidence sink, rollout scorecard, effect queue, focus, modal, lens, persistence, macro, simulator).
**Target state:** Every `rust` block in README.md, `docs/getting-started.md` and `docs/tutorials/agent-harness.md` is a rustdoc doc-test. Complete examples are `no_run` (they open a terminal); fragments are rewritten to be complete or marked `rust,ignore` with a visible "(fragment)" line.
**Success criteria:**
- [ ] `cargo test -p ftui --doc` compiles the README and both docs; CI `docs` job runs it.
- [ ] A deliberately broken snippet in a PR fails that job (verified once during rollout, then documented in `docs/testing/coverage-playbook.md`).
- [ ] `crates/ftui/examples/minimal_inline.rs` is byte-identical to the README block (checked by `scripts/check_readme_claims.py`, G06).
**Implementation plan:**
1. `crates/ftui/src/lib.rs`: add `#[cfg(doctest)] #[doc = include_str!("../../README.md")] pub struct ReadmeDoctests;` and the same for the two docs files.
2. Audit every `rust` fence: minimal example gets the `Widget` import and `rust,no_run`; evidence-sink and effect-queue examples become complete `no_run` programs using `ProgramConfig::default()`; ShadowRun/RolloutScorecard blocks (already exact per Section 5.3) get a `# fn main()` wrapper or `no_run`; blocks describing APIs that G06 decides to DOC-fix are rewritten to the real API; blocks for quarantined modules (lens, SLO, IVM) move to the "Experimental" section as `rust,ignore`.
3. `docs/getting-started.md`: same treatment; replace the crates.io sentence (G06).
4. `ci.yml` `docs` job: add `cargo test -p ftui --doc` after `cargo doc`.
**Dependencies:** G01 (the example must run under defaults), G06 (which mismatches are CODE vs DOC).
**Complexity:** M
**Vision goals served:** A.1, A.4-A.6, C (all rows), V41-V45.
**Would open beads close it?** No.

#### G03: The runtime test binary can hang forever — WRONG_APPROACH -> WORKING

**Current state:** `ftui_core::shutdown_signal` keeps one process-global `AtomicI32` (`crates/ftui-core/src/lib.rs:79-140`). `record_pending_termination_signal` is a CAS from 0; `clear_pending_termination_signal` is an unconditional store. `Program::complete_lifecycle` clears it (`program.rs:5490`) and the test helper `headless_program_with_resolved_config` clears it at construction (`program.rs:11274`). Only two tests take `with_test_signal_serialization`. Result: any parallel headless test wipes a pending signal between `record` and the first `observed_termination_signal()` check and `run()` blocks in the headless event loop. Observed locally (Section 1) and in CI macOS nightly (Section 5.1). No per-test timeout exists.
**Target state:** Signal state is owned per `Program` for tests and per process only for the real OS handler; no test can clear another test's signal; the serialization helper is unnecessary; CI kills any test that runs longer than 120 s and reports it as a failure with the test name.
**Success criteria:**
- [ ] `crates/ftui-runtime/src/program.rs` test `two_concurrent_headless_programs_with_independent_pending_signals_both_terminate` (spawns two headless programs on threads, injects SIGTERM into one and SIGINT into the other, both return `SignalTerminationError` with the right signal).
- [ ] `for i in $(seq 20); do cargo nextest run -p ftui-runtime --test-threads 16; done` green (documented in the bead close reason with the run log).
- [ ] `.config/nextest.toml` with `slow-timeout = { period = "60s", terminate-after = 2 }`; CI uses `cargo nextest run --workspace --no-fail-fast`; a scratch test with `loop {}` fails CI in under 3 minutes (verified once).
**Implementation plan:**
1. `program.rs`: add `pending_signal: Arc<AtomicI32>` to `Program` (default: a fresh atomic for headless/simulator constructors; the process-global slot for interactive constructors that install the signal thread). `observed_termination_signal()` reads `self.pending_signal`. `complete_lifecycle` clears only its own slot with a CAS from the observed value.
2. Add `pub fn inject_termination_signal(&self, signal: i32)` (documented as test/harness API) and use it in `run_invokes_on_shutdown_before_returning_signal_error` and `run_pending_signal_skips_initial_render_and_subscription_start`; delete `clear_termination_signal()` from `headless_program_with_resolved_config`.
3. `ftui-core/src/lib.rs`: keep the global for the OS handler path; make `with_test_signal_serialization` a no-op wrapper marked deprecated, then remove it once no crate uses it (harness, doctor).
4. Add `.config/nextest.toml`; `ci.yml` check matrix switches to nextest (G04.12); AGENTS.md "Compiler Checks" gains the nextest command.
**Dependencies:** none. Unblocks G04.
**Complexity:** M
**Vision goals served:** V70, E; plan-doc Gate 4 (cleanup) credibility.
**Would open beads close it?** No.

#### G04: `main` CI has not been green in 40 runs — REGRESSED -> WORKING (cluster of 15)

Each sub-block is one bead. Order inside the cluster: 04.1-04.5 and 04.10 first (they are pure fixes), then 04.6-04.9, then 04.11-04.15.

- **G04.1 Seed env leaks into unit and snapshot tests** (Widget API E2E, Demo Showcase). Current: `scripts/widget_api_e2e.sh:114` exports `FTUI_HARNESS_SEED=0` then runs `cargo test --workspace --lib`; `crates/ftui-harness/src/determinism.rs:518` reads it. `scripts/demo_showcase_e2e.sh` exports `E2E_SEED=0`; `crates/ftui-demo-showcase/src/determinism.rs:53` reads `FTUI_DEMO_SEED|FTUI_SEED|E2E_SEED` (default 7) so the blessed snapshot `determinism_lab_initial_80x24` shows `Seed: 7`. Target: scripts scope seed variables to the PTY invocations only (`env FTUI_HARNESS_SEED=0 cargo run ...`), never to `cargo test`; the showcase determinism screen ignores `E2E_SEED` under `cfg(test)`. Proof: both scripts green in CI; a unit test asserts the seed default is 7 when env is set under test. S.
- **G04.2 Wall-clock assertions on shared runners** (Check ubuntu/macos stable). Current: `crates/ftui-widgets/src/command_palette/scorer.rs:3508` asserts p95 under 5000 µs for a 1000-item corpus; `crates/ftui-runtime/src/subscription.rs:1768` asserts reconcile under 100 ms; `every_respects_interval` expects 2 ticks in a fixed sleep. Target: perf assertions move to the perf gate (G25) as criterion benches with baseline entries; timing tests use a virtual clock (`LabClock` exists in ftui-core `cx`) or generous CI multipliers via `FTUI_TEST_TIME_SCALE`. Proof: 20 consecutive green runs on `macos-latest`. M.
- **G04.3 Windows clippy dead code in ftui-tty** (Check windows stable). Current: 19 `dead_code` errors, unix-only items not cfg-gated (`crates/ftui-tty/src/lib.rs:217, 267, 597`). Target: the crate compiles clean on Windows as an empty shell (`#![cfg(unix)]` on the implementation module plus a documented stub `TtyBackend::open` returning `Unsupported` on non-unix), and `docs/WINDOWS.md` describes what is validated. Proof: Windows check job green; `docs/WINDOWS.md` row dated with the run id. S. Also unblocks G01 step 3.
- **G04.4 rustdoc `-D warnings`** (Documentation). Current: unresolved intra-doc link `ReceiptVerdict` and two redundant link targets in `crates/ftui-widgets/src/receipt_verifier_panel.rs`. Target: `cargo doc --workspace --no-deps` clean. S.
- **G04.5 Fuzz manifest** (Fuzz Build Check). Current: `fuzz/Cargo.toml:11` `[lints] workspace = true` while root `exclude = ["fuzz"]`. Target: `fuzz/Cargo.toml` gets its own `[workspace]` table and an inline `[lints.clippy]` mirror; all 12 targets build; a nightly job runs each target for 60 s with corpus artifacts. Proof: job green; corpus artifact uploaded. S.
- **G04.6 Runner tooling** (PTY E2E, FrankenTerm WS). Current: `rg` missing on runners (`tests/e2e/scripts/test_inline.sh:57,118,135`); python `websockets` never installed (`tests/e2e/lib/ws_client.py:46`). Target: workflow steps install `ripgrep` and `pip install websockets`; scripts fail fast with a clear message listing missing tools (`tests/e2e/lib/common.sh` gains `require_tools`). S.
- **G04.7 VHS install steps** (doctor_frankentui Verification, Extended). Current: `ci.yml:1147` `vhs_bin="$(find /tmp ... | head -n 1)"` under `set -euo pipefail` aborts when `find` returns 1; `doctor_frankentui_extended.yml:85` installs `/tmp/vhs` but the tarball extracts to `/tmp/vhs_0.10.0_Linux_x86_64/vhs`. Target: one shared composite action `.github/actions/install-vhs` that downloads the pinned release, verifies its sha256, and installs from the real path; both workflows use it; the 68 skipped gate steps run. Proof: both workflows execute `doctor` gates and upload `artifact_map.txt`. S.
- **G04.8 Golden Trace gate** (`frankenterm_js_parser_hooks_compat` exit 101, output hidden in /tmp). Target: the harness cell prints the failing test's stdout to the job log and uploads `/tmp/frankenterm_release_gates` as an artifact on failure; the test itself is fixed (root cause to be captured in the bead once visible). M.
- **G04.9 PTY E2E real failures** (42/166 on ubuntu). Current: after tooling, remaining failures cluster as `cleanup_*` (4), `keybind_*` (3), `voi_marker_missing` (4), `rtl_locale_not_selected` (4), mouse SGR, paste; macOS adds `vsearch_*`, `inline_story_*`, `dashboard_typewriter`, `bidi`. Target: each cluster gets a root-cause bead; likely links: `keybind_*` to G14, `voi_marker` to G10/G20, `rtl_locale` to G29, `cleanup_*` to G03/G13, `vsearch` to G10. Proof: `tests/e2e/scripts/run_all.sh` 166/166 on both OSes with JSONL logs archived. L (as a cluster).
- **G04.10 Pin CI to the toolchain file.** Current: jobs pass `toolchain: nightly` (floating) while `rust-toolchain.toml` pins `nightly-2026-08-25` with a documented ICE rationale. Target: every job uses `dtolnay/rust-toolchain@master` with `toolchain: ${{ steps.pin.outputs.channel }}` read from the file, or simply omits the input so the file wins. S.
- **G04.11 Release idempotency.** Current: `release.yml` publish loop fails on `ftui-simd@0.6.0 already exists`. Target: the loop queries `cargo info`/crates.io API per crate and skips already-published versions, logging `skip` vs `published`; dry-run mode in PRs. S.
- **G04.12 Job topology.** Current: one all-features test job exhausts runner disk; a hang holds the whole matrix for 6 h. Target: split `check` into `check` (clippy+fmt+check), `test-unit` (nextest, G03), `test-all-features` (with `cargo clean` of intermediates and `CARGO_INCREMENTAL=0`), each with a 45-minute timeout; `continue-on-error` is not used, but advisory jobs (coverage, benchmarks) move to a separate workflow so red there does not mask code failures. M.
- **G04.13 wasm32 builds.** Current: `wasm` job only checks core crates. Target: it builds `ftui-web` and `ftui-showcase-wasm` for `wasm32-unknown-unknown` (and `wasm-pack build` of the showcase when G23 lands). S.
- **G04.14 Scripts that are not gates.** Current: `scripts/e2e_test.sh` and `scripts/pane_e2e.sh` are invoked by no workflow; README lists them as E2E scripts. Target: both run in the `e2e-pty` job (smoke mode) with artifacts, or README stops implying they gate. S.
- **G04.15 `msrv` job.** Current: installs floating nightly and runs `cargo check`. Target: rename to `toolchain-pin-check` and make it assert the pinned nightly builds, or delete the job and the README badge claim. S.

**Success criteria for the cluster:** three consecutive green `ci.yml` runs on `main`; `doctor_frankentui Extended Verification` green three nights running (or demoted per G22); the "Landing the Plane" section of AGENTS.md links the green run id.
**Dependencies:** G03 before G04.12; G14/G10/G29 for parts of G04.9.
**Complexity:** L (cluster)
**Vision goals served:** V70, V68, E.
**Would open beads close it?** No.

#### G05: The flicker-free guarantee is off on most terminals — PARTIAL -> WORKING

**Current state:** `use_sync_output()` returns `sync_output && !in_any_mux` (`crates/ftui-core/src/terminal_capabilities.rs:1233`); `sync_output` is true only for the `modern()` and `kitty()` profiles; `xterm_256color()` has `sync_output: false`. Identity mapping recognizes `kitty`/`xterm-kitty`, `TERM_PROGRAM` values for ghostty/Alacritty, and treats any WezTerm identity as mux evidence (`:1040-1062`). `caps_probe.rs` can query DA1, DA2, truecolor and background but has **no DECRPM 2026 probe** (`probe_capabilities`, `:146-200`), and `Program::with_native_backend` only probes when color depth is Ansi256 (`program.rs:5175`). Measured result: Section 2.2.D table.
**Target state:** Sync output is enabled whenever the terminal says it supports DEC 2026 (probe), or when identity is known-good; WezTerm is treated as a modern terminal unless mux-domain evidence exists; the inline scroll-region strategy verifies DECSTBM at runtime and falls back to overlay when it misbehaves (this makes the README's "Hybrid with fallback" claim true); every decision is logged with its reason; a compat matrix is asserted in CI per identity.
**Success criteria:**
- [ ] Unit tests in `caps_probe.rs`: DECRPM reply parsing for `?2026;1$y`, `;2$y`, `;0$y`, `;3$y`, `;4$y`, timeout, garbage.
- [ ] `scripts/pty_identity_matrix.py` (the driver from this audit) asserts, for each identity row of Section 2.2.D plus `TERM=alacritty`, `LC_TERMINAL=iTerm2`, `TERM_PROGRAM=vscode`, `TERM_PROGRAM=Apple_Terminal`, `TERM_PROGRAM=WezTerm` with and without `WEZTERM_UNIX_SOCKET`, the expected sync-pair count (>0 or 0), DECSTBM count in inline mode, and clean teardown; it emits JSONL and runs in `emulator_compat_matrix.yml`.
- [ ] With a PTY that answers `?2026;2$y` (the driver can reply), `TERM=xterm-256color` produces sync pairs; with no reply it does not.
- [ ] `docs/compat-matrix.md` generated from the JSONL and linked from README "Synchronized Output", which states the preconditions.
**Implementation plan:**
1. `crates/ftui-core/src/caps_probe.rs`: add `SYNC_OUTPUT_QUERY = "\x1b[?2026$p"`, `probe_sync_output(timeout) -> Option<bool>`, `ProbeConfig.probe_sync_output: bool` (default true), `ProbeResult.sync_output: Option<bool>`; the same for DECSTBM cannot be queried, so add `probe_cursor_position` (CPR) for step 4.
2. `terminal_capabilities.rs`: `refine_from_probe` sets `sync_output = true` on `Some(true)` (upgrade-only; never downgrade a known-good profile). Add identities: `TERM=alacritty` -> modern; `LC_TERMINAL=iTerm2` or `TERM_PROGRAM=iTerm.app` -> modern-with-probe (sync false until the probe confirms); `TERM_PROGRAM=vscode` -> xterm-256color-with-probe; `TERM_PROGRAM=Apple_Terminal` -> scroll region yes, sync false, no probe; WezTerm -> modern; `in_wezterm_mux` only when `WEZTERM_UNIX_SOCKET` is set **and** `TERM_PROGRAM` is absent (ssh into a mux) or `WEZTERM_MUX_DOMAIN`-style evidence is present. Add `TerminalProfile::{Alacritty, ITerm2, VsCode, AppleTerminal, WezTerm}` to `from_str`/`as_str`.
3. `program.rs:5170-5185`: probe whenever stdin is a terminal, not in a mux, and `FTUI_CAPS_PROBE != "0"`; keep the truecolor probe restricted to Ansi256; total probe budget 300 ms.
4. `crates/ftui-runtime/src/terminal_writer.rs` inline path: on first present with `InlineStrategy::ScrollRegion`/`Hybrid`, run a one-time DECSTBM self-test (set region, emit a controlled `\n` at the region bottom, CPR, check the cursor stayed inside the region), else switch to `OverlayRedraw` and log `inline_strategy_fallback`. This is the runtime fallback `inline_mode.rs:93-107` currently lacks, and it makes Hybrid distinct from ScrollRegion.
5. `capability_override.rs`: add `FTUI_SYNC_OUTPUT=0|1`, `FTUI_SCROLL_REGION=0|1`; every capability decision emits a `capability_decision` evidence line (reuse the log-BF ledger from G28) with `source: env|probe|override|self_test`.
6. Add `scripts/pty_identity_matrix.py` and wire it into `emulator_compat_matrix.yml`; generate `docs/compat-matrix.md`.
7. README: rewrite "Synchronized Output" and "Inline Mode" sections to state the mechanism and its preconditions; AGENTS.md architecture note.
**Dependencies:** G28 (ledger) is helpful but not required; G04.13 not required.
**Complexity:** L
**Vision goals served:** V1, V31, V49, D; README "Guarantee" and "Theorem 1".
**Would open beads close it?** No.

#### G06: README and AGENTS.md describe code that does not exist — WRONG_API -> WORKING (claims ledger)

**Current state:** Section 2.2.C lists 25+ mismatches; counts (screens, widgets, borders), layouts (`CellAttrs`, `GraphemeId`), API names and shapes, defaults, evidence event names, benchmark numbers, and the architecture diagram are wrong in README.md and partly in AGENTS.md. Prior truth passes (bd-1zmo3, 2026-04-09) regressed within weeks because nothing checks them.
**Target state:** A checked-in `docs/claims-ledger.md` maps every tracked claim to its proof; a CI script fails when README contains a tracked number or backticked identifier without a ledger row, or when a ledger row's proof (test name, file path, or command) no longer exists. README and AGENTS.md are rewritten once against the ledger.
**Decision table (CODE vs DOC) for Section 2.2.C rows:**

| Row | Decision | Lands in |
|---|---|---|
| `Frame::render_widget/render_stateful_widget/area` | CODE (convenience methods) | G17.9 |
| `Layout::horizontal([..]).split(..)` | CODE (`Layout` alias + constructor taking constraints) | G17.9 |
| Focus `register(str)/set_next` | DOC (document `FocusId`, `insert`, `connect`) | this block |
| Modal `push(ConfirmDialog::new)` | DOC (`Dialog::confirm`) | this block |
| `frame.link_registry()` / `cell.link_id =` | DOC (`register_link`, `with_link`) | this block |
| Cell and GraphemeId layouts | DOC (draw the real layout) | this block |
| `TimeTravel` API | DOC + quarantine | G07 |
| `Stylesheet::register` | DOC (`StyleSheet::define`) + CODE consumer | G17.8 |
| `TableTheme::modern().with_*` | CODE | G17.7 |
| 9 border styles | DOC (5) unless G17.6 adds more | G17.6 |
| `Cmd::perform` | DOC (`Cmd::task`) | this block |
| `Cmd::SetClipboard/GetClipboard` | CODE | G15 |
| `tick_every`, `file_watcher` | CODE | G16 |
| `frame.checksum()`, `MacroPlayer::next`, `sim.send_event` | DOC | this block |
| `PersistenceConfig`/`FileBackend` names | DOC | this block |
| `field_lens!` | DOC + quarantine | G07 |
| `slo.yaml` schema | DOC + quarantine | G07 |
| Evidence event names | DOC for existing names; CODE for `voi_sample` | G20 |
| Degradation level names | DOC | this block |
| Editor coalescing, paragraph moves | CODE | G15 |
| Input history, Textarea syntax hook, indeterminate Progress, JsonView folding, Sparkline markers | CODE | G17 |
| Widget names (`CachedWidget`, no `DragHandle`, `InspectorOverlay`, `NotificationStack`, `ValidationErrorDisplay`) | DOC | this block |
| 46 screens / 11 categories / `3d_data` / `quake` | DOC (45, 6, real slugs) | this block |
| VFX attribution | DOC | this block |
| Command palette factor formulas | DOC (state the real formulas) | this block |
| i18n claims | CODE partial + DOC | G29 |
| Benchmark numbers | regenerate | G25 |
| `TerminalSession (crossterm)` diagram | DOC | this block |
| Inline "Hybrid with fallback" | CODE | G05.4 |
| 80+ widgets | DOC (57 production types, listed) | this block |
| 850K+ lines | DOC (1.05M) | this block |
| `ftui = "0.5"`; getting-started crates.io sentence | DOC | this block |
| `FTUI_HARNESS_VIEW ... ftui-demo-showcase` | DOC | G35 |
| VOI defaults, resize delays, gesture defaults | DOC | this block |
| SOS provenance | CODE (header) + DOC | G21 |

**Success criteria:**
- [ ] `scripts/check_readme_claims.py` runs in the `docs` job; it extracts every backticked identifier and every number with a unit or count noun from README.md and AGENTS.md, requires a ledger row, and verifies each row's proof exists (`cargo test -- --list` output for test names; `test -e` for paths; `rg` for identifiers in `crates/*/src`).
- [ ] Ledger has 100% coverage of Section 2.2.C rows with each row marked CODE (linking the closing bead) or DOC (linking the README diff).
- [ ] README doc-tests (G02) green after the rewrite.
**Implementation plan:**
1. Write `docs/claims-ledger.md` (table: claim, location, kind, proof, status) seeded from Sections 2.2.C and 3.
2. Write `scripts/check_readme_claims.py` with an allowlist file for prose numbers that are not claims (dates, version numbers).
3. Rewrite README sections in this order: Installation and Quick Start (G01), Minimal API Example (G02), Workspace Overview (add ftui-extras' real contents: Mermaid, terminal emulator, Doom/Quake, text effects, Sinkhorn morph), Demo Showcase Gallery (45 screens, 6 categories), Widget System (57 types, real names, real features), Table Theming (real presets and builders), Alien Artifact sections (mark each as "wired by default", "opt-in", or "experimental" per G07), Performance Engineering (real layouts), Runtime Migration (G24 wording), Web/WASM (G23 wording), Synchronized Output (G05 wording), Benchmarks (G25 artifact), FAQ counts.
4. AGENTS.md: Key Dependencies table (crossterm optional and legacy, `ftui-tty` native, `nix`/`rustix`), architecture diagram, Workspace Structure note on `tests/`, `doctor_frankentui` verification block updated to commands that pass (G22), add nextest and the claims check to Compiler Checks.
5. `docs/getting-started.md`: crates.io sentence, features, example.
6. Add an "Experimental modules" README section listing G07's quarantined modules with one line each and the feature flag.
**Dependencies:** G01, G02, G07 (to know what is experimental), G25 (numbers). Can start immediately for pure DOC rows.
**Complexity:** L
**Vision goals served:** every C row, V6, V39-V45, V51, V65, V69.
**Would open beads close it?** No.

#### G07: Dead modules masquerade as features — DEAD -> WORKING (wired) or EXPERIMENTAL (quarantined)

**Current state:** About 30 of 63 `ftui-runtime` modules, three width caches, the a11y tree, `height_predictor`, `fenwick` mode, `egraph`, `S3FifoLayoutCache`, `gesture`, `hover_stabilizer`, `keybinding`, `roaring_bitmap`, `tier_budget`, bidi/shaping/normalization, `ConformalRanker`, `DecisionCard`, `DriftVisualization`, `CachedWidget`, `ErrorBoundary<W>`, `TimeTravel` have no production consumer (Section 2.2.B). `timeline_aggregator.rs` and `countmin_sketch.rs` are not declared in `lib.rs`.
**Target state:** Every declared module is either reachable from a production path (with a test proving it) or compiled only under an `experimental` cargo feature and listed in the README "Experimental modules" section. A CI gate fails on new orphans.
**Wire list (each is its own block):** width cache G08, a11y G09, VirtualizedList G10, conformal G11, BOCPD G12, controllers G13, keybinding G14, gesture and hover G18, hint ranker G19, evidence G20, SAT and caps ledger G28.
**Quarantine list (this block, internal modules only):** `rough_path`, `flat_combine`, `lens`, `ivm`, `cost_model`, `sos_barrier` (+ `sos_barrier_coeffs`, after G21), `alpha_investing`, `flake_detector`, `slo`, `policy_config`, `policy_registry`, `evidence_bridges`, `validation_pipeline`, `degradation_cascade` (until G13 merges it), `conformal_frame_guard`, `conformal_alert`, `conformal_stages`, `eprocess_throttle` (until G13), `allocation_budget` (until G13), `resize_sla`, `reversible`, `schedule_trace`, `wasm_runner`, `diff_evidence` (until G13), `egraph`, `S3FifoLayoutCache`, `roaring_bitmap`, `tier_budget`, `ConformalRanker`, `timeline_aggregator` + `countmin_sketch` (declared under the feature; `action_timeline` demo may adopt the aggregator).
**Public-API rule (not quarantined):** widgets and harness types are library surface; a widget does not need an in-tree consumer to be legitimate, it needs to be exercised. So `DecisionCard`, `DriftVisualization`, `CachedWidget` and `ErrorBoundary<W>` get a `widget_gallery` entry plus a snapshot (S each), and `TimeTravel`/`TimeTravelInspector` back the `snapshot_player` screen's scrubber (currently only a label) with README API names corrected (G06). The reachability gate treats `ftui-widgets` and `ftui-harness` public types as reachable when a showcase screen or a harness binary/example uses them.
**Success criteria:**
- [ ] `scripts/check_module_reachability.py`: for each `pub mod` in each crate's `lib.rs` not under `#[cfg(feature = "experimental")]`, require a reference (`X::`, `use crate::X`, `use ftui_<crate>::X`) from a non-test file outside the module's own file/dir; the allowlist `docs/module-reachability-allowlist.txt` starts at today's set and may only shrink; runs in the `check` job.
- [ ] `cargo check --workspace --all-targets` with and without `--features experimental` both green; the `features` CI job includes the experimental combination.
- [ ] README "Experimental modules" section exists and each listed module's tests are gated `#![cfg(feature = "experimental")]`.
**Implementation plan:**
1. Add `experimental = []` to `ftui-runtime`, `ftui-widgets`, `ftui-layout`, `ftui-render`, `ftui-text`, `ftui-core`, `ftui-harness`; gate the `pub mod` lines and their `tests/*.rs` and `benches/*.rs` files.
2. Declare `timeline_aggregator` and `countmin_sketch` under the feature; fix whatever no longer compiles (they have been orphaned since 2026-02/03).
3. Write the reachability script and allowlist; wire into CI.
4. Owner decision list for deletion (needs explicit permission): `roaring_bitmap`, `flat_combine`, `rough_path`, `resize_sla`, `reversible`, `schedule_trace`, `wasm_runner` (harness has its own asciicast; ftui-web has its own `StepResult`).
**Dependencies:** none for quarantine; G13 for the merged pairs.
**Complexity:** M (quarantine + gate); deletions S each after permission.
**Vision goals served:** V12-V25, V45, V46, V55, V58-V60, B.
**Would open beads close it?** No.

### 7.2 Major gaps (significantly degrade the vision)

#### G08: Width cache is not on the production path — DEAD -> WORKING

**Current state:** `crates/ftui-text/src/width_cache.rs` has `WidthCache` (LRU, `:97`), `TinyLfuWidthCache` (`:1034`, CMS + doorkeeper), `S3FifoWidthCache` (`:1233`); none is constructed outside docs, tests and `benches/cache_bench.rs`. Production width goes `ftui-text/src/wrap.rs:451` -> `ftui_core::text_width::grapheme_width` (ASCII fast path, then `unicode_display_width`, uncached). `ftui-render` depends only on `ftui-core`, so a cache in `ftui-text` cannot serve the grapheme pool.
**Target state:** One cache implementation lives in `ftui-core::text_width` (ftui-core already hosts `s3_fifo.rs`) behind a thread-local, keyed by grapheme hash, consulted for non-ASCII graphemes by `grapheme_width`; `ftui-text` wrap and `ftui-render` grapheme pool both benefit; the README names the policy actually used.
**Success criteria:**
- [ ] `crates/ftui-text/benches/cache_bench.rs` extended to a wrap benchmark over a mixed CJK/emoji/ZWJ corpus; the chosen policy shows at least 30% fewer nanoseconds per non-ASCII grapheme than uncached at steady state, recorded in `tests/baseline.json` (`text_width_non_ascii`).
- [ ] Proptest: cached width equals uncached width for arbitrary grapheme clusters (`proptest_width_cache_transparency`).
- [ ] Hit-rate telemetry exposed via `text_width::cache_stats()` and logged once per showcase run in `scripts/demo_showcase_e2e.sh` JSONL.
**Implementation plan:**
1. Run `cache_bench.rs` for LRU vs TinyLFU vs S3-FIFO on the corpus; pick the winner (S3-FIFO is the expected winner per its own module docs; decide by data).
2. Move the winner into `crates/ftui-core/src/text_width/cache.rs` (submodule of the existing inline `text_width` module); `grapheme_width` consults it after the ASCII fast path; cap 4,096 entries; `FTUI_WIDTH_CACHE=0` disables.
3. `ftui-text/src/width_cache.rs`: keep only the thin `cached_width` shim delegating to ftui-core, or quarantine the losers (deletion needs permission).
4. Update README "Width Calculation" (G06 ledger row).
**Dependencies:** none.
**Complexity:** M
**Vision goals served:** V20, V21, B.
**Would open beads close it?** No.

#### G09: Accessibility tree is never built — DEAD -> WORKING

**Current state:** `ftui-a11y` (2,019 lines, no dependencies) provides `A11yNodeInfo`, `A11yTreeBuilder`, `A11yTreeDiff`, live regions; nine widgets implement `Accessible::accessibility_nodes()` (`list.rs:973`, `table.rs:332`, `block.rs:440`, `tabs.rs:518`, `progress.rs:209`, `input.rs:1155`, `spinner.rs:187`, paragraph, scrollbar) but nothing calls it; `Frame` (`crates/ftui-render/src/frame.rs`) has `links`, `hit_grid`, `widget_signals`, `arena` but no a11y hook; `accessibility_panel` renders theme toggles.
**Target state:** When enabled, the runtime builds an accessibility tree every frame from widgets' declarations during `view()`, diffs it against the previous frame, emits live-region announcements as evidence, exposes the tree to the model, and the showcase panel renders the real tree.
**Success criteria:**
- [ ] `ftui-render` unit tests: `frame.push_a11y(node)` collects nodes in render order with parent nesting from `Block` children.
- [ ] Snapshot `dashboard_a11y_tree_80x24.snap` of the tree text dump; `A11yTreeDiff` announcement test on focus move between two `TextInput`s.
- [ ] Evidence line `a11y_announcement` written through the sink; tracing target `ftui.a11y` added to `telemetry_schema.rs`.
- [ ] `scripts/a11y_transitions_e2e.sh` (exists) extended to assert announcements for Tab navigation in the forms screen.
**Implementation plan:**
1. `crates/ftui-render/Cargo.toml`: add `ftui-a11y` (no cycle: it has no deps). `frame.rs`: `pub a11y: Option<&'a mut A11yTreeBuilder>`, `push_a11y(&mut self, node)`, `with_a11y_scope(role, f)` for containers.
2. `ftui-widgets`: in the nine `Widget::render` impls, call `frame.push_a11y` with the existing `accessibility_nodes()` output; `Block` wraps children in a scope.
3. `ftui-runtime/src/program.rs`: `ProgramConfig::with_accessibility(bool)` (default off; showcase on); `Program` owns a builder, resets per frame, stores `last_a11y_tree: Arc<A11yTree>`, diffs, emits `a11y_announcement` evidence and a `Msg`-independent hook `Model::on_accessibility_tree(&Arc<A11yTree>)` with a default no-op (keeps `Model` backward compatible).
4. Showcase: `accessibility_panel.rs` renders the tree from the hook; keep the theme toggles.
**Dependencies:** G07 (experimental gate not needed here), G20 for the evidence name.
**Complexity:** L
**Vision goals served:** V46, B.
**Would open beads close it?** No.

#### G10: VirtualizedList's Bayesian machinery is disconnected — DEAD -> WORKING

**Current state:** `ItemHeight::{Fixed, Variable(HeightCache), VariableFenwick}` (`crates/ftui-widgets/src/virtualized.rs:88-95`); default `Fixed(1)`; `with_variable_heights_fenwick` exists (`:164`) but no caller uses it. `height_predictor.rs` (1,079 lines: `HeightPredictor::{predict, observe, posterior_mean}`) has zero consumers; no VOI remeasurement exists. `virtualized_search.rs:613` and `log_search.rs:43` keep their own vectors and `LogViewer`; only `widget_gallery.rs:1920` uses `VirtualizedList` (fixed height).
**Target state:** Variable-height lists default to the Fenwick index; unmeasured rows use the predictor; remeasurement is scheduled by a VOI rule surfaced through `WidgetSignal`; the two search demos use `VirtualizedList`; the runtime writes `voi_sample` evidence for those decisions.
**Success criteria:**
- [ ] Proptest `scroll_to_index_is_stable_under_late_measurements`: after measuring rows out of order, `scroll_to(i)` lands within the conformal interval, and the "scroll jump" metric (sum of absolute offset corrections) is lower with the predictor than with the mean-height baseline on a synthetic long-tail corpus (bench in `ftui-widgets/benches/virtualized_bench.rs`).
- [ ] `virtualized_search` and `log_search` snapshots re-blessed with `VirtualizedList`; PTY tests `vsearch_*` (G04.9) green.
- [ ] Evidence `voi_sample` lines appear in `scripts/demo_showcase_e2e.sh` JSONL with the fields `alpha, beta, voi, sample_cost, decision`.
**Implementation plan:**
1. `virtualized.rs`: `with_variable_heights(default)` returns `VariableFenwick`; add `predictor: Option<HeightPredictor>` with per-category registration (category = item kind supplied by the caller or a default); `measure(i, h)` calls `observe`; unmeasured rows use `predict().mean`.
2. Add `RemeasurePolicy` (Beta-VOI, same formula as README) in `virtualized.rs`; when it decides to sample, push `WidgetSignal::Remeasure { index, voi, cost }`.
3. `program.rs`: translate that signal into a `voi_sample` evidence line (G20).
4. Rewrite `virtualized_search.rs` and `log_search.rs` on `VirtualizedList` with the search filter applied to the index set.
**Dependencies:** G20 (evidence writer).
**Complexity:** L
**Vision goals served:** V11, B; README "Fenwick-backed virtualization" and "Bayesian height prediction".
**Would open beads close it?** No.

#### G11: Conformal frame-time gating is off by default — OPT-IN -> WORKING

**Current state:** `ProgramConfig.conformal_config: None` (`program.rs:3008`); only `ftui-harness/src/main.rs:1925` and tests set it. When set, predict/degrade at `:6188-6234` and observe at `:6393-6406` are real; `budget_decision` evidence carries bucket, `q_b`, `upper_us`, `risk`, `fallback_level`. `conformal_stages.rs` (per-stage monitors) is unreferenced.
**Target state:** The predictor is on by default with a warm-up (no gating until 30 observations per bucket), disabled for headless/simulator constructors, tunable via `ProgramConfig::with_conformal(None)`; the showcase runs with it; `conformal_stages` stays experimental until stage timings justify it.
**Success criteria:**
- [ ] Unit test `conformal_default_on_with_warmup`: first 30 frames never degrade; a synthetic 3x budget frame series after warm-up triggers `fallback_level >= 1` and recovers.
- [ ] `budget_decision` lines present in the showcase E2E JSONL with `fallback_level` distribution logged.
- [ ] Bench `frame_render` p99 in `tests/baseline.json` unchanged within threshold with the predictor on (it costs one quantile lookup per frame).
**Implementation plan:**
1. `program.rs`: `ProgramConfig::default()` sets `conformal_config: Some(ConformalConfig::default_with_warmup(30))`; `headless_*` and simulator constructors force `None`.
2. Add `with_conformal(Option<ConformalConfig>)` builder; document in README "Degradation Cascade".
3. Quarantine `conformal_stages` (G07) with a follow-up bead: emit per-stage timings in `budget_decision` first, then wire stages if any stage dominates in the collected evidence.
**Dependencies:** G25 (baseline entry), G20.
**Complexity:** S
**Vision goals served:** V13, V14, V37.
**Would open beads close it?** No.

#### G12: BOCPD regime detection is off by default — OPT-IN -> WORKING

**Current state:** `CoalescerConfig::default().enable_bocpd = false` (`crates/ftui-runtime/src/resize_coalescer.rs:202`); default regime detection is a 10/5 events-per-second heuristic (`:197-198`); `bocpd.rs` defaults match the README; the log10 Bayes-factor ledger is real (`:367-430`). Open bead bd-1za0z lists telemetry defects: `forced_by_deadline` inflation, heuristic cooldown-exit running in BOCPD mode, Immediate-mode Burst pinning, `ShowPlaceholder` dead action.
**Target state:** BOCPD is the default regime detector with the heuristic as fallback when the posterior is undefined; the four telemetry defects are fixed; the differential harness proves parity or improvement.
**Success criteria:**
- [ ] `tests/e2e/lib/resize_storm_differential.py` run over the recorded traces in `crates/ftui-harness/src/resize_storm.rs` fixtures: BOCPD-on renders no more frames during drag than heuristic and applies the final size within 40 ms of the last event; report archived as `docs/perf/resize_differential_<date>.md`.
- [ ] Unit tests for each bd-1za0z defect (quiet-gap resize not counted as forced; no contradictory `regime_transition` pairs in BOCPD mode; Immediate mode reports `Steady`; `ShowPlaceholder` either consumed or removed).
- [ ] `decision_evidence` lines carry `detector: bocpd|heuristic`.
**Implementation plan:**
1. Fix the bd-1za0z items in `resize_coalescer.rs` (they are enumerated in the bead with line-level detail).
2. Flip `enable_bocpd` default to true; keep `heuristic_fallback: true`.
3. Run the differential; flip back if it loses, and record why.
4. README "BOCPD" section states defaults and delays (16/40 ms coalescing, 200/20 ms observation means).
**Dependencies:** none. Closes bd-1za0z items (1)-(2).
**Complexity:** M
**Vision goals served:** V10, V36, V51.
**Would open beads close it?** Partially (bd-1za0z covers the telemetry defects, not the default).

#### G13: Duplicate controllers and half-finished seams — WRONG_APPROACH -> WORKING

**Current state:** Two e-processes (`ftui-render/src/budget.rs:212-330 EProcessState` wired; `ftui-runtime/src/eprocess_throttle.rs` with GRAPA, dead). Two degradation ladders (`BudgetController` wired; `degradation_cascade.rs` dead). Two diff-evidence ledgers (`terminal_writer.rs:1655` wired; `diff_evidence.rs` dead). Two allocation monitors (`ftui-render/src/alloc_budget.rs` referenced only by a doc comment in `frame_guardrails.rs:6`; `ftui-runtime/src/allocation_budget.rs` dead): allocation leak detection is not wired at all. Two terminal-session stacks (`ftui-core/src/terminal_session.rs` crossterm with panic hook `:1194`; `ftui-tty` `RawModeGuard` `:365-399` with its own hook `:312`). `ftui-backend` seam: events go through `BackendEventSource`; presentation bypasses `BackendPresenter` (only ftui-web implements it).
**Target state:** One e-process (with GRAPA adaptive betting) inside `BudgetController`; one degradation ladder; one diff ledger; allocation leak detection wired into `FrameGuardrails::check_frame`; shared session teardown logic in ftui-core used by both session stacks; `Program` presents through `BackendPresenter` implemented by ftui-tty and ftui-web.
**Success criteria:**
- [ ] After the merge, `scripts/check_module_reachability.py` shows no duplicate implementations (`eprocess_throttle`, `degradation_cascade`, `diff_evidence`, `allocation_budget` gone or experimental).
- [ ] Test `budget_controller_grapa_adapts_lambda`: with GRAPA the e-process crosses `1/alpha` sooner than fixed lambda on a step change, never on the null.
- [ ] Test `guardrails_detect_allocation_drift`: a synthetic linear memory growth triggers `AllocLeakDetector` through `check_frame` and a `guardrail_snapshot` line.
- [ ] PTY test `teardown_sequence_identical_native_vs_crossterm`: byte-identical teardown escape sequence order under both backends (kitty pop once, mouse off, paste off, cursor show, alt-screen leave).
- [ ] ftui-web's presenter and ftui-tty's presenter both implement `BackendPresenter`; `Program` no longer takes `W: Write` for presentation.
**Implementation plan:**
1. Port GRAPA lambda adaptation from `eprocess_throttle.rs` into `budget.rs::EProcessState`; quarantine then delete `eprocess_throttle.rs` (permission).
2. Delete-or-quarantine `degradation_cascade.rs`, `diff_evidence.rs`, `allocation_budget.rs`; wire `alloc_budget::AllocLeakDetector` into `frame_guardrails.rs` using the `memory_bytes` series already passed to `check_frame`.
3. Extract `ftui-core::session_teardown` (ordered cleanup steps, panic-hook chaining, kitty pop-once latch) used by `TerminalSession::drop` and `ftui-tty::RawModeGuard::drop`.
4. `ftui-backend`: keep `BackendPresenter`; implement it in `ftui-tty` (over the existing writer) and make `Program<M, E, P: BackendPresenter>`; `TerminalWriter` becomes the shared presenter core.
**Dependencies:** G07 (quarantine mechanics), G03 (lifecycle tests), G01 (constructors).
**Complexity:** XL
**Vision goals served:** V4, V12, V15, V37, V58, B, design gap row.
**Would open beads close it?** No.

#### G14: Keybinding system does not exist as described — NOT_STARTED -> WORKING

**Current state:** `crates/ftui-core/src/keybinding.rs` (1,913 lines) is an Esc-Esc `SequenceDetector` plus `SequenceConfig` (`:308-416`, env `FTUI_DISABLE_ESC_SEQ`). Widgets' `Keybinding`/`KeybindingHints` (`help_registry.rs:55`, `help.rs:1088`) are display-only. `pane_keymap` in ftui-runtime hardcodes pane keys. PTY tests `keybind_*` fail (Section 5.1).
**Target state:** A real keymap: bindings with priority levels (global, mode, widget), chord sequences (`g g`, `Ctrl+x Ctrl+s`) with timeout, context activation, conflict/shadowing report, serde load/save (TOML and JSON, feature `serde`), and a dispatcher used by the showcase, `pane_keymap`, and `Help` hints (G19).
**Success criteria:**
- [ ] Unit tests with a virtual clock: chord completes within timeout, expires after, single-key bindings still fire while a chord is pending, priority resolution (widget beats mode beats global), `conflicts()` reports shadowed bindings.
- [ ] Round-trip test: `KeyMap -> TOML -> KeyMap` equality; JSON likewise.
- [ ] PTY E2E `tests/e2e/scripts/test_keybinding_chords.sh`: `g g` jumps to top in the log viewer screen, `Ctrl+x Ctrl+s` shows the save toast; the existing `keybind_*` cases pass.
**Implementation plan:**
1. `keybinding.rs`: add `KeyCombo`, `Chord(Vec<KeyCombo>)`, `Binding<A> { chord, action: A, priority: Priority, context: Option<ContextId> }`, `KeyMap<A>`, `KeyDispatcher<A>` state machine reusing `SequenceDetector`'s timing, `ConflictReport`.
2. `serde` feature: derive on the types; `KeyMap::from_toml/to_toml` via the `toml` dep already used by `policy-config`.
3. `ftui-runtime/src/pane_keymap.rs` and the showcase `app.rs` global keys migrate to `KeyMap`; `Help`/`KeybindingHints` read from the same map.
4. Document in `docs/spec/keybinding-policy.md` (exists) and README.
**Dependencies:** none; G19 builds on it.
**Complexity:** L
**Vision goals served:** V62; README "Keybinding System (1,900+ Lines)".
**Would open beads close it?** No.

#### G15: Editor lacks coalescing, paragraph movement and clipboard commands — PARTIAL -> WORKING

**Current state:** `crates/ftui-text/src/editor.rs:498-516` `push_undo` pushes every operation; no paragraph movement; `Cmd` (`program.rs:325-373`) has no clipboard variants; only inbound `Event::Clipboard` exists (`ftui-core/src/event.rs:52`); `ftui-extras/src/clipboard.rs` (1,861 lines) already implements OSC 52 encoding.
**Target state:** Typing bursts coalesce into one undo step (break on word boundary, direction change, or 500 ms idle); paragraph movement exists; `Cmd::SetClipboard(String)` and `Cmd::GetClipboard` emit OSC 52 through `TerminalWriter` and deliver `Event::Clipboard` on reply.
**Success criteria:**
- [ ] Tests: typing "hello world" yields two undo steps; deletion runs coalesce; paragraph movement over mixed blank-line layouts.
- [ ] PTY E2E `test_clipboard_osc52.sh`: asserts `\x1b]52;c;<base64>\x07` on the wire for `SetClipboard`, and that a scripted reply produces one `Event::Clipboard`.
- [ ] `TextArea` in the `advanced_text_editor` screen wired to both (`y`/`p`).
**Implementation plan:**
1. `editor.rs`: `UndoGroup` with coalescing rules and an explicit `break_undo_group()`; expose `set_coalesce_idle(Duration)`.
2. `cursor.rs`: `move_paragraph_{up,down}` using blank-line boundaries.
3. `program.rs`: add the two `Cmd` variants; `TerminalWriter::write_osc52_set/query` reusing `ftui-extras` encoding moved into `ftui-core` (small module) to avoid a runtime->extras dependency.
**Dependencies:** none.
**Complexity:** M
**Vision goals served:** V57, C rows for editor and clipboard.
**Would open beads close it?** No.

#### G16: Subscription conveniences promised by the README — NOT_STARTED -> WORKING

**Current state:** `Every` subscription exists (`subscription.rs:477`); no `tick_every` function; no filesystem watcher; `Cmd::perform` does not exist (`Cmd::task*` does).
**Target state:** `tick_every(Duration)` returns a boxed `Every`; `file_watcher(path)` behind feature `fs-watch` (crate `notify`) yields `Event::Custom`-mapped messages; README documents `Cmd::task` (DOC).
**Success criteria:**
- [ ] Unit test with `LabClock`: `tick_every(16ms)` yields 3 ticks in 50 ms virtual time.
- [ ] Integration test with a temp dir: create/modify/delete produce three watcher messages within 1 s; feature-gated in CI `features` job.
- [ ] Showcase `async_tasks` screen shows a watched temp file changing.
**Implementation plan:** `subscription.rs` helpers; new `fs_watch.rs` under the feature; README edits.
**Dependencies:** none.
**Complexity:** S
**Vision goals served:** V32, C.
**Would open beads close it?** No.

#### G17: Widget features the README promises — PARTIAL/WRONG_API -> WORKING (cluster of 9)

- **G17.1 `ProgressBar` indeterminate mode**: animated marquee with `Spinner`-style frames driven by `frame` tick; snapshot at three phases. S.
- **G17.2 `JsonView` fold/unfold**: node ids, `toggle(path)`, keyboard `Enter`/`Space`, snapshot folded/unfolded. M.
- **G17.3 `TextArea` syntax hook**: `with_highlighter(Box<dyn Fn(&str) -> Vec<Span>>)` consumed per line; the `markdown_live_editor` screen uses `ftui-extras::syntax`. M.
- **G17.4 `TextInput` history**: ring buffer with Up/Down recall, `HistoryManager` reuse. S.
- **G17.5 `Sparkline` min/max markers**: glyph overrides for min and max samples with a style; snapshot. S.
- **G17.6 Border styles**: keep 5 (`Square, Ascii, Rounded, Double, Heavy`) and fix README, or add `Thick`, `Dashed`, `Dotted`, `Custom(BorderChars)` to reach the documented breadth. Decision: add `Custom` and `Dashed` (useful), README states the real count. S.
- **G17.7 `TableTheme` builders and per-column options**: `with_stripe_period(u8)`, `with_header_style`, `with_selection_style`, `with_column_truncation(col, Truncate::{Ellipsis, Clip, Wrap})`, `with_column_alignment`; `Table` honors them; snapshots in `table_theme_gallery`. M.
- **G17.8 `StyleSheet` consumers**: `Block::styled("heading")` and `Table::with_stylesheet(&sheet)` resolve names; test that a renamed style propagates. S.
- **G17.9 Convenience API**: `Frame::render_widget`, `Frame::render_stateful_widget`, `Frame::area()`; `pub type Layout = Flex` with `Layout::horizontal(constraints)`; README examples switch to them (or to the existing idioms; G06 decides CODE). S.

**Success criteria for the cluster:** each item has a unit test and a re-blessed snapshot; README widget table rows match; `scripts/widget_api_e2e.sh` extended with one scenario per item.
**Dependencies:** G06 decisions.
**Complexity:** M (cluster)
**Vision goals served:** V6, V39, V40, V41, C rows.
**Would open beads close it?** No.

#### G18: Gesture recognizer and hover stabilizer are unwired — DEAD -> WORKING

**Current state:** `crates/ftui-core/src/gesture.rs` (2,125 lines) has zero callers; defaults multi-click 300 ms, drag threshold 3 cells (`:66-69`); README says 500 ms and 2 cells. `hover_stabilizer.rs` (CUSUM) is used only by `mouse_playground`; `Table` hover is a plain compare (`table.rs:608-611`).
**Target state:** `Draggable`/`DropTarget` (`drag.rs`) and `TextArea` (double-click word, triple-click line) use `GestureRecognizer`; `Table` and `List` hover use `HoverStabilizer`; README states the real defaults.
**Success criteria:** unit tests for double/triple click selection in `TextArea`; a jitter test where one-cell mouse noise across a row boundary does not change Table hover; PTY E2E `mouse_playground` scenario logs recognized gestures as JSONL.
**Implementation plan:** wire in `drag.rs`, `textarea.rs`, `table.rs`, `list.rs`; expose `GestureConfig` on `ProgramConfig` so apps tune thresholds; README (G06).
**Dependencies:** none.
**Complexity:** M
**Vision goals served:** V15, V60.
**Would open beads close it?** No.

#### G19: Hint ranking is demo-only — DEAD -> WORKING

**Current state:** `hint_ranker.rs` (846 lines; Beta utility, VOI bonus, hysteresis 0.02) used only by `command_palette_lab.rs`; `Help`/`KeybindingHints` do not use it.
**Target state:** `Help::with_ranker(HintRanker)` orders hints by net value with hysteresis; usage feedback comes from the `KeyDispatcher` (G14) so shown hints learn from actual key use; `RankingEvidence::to_jsonl` goes to the evidence sink as `hint_ranking`.
**Success criteria:** no-flicker test (ranking stable under small utility noise), learning test (a used hint rises), evidence lines in the showcase E2E JSONL.
**Dependencies:** G14, G20.
**Complexity:** S
**Vision goals served:** README "Bayesian Hint Ranking".
**Would open beads close it?** No.

#### G20: Evidence and telemetry do not match the README; queue depth hardcoded — PARTIAL -> WORKING

**Current state:** Emitted events are `diff_decision`, `budget_decision`, `guardrail_snapshot`, `fairness_*`, `decision`/`decision_evidence`/`regime_transition`, `effect_queue_select`, `certificate_decision`, `task_executor_*`, `widget_refresh`; README names `resize_decision`, `conformal_gate`, `degradation_event`, `queue_select`, `voi_sample`. `voi_decision`/`voi_observe` have `to_jsonl` but Program never writes them (`program.rs:6301`). `telemetry_schema.rs` constants are referenced by nothing (literals match). `check_frame(memory_bytes, 0)` hardcodes queue depth (`program.rs:6117`) while `queue_telemetry().in_flight` is available.
**Target state:** README lists the real event names (DOC); `voi_sample` is emitted for inline-auto and for G10 signals (CODE); all tracing targets use `telemetry_schema` constants (mechanical edit across files, done by parallel subagents per AGENTS.md, not a script); `ftui.guardrails` and `ftui.a11y` added; queue depth fed from telemetry; a JSON schema for every event lives in `docs/spec/telemetry-events.md` and `tests/e2e/lib/e2e_jsonl_schema.json` validates showcase E2E output.
**Success criteria:** schema validation passes over a showcase run; unit test `guardrails_receive_live_queue_depth`; grep in CI (part of `check_readme_claims.py`) that no `"ftui."` string literal appears outside `telemetry_schema.rs`.
**Dependencies:** G10, G09 for new events.
**Complexity:** M
**Vision goals served:** V51, V54, V20; closes bd-1za0z item (3).
**Would open beads close it?** Partially (bd-1za0z item 3).

#### G21: SOS barrier provenance is false; two source files are orphaned — WRONG_APPROACH -> WORKING

**Current state:** `crates/ftui-runtime/src/sos_barrier_coeffs.rs:1-41` says "Auto-generated ... 2026-03-05" by `scripts/solve_sos_barrier.py`, which never existed; the constants are round hand-typed numbers; `sos_barrier.rs` is not used for admissibility. `timeline_aggregator.rs` (990) and `countmin_sketch.rs` (1,022) are not declared in `lib.rs`.
**Target state:** Either a real solver script exists and regenerates the coefficients reproducibly, or the header says the constants are hand-chosen and the module is experimental. The two orphans compile under `experimental`, and the aggregator backs the `action_timeline` screen.
**Success criteria:** header truthful; if the script route is chosen, `scripts/solve_sos_barrier.py` (cvxpy + SCS, spec in `sos_barrier_spec.toml`) regenerates a byte-identical file in CI; `action_timeline` snapshot shows aggregated counts from `TimelineAggregator`.
**Implementation plan:** decision by owner (script vs hand-chosen); this plan defaults to hand-chosen + experimental (G07) because nothing consumes the barrier; wire the aggregator into the demo under the feature.
**Dependencies:** G07.
**Complexity:** S (doc route) / M (script route)
**Vision goals served:** V19, V21.
**Would open beads close it?** No.

#### G22: doctor_frankentui is three products with gates that never run — WRONG_APPROACH -> WORKING (owner decision)

**Current state:** Section 5.4. Both workflows die at VHS install (G04.7). 192K lines, 47% tests, 7 of 128 files touch ftui.
**Target state (recommended):** the verification core (capture, seed-demo, suite, report, doctor, import, list-profiles) stays and its gates run nightly and per push; the TSX migration compiler, the alien-graveyard governance framework, and the nightly/stress machinery are moved to their own workspace members or repositories with their own CI, or feature-gated as `experimental` inside the crate so `cargo test -p doctor_frankentui` runs the core in minutes.
**Success criteria:**
- [ ] `doctor_frankentui Verification` job executes the happy, failure, determinism and coverage scripts and uploads the artifact map; Extended Verification green three nights.
- [ ] `cargo test -p doctor_frankentui` (core only) under 5 minutes locally via rch.
- [ ] README and AGENTS.md describe exactly what the binary does and which subcommands are experimental.
**Implementation plan:** (1) G04.7; (2) module map by product with line counts (Section 5.4 lists them); (3) owner decision; (4) execute the split or gating; (5) docs.
**Dependencies:** G04.7 first; owner decision.
**Complexity:** XL
**Vision goals served:** V68.
**Would open beads close it?** No.

#### G23: "Runs in a browser" cannot be reproduced from this repo — PARTIAL -> WORKING (owner decision on scope)

**Current state:** Section 5.2. `ftui-web` emits patches for an external host; no DOM/canvas code; DPR/zoom is a comment; the showcase HTML needs an out-of-tree bundle and an unbuilt `pkg/`; CI never builds either crate for wasm32.
**Target state:** Both crates build for wasm32 in CI (G04.13); a minimal in-tree JS host (`sdk/showcase-host.js`, no bundler) drives `ShowcaseRunner` and paints flat patches into a `<pre>` grid so `frankentui_showcase_demo.html` works from a `wasm-pack build` alone; DPR/zoom is implemented for that host (cell metrics from `getBoundingClientRect`) or the README claim is removed; README web sections say "host-driven patch producer" until a renderer exists.
**Success criteria:** a headless-browser CI step (playwright or `wasm-bindgen-test` in node) loads the page, advances 60 frames, and asserts the Dashboard title text is present in the grid; `docs/spec/wasm-showcase-runner-contract.md` matches the exports (already true).
**Dependencies:** G04.13; owner decision on how far to go.
**Complexity:** L
**Vision goals served:** V8.
**Would open beads close it?** No.

#### G24: Asupersync lane and Shadow policy are labels — PARTIAL -> WORKING

**Current state:** `RuntimeLane::resolve()` maps Asupersync to Structured unconditionally (`program.rs:2734-2742`); the `asupersync-executor` feature builds a real pool (`:3579-3690`) reachable only via `EffectQueueConfig::with_backend`; `RolloutPolicy::Shadow` logs at startup (`:4909`); shadow comparison lives in the harness.
**Target state:** With the feature on, selecting the Asupersync lane resolves to the Asupersync executor; without it, resolution logs a warning and falls back (documented). `RolloutPolicy::Shadow` in `Program` records per-frame checksums and lane metadata into the evidence sink so `ftui-harness` `ShadowRun` can compare two recorded runs; README describes shadow-run as a harness workflow. The README also presents the queueing scheduler (SRPT, Smith's rule, aging; `queueing_scheduler.rs`, 2,891 lines) as the effect scheduler, but it runs only under the opt-in `EffectQueue` backend while the default lanes spawn a thread per task (`program.rs:2785-2791`): this block also decides the default backend by benchmark (`runtime_effect_queue_drain` baseline row plus a burst-of-200-tasks latency bench) and either makes `EffectQueue` the default or documents the scheduler as opt-in (V48).
**Success criteria:** unit test `asupersync_lane_resolves_to_asupersync_backend_when_feature_enabled` (feature-gated) and its negative; `rollout_drills.rs` E2E compares two evidence files and yields `ShadowVerdict::Match`; the backend decision is recorded with the bench numbers in `docs/perf/effect_backend_<date>.md` and reflected in README "Queueing-Theoretic Scheduler".
**Dependencies:** none.
**Complexity:** M
**Vision goals served:** V52.
**Would open beads close it?** No.

#### G25: Performance budgets are unenforced and README numbers are unbacked — UNPROVEN -> WORKING

**Current state:** `scripts/perf_regression_gate.sh` consumes `tests/baseline.json` but no workflow runs it; `benchmarks` job runs `bench_budget.sh --quick` on main pushes with 1.5x envelopes; `runtime_first_frame`, `runtime_shutdown_latency`, `runtime_command_roundtrip` are skipped as `non_criterion_baseline`; no budgets for present at 120x40/200x60, input parse+dispatch, bytes emitted, wrap, allocations; README quotes 100x50 numbers no bench produces.
**Target state:** A `perf-gate` job runs the gate on main pushes and nightly with `--json` artifacts; baseline gains the plan's budgets with criterion names; README numbers are regenerated from the artifact by a script.
**Success criteria:**
- [ ] `tests/baseline.json` rows: `present_80x24_sparse` (p50 < 1 ms, p99 < 3 ms), `present_120x40_sparse` (p50 < 2 ms, p99 < 6 ms), `present_200x60_sparse` (p50 < 6 ms, p99 < 18 ms), `input_parse_dispatch_event` (< 100 µs), `bytes_emitted_sparse_5pct` (O(changes): bytes < 8 x changed cells + 64), `wrap_200_lines` (< 2 ms), `frame_allocations_ascii_scene` (0 allocations in the ASCII path), `text_width_non_ascii` (G08).
- [ ] New benches: `crates/ftui-core/benches/input_parser_bench.rs`, `crates/ftui-text/benches/wrap_bench.rs`, presenter sizes exist; `CountingWriter` used for bytes; feature `alloc-count` with a counting `#[global_allocator]` in benches.
- [ ] `FrameArena` (V29) carries the per-frame allocations that the allocation bench exposes: wrapped-line span vectors in `ftui-text` wrap, solved `Rect` lists in `Flex::split`, and `ChangeRun` vectors in the diff take their storage from `frame.arena` when present (the arena is already plumbed through `Frame` and reset by `Program`; only `TextInput` and the dashboard use it today). Acceptance is the `frame_allocations_ascii_scene` row reaching zero and a non-ASCII scene dropping by at least half.
- [ ] `scripts/render_perf_readme.py` writes `docs/perf/baseline_<date>.md` and the README "Benchmark Suite" block from the gate JSON; `check_readme_claims.py` verifies the block hash.
**Dependencies:** G04.12 (job topology).
**Complexity:** L
**Vision goals served:** V35, F; plan-doc 0.12.
**Would open beads close it?** No.

#### G26: The plan's primary target has no consumer — PARTIAL -> WORKING

**Current state:** `ftui-harness` is a test harness, not an app; `docs/tutorials/agent-harness.md` describes a Claude/Codex-style session; no in-tree app streams a child process under stable chrome.
**Target state:** `crates/ftui/examples/agent_shell.rs`: spawns a command (`ProcessSubscription`), streams its stdout/stderr into scrollback via `write_log` with sanitization, keeps a status line and a `TextInput` in the inline chrome, supports links, resize, Ctrl-C forwarding, and crash-safe teardown; the tutorial targets it; it is the flagship inline demo in the README.
**Success criteria:** `scripts/e2e_test.sh` scenario `agent_shell_log_spam`: 10,000 log lines at full speed while the chrome stays stable; assertions on scrollback integrity (canonicalized transcript contains all lines in order), zero `2J`/`1049h` in inline mode, and teardown sequence; JSONL log of frame counts and bytes.
**Dependencies:** G01, G27 (sanitization modes).
**Complexity:** M
**Vision goals served:** plan-doc 0.1 primary target; V1.
**Would open beads close it?** No.

#### G27: Untrusted-output policy is half built — PARTIAL -> WORKING

**Current state:** `write_log` and `LogSink` sanitize by default (`terminal_writer.rs:2157`, `log_sink.rs:54`); no `write_raw`/SGR-only mode (ADR-006); adversarial tests are unit-level; no named "inline never clears the screen" test.
**Target state:** `TerminalWriter::write_log_raw` (explicit opt-in) and `write_log_sgr_only`; `LogSink::raw()`; adversarial PTY tests; the invariant test.
**Success criteria:** `crates/ftui-harness/tests/pty_injection_adversarial.rs` feeds ESC/CSI/OSC/DCS/APC/C1 payloads and asserts the terminal model is unchanged and no full-clear sequences appear; `inline_never_clears_screen` proptest over harness scenarios asserts no `\x1b[2J`, `\x1b[3J`, `\x1b[?1049h` in inline mode; ADR-006 status Accepted.
**Dependencies:** none.
**Complexity:** M
**Vision goals served:** plan-doc ADR-006, kernel invariant "inline never clears".
**Would open beads close it?** No.

#### G28: Built-but-unqueried structures — PARTIAL -> WORKING

**Current state:** `diff.rs` computes a summed-area table (`:789-805`, `:1010-1026`) that only tests read; tile skipping uses a boolean grid and engages at 12,000+ cells (`:483`). `caps_probe.rs` builds a log-BF ledger only for the demo (`:1153`); production `probe_capabilities_unix` sets booleans.
**Target state:** SAT either drives a two-level (tile-row then tile) skip that wins on 200x60 sparse frames by at least 10% in the diff bench, or it is deleted (permission) and the README sentence goes; the capability ledger is the production combiner for env + probe evidence and emits `capability_decision` (feeds G05).
**Success criteria:** bench `diff_200x60_sparse` before/after; unit tests for ledger combination with conflicting env and probe evidence.
**Dependencies:** G05 uses the ledger.
**Complexity:** M
**Vision goals served:** V64, README "Summed-Area Table", "Bayesian Capability Detection".
**Would open beads close it?** No.

#### G29: i18n overclaims — PARTIAL -> WORKING (scoped)

**Current state:** `ftui-i18n` is a string catalog plus plural rules (1,160 lines); no number/date formatting; no bidi integration; demo languages en/es/fr/ru/ar/ja.
**Target state:** README claims reduced to what exists (DOC) plus two CODE items: `LocaleContext::direction()` drives `Paragraph` alignment and cursor movement through `ftui-text` bidi when the `bidi` feature is on; the demo adds German. Number/date formatting is retracted (a full ICU dependency is out of scope; recorded as a decision).
**Success criteria:** RTL snapshot for the i18n screen in Arabic; PTY test `rtl_locale_not_selected` (G04.9) green; German strings present.
**Dependencies:** none.
**Complexity:** M
**Vision goals served:** V47.
**Would open beads close it?** No.

#### G30: Runtime API names in README vs code (persistence, macro player, simulator checksum, SLO) — WRONG_API -> WORKING

Resolved as DOC rows in G06 plus quarantine of `slo` in G07; no separate code work. Listed here so the vision checklist rows V25-V28 have an owner. S.

#### G31: Windows is "validated" on paper — PARTIAL -> WORKING (scoped)

**Current state:** `docs/WINDOWS.md` says validated 2026-02-03; every Windows CI job since is red (G04.3); native backend deferred; `run_native` errors on Windows; crossterm path needs the feature (G01).
**Target state:** Windows builds and runs the README example over crossterm by default (G01 + G04.3), the PTY-less smoke (ConPTY via `script`-equivalent is not available; use the headless simulator plus a `cargo run` start/stop check) runs on `windows-latest`, and `docs/WINDOWS.md` states the real matrix with run ids. ADR-004 accepted with the "crossterm-only on Windows" decision.
**Complexity:** M (after G01/G04.3)
**Vision goals served:** V67.

### 7.3 Minor gaps (polish and completeness)

#### G32: SIGTSTP/SIGCONT leaves the shell in raw mode — NOT_STARTED -> WORKING
Covered by open bead **bd-d4dtr** (design needed: restore cooked state on TSTP, re-raise with default disposition, re-arm on CONT, force full repaint). Add a PTY test that sends `SIGTSTP` then `SIGCONT` and asserts the mode transitions. S-M. **Would open beads close it?** Yes.

#### G33: `ftui-simd` is an empty published crate; `ftui-demo-showcase` 0.1.1 lingers on crates.io — WRONG_APPROACH -> WORKING
Owner decision: give `ftui-simd` real safe SIMD paths (portable_simd is nightly; the workspace is nightly) for `bits_eq` row compare and ASCII width with benches, or unpublish/yank and remove it from the workspace (permission). Yank `ftui-demo-showcase` 0.1.1 or publish a README-only 0.6.0 marked deprecated. S (decision) / L (implement).

#### G34: Stale governance docs — WRONG -> WORKING
`docs/risk-register.md` summary vs rows; `docs/main-todo-bead-map.md` regenerated from beads by `scripts/pane_test_summary_aggregate.py`-style script or deleted (permission); ADR-004/005/006/008/010 accepted or superseded; `docs/reports/deep-codebase-review-final.md` gets a superseded banner pointing here. S.

#### G35: Harness and showcase usage docs — WRONG_API -> WORKING
`ftui-harness/examples/minimal.rs` becomes a hello world; README Configuration section lists `FTUI_HARNESS_*` for the harness and `--screen`/`FTUI_DEMO_SCREEN`/`FTUI_DEMO_SCREEN_MODE` for the showcase; Troubleshooting mouse line corrected. S.

#### G36: Input parser gaps — PARTIAL -> WORKING
SGR-pixels (1016) parsing and DCS/APC payload capture (currently consumed/discarded), or README retracts. Fuzz targets already cover the parser; extend with 1016 sequences. M.

#### G37: Process guardrails — NOT_STARTED -> WORKING
`br` pre-close check (`scripts/br_close_guard.sh`): refuse closing a bead whose reason lacks a test name, CI run id, or PR/commit; AGENTS.md "Landing the Plane" requires it; monthly reality-check job re-runs Section 1 commands and diffs the claims ledger. S.

#### G38: Plan-doc leftovers — NOT_STARTED -> DECIDED
SSH extra: drop from the plan (documented); formal TLA+ specs: keep `docs/spec/state-machines.md` as "formal-ish" and say so; execution tracker regenerated (G34). S.

#### G39: AGENTS.md `tests/` claim and fuzz cadence — WRONG -> WORKING
AGENTS.md says cross-component tests live in `tests/`; they live in per-crate `tests/`. Fix the text; add the nightly fuzz job (G04.5). S.

#### G40: `verify_no_regression` order dependence — WRONG_APPROACH -> WORKING
One test captures and verifies in-process; the gitignored file becomes an optional cache with provenance; a stale file is ignored with a logged reason. S.

#### G41: Release and version hygiene — PARTIAL -> WORKING
Release checklist file `docs/release-checklist.md`: README version string, crates.io versions, claims ledger green, compat matrix green, consumer smoke green, CHANGELOG entry; `release.yml` idempotent (G04.11). S.

#### G42: Final integration verification — NOT_STARTED -> WORKING
One closing block that depends on every other block: run every row of Section 7.6 on a clean clone, archive the outputs under `docs/reports/reality-check-verification-<date>/` (JSONL logs, compat matrix, perf artifact, claims-ledger report, three green CI run ids), and record the vision-delivery percentage against Section 3 in a short table at the top of this document. This is the bead that closes the reality-gap epic; it may not close while any Section 3 row is still PARTIAL, DEAD, WRONG_API or NOT_STARTED without a documented owner decision. M.

### 7.4 Would existing open beads close the gaps?

| Bead | Covers | Verdict |
|---|---|---|
| bd-d4dtr (P3) | G32 | Yes, fully |
| bd-1za0z (P3) | telemetry defects in G12; queue depth in G20 | Partially |
| everything else (G01-G31, G33-G41) | nothing | No bead exists |

### 7.5 Dependency graph

```mermaid
flowchart TD
  G03[G03 signal race + nextest] --> G04[G04 CI to green]
  G04_3[G04.3 ftui-tty on Windows] --> G01[G01 default backend]
  G01 --> G02[G02 README doc-tests]
  G07[G07 quarantine + reachability gate] --> G06[G06 claims ledger + README rewrite]
  G02 --> G06
  G25[G25 perf gates] --> G06
  G01 --> G26[G26 agent shell app]
  G27[G27 write_raw + adversarial] --> G26
  G28[G28 caps ledger + SAT] --> G05[G05 probing + compat matrix]
  G07 --> G13[G13 one controller each]
  G03 --> G13
  G01 --> G13
  G20[G20 evidence/telemetry] --> G10[G10 VirtualizedList]
  G20 --> G09[G09 a11y tree]
  G20 --> G19[G19 hint ranker]
  G14[G14 keybindings] --> G19
  G14 --> G04_9[G04.9 PTY E2E failures]
  G10 --> G04_9
  G29[G29 i18n] --> G04_9
  G04_7[G04.7 VHS] --> G22[G22 doctor scope]
  G04_13[G04.13 wasm32 builds] --> G23[G23 web host]
  G06 --> G34[G34 governance docs]
  G04_12[G04.12 job topology] --> G25
  G04 --> G42[G42 final verification]
  G06 --> G42
  G05 --> G42
  G13 --> G42
  G22 --> G42
  G23 --> G42
```

Parallel tracks that can start on day one with no dependencies: G03, G04.1-G04.7/G04.10/G04.11, G05 (probe), G07, G08, G11, G12, G14, G15, G16, G17, G18, G21, G24, G27, G28, G36, G37, G40.

### 7.6 Verification plan (after all bridge work)

| Vision goal | How to verify |
|---|---|
| V1 inline mode, V31 sync output, V49 strategies | `scripts/pty_identity_matrix.py` matrix green; `agent_shell_log_spam` E2E; `inline_never_clears_screen` test |
| V5 composable crates, A onboarding | `scripts/consumer_smoke_e2e.sh` on Linux, macOS, Windows |
| V6 widgets, V39-V45 APIs | README doc-tests; `widget_api_e2e.sh` scenarios; claims ledger check |
| V8 web | headless-browser CI step (G23) |
| V10-V14, V36, V37 Bayesian layer | evidence JSONL schema validation over a showcase run showing `decision_evidence` (bocpd), `budget_decision`, `voi_sample`, `guardrail_snapshot`, `capability_decision`, `hint_ranking`, `a11y_announcement` |
| V20, V21 caches | `text_width_non_ascii` baseline row; reachability gate |
| V33 unsafe | existing forbid check plus `scripts/check_readme_claims.py` |
| V35 perf | `perf-gate` job artifact and `docs/perf/baseline_<date>.md` |
| V46 a11y | `dashboard_a11y_tree_80x24.snap`; `a11y_transitions_e2e.sh` |
| V52 lanes | feature-gated lane resolution tests; `rollout_drills.rs` |
| V62 keybindings | `test_keybinding_chords.sh` |
| V65 showcase counts | `all_screens_count` test and ledger row |
| V67 Windows | Windows CI job green; `docs/WINDOWS.md` run ids |
| V68 doctor | both doctor workflows green three runs |
| V70 gates | three consecutive green `ci.yml` runs; nextest timeouts |
| C every row | claims ledger 100% with proofs; README doc-tests |
| F plan-doc DoD | `write_raw` tests, agent shell E2E, perf rows, ADR statuses |

### 7.7 Suggested bead structure for Phase 3a

One epic per gap block (41 epics), children: implementation task(s), companion test task, and where applicable an E2E-script task and a docs/ledger task; dependencies per Section 7.5; priorities: G01-G07 P0/P1, G08-G31 P1/P2, G32-G41 P2/P3. Every bead's description carries the block's current state, target state, success criteria and implementation steps verbatim so the markdown plan is never needed again.

---

## 8. Immediate next step

Phase 3a: convert Section 7 into beads with the frozen template after the owner steers on the four owner decisions (G22 doctor scope, G23 web scope, G33 `ftui-simd`, and the deletion list in G07/G13) and on the CODE/DOC decision table in G06. Highest-leverage first five: G01, G03, G02, G05, G04.
