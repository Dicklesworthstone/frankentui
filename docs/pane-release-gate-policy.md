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
| `ga` | GA release / CI default | everything in `strict` **plus** every declared suite **observed green** via the cross-job test-summary aggregation (bd-nqxa5) — no `declared` placeholders survive |

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
| `suites_observed_green` | **ga** | every declared suite carries observed pass/fail counts and is `green` (aggregated via `scripts/pane_test_summary_aggregate.py`) |

### How suite green-ness is established

The release-evidence bundle records each authoritative suite. A suite is:

- **`green`/`red`** when a `--test-summary` (observed pass/fail counts) is supplied
  to the evidence `collect` step;
- **`declared`** otherwise — it names the authoritative test but does not assert
  it ran.

In `advisory`/`strict` the gate **blocks on `red`** but treats `declared` as
non-blocking, because in CI the unit/e2e/parity/a11y/logging suites also run
in jobs that fail the build independently. In **`ga` mode** `declared` is
itself blocking: the bundle must *prove* every suite green.

### Cross-job test-summary aggregation (bd-nqxa5)

`scripts/pane_test_summary_aggregate.py` turns per-suite cargo-test logs into
mergeable summary fragments:

```bash
# per suite (in whichever job runs it): parse the log into a fragment
python3 scripts/pane_test_summary_aggregate.py capture \
  --crate ftui-layout --target pane_margin --log pane_margin.log \
  --out fragments/ftui-layout__pane_margin.json

# gate job: merge fragments (files, dirs, or downloaded artifacts) + verify
python3 scripts/pane_test_summary_aggregate.py merge fragments/ --out pane_test_summary.json
python3 scripts/pane_test_summary_aggregate.py check --summary pane_test_summary.json --require-all
```

`list` prints the declared suites from the same table the bundler uses (no
drift); `check --require-all` fails if any declared suite is missing, red, or
ran zero tests; `merge` refuses conflicting duplicates unless
`--on-conflict=worst` (which keeps the redder record, so a flaky re-run can
never launder a red suite). Fragments carry provenance under `_meta.sources`.

---

## CI enforcement

The `pane-perf-artifacts` job:

1. produces the perf runtime artifacts (replay index + differential certification);
2. **observes every declared suite** (driven by `aggregate list`), captures +
   merges the summaries, and verifies completeness (`check --require-all`);
3. collects + validates the release-evidence bundle (`bd-1w0w4.7`) with
   `--test-summary`, so every suite record is observed-green;
4. runs `pane_release_gate.py selftest`, then `evaluate --mode ga` with the
   certification, writing `pane_release_gate.json` and **failing the job on
   NO-GO**;
5. uploads the decision (`pane-release-gate`) and the aggregated summary
   (`pane-test-summary`) as artifacts.

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
