# FrankenTUI Release Checklist

This checklist mirrors the fail-closed preflight gate in [`scripts/release_preflight.sh`](../scripts/release_preflight.sh) and the DSR publication scripts [`scripts/release_publish_crates.sh`](../scripts/release_publish_crates.sh) and [`scripts/consumer_smoke_e2e.sh`](../scripts/consumer_smoke_e2e.sh). Every release must pass all 14 preflight checks under `--strict` before tagging or publishing.

## 1. Preconditions

1. **DSR Verification Only:** Per [`AGENTS.md`](../AGENTS.md), GitHub Actions is disabled for this repository. All verification, artifact builds, and publication run on configured native DSR hosts (`dsr quality`, `dsr build`, `dsr release`).
2. **Owner Scope Decisions Resolved:**
   - The 17 publishable library crates (`ftui-i18n`, `ftui-style`, `ftui-simd`, `ftui-layout`, `ftui-a11y`, `ftui-core`, `ftui-pty`, `ftui-tty`, `ftui-text`, `ftui-render`, `ftui-backend`, `ftui-widgets`, `ftui-extras`, `ftui-runtime`, `ftui-web`, `ftui-harness`, `ftui`) are version-locked and published in topological order.
   - `doctor_frankentui`, `ftui-demo-showcase`, and `ftui-showcase-wasm` carry `publish = false` and are repository-only targets.
3. **DSR Quality Gates Green on Candidate Commit:**
   ```bash
   DSR_REPOS_FILE="$PWD/.config/dsr-quality.yaml" dsr quality --tool frankentui-truth-gates -w "$PWD"
   DSR_REPOS_FILE="$PWD/.config/dsr-quality.yaml" dsr quality --tool frankentui-full-gates -w "$PWD"
   ```

## 2. Version Bump & Changelog Procedure

1. **Bump Workspace & Internal Dependency Versions (`X.Y.Z`):**
   - Edit `[workspace.package] version = "X.Y.Z"` in the root `Cargo.toml`.
   - Update every internal `ftui-*` dependency pin in `crates/*/Cargo.toml` to `version = "X.Y.Z"` manually (never via regex replacement scripts, per `AGENTS.md`).
2. **Update Documentation Version References:**
   - Update the `ftui = "=X.Y.Z"` dependency snippets and target version note in [`README.md`](../README.md) (`### Crates.io Dependency`).
   - Update the version references in [`docs/getting-started.md`](getting-started.md).
3. **Add `CHANGELOG.md` Entry:**
   - Add a row at the top of the `## Version Timeline` table in [`CHANGELOG.md`](../CHANGELOG.md):
     ```markdown
     | [vX.Y.Z] | Release | YYYY-MM-DD | Brief capability summary |
     ```
   - Add a matching `## [vX.Y.Z] — YYYY-MM-DD (GitHub Release)` section and update the `[vX.Y.Z]: https://github.com/Dicklesworthstone/frankentui/releases/tag/vX.Y.Z` link definition at the bottom of `CHANGELOG.md`.
4. **Run the Fail-Closed Release Preflight Gate:**
   ```bash
   bash scripts/release_preflight_test.sh
   bash scripts/release_preflight.sh vX.Y.Z --strict --out /tmp/preflight_vX_Y_Z.jsonl
   ```
5. **Commit, Tag, and Sync Branches:**
   ```bash
   git commit -m "chore(release): vX.Y.Z"
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   git push origin main
   git push origin main:master
   git push origin vX.Y.Z
   ```

## 3. Preflight Checks Reference (`scripts/release_preflight.sh`)

Every run emits one JSONL line per check (`{"check":"...","status":"pass|fail|pending:<slug>","detail":"...","elapsed_ms":N}`) and prints a human-readable summary table:

| Check Name | What It Proves | Owner Bead |
|---|---|---|
| `workspace_version` | Root `Cargo.toml` `[workspace.package] version` equals `X.Y.Z` (tag `vX.Y.Z` stripped of `v`). | `bd-g00-root-epic-ewths.10.1` |
| `facade_version` | All 17 publishable crates resolve to `X.Y.Z` and all internal workspace dependency pins match `X.Y.Z`. | `bd-g00-root-epic-ewths.10.1` |
| `readme_version` | Every `ftui = ...` dependency snippet under `### Crates.io Dependency` in `README.md` specifies `X.Y.Z`. | `bd-g00-root-epic-ewths.10.1` |
| `readme_crate_list` | `README.md` lists the exact set of 17 publishable workspace crates (`publish != false`). | `bd-g00-root-epic-ewths.38.1` |
| `getting_started` | `docs/getting-started.md` references `X.Y.Z` and contains no stale claim that only a 3-crate subset is published. | `bd-g00-root-epic-ewths.38.3` |
| `changelog_entry` | `CHANGELOG.md` has both a `## [vX.Y.Z]` section heading and a `\| [vX.Y.Z]` row in `## Version Timeline`. | `bd-g00-root-epic-ewths.10.1` |
| `git_tag_on_main` | Tag `vX.Y.Z` (if already created) or `HEAD` is an ancestor of `origin/main` (`main`). | `bd-g00-root-epic-ewths.10.1` |
| `ci_green_on_commit` | `.config/dsr-quality.yaml` defines the required DSR quality tools (`frankentui-truth-gates`, `frankentui-full-gates`, `frankentui-fuzz-campaign`) and `scripts/release_publish_crates.sh` is executable. | `bd-g00-root-epic-ewths.6.23` |
| `claims_ledger` | `scripts/check_readme_claims.py` passes `--schema-check`, `--proof-refs`, and `--experimental-check`. | `bd-g00-root-epic-ewths.5.3` |
| `compat_matrix` | `docs/compat-matrix.md` exists and has zero failing terminal identity rows. | `bd-g00-root-epic-ewths.22.5` |
| `consumer_smoke` | `scripts/consumer_smoke_e2e.sh --selftest` passes its isolated consumer smoke harness checks. | `bd-g00-root-epic-ewths.10.3` |
| `perf_gate` | `scripts/check_baseline_benches.py` and `tests/baseline.json` pass benchmark-ID and schema validation. | `bd-g00-root-epic-ewths.9.3` |
| `cratesio_state` | Queries crates.io (or `FTUI_PREFLIGHT_CRATESIO_FIXTURE`): `0/17` crates at `X.Y.Z` is ready for fresh publish; `17/17` is recorded as idempotent re-run; `1..16/17` fails closed as partial publication requiring resume investigation. | `bd-g00-root-epic-ewths.6.21` |
| `publish_dry_run` | Resolves the 17-crate topological DAG (`ftui-i18n` through `ftui`) matching `scripts/release_publish_crates.sh`. | `bd-g00-root-epic-ewths.6.21` |

### When a Check Emits `pending:<slug>`

Both default mode and `--strict` are **fail-closed**: any check with status `fail` or `pending:<slug>` causes `scripts/release_preflight.sh` to exit `1`. Never bypass `--strict` for a real release. The `--diagnostic-pending` flag exists solely for local dry-run inspection when auditing which prerequisite gate (`pending:<slug>`) is missing in a synthetic or incomplete checkout.

## 4. Dry-Run & Idempotent Publication via DSR

1. **Registry Inventory & Dry-Run Packaging Verification:**
   ```bash
   # Registry-only check (requires clean working tree and new output directory outside checkout):
   bash scripts/release_publish_crates.sh vX.Y.Z /tmp/publish_registry_vX_Y_Z --registry-only

   # Full dry-run packaging across all 17 crates in topological order:
   bash scripts/release_publish_crates.sh vX.Y.Z /tmp/publish_dry_run_vX_Y_Z --dry-run
   ```
2. **Idempotent Crates.io Publication (Authorized Release Only):**
   - `bash scripts/release_publish_crates.sh vX.Y.Z /tmp/publish_live_vX_Y_Z --publish` checks each crate in topological order against crates.io before running `cargo publish`.
   - If a crate is already at `X.Y.Z`, it downloads the published `.crate` archive and verifies that its contents match the local source checkout (`already_published_verified`) before skipping, making re-runs after transient registry timeouts safe and idempotent.
3. **Post-Publish External Consumer Smoke Verification:**
   ```bash
   bash scripts/consumer_smoke_e2e.sh \
     --mode registry \
     --version "=X.Y.Z" \
     --expected-commit "$(git rev-parse HEAD)" \
     --expected-tag "vX.Y.Z" \
     --output-root /tmp/consumer_smoke_vX_Y_Z
   ```
4. **Post-Release Verification:**
   - Confirm crates.io max version: `curl -fsSL https://crates.io/api/v1/crates/ftui | jq -r .crate.max_version`
   - Confirm docs.rs build status for `ftui` `X.Y.Z`.
   - Ensure `origin/master` matches `origin/main`: `git push origin main:master`.

## 5. Rollback & Yank Policy

- **Never delete git tags or force-push `main`/`master`.** Tag deletion is forbidden by `AGENTS.md`.
- **Crates.io versions are immutable.** If a broken release is published, fix forward with `vX.Y.(Z+1)`.
- **`cargo yank` requires explicit owner authorization.** Only run `cargo yank --vers X.Y.Z <crate>` when instructed in writing by the repository owner, and yank in reverse topological order (`ftui` first, leaf crates last).
