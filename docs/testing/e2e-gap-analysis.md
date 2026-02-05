# E2E Test Suite: Inventory and Gap Analysis

Bead: bd-2nu8.3

## Current Inventory (2026-02-04)

### Harness PTY E2E Suites (tests/e2e/scripts, 46 suites + run_all)

Default mode for `ftui-harness` is inline unless `FTUI_HARNESS_SCREEN_MODE=alt` is set.
Default size is 80x24 unless `PTY_COLS/PTY_ROWS` override.

Matrix cells used below:
| Cell | Mode | Size |
|------|------|------|
| A1 | alt | 80x24 |
| A2 | alt | 120x40 |
| A3 | alt | 200x50 |
| I1 | inline | 80x24 |
| I2 | inline | 120x40 |
| I3 | inline | 200x50 |

Legend: `I2*` = 120x24 width-only, `I3*` = 200x60 near-size, `+small` = additional non-matrix sizes.

| Script | Focus | Mode | Sizes | Cells |
|--------|-------|------|-------|-------|
| `test_a11y_modes.sh` | A11y mode toggles | inline (default) | 80x24, 120x40 | I1, I2 |
| `test_action_timeline.sh` | Action timeline UI | inline (default) | 120x40 | I2 |
| `test_altscreen.sh` | Alt-screen enter/exit | alt | 80x24 (default) | A1 |
| `test_ansi.sh` | ANSI/SGR/CUP/sync | inline (default) | 80x24 (default) | I1 |
| `test_async_tasks.sh` | Async tasks UI | inline (default) | 120x40 | I2 |
| `test_cleanup.sh` | Cleanup sequences | alt | 80x24 (default) | A1 |
| `test_command_palette.sh` | Command palette UI | inline (default) | 120x40 | I2 |
| `test_drag_drop.sh` | Drag/drop | inline (default) | 80x24 (default) | I1 |
| `test_embedded_terminal.sh` | Embedded terminal | inline (default) | 80x24, 80x10, 40x10 | I1 +small |
| `test_flake_detector.sh` | Flake detector | inline (default) | 80x24 (default) | I1 |
| `test_focus_events.sh` | Focus events | inline (default) | 80x24 (default) | I1 |
| `test_form_validation.sh` | Form validation | inline (default) | 80x24 (default) | I1 |
| `test_golden_resize.sh` | Golden resize snapshots | inline (default) | 80x24 (default) | I1 |
| `test_inline.sh` | Inline mode core | inline (default) | 80x24 (default), 60x15 | I1 +small |
| `test_input.sh` | Input events | inline (default) | 80x24 (default) | I1 |
| `test_intrinsic_sizing.sh` | Intrinsic sizing | inline (default) | 80x24 (default) | I1 |
| `test_keybinding.sh` | Keybinding handling | inline (default) | 80x24 (default) | I1 |
| `test_kitty_keyboard.sh` | Kitty keyboard | inline (default) | 80x24 (default) | I1 |
| `test_layout_composer_resize.sh` | Layout composer resize | inline (default) | 80x24, 120x40 | I1, I2 |
| `test_locale_context.sh` | Locale context | inline (default) | 80x24 (default) | I1 |
| `test_log_search.sh` | Log search | inline (default) | 120x40 | I2 |
| `test_macro_scenarios.sh` | Macro scenarios | inline (default) | 80x24 (default) | I1 |
| `test_migration_smoke.sh` | Migration smoke | inline | 80x24, 40x10, 200x60 | I1, I3* +small |
| `test_modal_dialog.sh` | Modal dialog | inline (default) | 80x24 (default) | I1 |
| `test_mouse_sgr.sh` | Mouse SGR input | inline (default) | 80x24 (default) | I1 |
| `test_mux.sh` | Mux env (tmux/zellij) | inline | 80x24 | I1 |
| `test_mux_compatibility.sh` | Mux compat (alt+inline) | alt + inline | 80x24 | A1, I1 |
| `test_osc8.sh` | OSC 8 sequences | inline (default) | 80x24, 90x24 | I1 |
| `test_osc8_hyperlinks.sh` | Hyperlink rendering | inline (default) | 80x24 (default) | I1 |
| `test_paste.sh` | Bracketed paste | inline (default) | 80x24 (default) | I1 |
| `test_reflow_telemetry.sh` | Reflow telemetry | inline (default) | 80x24 (default) | I1 |
| `test_resize.sh` | Resize matrix | inline | 60x15, 80x24, 80x40, 120x24, 40x10, 200x60 | I1, I2*, I3* +small |
| `test_resize_scroll_region.sh` | Scroll region | inline | 80x24 | I1 |
| `test_resize_storm.sh` | Resize storm | inline (default) | 80x24 (default) | I1 |
| `test_schedule_trace.sh` | Schedule trace | inline (default) | 80x24 (default) | I1 |
| `test_snapshot_player.sh` | Snapshot player | inline (default) | 80x24, 120x40 | I1, I2 |
| `test_sync_output.sh` | Sync output (DEC 2026) | alt | 80x24 | A1 |
| `test_telemetry.sh` | Telemetry (inline) | inline | 120x40 | I2 |
| `test_terminal_capabilities.sh` | Terminal capabilities | inline (default) | 80x24, 120x40 | I1, I2 |
| `test_terminal_quirks.sh` | Terminal quirks | inline (default) | 80x24 (default) | I1 |
| `test_text_editor.sh` | Text editor | inline (default) | 80x24 (default) | I1 |
| `test_theme_studio.sh` | Theme studio | inline (default) | 120x40 | I2 |
| `test_toast_system.sh` | Toast system | inline (default) | 80x24 (default) | I1 |
| `test_ui_inspector.sh` | UI inspector | inline (default) | 80x24 (default) | I1 |
| `test_unicode.sh` | Unicode rendering | inline (default) | 80x24 (default) | I1 |
| `test_virtualized_search.sh` | Virtualized search | inline | 120x40 | I2 |

### Demo/Script E2E Suites (scripts/* and scripts/e2e/*)

Legend: Cells refer to the matrix above. Non-PTY scripts are marked `N/A`.

| Script | Focus | Mode | Sizes | Cells |
|--------|-------|------|-------|-------|
| `scripts/a11y_transitions_e2e.sh` | A11y transitions + JSONL | non-PTY | N/A | N/A |
| `scripts/command_palette_e2e.sh` | Command palette E2E | non-PTY | N/A | N/A |
| `scripts/cross_platform_e2e.sh` | Cross-platform smoke | non-PTY | N/A | N/A |
| `scripts/demo_showcase_e2e.sh` | Demo showcase full run + core nav/editor/markdown/data/VFX/forms/inline hashes | alt (default) + inline step | 80x24, 120x40, 40x10, 132x43, 200x24 | A1, A2, I1 +small |
| `scripts/demo_text_effects_e2e.sh` | Text effects E2E | non-PTY | N/A | N/A |
| `scripts/e2e_demo_tour.sh` | Guided tour JSONL | alt | 80x24 (default) | A1 |
| `scripts/e2e_test.sh` | PTY suite wrapper | wrapper | N/A | N/A |
| `scripts/hover_stabilizer_e2e.sh` | Hover stabilizer | non-PTY | N/A | N/A |
| `scripts/perf_hud_demo.sh` | Performance HUD demo | interactive | 120x40 (optional) | I2 (manual) |
| `scripts/widget_api_e2e.sh` | Widget API E2E | non-PTY | N/A | N/A |
| `scripts/e2e/run_all.sh` | Scripted E2E aggregator | wrapper | N/A | N/A |
| `scripts/e2e/capability_sim/run_capability_sim_e2e.sh` | Capability simulator | non-PTY | N/A | N/A |
| `scripts/e2e/focus_management/run_focus_e2e.sh` | Focus management suite | non-PTY | N/A | N/A |
| `scripts/e2e/focus_management/test_focus_nav.sh` | Focus nav | non-PTY | N/A | N/A |
| `scripts/e2e/focus_management/test_focus_trap.sh` | Focus trap | non-PTY | N/A | N/A |
| `scripts/e2e/focus_management/test_focus_persist.sh` | Focus persistence | non-PTY | N/A | N/A |
| `scripts/e2e/focus_management/test_focus_a11y.sh` | Focus a11y | non-PTY | N/A | N/A |
| `scripts/e2e/modal_dialog/run_modal_dialog_e2e.sh` | Modal dialog suite | non-PTY | N/A | N/A |
| `scripts/e2e/state_persistence/test_state_persistence.sh` | State persistence | non-PTY | N/A | N/A |
| `scripts/e2e/i18n/run_i18n_e2e.sh` | i18n suite | non-PTY | N/A | N/A |

### Supporting Infrastructure

| Component | Path | Purpose |
|-----------|------|---------|
| Orchestrator | `tests/e2e/scripts/run_all.sh` | Builds harness, runs suites, aggregates results |
| PTY capture | `tests/e2e/lib/pty.sh` | Python-based PTY spawn with output capture + JSONL metadata |
| Logging | `tests/e2e/lib/logging.sh` | Result JSON recording, summary, JUnit XML |
| Common | `tests/e2e/lib/common.sh` | Python resolver, command validation |
| Test lib | `scripts/e2e/lib/test_lib.sh` | Scripted E2E helper library |
| Fixture | `tests/e2e/fixtures/unicode_lines.txt` | Unicode test corpus |
| Widget API | `scripts/widget_api_e2e.sh` | Build, test, clippy, features, signatures, docs, snapshots |

### Artifact Checklist (bd-1av4o.14.9)

Every E2E case should emit JSONL `artifact` events that point to the
artifacts it produces. The minimal checklist:

| Artifact Type | Required For | Notes |
|--------------|--------------|-------|
| `log_dir` | all suites | Root log directory for the run |
| `e2e_jsonl` | all suites | The JSONL event log (`e2e.jsonl`) |
| `pty_output` | PTY cases | Per‑case `.pty` capture |
| `summary_json` | suite-level | `results/summary.json` |
| `snapshot` | snapshot suites | Snapshot output paths (if generated) |
| `hash_registry` | hash cases | Determinism/hash registry outputs |

When scripts emit `jsonl_assert` entries with names starting `artifact_*`,
`tests/e2e/lib/logging.sh` now records `artifact` events automatically and
fails in CI strict mode if a required artifact is missing.

### Snapshot Tests (ftui-harness)

Block, Paragraph, List, Scrollbar, Panel, Columns, Raw buffer snapshots (see `crates/ftui-harness` tests).

### Harness Views Available

Default, LayoutFlexRow, LayoutFlexCol, LayoutGrid, LayoutNested, WidgetBlock, WidgetParagraph, WidgetTable, WidgetList, WidgetInput.

---

## Deterministic Coverage Matrix (v1)

Target matrix for deterministic E2E coverage:
| Cell | Mode | Size | Goal |
|------|------|------|------|
| A1 | alt | 80x24 | Baseline alt-screen correctness |
| A2 | alt | 120x40 | Medium alt-screen regression guard |
| A3 | alt | 200x50 | Large alt-screen regression guard |
| I1 | inline | 80x24 | Baseline inline scrollback correctness |
| I2 | inline | 120x40 | Medium inline regression guard |
| I3 | inline | 200x50 | Large inline regression guard |

### Matrix Coverage Summary (by cell)

| Cell | Current Coverage | Gaps |
|------|------------------|------|
| A1 | `test_altscreen.sh`, `test_cleanup.sh`, `test_sync_output.sh`, `test_mux_compatibility.sh` (alt), `scripts/demo_showcase_e2e.sh` (default alt), `scripts/e2e_demo_tour.sh` | Missing doom/quake, perf HUD, text effects, terminal caps at A1 |
| A2 | `scripts/demo_showcase_e2e.sh` (VFX 120x40) | No cleanup/input/resize/unicode at 120x40 alt |
| A3 | None | Entire cell missing (need 200x50 alt runs) |
| I1 | Broad harness coverage (`test_inline.sh`, `test_ansi.sh`, `test_input.sh`, `test_unicode.sh`, `test_mouse_sgr.sh`, `test_paste.sh`, `test_focus_events.sh`, `test_resize.sh`, `test_terminal_capabilities.sh`) | Missing guided tour, doom/quake, perf HUD, text effects in inline |
| I2 | `test_action_timeline.sh`, `test_command_palette.sh`, `test_terminal_capabilities.sh`, `test_theme_studio.sh`, `test_snapshot_player.sh`, `test_virtualized_search.sh`, `test_telemetry.sh` | No inline story, doom/quake, perf HUD, guided tour at 120x40 |
| I3 | `test_resize.sh` (200x60), `test_migration_smoke.sh` (200x60) | Missing true 200x50 coverage and most scenarios |

### Missing Flows (bd-2txo scope)

| Flow | Current Coverage | Missing Matrix Cells |
|------|------------------|----------------------|
| Guided tour | `scripts/e2e_demo_tour.sh` (alt default) | A2, A3, I1, I2, I3 |
| Doom/Quake | `scripts/demo_showcase_e2e.sh` VFX sweep (A1) | A2, A3, I1, I2, I3 |
| Dashboard | `scripts/demo_showcase_e2e.sh` screen navigation + core nav hashes (A1) | A2, A3, I2, I3 |
| Markdown live editor | Not registered in demo screen list (module exists only) | A1, A2, A3, I1, I2, I3 |
| Performance HUD | `scripts/demo_showcase_e2e.sh` core nav hash (A1), `scripts/perf_hud_demo.sh` (manual) | A2, A3, I1, I2, I3 |
| Virtualized search | `scripts/demo_showcase_e2e.sh` forms sweep (A1) | A2, A3, I1, I2, I3 |
| Inline mode story | `scripts/demo_showcase_e2e.sh` inline story sweep (I1) | A1, A2, A3, I2, I3 |
| Terminal capabilities | `test_terminal_capabilities.sh` (I1, I2) | A1, A2, A3, I3 |
| Text effects | `scripts/demo_text_effects_e2e.sh` (non-PTY) | A1, A2, A3, I1, I2, I3 |
| Inline mode story | `test_inline.sh` (I1) | I2, I3 |

## Gap Analysis

### Gap 1: Mouse Event Handling

**Current state:** Only mouse _cleanup_ is tested (disable sequence in `test_cleanup.sh`). No test exercises actual mouse event processing (clicks, drags, scroll wheel).

**Why it matters:** The harness processes `Msg::Mouse(MouseEvent)` and the HitGrid maps coordinates to widget IDs. Mouse interaction is a core feature with no E2E validation.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `mouse_click_stable` | `test_mouse.sh` | Send SGR mouse click sequence; harness renders without crash; output > 200 bytes |
| `mouse_scroll_wheel` | `test_mouse.sh` | Send wheel-up/down sequences; verify harness stays alive and renders status bar |
| `mouse_drag_stable` | `test_mouse.sh` | Send drag sequence (button down + motion + button up); no crash, output valid |

**PTY input format:** SGR mouse encoding (`\x1b[<0;10;5M` for press, `\x1b[<0;10;5m` for release).

---

### Gap 2: Paste Event Content

**Current state:** Bracketed paste _cleanup_ (disable `\x1b[?2004l`) tested. Actual paste content delivery via `\x1b[200~...\x1b[201~` is never exercised.

**Why it matters:** The harness has a `Msg::Paste(PasteEvent)` handler. Paste is a common user interaction, and malformed paste sequences could corrupt state.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `paste_basic` | `test_paste.sh` | Send bracketed paste with ASCII content; verify harness doesn't crash |
| `paste_multiline` | `test_paste.sh` | Send multi-line paste; verify output renders normally after paste |
| `paste_unicode` | `test_paste.sh` | Send paste with CJK/emoji content; verify no render corruption |

**PTY input format:** `\x1b[200~pasted text\x1b[201~`

---

### Gap 3: Focus Event Processing

**Current state:** Focus _cleanup_ (`\x1b[?1004l`) tested. Actual focus-in (`\x1b[I`) and focus-out (`\x1b[O`) event processing is never exercised.

**Why it matters:** The harness has `Msg::Focus(bool)`. Focus events affect UI state (e.g., dimming on focus loss). No validation that focus events are correctly decoded and processed.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `focus_in_out` | `test_focus.sh` | Send `\x1b[I` then `\x1b[O`; verify harness doesn't crash; output valid |
| `focus_rapid_toggle` | `test_focus.sh` | Send alternating focus-in/out; verify render stability |

**PTY input format:** `\x1b[I` (focus in), `\x1b[O` (focus out).

---

### Gap 4: Hyperlink (OSC 8) Rendering

**Current state:** Not tested at all. The Presenter has link tracking and OSC 8 open/close support. The coverage matrix explicitly lists "Link tracking correctness (OSC 8 open/close)" as a target.

**Why it matters:** Hyperlinks are increasingly used in CLI tools. The Presenter's link state machine and the `link_registry.rs` module have zero E2E coverage.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `hyperlink_render` | `test_hyperlink.sh` | Inject log line containing hyperlink markup; verify output contains `\x1b]8;;` (OSC 8 opener) or at least that render completes without crash |

**Dependency:** Requires harness support for rendering hyperlinks in log content. May need a fixture or harness enhancement.

---

### Gap 5: Kitty Keyboard Protocol

**Current state:** Not tested at all. `TerminalSession` supports kitty keyboard enable/disable, and `CleanupExpectations` tracks it, but no E2E test exercises it.

**Why it matters:** Kitty keyboard protocol provides disambiguated key events. If enabled, cleanup must disable it; if not enabled, the disable sequence must not be emitted.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `kitty_keyboard_cleanup` | `test_kitty.sh` | Enable kitty keyboard via harness config; verify `\x1b[>4;0u` (disable) appears in cleanup |
| `kitty_key_event` | `test_kitty.sh` | Send kitty-encoded key event; verify harness processes it without crash |

**Dependency:** Requires harness config option to enable kitty keyboard protocol.

---

### Gap 6: Scroll Region Management

**Current state:** Not tested. The `TerminalWriter` supports scroll region optimization for inline mode. No test validates scroll region sequences (`\x1b[<top>;<bottom>r`).

**Why it matters:** Scroll regions are a performance optimization for inline rendering. Incorrect scroll region handling causes visual corruption.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `scroll_region_inline` | `test_scroll_region.sh` | Run in inline mode with log streaming; check for DECSTBM sequences (`\x1b[\d+;\d+r`) or verify content doesn't bleed into UI region |

---

### Gap 7: Terminal Multiplexer (tmux/zellij) Passthrough

**Current state:** Not tested. `TerminalCapabilities` detects mux environments and `mux_passthrough.rs` handles passthrough sequences. No E2E test validates behavior under mux.

**Why it matters:** Many users run inside tmux/zellij. Incorrect mux detection or passthrough can cause visual artifacts.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `mux_tmux_env` | `test_mux.sh` | Set `TMUX=/tmp/tmux-1000/default,12345,0` env var; verify harness starts and renders without crash |
| `mux_zellij_env` | `test_mux.sh` | Set `ZELLIJ=0` env var; verify harness starts and renders without crash |

**Notes:** Does not require actual tmux/zellij; tests env-based detection path.

---

### Gap 8: Performance Degradation Levels

**Current state:** Not tested in E2E. The budget system (Full, SimpleBorders, NoStyling, EssentialOnly, Skeleton) affects render output. No test validates degraded rendering.

**Why it matters:** Under frame pressure, rendering degrades. Users should still see usable output. Degradation bugs could hide behind normal-speed testing.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `degrade_simple_borders` | `test_degradation.sh` | Configure budget=SimpleBorders (if harness supports it); verify ASCII borders appear instead of box-drawing |
| `degrade_essential_only` | `test_degradation.sh` | Configure budget=EssentialOnly; verify essential widgets still render |

**Dependency:** Requires harness config to force a specific degradation level (e.g., `FTUI_HARNESS_BUDGET` env var). Likely needs harness enhancement.

---

### Gap 9: Multiple Harness Views

**Current state:** All E2E tests use the Default view. The harness supports 10 views (LayoutFlexRow, LayoutFlexCol, LayoutGrid, LayoutNested, WidgetBlock, WidgetParagraph, WidgetTable, WidgetList, WidgetInput). None are tested in E2E.

**Why it matters:** Layout and widget rendering bugs may only manifest in specific view configurations. The flex and grid solvers need E2E validation.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `view_layout_grid` | `test_views.sh` | Set `FTUI_HARNESS_VIEW=layout-grid`; verify render completes; output > 200 bytes |
| `view_widget_table` | `test_views.sh` | Set `FTUI_HARNESS_VIEW=widget-table`; verify render completes; output > 200 bytes |
| `view_widget_list` | `test_views.sh` | Set `FTUI_HARNESS_VIEW=widget-list`; verify render completes; output > 200 bytes |
| `view_layout_nested` | `test_views.sh` | Set `FTUI_HARNESS_VIEW=layout-nested`; verify render completes; output > 200 bytes |

---

### Gap 10: Color Downgrade / Color Profile Handling

**Current state:** Not tested. The style system supports truecolor -> 256 -> 16 -> mono downgrade. No test validates output under restricted color profiles.

**Why it matters:** Users on basic terminals (TERM=linux, NO_COLOR=1) need usable output. Incorrect downgrade can produce unreadable text.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `color_no_color` | `test_color.sh` | Set `NO_COLOR=1`; verify no SGR color sequences appear in output |
| `color_term_linux` | `test_color.sh` | Set `TERM=linux`; verify only 16-color SGR sequences (30-37, 40-47, 90-97, 100-107) |
| `color_truecolor` | `test_color.sh` | Set `COLORTERM=truecolor`; verify `\x1b[38;2;` or `\x1b[48;2;` sequences present |

---

### Gap 11: Log Streaming from File

**Current state:** Not tested. The harness supports `FTUI_HARNESS_LOG_FILE` to load logs from a file, and `FTUI_HARNESS_LOG_MARKUP` for styled markup. Neither is exercised in E2E.

**Why it matters:** File-based log loading is a documented feature. Markup rendering bugs could crash the harness.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `log_from_file` | `test_log_file.sh` | Create temp file with 50 lines; set `FTUI_HARNESS_LOG_FILE`; verify log content appears in output |
| `log_markup` | `test_log_file.sh` | Create temp file with markup tags; set both `LOG_FILE` and `LOG_MARKUP=true`; verify render completes |

---

### Gap 12: Panic/Crash Recovery (RAII Cleanup)

**Current state:** `test_cleanup.sh` tests SIGTERM. No test exercises panic recovery (the RAII cleanup path that runs during stack unwinding).

**Why it matters:** The core value proposition of `TerminalSession` is RAII cleanup even on panic. This is explicitly listed in the coverage matrix ("Panic cleanup paths are idempotent").

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `crash_recovery` | `test_crash.sh` | Force a panic in the harness (e.g., via a special command or env var); verify cursor show and alt-screen exit sequences still appear in PTY output |

**Dependency:** Requires harness support to trigger a controlled panic (e.g., `FTUI_HARNESS_PANIC_AFTER_MS` env var). Since the project uses `panic = "abort"` in release, this test must run in debug mode.

---

### Gap 13: Rapid Resize Sequences

**Current state:** `test_inline.sh` has one resize test (60x15 PTY). No test sends multiple rapid SIGWINCH signals to stress the resize path.

**Why it matters:** Real terminals send rapid resize events during window resizing. The EventCoalescer should deduplicate them, but this path has no E2E validation.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `resize_rapid` | `test_resize.sh` | Send 5 rapid SIGWINCH signals with different sizes; verify harness adapts and renders final size correctly |

---

### Gap 14: Subscription Lifecycle

**Current state:** Not tested directly. The spinner tick subscription runs implicitly in all tests, but no test validates subscription start/stop behavior or custom subscriptions.

**Why it matters:** The subscription system is a core runtime feature (start/stop background threads, StopSignal). Bugs could cause resource leaks or missed events.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `subscription_spinner` | `test_subscription.sh` | Run harness for 1 second with spinner; verify spinner frame characters change across PTY captures (animation is progressing) |

---

### Gap 15: Long-Running Session Stability

**Current state:** All tests run for <5 seconds. No endurance test validates memory stability or render correctness over extended periods.

**Why it matters:** Memory leaks, accumulating state, or GraphemePool growth could cause degradation over time.

**Proposed test cases:**

| Test Case | Script | Assertion |
|-----------|--------|-----------|
| `endurance_30s` | `test_endurance.sh` | Run harness for 30 seconds with continuous log input; verify it exits cleanly and final output is valid |

**Notes:** This test should be optional (gated behind `--full` flag) to avoid slowing CI.

---

## Summary: Gap Prioritization

### Critical (block downstream work)

| # | Gap | Impact | Effort | Harness Changes |
|---|-----|--------|--------|-----------------|
| 1 | Mouse events | Core interaction model untested | Medium | None (harness already handles mouse) |
| 2 | Paste events | Common user interaction untested | Low | None (harness already handles paste) |
| 3 | Focus events | UI state changes untested | Low | None (harness already handles focus) |
| 9 | Multiple views | Layout/widget rendering untested | Low | None (harness already supports views via env var) |

### High (important for correctness)

| # | Gap | Impact | Effort | Harness Changes |
|---|-----|--------|--------|-----------------|
| 4 | Hyperlinks (OSC 8) | Presenter link state machine untested | Medium | Needs hyperlink content in logs |
| 10 | Color downgrade | Color profile handling untested | Low | None (env var driven) |
| 11 | Log file streaming | Documented feature untested | Low | None (env var driven) |
| 12 | Panic recovery | RAII safety claim unvalidated | Medium | Needs panic trigger mechanism |
| 13 | Rapid resize | EventCoalescer stress untested | Low | None |

### Medium (defense-in-depth)

| # | Gap | Impact | Effort | Harness Changes |
|---|-----|--------|--------|-----------------|
| 5 | Kitty keyboard | Modern protocol untested | Medium | Needs config option |
| 6 | Scroll regions | Inline optimization untested | Medium | None |
| 7 | Mux passthrough | tmux/zellij behavior untested | Low | None (env var driven) |
| 8 | Degradation levels | Budget system untested | Medium | Needs config option |
| 14 | Subscription lifecycle | Runtime feature untested | Low | None |

### Low (nice-to-have)

| # | Gap | Impact | Effort | Harness Changes |
|---|-----|--------|--------|-----------------|
| 15 | Endurance | Long-run stability untested | Medium | Optional flag |

---

## Proposed New Test Scripts

Based on the gaps above, the following new scripts should be created:

1. `test_mouse.sh` - Gaps 1 (3 test cases)
2. `test_paste.sh` - Gap 2 (3 test cases)
3. `test_focus.sh` - Gap 3 (2 test cases)
4. `test_views.sh` - Gap 9 (4 test cases)
5. `test_color.sh` - Gap 10 (3 test cases)
6. `test_log_file.sh` - Gap 11 (2 test cases)
7. `test_hyperlink.sh` - Gap 4 (1 test case, needs harness work)
8. `test_crash.sh` - Gap 12 (1 test case, needs harness work)
9. `test_resize.sh` - Gap 13 (1 test case)
10. `test_mux.sh` - Gap 7 (2 test cases)
11. `test_degradation.sh` - Gap 8 (2 test cases, needs harness work)
12. `test_kitty.sh` - Gap 5 (2 test cases, needs harness work)
13. `test_scroll_region.sh` - Gap 6 (1 test case)
14. `test_subscription.sh` - Gap 14 (1 test case)
15. `test_endurance.sh` - Gap 15 (1 test case, optional)

**Total new test cases: 29**
**Total after implementation: 60 (31 existing + 29 new)**

---

## Required Harness Enhancements

Some gaps require harness changes before tests can be written:

1. **Hyperlink content** (Gap 4): Add log lines containing hyperlink markup, or accept hyperlink markup via `FTUI_HARNESS_LOG_FILE`.
2. **Panic trigger** (Gap 12): Add `FTUI_HARNESS_PANIC_AFTER_MS` env var for controlled panic testing. Must be debug-only since release uses `panic="abort"`.
3. **Budget override** (Gap 8): Add `FTUI_HARNESS_BUDGET` env var to force a specific degradation level.
4. **Kitty keyboard enable** (Gap 5): Add `FTUI_HARNESS_ENABLE_KITTY_KEYBOARD` env var.

Gaps 1-3, 7, 9-11, 13-15 need **no harness changes** and can be implemented immediately.

---

## run_all.sh Updates

The orchestrator should be updated to include new suites:

```bash
# Current suites (always run)
run_suite "inline"  "$SCRIPT_DIR/test_inline.sh"
run_suite "cleanup" "$SCRIPT_DIR/test_cleanup.sh"

# Extended suites
run_suite "altscreen" "$SCRIPT_DIR/test_altscreen.sh"
run_suite "input"     "$SCRIPT_DIR/test_input.sh"
run_suite "ansi"      "$SCRIPT_DIR/test_ansi.sh"
run_suite "unicode"   "$SCRIPT_DIR/test_unicode.sh"

# New suites (proposed)
run_suite "mouse"      "$SCRIPT_DIR/test_mouse.sh"
run_suite "paste"      "$SCRIPT_DIR/test_paste.sh"
run_suite "focus"      "$SCRIPT_DIR/test_focus.sh"
run_suite "views"      "$SCRIPT_DIR/test_views.sh"
run_suite "color"      "$SCRIPT_DIR/test_color.sh"
run_suite "log_file"   "$SCRIPT_DIR/test_log_file.sh"
run_suite "resize"     "$SCRIPT_DIR/test_resize.sh"
run_suite "mux"        "$SCRIPT_DIR/test_mux.sh"
```
