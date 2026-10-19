# Fuzzing

This directory contains cargo-fuzz targets for FrankenTUI.

## Targets

| Target | Crate | What it fuzzes |
|--------|-------|---------------|
| `fuzz_input_parser` | ftui-core | Raw byte input parser (no-panic) |
| `fuzz_input_parser_structured` | ftui-core | Structured input events via Arbitrary |
| `fuzz_input_parser_long_seq` | ftui-core | Long input sequences |
| `fuzz_vt_parser` | ftui-pty | Virtual terminal state machine, including whole-stream vs chunked feeds |
| `fuzz_text_cluster_map` | ftui-text | Grapheme cluster mapping |
| `fuzz_text_shaped_layout` | ftui-text | Shaped text layout pipeline |
| `fuzz_text_wrap` | ftui-text | Text wrapping (Word/Char/WordChar/Optimal) |
| `fuzz_text_width` | ftui-text | Display width calculations |
| `fuzz_text_hyphenation` | ftui-text | Hyphenation break points |
| `fuzz_layout_constraints` | ftui-layout | Constraint solver (Flex split) |
| `fuzz_widget_render` | ftui-widgets | Widget rendering (Block/Paragraph/Sparkline/ProgressBar) |
| `fuzz_sanitize_sgr_only` | ftui-render | Log sanitizer idempotence, raw passthrough, and bounded SGR-only output |

## Run

Run these commands from the repository or this directory; `rust-toolchain.toml`
selects the dated nightly used by CI. The campaign script also requires Python
3.11+ to parse that same pin.

```bash
# Run a single target
cargo fuzz run fuzz_layout_constraints -- -max_len=256
cargo fuzz run fuzz_widget_render -- -max_len=512
cargo fuzz run fuzz_text_wrap -- -max_len=512
cargo fuzz run fuzz_vt_parser -- -max_len=4096

# Run all targets briefly (smoke test)
for target in $(cargo fuzz list); do
  echo "--- $target ---"
  cargo fuzz run "$target" -- -max_total_time=10 -max_len=512
done
```

## Cadence & DSR Verification

Scheduled and on-demand fuzz campaigns run on the native DSR host via the
`frankentui-fuzz-campaign` tool in `.config/dsr-quality.yaml` (invoking
`scripts/fuzz_campaign_e2e.sh`):

```bash
# Run the DSR fuzz campaign (default 60s per target; customizable via FUZZ_DURATION_SECS)
DSR_REPOS_FILE="$PWD/.config/dsr-quality.yaml" dsr quality --tool frankentui-fuzz-campaign -w "$PWD"

# Or invoke the campaign script directly with a retained log directory
RCH_CARGO_WRAPPER_BYPASS=1 FUZZ_DURATION_SECS=60 LOG_DIR=/data/tmp/fuzz_campaign_$(date +%s) ./scripts/fuzz_campaign_e2e.sh
```

Each campaign run:
- Builds all 12 declared `[[bin]]` targets using the pinned nightly toolchain from `rust-toolchain.toml` and `CARGO_BUILD_FINGERPRINT=content` (`-Z checksum-freshness`).
- Executes the retained libFuzzer binaries directly against `fuzz/corpus/<target>` so corpus inputs accumulate across runs without invoking `cargo fuzz run`'s deleting temporary-file wrapper.
- Writes structured JSONL events (`env`, `target_start`, `target_end` with `runs`, `edges`, `peak_rss_mb`, `crashes`, `corpus_size`, and `run_end`) to `$LOG_DIR/fuzz_campaign_e2e.jsonl`, plus per-target corpus counts and sizes in `$LOG_DIR/corpus_stats.txt`.
- Retains any crash/oom/timeout/leak inputs under `$FUZZ_ARTIFACT_ROOT/<target>/<run_id>/` and exits non-zero if any target crashes or fails to complete its requested duration.

## Standalone Workspace

`fuzz/Cargo.toml` declares its own `[workspace]` because the root `Cargo.toml`
excludes `fuzz/`. It mirrors `[workspace.lints.clippy]` from the root manifest
and inherits `[lints] workspace = true` locally; `frankentui-truth-gates` checks
that the two lint tables stay in sync.

## Notes

- `fuzz/target/` contains build artifacts (ignored by git).
- `fuzz/artifacts/` contains crash reproducers (ignored by git).
- `fuzz/corpus/` contains persistent per-target corpus inputs across campaigns (ignored by git).
- `scripts/fuzz_campaign_e2e.sh` emits JSONL records with a run ID,
  correlation IDs, target log paths, corpus sizes, artifact paths, and replay commands.
- Set `FUZZ_INJECT_CRASH_TARGET=<target>` and
  `FUZZ_ARTIFACT_ROOT=/tmp/frankentui-fuzz-artifacts` to validate the deliberate
  failure-injection path without polluting the repository.
- `fuzz_grid_mutations` remains inactive because it targets the removed
  frankenterm-core grid/cursor API.
