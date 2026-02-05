# Dependency Upgrade Log

**Date:** 2026-02-05
**Project:** FrankenTUI
**Language:** Rust
**Manifest:** Cargo.toml

---

## Summary

| Metric | Count |
|--------|-------|
| **Total dependencies** | 38 |
| **Updated** | 14 |
| **Skipped** | 24 |
| **Failed (rolled back)** | 0 |
| **Requires attention** | 0 |

---

## Successfully Updated

### bytemuck: 1.15.0 → 1.25.0
**Notes:** Updated optional dependency in `crates/ftui-extras/Cargo.toml`. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### math-text-transform: 0.1 → 0.1.1
**Notes:** Updated optional dependency in `crates/ftui-extras/Cargo.toml`. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### proptest: 1.x → 1.7.0
**Notes:** Updated dev-dependency across ftui workspace crates (most were `1`, ftui-demo-showcase was `1.4`). `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### serde: 1.x → 1.0.227
**Notes:** Updated across ftui workspace `Cargo.toml` files. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### serde_json: 1.x → 1.0.145
**Notes:** Updated across ftui workspace `Cargo.toml` files. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### tempfile: 3 → 3.22.0
**Notes:** Updated in `crates/ftui-runtime/Cargo.toml`. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### time: 0.3 → 0.3.44
**Notes:** Updated in `crates/ftui-pty/Cargo.toml`. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### tracing: 0.1 → 0.1.41
**Notes:** Updated across ftui workspace `Cargo.toml` files. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### tracing-subscriber: 0.3 → 0.3.20
**Notes:** Updated across ftui workspace `Cargo.toml` files. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### ropey: 1.6 → 1.6.1
**Notes:** Updated in `crates/ftui-text/Cargo.toml`. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### rustc-hash: 2 → 2.1.1
**Notes:** Updated in `crates/ftui-text/Cargo.toml`. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### unicode-bidi: 0.3 → 0.3.18
**Notes:** Updated optional dependency in `crates/ftui-text/Cargo.toml`. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### unicode-normalization: 0.1 → 0.1.24
**Notes:** Updated optional dependency in `crates/ftui-text/Cargo.toml`. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

### tracing-test: 0.2 → 0.2.5
**Notes:** Updated dev-dependency in `crates/ftui-text/Cargo.toml` and `crates/ftui-style/Cargo.toml`. `cargo check`, `cargo clippy`, and `cargo fmt` clean.

---

## Skipped

### base64: 0.22.1 → 0.22.1
**Reason:** Already on latest stable (docs.rs shows 0.22.1).

### bitflags: 2.10.0 → 2.10.0
**Reason:** Already on latest stable (docs.rs shows 2.10.0).

### crossterm: 0.29.0 → 0.29.0
**Reason:** Already on latest stable (docs.rs shows 0.29.0).

### criterion: 0.8.2 → 0.8.2
**Reason:** Already on latest stable (docs.rs shows 0.8.2).

### image: 0.25.9 → 0.25.9
**Reason:** Already on latest stable (docs.rs shows 0.25.9).

### unicode-width: 0.2.2 → 0.2.2
**Reason:** Already on latest stable (docs.rs shows 0.2.2).

### lru: 0.16.3 → 0.16.3
**Reason:** Already on latest stable (changelog shows 0.16.3 latest).

### memchr: 2.7.6 → 2.7.6
**Reason:** Already on latest stable (docs.rs shows 2.7.6).

### opentelemetry: 0.31.0 → 0.31.0
**Reason:** Already on latest stable (docs.rs shows 0.31.0).

### opentelemetry-otlp: 0.31.0 → 0.31.0
**Reason:** Already on latest stable (docs.rs shows 0.31.0).

### opentelemetry_sdk: 0.31.0 → 0.31.0
**Reason:** Already on latest stable (docs.rs shows 0.31.0).

### pollster: 0.4.0 → 0.4.0
**Reason:** Already on latest stable (docs.rs shows 0.4.0).

### portable-pty: 0.9.0 → 0.9.0
**Reason:** Already on latest stable (docs.rs shows 0.9.0).

### pulldown-cmark: 0.13.0 → 0.13.0
**Reason:** Already on latest stable (docs.rs shows 0.13.0).

### regex: 1.12.3 → 1.12.3
**Reason:** Already on latest stable (docs.rs page exists for 1.12.3).

### serial_test: 3.2.0 → 3.2.0
**Reason:** Already on latest stable (docs.rs shows 3.2.0).

### signal-hook: 0.4.3 → 0.4.3
**Reason:** Already on latest stable (crates.io API shows 0.4.3 max_version).

### smallvec: 1.15.1 → 1.15.1
**Reason:** Already on latest stable (docs.rs shows 1.15.1).

### unicode-display-width: 0.3.0 → 0.3.0
**Reason:** Already on latest stable (docs.rs shows 0.3.0).

### unicode-segmentation: 1.12.0 → 1.12.0
**Reason:** Already on latest stable (docs.rs shows 1.12.0).

### unicodeit: 0.2.0 → 0.2.0
**Reason:** Already on latest stable (crates.io shows 0.2.0).

### vte: 0.15.0 → 0.15.0
**Reason:** Already on latest stable (docs.rs shows 0.15.0).

### wgpu: 28.0.0 → 28.0.0
**Reason:** Already on latest stable (docs.rs shows 28.0.0).

### tracing-opentelemetry: 0.32.0 → 0.32.0
**Reason:** Already on latest stable (crates.io API shows 0.32.0 max_version).

---

## Failed Updates (Rolled Back)

_None yet._

---

## Requires Attention

_None yet._

---

## Deprecation Warnings Fixed

_None yet._

---

## Security Notes

**Vulnerabilities resolved:** None detected

**New advisories:** None detected

**Audit command:** `cargo audit`

---

## Post-Upgrade Checklist

- [ ] All tests passing
- [ ] No deprecation warnings
- [ ] Manual smoke test performed (if needed)
- [ ] Documentation updated (if needed)
- [ ] Changes committed

---

## Commands Used

```bash
# Update commands
cargo update -p bytemuck
cargo update -p rustc-hash@2.1.1 -p ropey -p unicode-bidi -p unicode-normalization

# Test commands
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check

# Audit commands
```

---

## Notes

Initial log created. Updates will be appended per dependency.
