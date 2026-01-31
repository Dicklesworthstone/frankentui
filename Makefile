# FrankenTUI Makefile
#
# This Makefile provides convenient targets for building and developing FrankenTUI.
# The reference libraries are automatically synchronized before builds.

.PHONY: all build check test clean sync-refs setup help

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
	@echo "  make clean      - Clean build artifacts"
	@echo "  make help       - Show this help"
