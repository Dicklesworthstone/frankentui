# Claims ledger

Tracks the 37 mismatch claims (C01–C37) and 71 vision goals (V01–V71)
from `bd-g00-root-epic-ewths.5` and `.5.2`, plus 39 current artifact-status
claims (S rows). N rows await the checker's extraction pass. Historical
observations from `ab07291f` are not evidence that a claim is true today.

## Update rule and verification boundary

A README/AGENTS change that adds or changes a tracked claim must add or
update its row in the same commit. Enforcement belongs to the DSR claims
gate in `bd-g00-root-epic-ewths.5.3`; GitHub Actions is not authorized.
The schema validator checks structure only. Pending rows are not passes.
Neither a referenced path nor an identifier alone proves runtime behavior.
`last_verified` is `-` until the stated claim has actually been verified.

Locations retain both a source filename and a text anchor, rather than relying
on drifting line numbers. C/V anchors may describe superseded historical text;
these rows preserve the audit obligation, not an assertion about today's README.
The owner is the accountable current bead; exact historical decisions and
secondary owner aliases remain in the Historical decision details section.

## Proof grammar

`test:<crate>::<name>` — a named test in the retained test inventory.
`path:<repo-relative>` — a repository path exists; proves existence only.
`ident:<Rust identifier or string>` — production source contains the identifier.
`cmd:<shell command that must exit 0>` — an explicitly permitted, bounded command.
`job:<workflow>:<job>` — historical syntax only; not an executable proof under DSR-only policy.
`count:<python expression over repo files>` — a restricted counting expression.
`manual:<YYYY-MM-DD>:<who>` — a dated manual verification, expires after 90 days.
`bead:<slug>` — an unresolved implementation obligation, not a passing proof.

Multiple proofs are separated by `; `. Table pipes inside cells are escaped.
Kinds: `count`, `api`, `constant`, `default`, `event`, `file`, `algorithm`,
`example`, `ci`, `env`, `status`.
Decisions: `CODE`, `DOC`, `DOC+quarantine`, `regenerate`, `n/a`.
Statuses: `proven`, `pending-code`, `pending-doc`, `retracted`, `allowlisted`.
An open owner is not evidence of correctness; a closed owner requires a concrete
replacement proof before a pending row can become proven.

## Seed verification and remaining acceptance

The 2026-09-17 seed contains 147 rows: C01–C37, V01–V71 and S01–S39.
Source-to-ledger comparison verified exact C/V claim text, unique IDs, nine
columns, text anchors and existing owner IDs.

As of 2026-09-19 the distribution across 152 rows is 6 pending-code,
41 pending-doc, 56 retracted and 49 proven. Regenerate this sentence from
`python3 scripts/check_readme_claims.py --schema-check` rather than by hand;
it had drifted from the table before 2026-09-19.

The jump in `retracted` on 2026-09-19 is one audit, not a collapse: every
README section marked **Status: experimental** was checked against whether any
crate's `src/` imports the module it describes. None of the eleven did. Those
sections now carry a `Where it runs` line, `make claims` keeps them carrying
one, and their rows are retracted. The modules are real, tested research code;
the claim that the runtime uses them was not.

(A hand count of those sections found eight. The gate found eleven — three
subsections carry the marker under a parent heading that does not. That is the
argument for the gate over the audit.)

A row becomes `proven` only when a named test pins the specific thing the
README says, not something adjacent. The proven set:

- V64 / S04 (Bayesian capability detection): every log-odds weight the README
  quotes matches `caps_probe.rs`, pinned by `weights_are_unchanged`.
- S09 (BOCPD): on by default, pinned by
  `config_default_enables_bocpd_with_heuristic_fallback`.
- S11 (VOI): the defaults the README lists are set in
  `InlineAutoRemeasureConfig::default()`, **not** `VoiConfig::default()`, whose
  values differ. Reading past that heading produces a false mismatch report.
- S14 (Mondrian conformal): the four documented values were true but
  unguarded, so `default_config_matches_the_documented_values` was added.
- S15 (CUSUM): `alloc_budget.rs` is wired into `FrameGuardrails`, pinned by
  `guardrails_detect_allocation_drift`.
- S17 (gestures): defaults pinned by `gesture::default_config_values`.
- S25 (e-graph): proven in the other direction — the section now states the
  measured truth, that it runs nowhere on the layout path.
- C26, V47, N01, N02, N03, N04, N05 (G29 locale context, bidi integration, seven demo
  languages, CLDR v45.0 number/date formatting, plural rules).
- C01 (`frame.render_widget` / `render_stateful_widget` / `area`), C12
  (clipboard commands) and C13 (`tick_every`, `file_watcher`): all three were
  filed as `pending-code` — implementation outstanding — for code that already
  existed and was already covered by a test. Verified 2026-09-19.
- S03 (Bayesian diff strategy): on by default (`bayesian_enabled: true`) and
  driving the per-frame choice via `select_with_scan_estimate` in
  `terminal_writer.rs`. The README's Beta prior — "α₀ = 1, β₀ = 19 → E[p] = 5%"
  — and the `c_scan`/`c_emit` cost weights were true but unguarded, so
  `config_default_all_fields` was extended to pin them, same as S14.
- S27 (degradation cascade): **real, and easy to mis-audit.** The runtime's PID
  controller sets a level (`program.rs:7048`), pushes it to the frame
  (`frame.set_degradation`), and **34 of 68 widget modules read
  `frame.buffer.degradation`** and shed work accordingly. Do not confuse it with
  `ftui-runtime::degradation_cascade`, which is quarantined and wired to
  nothing: the cascade that runs lives in `ftui-render::budget` plus
  `program.rs` plus the widgets. Marking this section experimental because of
  the module name would be wrong.
- V58 is the same distinction from the other side, and the two rows must not be
  collapsed: V58 says *"degradation cascade **module**"* and is retracted,
  because `ftui-runtime::degradation_cascade` is quarantined and imported by
  nothing. S27 says the cascade *runs*, and is proven. Both are correct at once.
  A row that names a module is a claim about that module, not about the
  capability that shares its name.

Three retractions on 2026-09-19 are **half-true claims**, which are their own
hazard: the row reads as correct because part of it is.

- V20 "S3-FIFO cache for caps **+ width**" — S3-FIFO really does back the width
  cache (`ftui-core/src/lib.rs:378`), but terminal capability detection is not
  cached at all; it runs once per session, as the README now says.
- V21 "**W-TinyLFU** width cache + PAC-Bayes CMS" — the W-TinyLFU and LRU caches
  in `ftui-text` are benchmark subjects and are not on the render path.
- V41 "Widget composition helpers `render_widget`, **`Layout`**" —
  `render_widget` is real and compiled by a snippet; there is no `Layout` type
  in ftui-layout, only `Flex`.
- V12 "E-process / **GRAPA** anytime-valid monitors" is the fourth and the
  subtlest. The e-process is genuinely on the render path —
  `ftui_render::budget`'s `EProcessState` runs `E_t = E_{t-1}·exp(λ·r_t − λ²/2)`
  every frame and gates degradation on `E_t > 1/α`. But `λ` is read straight
  from config: the production test bets at a **fixed** fraction. GRAPA, which
  adapts `λ`, exists only in the experimental `conformal_alert`. The README's
  formula block compounded it by writing the multiplicative-wealth form
  `W_t = W_{t-1}(1 + λ_t(X_t − μ₀))`, which matches *neither* implementation;
  both use the exponential form.

V69 is the only claim found so far that misdirected **agents** rather than
users. AGENTS.md said in three places that cross-component integration tests
live in the repository-root `tests/` directory. That directory holds fixtures,
baselines and captured artifacts; it contains no `.rs` files, and could not run
them if it did, because the root `Cargo.toml` is a virtual manifest with no
`[package]` and cargo never builds a test target there. The 292 integration
test files all live in each crate's own `tests/`.

Retracted: C36 and V19 (the SOS barrier coefficients were claimed to be
SDP-solved by a script that does not exist; the source header says they were
hand-chosen).

Most C-row retractions on 2026-09-19 are the same shape: the API named in the
claim no longer exists, the README was corrected to the API that does, **and
that corrected form is now compiled and byte-matched by `readme_snippets`**. So
the retraction cites a test rather than a date — `Layout::horizontal` became
`Flex::horizontal`, `frame.link_registry().register(url)` became
`frame.register_link(url)`, `Stylesheet`/`register`/`get().unwrap_or_default()`
became `StyleSheet`/`define`/`get_or_default`. A `manual:` proof expires in 90
days; a snippet test does not.

Historical WORKING labels below are not current test results.

Run `python3 -B scripts/check_readme_claims.py --schema-check` to validate
the nine-column table, IDs, enums, anchors, proof syntax and dates. Run
`python3 -B scripts/check_readme_claims.py --self-test` for in-memory malformed
fixtures. Neither command executes proofs or establishes extracted coverage.

`--proof-refs` (also in `make claims`) checks that every `test:` proof names a
`fn` that exists under `crates/`, and every `path:` proof names a file that
exists. **This validates the citation, not the claim** — a real test can still
pin the wrong thing. It exists because on 2026-09-19 three rows cited tests that
did not exist: `history_records_on_enter`, `render_plasma_frame_deterministic`
and `russian_rules`, each a near-miss for a real test, written from memory
rather than looked up. A fabricated proof is worse than the `bead:` placeholder
it replaces, because it reads as settled.
The full `.5.3` checker must still extract N rows, establish coverage and
reconcile this grammar with its help text. The proposed 60% proven threshold
is not met. `.5.1` must still approve the decision table. This seed does not
complete `.5.2`.

## Claims

| id | claim | location | kind | decision | owner | proof | status | last_verified |
|---|---|---|---|---|---|---|---|---|
| C01 | `frame.render_widget(w, area)`, `frame.render_stateful_widget(..)`, `frame.area()` | README.md :: `frame.render_widget(w, area)`, `frame.render_stateful_widget(..)`, `frame.area()` | api | CODE | bd-g00-root-epic-ewths.23.17 | test:ftui::readme_snippets::readme_model_snippets_match | proven | 2026-09-19 |
| C02 | `Layout::horizontal([Constraint::Percentage(30), ..]).split(frame.area())` | README.md :: `Layout::horizontal([Constraint::Percentage(30), ..]).split(frame.area())` | api | CODE | bd-g00-root-epic-ewths.23.17 | test:ftui::readme_snippets::readme_model_snippets_match | retracted | 2026-09-19 |
| C03 | `focus_manager.register("input1", FocusNode::new()); set_next(..)` | README.md :: `focus_manager.register("input1", FocusNode::new()); set_next(..)` | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui::readme_snippets::readme_focus_graph_snippet | retracted | 2026-09-19 |
| C04 | `modal_stack.push(ConfirmDialog::new("Delete file?"))` | README.md :: `modal_stack.push(ConfirmDialog::new("Delete file?"))` | api | DOC | bd-g00-root-epic-ewths.5.5 | ident:pub trait StackModal: Send; ident:pub struct ModalStack | retracted | 2026-09-19 |
| C05 | `frame.link_registry().register(url)`; `cell.link_id = id` | README.md :: `frame.link_registry().register(url)`; `cell.link_id = id` | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui::readme_snippets::readme_hyperlink_snippet | retracted | 2026-09-19 |
| C06 | Cell layout content 4 + fg 4 + bg 4 + attrs 2 + link 2; `GraphemeId` width bits [31:25], 16M slots, width 0-127 | README.md :: Cell layout content 4 + fg 4 + bg 4 + attrs 2 + link 2; `GraphemeId` width bits [31:25], 16M slots, width 0-127 | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-render::cell::cell_is_16_bytes; ident:pub const LINK_ID_MAX: u32 = 0x00FF_FFFF | retracted | 2026-09-19 |
| C07 | `TimeTravel::new(); record(frame); seek(i); current()` | README.md :: `TimeTravel::new(); record(frame); seek(i); current()` | api | DOC+quarantine | bd-g00-root-epic-ewths.5.5 | test:ftui::readme_snippets::readme_time_travel_snippet | retracted | 2026-09-19 |
| C08 | `Stylesheet::new(); sheet.register(..); sheet.get(..).unwrap_or_default()` | README.md :: `Stylesheet::new(); sheet.register(..); sheet.get(..).unwrap_or_default()` | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui::readme_snippets::readme_stylesheet_snippet | retracted | 2026-09-19 |
| C09 | `TableTheme::modern().with_stripe_period(2).with_header_style(..).with_selection_style(..)` | README.md :: `TableTheme::modern().with_stripe_period(2).with_header_style(..).with_selection_style(..)` | api | CODE | bd-g00-root-epic-ewths.23.13 | test:ftui::readme_snippets::readme_table_theme_snippet | retracted | 2026-09-19 |
| C10 | 9 border styles | README.md :: 9 border styles | api | DOC | bd-g00-root-epic-ewths.23.11 | test:ftui-widgets::borders::tests::border_type_variant_count_matches_readme | retracted | 2026-09-19 |
| C11 | `Cmd::perform(future, mapper)` | README.md :: `Cmd::perform(future, mapper)` | api | DOC | bd-g00-root-epic-ewths.5.5 | manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| C12 | `Cmd::SetClipboard/GetClipboard` | README.md :: `Cmd::SetClipboard/GetClipboard` | api | CODE | bd-g00-root-epic-ewths.21 | test:ftui-runtime::program::clipboard_commands_reach_writer_and_reply_reaches_model_once | proven | 2026-09-19 |
| C13 | `tick_every`, `file_watcher` | README.md :: `tick_every`, `file_watcher` | api | CODE | bd-g00-root-epic-ewths.22 | test:ftui::readme_snippets::readme_model_snippets_match | proven | 2026-09-19 |
| C14 | `frame.checksum()`, `MacroPlayer::next() -> (event, delay)`, `simulator.send_event` | README.md :: `frame.checksum()`, `MacroPlayer::next() -> (event, delay)`, `simulator.send_event` | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui::readme_snippets::readme_simulator_snippet | retracted | 2026-09-19 |
| C15 | `PersistenceConfig::new().with_auto_save(true).with_backend(FileBackend::new(..))`, `MemoryBackend` | README.md :: `PersistenceConfig::new().with_auto_save(true).with_backend(FileBackend::new(..))`, `MemoryBackend` | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui::readme_snippets::readme_persistence_snippet | retracted | 2026-09-19 |
| C16 | `field_lens!` macro | README.md :: `field_lens!` macro | api | DOC+quarantine | bd-g00-root-epic-ewths.5.5 | test:ftui::readme_snippets::readme_lens_snippet | retracted | 2026-09-19 |
| C17 | `slo.yaml` with `objectives / budget_us / window_seconds / error_budget_pct` | README.md :: `slo.yaml` with `objectives / budget_us / window_seconds / error_budget_pct` | api | DOC+quarantine | bd-g00-root-epic-ewths.5.5 | test:ftui-runtime::slo_yaml_validation::readme_slo_yaml_example_parses | retracted | 2026-09-19 |
| C18 | Evidence events `resize_decision`, `conformal_gate`, `degradation_event`, `queue_select`, `voi_sample` | README.md :: Evidence events `resize_decision`, `conformal_gate`, `degradation_event`, `queue_select`, `voi_sample` | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-runtime::telemetry_schema::tests::schema_events_match_constants | retracted | 2026-09-19 |
| C19 | Degradation ladder Full, SimpleBorders, NoColors, TextOnly | README.md :: Degradation ladder Full, SimpleBorders, NoColors, TextOnly | api | DOC | bd-g00-root-epic-ewths.5.5 | ident:pub enum DegradationLevel; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| C20 | Editor: undo coalescing, paragraph movement | README.md :: Editor: undo coalescing, paragraph movement | api | CODE | bd-g00-root-epic-ewths.21 | test:ftui-text::editor::tests::undo_groups_virtual_idle_boundary_and_clock_reset; test:ftui-text::editor::tests::paragraph_selection_preserves_anchor_and_exact_text | proven | 2026-09-19 |
| C21 | Input "history"; Textarea "syntax hooks"; Progress "indeterminate"; JsonView "collapse/expand"; Sparkline "min/max markers" | README.md :: Input "history"; Textarea "syntax hooks"; Progress "indeterminate"; JsonView "collapse/expand"; Sparkline "min/max markers" | api | CODE | bd-g00-root-epic-ewths.23.1 | test:ftui-widgets::input::history_dedups_consecutive; test:ftui-widgets::progress::progress_indeterminate_phase0_40x1; test:ftui-widgets::sparkline::min_marker_placed_at_first_min | proven | 2026-09-19 |
| C22 | "Plus" widget names `Cached`, `DragHandle`, `Inspector`, `NotificationQueue`, `ValidationError` | README.md :: "Plus" widget names `Cached`, `DragHandle`, `Inspector`, `NotificationQueue`, `ValidationError` | api | DOC | bd-g00-root-epic-ewths.5.5 | ident:pub struct CachedWidget; ident:pub struct Draggable; ident:pub struct InspectorOverlay; ident:pub struct ValidationErrorDisplay | retracted | 2026-09-19 |
| C23 | 46 screens, 11 categories, screens `3d_data` and `quake` | README.md :: 46 screens, 11 categories, screens `3d_data` and `quake` | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-demo-showcase::app::tests::all_screens_count | retracted | 2026-09-19 |
| C24 | VFX list credited to ftui-extras | README.md :: VFX list credited to ftui-extras | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-extras::proptest_sampling_invariants::plasma_deterministic | retracted | 2026-09-19 |
| C25 | Command palette BF word-boundary about 2.0, position proportional to 1/pos, length proportional to 1/len | README.md :: Command palette BF word-boundary about 2.0, position proportional to 1/pos, length proportional to 1/len | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-widgets::command_palette::scorer::tests::tag_match_boosts_score; test:ftui-widgets::command_palette::scorer::tests::evidence_description_word_boundary_count_display | proven | 2026-09-19 |
| C26 | i18n: number/date formatting, LTR/RTL via ftui-text bidi, demo in EN/FR/DE/JA/AR | README.md :: i18n: number/date formatting, LTR/RTL via ftui-text bidi, demo in EN/FR/DE/JA/AR | api | CODE | bd-g00-root-epic-ewths.34 | test:ftui-demo-showcase::tests::i18n_e2e::formatting_numbers_all_seven_locales; test:ftui-widgets::paragraph::tests::paragraph_rtl_visual_order_matches_unicode_bidi | proven | 2026-09-19 |
| C27 | Benchmarks `diff/identical_100x50 1.2 µs`, `sparse 8.3 µs`, `dense 45 µs` | README.md :: Benchmarks `diff/identical_100x50 1.2 µs`, `sparse 8.3 µs`, `dense 45 µs` | api | regenerate | bd-g00-root-epic-ewths.31 | bead:bd-g00-root-epic-ewths.31 | pending-doc | - |
| C28 | `prop_diff_soundness`, `counterexample_dirty_soundness` | README.md :: `prop_diff_soundness`, `counterexample_dirty_soundness` | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-render::buffer::set_marks_row_dirty; test:ftui-render::proptest_diff_invariants::no_false_negative_changes | retracted | 2026-09-19 |
| C29 | Architecture diagram "TerminalSession (crossterm)" (README and AGENTS.md) | README.md :: Architecture diagram "TerminalSession (crossterm)" (README and AGENTS.md) | api | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| C30 | "Hybrid" inline strategy is default with runtime DECSTBM-reliability fallback | README.md :: "Hybrid" inline strategy is default with runtime DECSTBM-reliability fallback | api | CODE | bd-g00-root-epic-ewths.4.3 | test:ftui-core::inline_mode::tests::strategy_selection_uses_hybrid_without_sync | retracted | 2026-09-19 |
| C31 | 80+ widgets | README.md :: 80+ widgets | api | DOC | bd-g00-root-epic-ewths.5.5 | count:91 Widget/StatefulWidget impls under crates/ftui-widgets/src >= 80 | proven | 2026-09-19 |
| C32 | 850K+ lines | README.md :: 850K+ lines | api | DOC | bd-g00-root-epic-ewths.5.5 | count:1,110,773 lines across 981 .rs files under crates/ | retracted | 2026-09-19 |
| C33 | `ftui = "0.5"`; getting-started "only ftui-core, ftui-layout, ftui-i18n are published" | README.md :: `ftui = "0.5"`; getting-started "only ftui-core, ftui-layout, ftui-i18n are published" | api | DOC | bd-g00-root-epic-ewths.1.5 | test:ftui::readme_snippets::readme_versions_match_the_workspace | retracted | 2026-09-19 |
| C34 | `FTUI_HARNESS_VIEW=dashboard cargo run -p ftui-demo-showcase`; `cargo run -p ftui-harness --example minimal` is a hello world | README.md :: `FTUI_HARNESS_VIEW=dashboard cargo run -p ftui-demo-showcase`; `cargo run -p ftui-harness --example minimal` is a hello world | api | DOC | bd-g00-root-epic-ewths.40.3 | cmd:python3 scripts/check_env_docs.py | retracted | 2026-09-19 |
| C35 | VOI defaults 1 / 9 / 1000 / 100 / 0.08; resize coalescing 200 / 20 ms; gesture 2 cells / 500 ms | README.md :: VOI defaults 1 / 9 / 1000 / 100 / 0.08; resize coalescing 200 / 20 ms; gesture 2 cells / 500 ms | api | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-runtime::program::inline_auto_remeasure_config_defaults | proven | 2026-09-19 |
| C36 | SOS coefficients "Auto-generated 2026-03-05 by scripts/solve_sos_barrier.py" | README.md :: SOS coefficients "Auto-generated 2026-03-05 by scripts/solve_sos_barrier.py" | api | CODE | bd-g00-root-epic-ewths.18 | ident:PROVENANCE: these constants were written by hand | retracted | 2026-09-18 |
| C37 | `no_flicker_proof.rs` | README.md :: `no_flicker_proof.rs` | api | DOC | bd-g00-root-epic-ewths.4.7 | path:crates/ftui-render/tests/no_flicker_proof.rs | proven | 2026-09-19 |
| V01 | Inline mode with scrollback preservation and stable chrome | README.md :: Inline mode with scrollback preservation and stable chrome | status | DOC | bd-g00-root-epic-ewths.4 | bead:bd-g00-root-epic-ewths.4 | pending-doc | - |
| V02 | Deterministic Buffer -> Diff -> Presenter -> ANSI | README.md :: Deterministic Buffer -> Diff -> Presenter -> ANSI | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V03 | One-writer rule | README.md :: One-writer rule | status | DOC | bd-g00-root-epic-ewths.4 | bead:bd-g00-root-epic-ewths.4 | pending-doc | - |
| V04 | RAII cleanup even on panic | README.md :: RAII cleanup even on panic | status | DOC | bd-g00-root-epic-ewths.37 | bead:bd-g00-root-epic-ewths.37 | pending-doc | - |
| V05 | Composable crates, add only what you need | README.md :: Composable crates, add only what you need | status | CODE | bd-g00-root-epic-ewths.1 | bead:bd-g00-root-epic-ewths.1 | pending-code | - |
| V06 | 80+ widgets | README.md :: 80+ widgets | status | CODE | bd-g00-root-epic-ewths.23 | count:91 Widget/StatefulWidget impls under crates/ftui-widgets/src >= 80 | proven | 2026-09-19 |
| V07 | Pane workspaces with drag/dock/snap/throw/undo | README.md :: Pane workspaces with drag/dock/snap/throw/undo | status | DOC | bd-g00-root-epic-ewths.23 | bead:bd-g00-root-epic-ewths.23 | pending-doc | - |
| V08 | Web/WASM backend, runs in browser | README.md :: Web/WASM backend, runs in browser | status | CODE | bd-g00-root-epic-ewths.29 | test:ftui-showcase-wasm::runner_core::screen_selector_reaches_model_and_preserves_invalid_selection | proven | 2026-09-19 |
| V09 | Bayesian diff strategy | README.md :: Bayesian diff strategy | status | DOC | bd-g00-root-epic-ewths.17 | bead:bd-g00-root-epic-ewths.17 | pending-doc | - |
| V10 | BOCPD resize coalescing | README.md :: BOCPD resize coalescing | status | CODE | bd-g00-root-epic-ewths.16 | test:ftui-runtime::resize_coalescer::config_default_enables_bocpd_with_heuristic_fallback | proven | 2026-09-19 |
| V11 | VOI sampling for expensive ops | README.md :: VOI sampling for expensive ops | status | CODE | bd-g00-root-epic-ewths.14 | test:ftui::readme_snippets::readme_model_snippets_match | proven | 2026-09-19 |
| V12 | E-process / GRAPA anytime-valid monitors | README.md :: E-process / GRAPA anytime-valid monitors | status | CODE | bd-g00-root-epic-ewths.17 | test:ftui-render::budget::eprocess_grows_under_overload | retracted | 2026-09-19 |
| V13 | Conformal frame-time gating (Mondrian) | README.md :: Conformal frame-time gating (Mondrian) | status | CODE | bd-g00-root-epic-ewths.15 | test:ftui-runtime::conformal_predictor::default_config_matches_the_documented_values | proven | 2026-09-19 |
| V14 | Multi-stage conformal monitors | README.md :: Multi-stage conformal monitors | status | DOC | bd-g00-root-epic-ewths.15 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| V15 | CUSUM allocation + hover | README.md :: CUSUM allocation + hover | status | CODE | bd-g00-root-epic-ewths.17 | test:ftui-render::frame_guardrails::guardrails_detect_allocation_drift; test:ftui-core::hover_stabilizer::default_config_values | proven | 2026-09-19 |
| V16 | Alpha-investing FDR across monitors | README.md :: Alpha-investing FDR across monitors | status | CODE | bd-g00-root-epic-ewths.11 | cmd:python3 scripts/check_readme_claims.py --experimental-check | retracted | 2026-09-19 |
| V17 | Flake detector for E2E timing | README.md :: Flake detector for E2E timing | status | CODE | bd-g00-root-epic-ewths.11 | cmd:python3 scripts/check_readme_claims.py --experimental-check | retracted | 2026-09-19 |
| V18 | Rough-path signatures | README.md :: Rough-path signatures | status | DOC | bd-g00-root-epic-ewths.11 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| V19 | SOS barrier certificates (SDP-solved) | README.md :: SOS barrier certificates (SDP-solved) | status | CODE | bd-g00-root-epic-ewths.18 | ident:PROVENANCE: these constants were written by hand | retracted | 2026-09-18 |
| V20 | S3-FIFO cache for caps + width | README.md :: S3-FIFO cache for caps + width | status | CODE | bd-g00-root-epic-ewths.12 | test:ftui-core::s3_fifo::scan_resistance | retracted | 2026-09-19 |
| V21 | W-TinyLFU width cache + PAC-Bayes CMS | README.md :: W-TinyLFU width cache + PAC-Bayes CMS | status | CODE | bd-g00-root-epic-ewths.12 | path:docs/perf/text_width_cache_2026-09-02.md | retracted | 2026-09-19 |
| V22 | Flat combining | README.md :: Flat combining | status | DOC | bd-g00-root-epic-ewths.11 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| V23 | Bidirectional lenses `field_lens!` | README.md :: Bidirectional lenses `field_lens!` | status | DOC | bd-g00-root-epic-ewths.11 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| V24 | IVM DAG | README.md :: IVM DAG | status | DOC | bd-lksq7 | ident:There is no propagation engine | retracted | 2026-09-19 |
| V25 | SLO schema + safe mode | README.md :: SLO schema + safe mode | status | DOC | bd-g00-root-epic-ewths.11 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| V26 | State persistence | README.md :: State persistence | status | DOC | bd-g00-root-epic-ewths.35 | bead:bd-g00-root-epic-ewths.35 | pending-doc | - |
| V27 | Input macro record/playback | README.md :: Input macro record/playback | status | DOC | bd-g00-root-epic-ewths.35 | bead:bd-g00-root-epic-ewths.35 | pending-doc | - |
| V28 | Headless simulator | README.md :: Headless simulator | status | DOC | bd-g00-root-epic-ewths.35 | bead:bd-g00-root-epic-ewths.35 | pending-doc | - |
| V29 | Frame arena in hot path | README.md :: Frame arena in hot path | status | DOC | bd-g00-root-epic-ewths.31 | bead:bd-g00-root-epic-ewths.31 | pending-doc | - |
| V30 | Grapheme pool with width bits | README.md :: Grapheme pool with width bits | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V31 | Synchronized output every frame | README.md :: Synchronized output every frame | status | DOC | bd-g00-root-epic-ewths.4 | test:ftui-demo-showcase::capability_sim_e2e::degradation_sync_output_disabled_in_all_muxes | retracted | 2026-09-19 |
| V32 | Elm architecture Model/Cmd/Subscriptions | README.md :: Elm architecture Model/Cmd/Subscriptions | status | DOC | bd-g00-root-epic-ewths.1 | bead:bd-g00-root-epic-ewths.1 | pending-doc | - |
| V33 | Zero unsafe | README.md :: Zero unsafe | status | DOC | bd-g00-root-epic-ewths.5 | count:20 of 20 crate roots carry #![forbid(unsafe_code)] | proven | 2026-09-19 |
| V34 | Formal proof sketches Theorems 1-4 | README.md :: Formal proof sketches Theorems 1-4 | status | DOC | bd-g00-root-epic-ewths.5 | ident:Theorem 4 (Diff-Dirty Equivalence); test:ftui-render::buffer::set_marks_row_dirty | proven | 2026-09-19 |
| V35 | Property tests, snapshots, benches | README.md :: Property tests, snapshots, benches | status | DOC | bd-g00-root-epic-ewths.31 | bead:bd-g00-root-epic-ewths.31 | pending-doc | - |
| V36 | Resize coalescing regimes | README.md :: Resize coalescing regimes | status | DOC | bd-g00-root-epic-ewths.16 | bead:bd-g00-root-epic-ewths.16 | pending-doc | - |
| V37 | Budget degradation PID | README.md :: Budget degradation PID | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V38 | Input fairness guard | README.md :: Input fairness guard | status | DOC | bd-g00-root-epic-ewths.17 | bead:bd-g00-root-epic-ewths.17 | pending-doc | - |
| V39 | Table theming engine | README.md :: Table theming engine | status | CODE | bd-g00-root-epic-ewths.23 | test:ftui::readme_snippets::readme_table_theme_snippet | proven | 2026-09-19 |
| V40 | Stylesheet | README.md :: Stylesheet | status | CODE | bd-g00-root-epic-ewths.23 | test:ftui::readme_snippets::readme_stylesheet_snippet | proven | 2026-09-19 |
| V41 | Widget composition helpers `render_widget`, `Layout` | README.md :: Widget composition helpers `render_widget`, `Layout` | status | CODE | bd-g00-root-epic-ewths.23 | test:ftui::readme_snippets::readme_model_snippets_match | retracted | 2026-09-19 |
| V42 | Hyperlinks | README.md :: Hyperlinks | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V43 | Focus management | README.md :: Focus management | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V44 | Modal system | README.md :: Modal system | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V45 | Time-travel debugging | README.md :: Time-travel debugging | status | CODE | bd-g00-root-epic-ewths.11 | test:ftui::readme_snippets::readme_time_travel_snippet | proven | 2026-09-19 |
| V46 | Accessibility tree, live regions | README.md :: Accessibility tree, live regions | status | CODE | bd-g00-root-epic-ewths.13 | test:ftui::readme_snippets::readme_accessibility_snippets | proven | 2026-09-19 |
| V47 | i18n formatting/bidi/5 languages | README.md :: i18n formatting/bidi/5 languages | status | CODE | bd-g00-root-epic-ewths.34 | test:ftui-demo-showcase::tests::i18n_e2e::formatting_numbers_all_seven_locales; test:ftui-demo-showcase::screens::i18n_demo::tests::locales_list_has_seven_languages | proven | 2026-09-19 |
| V48 | Queueing scheduler SRPT/Smith/aging | README.md :: Queueing scheduler SRPT/Smith/aging | status | CODE | bd-g00-root-epic-ewths.30 | bead:bd-g00-root-epic-ewths.30 | pending-code | - |
| V49 | Inline strategies A/B/C auto-selected | README.md :: Inline strategies A/B/C auto-selected | status | DOC | bd-g00-root-epic-ewths.4 | bead:bd-g00-root-epic-ewths.4 | pending-doc | - |
| V50 | Color system profiles + WCAG | README.md :: Color system profiles + WCAG | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V51 | Evidence sink categories | README.md :: Evidence sink categories | status | CODE | bd-g00-root-epic-ewths.26 | test:ftui::readme_snippets::readme_model_snippets_match | proven | 2026-09-19 |
| V52 | Runtime lanes + rollout + shadow-run | README.md :: Runtime lanes + rollout + shadow-run | status | CODE | bd-g00-root-epic-ewths.30 | test:ftui::readme_snippets::readme_runtime_lanes_snippet; test:ftui::readme_snippets::readme_shadow_run_snippet; test:ftui::readme_snippets::readme_rollout_scorecard_snippet | proven | 2026-09-19 |
| V53 | Effect queue telemetry + backpressure | README.md :: Effect queue telemetry + backpressure | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V54 | Telemetry schema targets | README.md :: Telemetry schema targets | status | CODE | bd-g00-root-epic-ewths.26 | test:ftui-runtime::telemetry_schema::schema_events_match_constants | proven | 2026-09-19 |
| V55 | E-graph layout optimizer before solver | README.md :: E-graph layout optimizer before solver | status | CODE | bd-g00-root-epic-ewths.11 | path:docs/perf/egraph_vs_flex_2026-09-18.md | retracted | 2026-09-19 |
| V56 | Rope text engine | README.md :: Rope text engine | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V57 | Editor core features | README.md :: Editor core features | status | CODE | bd-g00-root-epic-ewths.21 | test:ftui-text::editor::undo_groups_virtual_idle_boundary_and_clock_reset; test:ftui-text::editor::paragraph_selection_preserves_anchor_and_exact_text | proven | 2026-09-19 |
| V58 | Degradation cascade module | README.md :: Degradation cascade module | status | CODE | bd-g00-root-epic-ewths.17 | cmd:python3 scripts/check_readme_claims.py --experimental-check | retracted | 2026-09-19 |
| V59 | Cost models (cache / M-G-1 / batching) | README.md :: Cost models (cache / M-G-1 / batching) | status | DOC | bd-g00-root-epic-ewths.11 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| V60 | Gesture recognizer | README.md :: Gesture recognizer | status | CODE | bd-g00-root-epic-ewths.24 | test:ftui-core::gesture::default_config_values | proven | 2026-09-19 |
| V61 | Input parser (CSI/SS3/DCS/OSC/APC, kitty, paste, mouse) | README.md :: Input parser (CSI/SS3/DCS/OSC/APC, kitty, paste, mouse) | status | DOC | bd-g00-root-epic-ewths.27 | bead:bd-g00-root-epic-ewths.27 | pending-doc | - |
| V62 | Keybinding system | README.md :: Keybinding system | status | CODE | bd-g00-root-epic-ewths.20 | bead:bd-g00-root-epic-ewths.20 | pending-code | - |
| V63 | Animation system | README.md :: Animation system | status | DOC | bd-g00-root-epic-ewths.5 | bead:bd-g00-root-epic-ewths.5 | pending-doc | - |
| V64 | Bayesian capability detection | README.md :: Bayesian capability detection | status | CODE | bd-g00-root-epic-ewths.19 | test:ftui-core::caps_probe::weights_are_unchanged | proven | 2026-09-18 |
| V65 | 46 demo screens, gallery table | README.md :: 46 demo screens, gallery table | status | DOC | bd-g00-root-epic-ewths.5 | test:ftui-demo-showcase::app::tests::all_screens_count | retracted | 2026-09-19 |
| V66 | crates.io: all 17 libraries | README.md :: crates.io: all 17 libraries | status | DOC | bd-g00-root-epic-ewths.1 | count:20 crates minus 3 publish=false = 17 library crates; path:CHANGELOG.md | proven | 2026-09-19 |
| V67 | Windows support | README.md :: Windows support | status | CODE | bd-g00-root-epic-ewths.36 | bead:bd-g00-root-epic-ewths.36 | pending-code | - |
| V68 | doctor_frankentui verification stack | README.md :: doctor_frankentui verification stack | status | CODE | bd-g00-root-epic-ewths.28 | bead:bd-g00-root-epic-ewths.28 | pending-code | - |
| V69 | Cross-component tests in workspace `tests/` | AGENTS.md :: Cross-component tests in workspace `tests/` | status | DOC | bd-g00-root-epic-ewths.8 | path:crates/ftui-runtime/tests; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| V70 | Mandatory gates green (check/clippy/fmt/tests) | README.md :: Mandatory gates green (check/clippy/fmt/tests) | status | CODE | bd-g00-root-epic-ewths.6 | bead:bd-g00-root-epic-ewths.6 | pending-code | - |
| V71 | `master` synchronized with `main` | README.md :: `master` synchronized with `main` | status | DOC | bd-g00-root-epic-ewths.5 | cmd:test "$(git rev-parse origin/main)" = "$(git rev-parse origin/master)" | proven | 2026-09-19 |
| S01 | Bayesian Fuzzy Scoring (Command Palette) production status | README.md:847 :: Bayesian Fuzzy Scoring (Command Palette) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S02 | Bayesian Hint Ranking (Keybinding Hints) production status | README.md:879 :: Bayesian Hint Ranking (Keybinding Hints) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S03 | Bayesian Diff Strategy Selection production status | README.md:898 :: Bayesian Diff Strategy Selection | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-runtime::terminal_writer::runtime_diff_config_default; test:ftui-render::diff_strategy::config_default_all_fields | proven | 2026-09-19 |
| S04 | Bayesian Capability Detection (Terminal Caps Probe) production status | README.md:928 :: Bayesian Capability Detection (Terminal Caps Probe) | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-core::caps_probe::weights_are_unchanged | proven | 2026-09-18 |
| S05 | Dirty-Span Interval Union (Sparse Diff Scans) production status | README.md:946 :: Dirty-Span Interval Union (Sparse Diff Scans) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S06 | Summed-Area Table (Tile-Skip Diff) production status | README.md:960 :: Summed-Area Table (Tile-Skip Diff) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S07 | Fenwick Tree (Prefix Sums for Virtualized Lists) production status | README.md:971 :: Fenwick Tree (Prefix Sums for Virtualized Lists) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S08 | Bayesian Height Prediction + Conformal Bounds (Virtualized Lists) production status | README.md:983 :: Bayesian Height Prediction + Conformal Bounds (Virtualized Lists) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S09 | BOCPD: Online Change-Point Detection production status | README.md:999 :: BOCPD: Online Change-Point Detection | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-runtime::resize_coalescer::config_default_enables_bocpd_with_heuristic_fallback | proven | 2026-09-18 |
| S10 | Bayes-Factor Evidence Ledger (Resize Coalescer) production status | README.md:1030 :: Bayes-Factor Evidence Ledger (Resize Coalescer) | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-runtime::resize_coalescer::config_default_enables_bocpd_with_heuristic_fallback | proven | 2026-09-19 |
| S11 | Value-of-Information (VOI) Sampling production status | README.md:1045 :: Value-of-Information (VOI) Sampling | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-runtime::program::inline_auto_remeasure_config_defaults | proven | 2026-09-18 |
| S12 | E-Process: Anytime-Valid Testing production status | README.md:1087 :: E-Process: Anytime-Valid Testing | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-render::budget::eprocess_grows_under_overload | retracted | 2026-09-19 |
| S13 | Conformal Alerting production status | README.md:1109 :: Conformal Alerting | status | DOC | bd-g00-root-epic-ewths.5.5 | cmd:python3 scripts/check_readme_claims.py --experimental-check | retracted | 2026-09-19 |
| S14 | Mondrian Conformal Frame-Time Risk Gating production status | README.md:1129 :: Mondrian Conformal Frame-Time Risk Gating | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-runtime::conformal_predictor::default_config_matches_the_documented_values | proven | 2026-09-18 |
| S15 | CUSUM Control Charts production status | README.md:1145 :: CUSUM Control Charts | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-render::frame_guardrails::guardrails_detect_allocation_drift | proven | 2026-09-18 |
| S16 | CUSUM Hover Stabilizer (Mouse Jitter) production status | README.md:1167 :: CUSUM Hover Stabilizer (Mouse Jitter) | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-core::hover_stabilizer::default_config_values | proven | 2026-09-19 |
| S17 | Gesture Recognition State Machine production status | README.md:1182 :: Gesture Recognition State Machine | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-core::gesture::default_config_values | proven | 2026-09-18 |
| S18 | Input Parser (3,200+ Lines) production status | README.md:1208 :: Input Parser (3,200+ Lines) | status | DOC | bd-g00-root-epic-ewths.5.5 | count:crates/ftui-core/src/input_parser.rs is 3,660 lines >= 3,200 | proven | 2026-09-19 |
| S19 | Keybinding System (1,900+ Lines) production status | README.md:1219 :: Keybinding System (1,900+ Lines) | status | DOC | bd-g00-root-epic-ewths.5.5 | count:crates/ftui-core/src/keybinding.rs is 3,796 lines | retracted | 2026-09-19 |
| S20 | Damped Spring Dynamics (Animation System) production status | README.md:1231 :: Damped Spring Dynamics (Animation System) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S21 | Easing Curves + Stagger Distributions production status | README.md:1250 :: Easing Curves + Stagger Distributions | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S22 | Sine Pulse Sequences (Attention Cues) production status | README.md:1272 :: Sine Pulse Sequences (Attention Cues) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S23 | Perceived Luminance (Terminal Background Probe) production status | README.md:1282 :: Perceived Luminance (Terminal Background Probe) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S24 | Jain's Fairness Index (Input Guard) production status | README.md:1292 :: Jain's Fairness Index (Input Guard) | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S25 | E-Graph Layout Optimizer production status | README.md:1453 :: E-Graph Layout Optimizer | status | DOC | bd-g00-root-epic-ewths.5.5 | path:docs/perf/egraph_vs_flex_2026-09-18.md | proven | 2026-09-18 |
| S26 | Text Engine production status | README.md:1480 :: Text Engine | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S27 | Degradation Cascade production status | README.md:1555 :: Degradation Cascade | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-runtime::program::widget_refresh_degradation_essential_only_skips_nonessential; test:ftui-widgets::badge::render_no_styling_drops_configured_style | proven | 2026-09-19 |
| S28 | Formal Cost Models production status | README.md :: Formal Cost Models | status | DOC | bd-g00-root-epic-ewths.5.5 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| S29 | Flake Detection & Sequential FDR Control production status | README.md:1625 :: Flake Detection & Sequential FDR Control | status | DOC | bd-g00-root-epic-ewths.5.5 | cmd:python3 scripts/check_readme_claims.py --experimental-check | retracted | 2026-09-19 |
| S30 | Rough-Path Signatures production status | README.md :: Rough-Path Signatures | status | DOC | bd-g00-root-epic-ewths.5.5 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| S31 | Incremental View Maintenance (IVM) production status | README.md :: Incremental View Maintenance (IVM) | status | DOC | bd-lksq7 | ident:Where it runs: nowhere. There is no propagation engine | retracted | 2026-09-19 |
| S32 | SOS Barrier Certificates production status | README.md:2667 :: SOS Barrier Certificates | status | DOC | bd-g00-root-epic-ewths.5.5 | cmd:python3 scripts/check_readme_claims.py --experimental-check | retracted | 2026-09-19 |
| S33 | S3-FIFO Cache production status | README.md:2694 :: S3-FIFO Cache | status | DOC | bd-g00-root-epic-ewths.5.5 | test:ftui-core::s3_fifo::scan_resistance | proven | 2026-09-19 |
| S34 | Flat Combining production status | README.md :: Flat Combining | status | DOC | bd-g00-root-epic-ewths.5.5 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| S35 | Bidirectional Lenses production status | README.md :: Bidirectional Lenses | status | DOC | bd-g00-root-epic-ewths.5.5 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| S36 | Input Macro Recording & Playback production status | README.md:2770 :: Input Macro Recording & Playback | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S37 | State Persistence production status | README.md:2791 :: State Persistence | status | DOC | bd-g00-root-epic-ewths.5.5 | bead:bd-g00-root-epic-ewths.5.5 | pending-doc | - |
| S38 | SLO Schema & Breach Detection production status | README.md :: SLO Schema & Breach Detection | status | DOC | bd-g00-root-epic-ewths.5.5 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| S39 | Multi-Stage Conformal Monitoring production status | README.md :: Multi-Stage Conformal Monitoring | status | DOC | bd-g00-root-epic-ewths.5.5 | cmd:python3 scripts/check_readme_claims.py --experimental-check; manual:2026-09-19:CrimsonElk | retracted | 2026-09-19 |
| N01 | Locale context propagated through runtime (ProgramConfig::with_locale, LocaleContext::direction()) | README.md :: Locale context | api | CODE | bd-g00-root-epic-ewths.34.1 | test:ftui-runtime::program::tests::frame_text_direction_follows_locale_context | proven | 2026-09-18 |
| N02 | Text direction from locale with per-line UAX#9 reordering (bidi integrated) | README.md :: Text direction | api | CODE | bd-g00-root-epic-ewths.34.1 | test:ftui-widgets::paragraph::tests::paragraph_rtl_visual_order_matches_unicode_bidi; test:ftui-demo-showcase::screen_snapshots::i18n_demo_arabic_rtl_80x24 | proven | 2026-09-18 |
| N03 | i18n_demo switches live between seven languages (EN/ES/FR/DE/RU/AR/JA) | README.md :: `i18n_demo` screen switches live between English, Spanish, French, German, Russian, Arabic and Japanese | example | CODE | bd-g00-root-epic-ewths.34.1 | test:ftui-demo-showcase::screens::i18n_demo::tests::german_catalog_coverage_is_complete; test:ftui-demo-showcase::screens::i18n_demo::tests::locales_list_has_seven_languages | proven | 2026-09-18 |
| N04 | Number/date formatting in ftui-i18n | README.md :: Number & date formatting backed by pinned Unicode CLDR v45.0 data | api | CODE | bd-g00-root-epic-ewths.34.6 | test:ftui-demo-showcase::tests::i18n_e2e::formatting_numbers_all_seven_locales; test:ftui-demo-showcase::tests::i18n_e2e::formatting_dates_and_times_all_seven_locales; test:ftui-i18n::tests::proptest_i18n_invariants::number_format_int_never_panics | proven | 2026-09-19 |
| N05 | String catalog with fallback chains and CLDR-style plural rules (plural rules) | README.md :: String catalog with fallback chains and CLDR-style plural rules | api | CODE | bd-g00-root-epic-ewths.34 | test:ftui-i18n::plural::locale_detection; test:ftui-i18n::plural::russian_complex_rules; test:ftui-i18n::plural::arabic_full_categories | proven | 2026-09-18 |

## Historical decision details

The proposed decisions are not final owner approval (`.5.1` remains open).
C rows retain the exact historical claim above; these details retain mixed
CODE/DOC decisions and all secondary owner aliases from the source table.

- C01: CODE (convenience methods). Historical owners: `g17-09-impl`.
- C02: CODE (`pub type Layout = Flex` + constructor taking constraints). Historical owners: `g17-09-impl`.
- C03: DOC (document `FocusId`, `insert`, `connect`). Historical owners: `g06-docs-readme`.
- C04: DOC (`Dialog::confirm`). Historical owners: `g06-docs-readme`.
- C05: DOC (`register_link`, `with_link`). Historical owners: `g06-docs-readme`.
- C06: DOC (draw the real layouts). Historical owners: `g06-docs-readme`.
- C07: DOC + quarantine (TimeTravel backs the `snapshot_player` scrubber per G07). Historical owners: DOC `g06-docs-readme`; consumer/quarantine `g07-impl`.
- C08: DOC (`StyleSheet::define`) + CODE consumer (`Block::styled`, `Table::with_stylesheet`). Historical owners: DOC `g06-docs-readme`; CODE `g17-08-impl`.
- C09: CODE. Historical owners: `g17-07-impl`.
- C10: DOC (state the real count) unless G17.6 adds `Custom`/`Dashed` (plan default: add them, README states the resulting count). Historical owners: `g17-06-impl` (count updated in `g06-docs-readme` after it lands).
- C11: DOC (`Cmd::task`). Historical owners: `g06-docs-readme`.
- C12: CODE. Historical owners: `g15-impl`.
- C13: CODE. Historical owners: `g16-impl`.
- C14: DOC. Historical owners: `g06-docs-readme`.
- C15: DOC. Historical owners: `g06-docs-readme`.
- C16: DOC + quarantine. Historical owners: DOC `g06-docs-readme`; quarantine `g07-impl`.
- C17: DOC + quarantine. Historical owners: DOC `g06-docs-readme`; quarantine `g07-impl`.
- C18: DOC for existing names; CODE for `voi_sample`. Historical owners: DOC `g06-docs-readme`; CODE `g20-impl`.
- C19: DOC. Historical owners: `g06-docs-readme`.
- C20: CODE. Historical owners: `g15-impl`.
- C21: CODE. Historical owners: `g17-01-impl` (indeterminate), `g17-02-impl` (JsonView), `g17-03-impl` (syntax hook), `g17-04-impl` (history), `g17-05-impl` (Sparkline).
- C22: DOC. Historical owners: `g06-docs-readme`.
- C23: DOC (45, 6, real slugs). Historical owners: `g06-docs-readme`.
- C24: DOC. Historical owners: `g06-docs-readme`.
- C25: DOC (state the real formulas). Historical owners: `g06-docs-readme`.
- C26: CODE partial (direction via bidi, German) + DOC (retract formatting). Realized by N01–N05. Historical owners: CODE `g29-impl`; DOC `g06-docs-readme`.
- N01–N05: G29 i18n delivery and limits. N01 (locale context), N02 (bidi integrated in Paragraph and editors), N03 (seven demo languages: EN/ES/FR/DE/RU/AR/JA), N04 (CLDR v45.0 number/date formatting across all 7 declared locales), N05 (CLDR plural rules) proven by tests. Delivered under `bd-g00-root-epic-ewths.34.5` and verified under `bd-g00-root-epic-ewths.34.6`.
- C27: regenerate from the perf-gate artifact. Historical owners: `g25-impl`.
- C28: DOC (name the real tests). Historical owners: `g06-docs-readme`.
- C29: DOC. Historical owners: `g06-docs-readme` (README :507), `g06-docs-agents` (AGENTS.md :272-297).
- C30: CODE. Historical owners: `g05-impl-decstbm-selftest` (text via `g05-docs`).
- C31: DOC (57, listed by name). Historical owners: `g06-docs-readme`.
- C32: DOC (1.05M, with the counting command). Historical owners: `g06-docs-readme`.
- C33: DOC. Historical owners: `g01-docs` (README Installation, getting-started lead lines), `g06-docs-getting-started` (remaining page).
- C34: DOC (+ example rewrite). Historical owners: `g35-docs`.
- C35: DOC (state the real defaults and which struct). Historical owners: `g06-docs-readme`.
- C36: CODE (truthful header) + DOC (README SOS section). Historical owners: header `g21-impl`; README `g06-docs-readme`.
- C37: DOC. Historical owners: `g05-docs` (it lives in the Synchronized Output area).

## Historical vision observations

These are September 1 observations, not current verification results.

- V01: WORKING (identity-gated). Historical evidence: DECSTBM + sync under kitty/ghostty; overlay elsewhere.
- V02: WORKING. Historical evidence: diff.rs, presenter.rs, proofs in harness.
- V03: WORKING. Historical evidence: TerminalWriter; docs/one-writer-rule.md.
- V04: WORKING (gap: SIGTSTP). Historical evidence: terminal_session.rs:1178,1197; ftui-tty RawModeGuard; empirical.
- V05: PARTIAL. Historical evidence: facade defaults cannot open a terminal.
- V06: PARTIAL. Historical evidence: 57 production types.
- V07: WORKING. Historical evidence: pane.rs, layout_lab.rs.
- V08: PARTIAL (host-driven patch producer; never built for wasm32 in CI). Historical evidence: Section 5.2.
- V09: WORKING. Historical evidence: diff_strategy.rs wired in terminal_writer.
- V10: OPT-IN (off). Historical evidence: resize_coalescer.rs:202.
- V11: PARTIAL (inline_auto only). Historical evidence: program.rs:6266.
- V12: PARTIAL: budget.rs has its own; eprocess_throttle DEAD. Historical evidence: agent 1 item 9.
- V13: OPT-IN (None by default). Historical evidence: program.rs:3008.
- V14: DEAD (`conformal_stages` unreferenced). Historical evidence: agent 1.
- V15: DEAD / demo-only. Historical evidence: alloc_budget doc-ref only; hover in mouse_playground.
- V16: UNREFERENCED. Historical evidence: alpha_investing.rs.
- V17: DEAD. Historical evidence: only proptest file.
- V18: UNREFERENCED. Historical evidence: rough_path.rs.
- V19: DEAD + provenance false. Historical evidence: no script; hand-typed coeffs.
- V20: DEAD. Historical evidence: width_cache.rs, cache.rs.
- V21: DEAD / NOT COMPILED. Historical evidence: width_cache.rs; countmin_sketch.rs orphan.
- V22: UNREFERENCED. Historical evidence: flat_combine.rs.
- V23: WRONG_API / DEAD. Historical evidence: lens.rs.
- V24: DEAD. Historical evidence: ivm.rs. Re-verified 2026-09-19 and still dead:
  no propagation engine, only `std::fmt`/`std::hash` imported, and the four
  operators the README diagram named (`StyleMap`, `TextWrap`, `FlexSolve`,
  `RenderPlan`) exist nowhere. Claim retracted in both README.md and ivm.rs;
  building the engine is bd-lksq7.
- V25: WRONG_API / DEAD. Historical evidence: slo.rs.
- V26: WORKING (API names wrong). Historical evidence: state_persistence.rs, program.rs:3224.
- V27: WORKING (player API wrong). Historical evidence: input_macro.rs.
- V28: WORKING (`checksum` name wrong). Historical evidence: simulator.rs.
- V29: WORKING (light use). Historical evidence: frame.rs:470; only input.rs + dashboard use it.
- V30: WORKING (bit layout wrong in README). Historical evidence: cell.rs:34-48.
- V31: WORKING (identity-gated). Historical evidence: Section 2.2.D.
- V32: WORKING (`perform`, `tick_every`, `file_watcher` missing). Historical evidence: program.rs.
- V33: WORKING. Historical evidence: 20/20 crates forbid; ftui-core `cfg_attr(not(test))`.
- V34: WORKING (file names differ). Historical evidence: harness render_no_flicker_proof.rs.
- V35: WORKING; bench numbers unbacked. Historical evidence: proptest files; 419 snaps.
- V36: WORKING (delays 16/40 ms not 200/20). Historical evidence: resize_coalescer.rs:194.
- V37: WORKING (level names wrong). Historical evidence: budget.rs.
- V38: WORKING. Historical evidence: input_fairness.rs, program.rs:5676.
- V39: PARTIAL / WRONG_API. Historical evidence: table_theme.rs.
- V40: WRONG_API / no consumer. Historical evidence: stylesheet.rs.
- V41: WRONG_API. Historical evidence: frame.rs, ftui-layout lib.rs.
- V42: WORKING (API wrong). Historical evidence: link_registry.rs, presenter OSC 8.
- V43: WORKING (API wrong). Historical evidence: focus/manager.rs.
- V44: WORKING (API wrong). Historical evidence: modal/stack.rs.
- V45: DEAD (no consumer), API wrong. Historical evidence: time_travel.rs.
- V46: DEAD (never built at runtime). Historical evidence: ftui-a11y; no callers.
- V47: PARTIAL (catalog + plurals only). Historical evidence: ftui-i18n.
- V48: OPT-IN. Historical evidence: program.rs:3884.
- V49: WORKING (Hybrid == ScrollRegion). Historical evidence: inline_mode.rs:93-107.
- V50: WORKING. Historical evidence: color.rs, ansi.rs.
- V51: PARTIAL (names differ; `voi_sample` never written). Historical evidence: agent 1 item 6.
- V52: PARTIAL (Asupersync falls back; Shadow is a label). Historical evidence: program.rs:2734, 4909.
- V53: WORKING. Historical evidence: effect_system.rs.
- V54: PARTIAL (constants unused; literals match). Historical evidence: telemetry_schema.rs.
- V55: DEAD. Historical evidence: egraph.rs.
- V56: WORKING (ropey wrapper). Historical evidence: rope.rs, textarea.
- V57: PARTIAL. Historical evidence: editor.rs.
- V58: DEAD (real controller is budget.rs). Historical evidence: degradation_cascade.rs.
- V59: DEAD. Historical evidence: cost_model.rs.
- V60: DEAD. Historical evidence: gesture.rs.
- V61: WORKING (APC/SOS/PM as Alt introducers; no 1016 pixel mouse). Historical evidence: input_parser.rs.
- V62: NOT_STARTED as described. Historical evidence: keybinding.rs.
- V63: WORKING. Historical evidence: animation/.
- V64: VERIFIED in production with CapabilityLedger and capability_decision evidence (ewths.19). Historical evidence: caps_probe.rs.
- V65: WRONG (45; names). Historical evidence: app.rs.
- V66: WORKING (getting-started contradicts). Historical evidence: crates.io.
- V67: PARTIAL. Historical evidence: docs/WINDOWS.md; Section 5.
- V68: see Section 5 (daily CI failure). Historical evidence: daily CI failure.
- V69: WRONG (no .rs files). Historical evidence: tests/.
- V70: PARTIAL locally, RED in CI. Historical evidence: Section 1.
- V71: WORKING (after fc67ab6e). Historical evidence: git.
