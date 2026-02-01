# Fuzzing

This directory contains cargo-fuzz targets for FrankenTUI.

## Run

```bash
cargo +nightly fuzz run fuzz_input_parser -- -max_len=4096
cargo +nightly fuzz run fuzz_input_parser_structured -- -max_len=4096
cargo +nightly fuzz run fuzz_input_parser_long_seq -- -max_len=4096
```

## Notes

- `fuzz/target/` contains build artifacts (ignored by git).
- `fuzz/artifacts/` contains crash reproducers (ignored by git).
- Consider checking in a minimal `fuzz/corpus/` once seeds are curated.
