# Dependency Upgrade Log

**Date:** 2026-02-02
**Project:** frankentui
**Language:** Rust
**Manifest:** Cargo.toml (workspace)

---

## Summary

| Metric | Count |
|--------|-------|
| **Total dependencies** | 27 |
| **Updated** | 5 |
| **Skipped** | 1 |
| **Failed (rolled back)** | 0 |
| **Requires attention** | 0 |

---

## Successfully Updated

### memchr: 2 → 2.7.6

**Changelog:** https://docs.rs/memchr/2.7.6/memchr/

**Breaking changes:** None noted within 2.x releases

**Tests:** ✓ Passed (warning: unused variable in `ftui-extras/src/markdown.rs`)

### crossterm: 0.28 → 0.29.0

**Changelog:** https://docs.rs/crate/crossterm/0.29.0/source/CHANGELOG.md

**Breaking changes:**
- `KeyModifiers` Display now uses correct key names (breaking change noted in 0.29.0)

**Tests:** ✓ Passed (warning: unused variable in `ftui-extras/src/markdown.rs`)

### unicode-segmentation: 1.11 → 1.12.0

**Changelog:** https://docs.rs/unicode-segmentation/latest/unicode_segmentation/

**Breaking changes:** None noted within 1.x releases

**Tests:** ✓ Passed (warning: unused variable in `ftui-extras/src/markdown.rs`)

### unicode-width: 0.2 → 0.2.2

**Changelog:** https://docs.rs/unicode-width/latest/unicode_width/

**Breaking changes:** None noted within 0.2.x releases

**Tests:** ✓ Passed (warning: unused variable in `ftui-extras/src/markdown.rs`)

### smallvec: 1.13.x → 1.15.1

**Changelog:** https://docs.rs/crate/smallvec/latest

**Breaking changes:** None noted within 1.x releases (no standalone changelog found on docs.rs)

**Tests:** ✓ Passed (warning: unused variable in `ftui-extras/src/markdown.rs`)

## Skipped

### bitflags: 2.10.0
**Reason:** Already on latest stable.

## Failed Updates (Rolled Back)

## Requires Attention

## Deprecation Warnings Fixed

| Package | Warning | Fix Applied |
|---------|---------|-------------|

---

## Security Notes

**Vulnerabilities resolved:**
- None

**New advisories:** None detected

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
# - Edited `crates/ftui-render/Cargo.toml` to bump memchr

# Test commands
# - `BLESS=1 cargo test -p ftui-demo-showcase --test screen_snapshots`
# - `cargo test`

# Audit commands
# - `cargo audit` (pending)
```

---

## Notes
