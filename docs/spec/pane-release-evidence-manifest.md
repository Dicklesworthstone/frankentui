# Pane Release-Evidence Manifest (`ftui.pane.release_evidence` v1)

The single, checksummed artifact that proves the pane workspace is shippable.
Produced by [`scripts/pane_release_evidence.py`](../../scripts/pane_release_evidence.py)
(`bd-1w0w4.7`) and consumed by the release gate
([`pane-release-gate-policy.md`](../pane-release-gate-policy.md), `bd-1w0w4.5`).

## Why

Six independent dimensions must be green before the pane system ships, and each
already emits its own artifacts. Rather than ask an operator (or a gate) to
chase a scatter of logs, this manifest **ties them into one coherent,
schema-versioned bundle** with SHA-256 checksums over every committed artifact,
so a release decision stands on a single trustworthy file.

The bundler **does not** decide go/no-go — it guarantees the evidence is
present, coherent, and checksummed. The verdict policy lives in the gate.

## Dimensions

| Dimension | Proves | Authoritative suites | Committed artifacts |
|-----------|--------|----------------------|---------------------|
| `unit` | split-tree invariants, solver stability, property/fuzz | `pane_invariant_fuzz`, `pane_determinism_matrix`, `pane_operation_family_equivalence`, `pane_persistent_equivalence`, `pane_monitor_gates`, `pane_margin` | — |
| `e2e` | terminal PTY + web drag/resize/keyboard | `pane_input_pty_e2e`, `pane_splitter_drag_pty_e2e`, `pane_web_e2e` | `scripts/pane_e2e.sh` |
| `parity` | cross-host observational identity | `pane_cross_host_parity` | `docs/spec/pane-parity-contract-and-program.md` |
| `perf` | replay/perf budgets, golden oracle, soak+rollback | `pane_soak_stress`, `pane_soak_rollback`, `pane_checkpoint_integration`, `pane_semantic_replay_harness` | `scripts/pane_replay_golden.json` (+ runtime: `replay_artifact_index.json`, `differential_certification.json`) |
| `a11y` | accessibility compliance + discoverability | `pane_a11y_compliance_a11y`, `pane_discoverability_a11y` | `tests/e2e/pane_a11y_compliance_matrix.json` |
| `logging` | structured observability + traceability | `e2e_observability_pipeline`, `traceability_matrix` | `tests/e2e/pane_traceability_matrix.json`, `tests/e2e/lib/e2e_jsonl_schema.json` |

## Schema (v1)

```jsonc
{
  "schema": "ftui.pane.release_evidence",
  "schema_version": 1,
  "feature": "pane-workspace",
  "dimensions": {
    "<name>": {
      "description": "...",
      "suites": [{ "crate": "...", "target": "...",
                   "status": "green|red|declared", "passed": 0, "failed": 0 }],
      "static_artifacts":  [{ "path": "...", "present": true, "sha256": "..." }],
      "runtime_artifacts": [{ "name": "...", "present": true,
                              "path": "...", "sha256": "..." }],
      "summary": { "suite_count": 0, "suites_red": 0,
                   "static_present": 0, "static_total": 0,
                   "runtime_present": 0, "runtime_total": 0 }
    }
  },
  "overall": {
    "dimension_count": 6,
    "static_complete": true,    // every committed artifact present + checksummed
    "runtime_complete": false,  // every CI runtime artifact present
    "no_red_suites": true       // no observed suite failed
  }
}
```

- `suites[].status` is `declared` when no `--test-summary` pass/fail counts were
  supplied, `green`/`red` otherwise. A `declared` suite names the authoritative
  test but does not by itself assert it ran.
- `static_artifacts[].sha256` lets a downstream gate confirm the bundle
  describes *this* tree; `validate` re-derives and compares them.

## Usage

```bash
# Self-contained contract test (no CI state needed).
python3 scripts/pane_release_evidence.py selftest

# Build a bundle from the repo + a CI results dir + observed test counts.
python3 scripts/pane_release_evidence.py collect \
  --out target/pane-release/pane_release_evidence.json \
  --results-dir target/pane-profiling/ci \
  --test-summary target/pane-test-summary.json \
  --strict --json

# Validate (checksums always; --require-runtime for the strict post-run gate).
python3 scripts/pane_release_evidence.py validate \
  --bundle target/pane-release/pane_release_evidence.json --json
```

`--test-summary` is a JSON map of `"<crate>::<target>"` → `{ "passed": N,
"failed": M }`; absent suites are recorded as `declared`.

## CI

The `pane-perf-artifacts` job runs `selftest`, then `collect` (with the perf
runtime artifacts from the run) and `validate`, and uploads
`pane_release_evidence.json` as the `pane-release-evidence` artifact. The
release gate consumes this bundle to render the go/no-go verdict.

## Versioning

`schema_version` is monotonic; a bump signals an incompatible bundle shape.
`validate` rejects a mismatched version rather than misreading it.
</content>
