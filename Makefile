# FrankenTUI Makefile
#
# This Makefile provides convenient targets for building and developing FrankenTUI.
# The reference libraries are automatically synchronized before builds.

.PHONY: all build check test clean sync-refs setup help clippy fmt-check reachability claims gates env-docs

# Default target
all: build

# Synchronize reference libraries before any build
sync-refs:
	@./scripts/pull_latest_reference_library_repos.sh

# Setup: sync refs (run this first on a fresh clone)
setup: sync-refs
	@echo "Setup complete. Reference libraries synchronized."

# Build the project (syncs refs first)
build: sync-refs
	@echo "Building FrankenTUI..."
	@if [ -f Cargo.toml ]; then cargo build; else echo "Note: Cargo.toml not yet created"; fi

# Check compilation without producing binaries, then the truth gates.
#
# The gates run here on purpose. AGENTS.md has listed them as mandatory after
# any substantive change since they were written, but nothing invoked them:
# they existed only as prose and as Make targets you had to remember. Six
# seconds of pure-stdlib Python is cheap enough that the obvious entry point
# should just run them. `make gates` alone skips the cargo work.
check: sync-refs gates
	@if [ -f Cargo.toml ]; then cargo check --all-targets; else echo "Note: Cargo.toml not yet created"; fi

# Every non-cargo correctness gate, about six seconds total. No compilation,
# no network, stdlib only, so this is safe to run constantly.
gates: reachability claims env-docs close-audit baseline-benches
	@echo "gates: reachability, claims, env-docs, close-audit and baseline-benches all green"

# Fail when an environment variable is read by the code but undocumented.
env-docs:
	@python3 scripts/check_env_docs.py > /dev/null

# Run tests
test: sync-refs
	@if [ -f Cargo.toml ]; then cargo test; else echo "Note: Cargo.toml not yet created"; fi

# Run clippy lints
clippy: sync-refs
	@if [ -f Cargo.toml ]; then cargo clippy --all-targets -- -D warnings; else echo "Note: Cargo.toml not yet created"; fi

# Format check
fmt-check:
	@if [ -f Cargo.toml ]; then cargo fmt --check; else echo "Note: Cargo.toml not yet created"; fi

# Fail when a pub module is reachable from nothing in production.
# Pure stdlib Python and about two seconds, so it belongs in every DSR
# verification run alongside clippy. GitHub Actions is not used in this
# project (AGENTS.md, owner override 2026-09-06).
reachability:
	@python3 scripts/check_module_reachability.py --quiet --json target/module-reachability.json

# Fail when a README section marked "Status: experimental" does not say where
# its module runs, or when a quarantined module gains a production consumer
# while the README still says it has none. On 2026-09-19 all ten such sections
# described modules no crate imports, in working present tense.
claims:
	@python3 scripts/check_readme_claims.py --schema-check
	@python3 scripts/check_readme_claims.py --proof-refs
	@python3 scripts/check_readme_claims.py --experimental-check

# Fail when a bead was closed without evidence anyone can follow. `br` enforces
# `.beads/policy.yaml` at close time; this catches what that cannot see -- a
# `--bypass-policy` close, or a status written straight into the JSONL. Only
# closes after the policy landed count toward the exit code; the 2,899 older
# ones are reported so the scale stays visible without making the gate
# permanently red. Widen with `--epoch` to inspect history:
#   python3 scripts/check_close_evidence.py --epoch 2026-09-01
close-audit:
	@python3 scripts/check_close_evidence.py --quiet

# Fail when a tests/baseline.json row binds an SLO budget to a criterion id no
# benchmark emits. `scripts/perf_regression_gate.sh` is fail-closed, so one such
# row makes the whole perf gate permanently INCOMPLETE (exit 3) whatever the
# real performance is — this catches it at edit time instead of after a full
# bench run. Known-unresolved ids live in docs/baseline-bench-exceptions.txt,
# each against the bead that will fix it.
baseline-benches:
	@python3 scripts/check_baseline_benches.py --quiet

# Clean build artifacts
clean:
	@if [ -f Cargo.toml ]; then cargo clean; fi
	@echo "Cleaned build artifacts"

# Help
help:
	@echo "FrankenTUI Makefile targets:"
	@echo "  make setup      - Initial setup: sync reference libraries"
	@echo "  make sync-refs  - Pull latest reference library code"
	@echo "  make build      - Build the project (syncs refs first)"
	@echo "  make check      - Check compilation, then run the gates"
	@echo "  make test       - Run tests"
	@echo "  make clippy     - Run clippy lints"
	@echo "  make fmt-check  - Check formatting"
	@echo "  make reachability - Fail on pub modules nothing references"
	@echo "  make claims     - Validate the claims ledger and experimental sections"
	@echo "  make gates      - All non-cargo gates (~6s): reachability, claims, env-docs"
	@echo "  make clean      - Clean build artifacts"
	@echo "  make help       - Show this help"
