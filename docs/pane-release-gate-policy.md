# Pane Release Gate Policy (Go / No-Go)

The pane workspace ships only on an **objective, automated** verdict — never on
subjective confidence. This document defines the gate clauses, the two gate
modes, the override process, and the staged rollout. The gate is implemented by
[`scripts/pane_release_gate.py`](../scripts/pane_release_gate.py) (`bd-1w0w4.5`)
and consumes the
[release-evidence bundle](spec/pane-release-evidence-manifest.md) (`bd-1w0w4.7`).

A release is **blocked whenever any mandatory clause fails.**

---

## Modes

| Mode | When | Requires |
|------|------|----------|
| `advisory` | pre-merge / local | structural completeness + no observed red suites (runtime artifacts reported but not required) |
| `strict` | release / tag | everything in `advisory` **plus** every perf runtime artifact present **and** the differential certification reads `certified` |

```bash
# pre-merge sanity (does not need the CI-only perf artifacts)
python3 scripts/pane_release_gate.py evaluate \
  --bundle target/pane-release/pane_release_evidence.json --mode advisory --json

# release decision (perf job)
python3 scripts/pane_release_gate.py evaluate \
  --bundle target/pane-release/pane_release_evidence.json \
  --certification target/pane-profiling/ci/differential_certification.json \
  --mode strict --out target/pane-release/pane_release_gate.json --json
```

Exit code: `0` = GO, `1` = NO-GO, `2` = usage error.

---

## Clauses

| Clause | Mandatory in | Passes when |
|--------|--------------|-------------|
| `all_dimensions_present` | advisory, strict | all six dimensions (unit/e2e/parity/perf/a11y/logging) are in the bundle |
| `correctness` | advisory, strict | unit + e2e suites not red, and the e2e harness (`scripts/pane_e2e.sh`) present |
| `parity` | advisory, strict | `pane_cross_host_parity` not red + the parity contract present |
| `accessibility` | advisory, strict | a11y suites not red + the compliance matrix present |
| `observability` | advisory, strict | logging suites not red + the jsonl schema + traceability matrix present |
| `perf_suites` | advisory, strict | soak/replay suites not red |
| `perf_runtime_artifacts` | **strict** | replay index + differential certification present in the run |
| `perf_certified` | **strict** | differential certification `classification == "certified"` |

### How suite green-ness is established

The release-evidence bundle records each authoritative suite. A suite is:

- **`green`/`red`** when a `--test-summary` (observed pass/fail counts) is supplied
  to the evidence `collect` step;
- **`declared`** otherwise — it names the authoritative test but does not assert
  it ran.

The gate **blocks on `red`** but treats `declared` as non-blocking, because in
CI the unit/e2e/parity/a11y/logging suites run in their **own** jobs that fail
the build independently if they fail. The pane gate's job is to add the
**aggregate** go/no-go decision plus the perf **behavioral certification** that
no single suite job covers. At release time, supply a `--test-summary` built
from the suite jobs to make the bundle assert `green` end to end.

---

## CI enforcement

The `pane-perf-artifacts` job:

1. produces the perf runtime artifacts (replay index + differential certification);
2. collects + validates the release-evidence bundle (`bd-1w0w4.7`);
3. runs `pane_release_gate.py selftest`, then `evaluate --mode strict` with the
   certification, writing `pane_release_gate.json` and **failing the job on
   NO-GO**;
4. uploads the decision as the `pane-release-gate` artifact.

A semantic perf regression fails earlier at the certify step; the gate makes the
aggregate decision explicit and machine-readable.

---

## Override process

Overrides are rare, logged, and never silent:

1. A gate failure is investigated, not bypassed by default.
2. If a clause must be waived for a release, record in the PR/release notes:
   the clause, the reason, the owner, and an expiry (a follow-up bead to restore
   the gate). Threshold changes (e.g. perf envelopes) live in the gate inputs,
   not ad-hoc edits to a green/red call.
3. `perf_certified` is **not** waivable — a semantic drift means the optimized
   pane path no longer reproduces certified behavior; fix or revert.

---

## Staged rollout

| Stage | Bar |
|-------|-----|
| **Alpha** (internal) | `advisory` GO; perf gate green in CI |
| **Beta** (opt-in) | `strict` GO on a real run; release-evidence bundle uploaded; runbook reviewed |
| **GA** | `strict` GO with a `--test-summary` asserting every suite `green`; parity diff empty; soak/rollback clean over the soak window |

Promotion is one-directional per release; a regression at any stage drops the
feature back to the conservative execution policy (see the
[operational runbook](pane-operational-runbook.md)).

---

## See also

- [Release-evidence manifest](spec/pane-release-evidence-manifest.md) — the bundle the gate consumes.
- [Operational runbook](pane-operational-runbook.md) — incident response + rollback.
- [Parity contract](spec/pane-parity-contract-and-program.md) — the cross-host guarantee.
</content>
