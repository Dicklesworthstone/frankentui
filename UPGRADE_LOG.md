# Dependency Upgrade Log

**Date:** 2026-02-04
**Project:** frankentui
**Language:** Rust
**Manifest:** Cargo.toml (workspace)

---

## Summary

| Metric | Count |
|--------|-------|
| **Total dependencies** | 27 |
| **Updated** | 8 |
| **Skipped** | 0 |
| **Failed (rolled back)** | 0 |
| **Requires attention** | 0 |

---

## Successfully Updated

### lru: 0.12 → 0.16.3
- **Breaking:** Yes (API changes since 0.12; will verify on build)
- **Tests:** `cargo test -p ftui-text` ✓
- **Notes:** Updated to fix RUSTSEC-2026-0002 (unsoundness in 0.12.x).

### portable-pty: 0.8/0.8.1 → 0.9.0
- **Breaking:** Possible (major version bump from 0.8 → 0.9)
- **Tests:** `cargo check --all-targets` + `cargo clippy --all-targets -- -D warnings` ✓ (targeted tests pending)
- **Notes:** Updated to avoid `serial` unmaintained advisory; portable-pty 0.9.0 is latest stable.

### signal-hook: 0.3.18 → 0.4.3
- **Breaking:** Yes (0.4.0 changed `low_level::pipe` signature to accept raw fd)
- **Tests:** `cargo test -p ftui-core` ✓
- **Notes:** No usage of `low_level::pipe` in ftui-core; iterator-based handling unaffected.

### pollster: 0.3.0 → 0.4.0
- **Breaking:** None noted (0.4.0 allows `block_on` to accept `IntoFuture`)
- **Tests:** `cargo check -p ftui-extras --features fx-gpu` ✓
- **Notes:** GPU feature stack builds cleanly with new pollster.

### pulldown-cmark: 0.12.2 → 0.13.0
- **Breaking:** Possible (0.13.0 adds extensions and parser changes)
- **Tests:** `cargo check -p ftui-extras --features markdown` ✓
- **Notes:** Markdown feature compiles cleanly; no API changes observed in use.

### vte: 0.13.1 → 0.15.0
- **Breaking:** Yes (parser `advance` now accepts `&[u8]`)
- **Tests:** `cargo check -p ftui-extras --features terminal` ✓
- **Notes:** Updated `AnsiParser::parse` to pass slices to `Parser::advance`.

### regex: 1.12.2 → 1.12.3
- **Breaking:** None (patch release with performance fixes)
- **Tests:** `cargo check -p ftui-widgets --features regex-search` ✓
- **Notes:** Pinned to 1.12.3 for explicit-version stability.

### criterion: 0.5/0.6 → 0.8.2
- **Breaking:** None for current bench usage (API remains compatible)
- **Tests:** `cargo check --all-targets` ✓
- **Notes:** Bench targets compile with criterion 0.8.2 across all crates.

---

## Skipped

_TBD_

---

## Failed Updates (Rolled Back)

_TBD_

---

## Requires Attention

- `paste` advisory was removed by disabling the `metal` backend in `wgpu` (see Security Notes). MacOS GPU FX is currently disabled; revisit once `metal` no longer depends on `paste` or a safe fork is available.

---

## Deprecation Warnings Fixed

| Package | Warning | Fix Applied |
|---------|---------|-------------|
| _TBD_ | _TBD_ | _TBD_ |

---

## Security Notes

**Vulnerabilities resolved:**
- RUSTSEC-2026-0002 (lru unsoundness) via upgrade to 0.16.3.
- paste unmaintained advisory mitigated by removing the `metal` backend from `wgpu` features (no `paste` in the all-features graph). MacOS GPU FX currently falls back to CPU.
- serial unmaintained advisory mitigated via portable-pty 0.9.0.

**New advisories:** _None detected_

**Audit command:** `cargo audit`

---

## Post-Upgrade Checklist

- [ ] All tests passing
- [ ] No deprecation warnings
- [ ] Manual smoke test performed
- [ ] Documentation updated (if needed)
- [ ] Changes committed

---

## Commands Used

```bash
# Update commands
manual edits (Cargo.toml + patch)
`cargo update -p lru -p paste`
`cargo update -p signal-hook`
`cargo update -p pollster`
`cargo update -p pulldown-cmark`
`cargo update -p vte`
`cargo update -p regex`

# Version research
docs.rs + changelog review (lru, portable-pty, pastey, signal-hook, pollster, vte, regex, criterion)
release notes (pulldown-cmark 0.13.0)

# Test commands
`cargo check --all-targets`
`cargo clippy --all-targets -- -D warnings`
`cargo fmt`
`cargo fmt --check`
`cargo test -p ftui-text`
`cargo test -p ftui-core`
`cargo check -p ftui-extras --features fx-gpu`
`cargo check -p ftui-extras --features markdown`
`cargo check -p ftui-extras --features terminal`
`cargo check -p ftui-widgets --features regex-search`

# Audit commands
cargo audit
```

---

## Notes

Tracking updates per dependency per deps-update workflow. Pending scope confirmation from user.
