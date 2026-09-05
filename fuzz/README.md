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

## Notes

- `fuzz/target/` contains build artifacts (ignored by git).
- `fuzz/artifacts/` contains crash reproducers (ignored by git).
- `scripts/fuzz_campaign_e2e.sh` emits JSONL records with a run ID,
  correlation IDs, target log paths, artifact paths, and replay commands.
- Set `FUZZ_INJECT_CRASH_TARGET=<target>` and
  `FUZZ_ARTIFACT_ROOT=/tmp/frankentui-fuzz-artifacts` to validate the deliberate
  failure-injection path without polluting the repository.
- `fuzz_grid_mutations` remains inactive because it targets the removed
  frankenterm-core grid/cursor API.
