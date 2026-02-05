# E2E Coverage Matrix and Artifact Checklist

This document is the authoritative matrix for demo screen coverage plus the
artifact checklist required for every E2E screen/flow. The goal is
deterministic, audit-friendly runs where every suite records its output paths
in JSONL, and CI fails when required artifacts are missing.

Screen numbering is 1-indexed and must match the CLI help list in
`crates/ftui-demo-showcase/src/cli.rs`. If a script hard-codes screen numbers,
keep it in sync with the registry and update this matrix.

## Demo Screen Coverage Matrix (v1)

Legend:
- Coverage notes reference demo showcase scripts unless stated otherwise.
- "Sweep" means the screen starts without panic (no interaction flow).
- Gaps list the next concrete fix or the script that needs updating.

| # | ScreenId | Category | Current E2E Coverage | Gaps / Notes |
| --- | --- | --- | --- | --- |
| 1 | GuidedTour | Tour | `scripts/e2e_demo_tour.sh` (guided tour JSONL); `scripts/demo_showcase_e2e.sh` sweep | OK |
| 2 | Dashboard | Tour | `scripts/demo_showcase_e2e.sh` core nav (keys `cemg`); sweep | OK |
| 3 | Shakespeare | Text | `scripts/demo_showcase_e2e.sh` search test + sweep | OK |
| 4 | CodeExplorer | Text | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic keyflow |
| 5 | WidgetGallery | Core | `scripts/demo_showcase_e2e.sh` data-viz group | OK |
| 6 | LayoutLab | Core | `scripts/demo_showcase_e2e.sh` core nav (keys `2d+`) | OK |
| 7 | FormsInput | Interaction | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic input flow |
| 8 | DataViz | Visuals | `scripts/demo_showcase_e2e.sh` data-viz group | OK |
| 9 | FileBrowser | Interaction | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic navigation flow |
| 10 | AdvancedFeatures | Core | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic keyflow |
| 11 | TableThemeGallery | Visuals | `scripts/demo_showcase_e2e.sh` data-viz group | OK |
| 12 | TerminalCapabilities | Systems | `scripts/demo_showcase_e2e.sh` terminal caps report + sweep | OK |
| 13 | MacroRecorder | Interaction | `scripts/demo_showcase_e2e.sh` sweep | Add record/replay flow |
| 14 | Performance | Systems | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic perf panel flow |
| 15 | MarkdownRichText | Text | `scripts/demo_showcase_e2e.sh` editors group | OK |
| 16 | VisualEffects | Visuals | `scripts/demo_showcase_e2e.sh` VFX backdrop + VFX sweep | OK |
| 17 | ResponsiveDemo | Core | `scripts/demo_showcase_e2e.sh` sweep | Add resize/breakpoint flow |
| 18 | LogSearch | Text | `scripts/demo_showcase_e2e.sh` editors group | OK |
| 19 | Notifications | Interaction | `scripts/demo_showcase_e2e.sh` core nav (keys `s`) | OK |
| 20 | ActionTimeline | Systems | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic action timeline flow |
| 21 | IntrinsicSizing | Core | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic sizing flow |
| 22 | LayoutInspector | Core | `scripts/demo_showcase_e2e.sh` layout inspector step + sweep | OK |
| 23 | AdvancedTextEditor | Text | `scripts/demo_showcase_e2e.sh` editors group | OK |
| 24 | MousePlayground | Interaction | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic mouse script |
| 25 | FormValidation | Interaction | `scripts/demo_showcase_e2e.sh` sweep | Add validation flow |
| 26 | VirtualizedSearch | Systems | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic search flow |
| 27 | AsyncTasks | Systems | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic task flow |
| 28 | ThemeStudio | Visuals | `scripts/demo_showcase_e2e.sh` sweep | Add deterministic theme edits |
| 29 | SnapshotPlayer | Visuals | `scripts/demo_showcase_e2e.sh` sweep | Add snapshot playback flow |
| 30 | PerformanceHud | Systems | `scripts/demo_showcase_e2e.sh` core nav (keys `pm`) | OK |
| 31 | I18nDemo | Text | `scripts/demo_showcase_e2e.sh` i18n report + sweep | OK |
| 32 | VoiOverlay | Systems | `scripts/demo_showcase_e2e.sh` VFX sweep | OK |
| 33 | InlineModeStory | Tour | `scripts/demo_showcase_e2e.sh` sweep | Add inline-mode story flow |
| 34 | AccessibilityPanel | Systems | `scripts/demo_showcase_e2e.sh` sweep | Add accessibility toggles flow |
| 35 | WidgetBuilder | Core | `scripts/demo_showcase_e2e.sh` widget builder export + sweep | OK |
| 36 | CommandPaletteLab | Interaction | `scripts/demo_showcase_e2e.sh` editors group; `scripts/command_palette_e2e.sh` | OK |
| 37 | DeterminismLab | Systems | `scripts/demo_showcase_e2e.sh` determinism report + VFX sweep | OK |
| 38 | HyperlinkPlayground | Interaction | `scripts/demo_showcase_e2e.sh` hyperlink JSONL + sweep | OK |

If you add a new E2E suite, add it here and wire its artifact logging
via `jsonl_assert "artifact_<type>" "pass" "path=<path>"`.

## Artifact Checklist (Required)

Every E2E case must record these artifacts in JSONL:

- `artifact_log_dir`: directory containing all logs for the run
- `artifact_jsonl`: the JSONL file path (`$E2E_JSONL_FILE`)
- `artifact_pty_output`: PTY output capture (when PTY is used)
- `artifact_hash_registry`: golden checksum registry file (when used)
- `artifact_snapshot`: snapshot output file (when snapshots are produced)
- `artifact_summary_json`: summary JSON for multi-suite runners (when produced)

Notes:
- `jsonl_assert` automatically emits `artifact` JSONL events and will fail in
  CI/strict mode if a required artifact is missing.
- PTY runs already emit `pty_capture` events with `output_file` and
  `canonical_file`. Still record `artifact_pty_output` for clarity and CI checks.

## Suite-Level Checklist

### Harness PTY Suites (`tests/e2e/scripts/test_*.sh`)

Required artifacts:
- `artifact_log_dir` = `$E2E_LOG_DIR`
- `artifact_jsonl` = `$E2E_JSONL_FILE`
- `artifact_pty_output` = PTY capture output file(s)
- `artifact_hash_registry` = any golden checksum file used by the test

### Demo/Script E2E Suites (`scripts/*` and `scripts/e2e/*`)

Required artifacts:
- `artifact_log_dir` = `$E2E_LOG_DIR` (or suite-specific log dir)
- `artifact_jsonl` = `$E2E_JSONL_FILE` (or suite-specific JSONL)
- `artifact_summary_json` when an aggregate summary is produced
- `artifact_snapshot` for snapshot-producing suites
- `artifact_hash_registry` when golden checksums are used

## Known Suites and Additional Artifacts

This section records extra artifacts per suite beyond the baseline list.

| Suite | Extra Artifacts | Notes |
| --- | --- | --- |
| `scripts/demo_showcase_e2e.sh` | `artifact_env_log`, `artifact_vfx_jsonl`, `artifact_layout_inspector_jsonl`, `artifact_summary_txt` | Demo showcase produces environment log + per-screen JSONL |
| `scripts/e2e_test.sh` | `artifact_summary_json` | PTY runner summary |
| `tests/e2e/scripts/test_golden_resize.sh` | `artifact_hash_registry` | Golden checksum file under `tests/golden_checksums/` |
| `tests/e2e/scripts/test_resize_storm.sh` | `artifact_pty_output` | Emits frame capture + checksum logs |

If a suite emits a checksum (or hash) for determinism, record the path to the
source file used to compute it via `artifact_hash_registry` or `artifact_snapshot`.

## Wiring Guidance

Example usage in a script:

```bash
jsonl_assert "artifact_log_dir" "pass" "log_dir=$E2E_LOG_DIR"
jsonl_assert "artifact_jsonl" "pass" "jsonl=$E2E_JSONL_FILE"
jsonl_assert "artifact_pty_output" "pass" "output=$OUTPUT_FILE"
jsonl_assert "artifact_hash_registry" "pass" "hash_registry=$CHECKSUM_FILE"
```

CI behavior:
- In CI (or with `E2E_JSONL_VALIDATE=1`), missing artifacts fail the run with
  a clear error from `jsonl_assert`.
