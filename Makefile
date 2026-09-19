# FrankenTUI Makefile
#
# This Makefile provides convenient targets for building and developing FrankenTUI.
# The reference libraries are automatically synchronized before builds.

.PHONY: all build check test clean sync-refs setup help clippy fmt-check reachability claims

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

# Check compilation without producing binaries
check: sync-refs
	@if [ -f Cargo.toml ]; then cargo check --all-targets; else echo "Note: Cargo.toml not yet created"; fi

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
	@python3 scripts/check_readme_claims.py --experimental-check

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
	@echo "  make check      - Check compilation"
	@echo "  make test       - Run tests"
	@echo "  make clippy     - Run clippy lints"
	@echo "  make fmt-check  - Check formatting"
	@echo "  make reachability - Fail on pub modules nothing references"
	@echo "  make claims     - Validate the claims ledger and experimental sections"
	@echo "  make clean      - Clean build artifacts"
	@echo "  make help       - Show this help"
