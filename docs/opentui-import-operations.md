# OpenTUI Import — Operator Runbooks, Playbooks & Troubleshooting

Operational guide for running OpenTUI → FrankenTUI migrations with the
`doctor_frankentui` CLI: planning intake, running migration suites, generating
reports, certifying readiness, interpreting verdicts, applying remediations, and
diagnosing failures.

This guide is self-contained and reflects the CLI as implemented. Every command,
flag, artifact path, exit code, and threshold below is taken from the source
(`crates/doctor_frankentui/`, plus the readiness rubric in
`crates/ftui-harness/src/rollout_scorecard.rs`). If the CLI changes, update this
file alongside it.

Related specs (the *what/why* behind the gates this guide *operates*):
[migration-map.md](migration-map.md),
[spec/opentui-semantic-equivalence-contract.md](spec/opentui-semantic-equivalence-contract.md),
[spec/opentui-transformation-policy-matrix.md](spec/opentui-transformation-policy-matrix.md),
[spec/opentui-evidence-manifest.md](spec/opentui-evidence-manifest.md).

---

## 1. The migration pipeline

A migration moves through four operator-facing stages, each a `doctor_frankentui`
subcommand. Commands are independent processes that communicate through artifacts
on disk under a `--run-root`; you chain them by pointing each step at the prior
step's output directory.

```
  plan ──────────▶ migrate ──────────▶ report ──────────▶ certify
 (import)          (suite)             (HTML/JSON)         (doctor)
  intake +          replay capture      aggregate runs      readiness +
  snapshot          across profiles     into a report       capture smoke
```

| Stage | Command (canonical / alias) | Purpose |
|-------|-----------------------------|---------|
| Intake | `plan` / `import` | Deterministically materialize a pinned source snapshot and forecast difficulty. |
| Replay suite | `migrate` / `suite` | Run capture/replay across one or more profiles. |
| Report | `report` | Aggregate suite runs into HTML + JSON. |
| Certify | `certify` / `doctor` | Verify environment wiring and capture readiness; emit a summary. |
| Capture (single) | `replay` / `capture` | Record one deterministic terminal session (used internally by `migrate`). |
| Seed | `seed-demo` | Seed MCP demo data via JSON-RPC for capture fixtures. |
| Profiles | `list-profiles` | Print built-in profile names. |

Every command accepts the global `--machine <auto|human|json>` flag (default
`auto`) for machine-readable output, e.g. `doctor_frankentui --machine json plan …`.

> Throughout this guide, `doctor_frankentui` is the binary
> (`cargo run -q -p doctor_frankentui --`). The legacy aliases (`import`,
> `suite`, `doctor`, `capture`) are interchangeable with the canonical names.

---

## 2. Prerequisites

- `git` and `tar` — required by `plan` to materialize a snapshot from a Git
  source. The `git archive | tar` pipeline is bounded (180 s) and is killed and
  reaped if it stalls, so a malformed repository fails fast rather than hanging.
- `cargo` — the default app command is `cargo run -q -p ftui-demo-showcase`.
- For capture (`migrate`/`replay`): `vhs` and, for `--observe tmux`, `tmux`.
  Run `certify` first to confirm these are wired up.

Confirm tooling before a real migration:

```bash
doctor_frankentui certify --project-dir /data/projects/frankentui
doctor_frankentui list-profiles
```

---

## 3. Runbook A — Happy-path migration

### Step 1: Plan intake

Materialize an immutable snapshot of the source at a pinned commit and produce a
deterministic forecast. Use `--dry-run` first to forecast without generating code.

```bash
doctor_frankentui plan \
  --source /path/to/opentui-project \
  --pinned-commit <sha> \
  --run-root /tmp/doctor_frankentui/import \
  --run-name intake_baseline \
  --dry-run
```

| Flag | Default | Notes |
|------|---------|-------|
| `--source` | *(required)* | Local path or Git URL. |
| `--pinned-commit` | resolve HEAD | Pin for reproducibility. |
| `--run-root` | `/tmp/doctor_frankentui/import` | Where artifacts are written. |
| `--run-name` | generated | Stable name for automation. |
| `--allow-non-opentui` | off | Accept snapshots that don't look like OpenTUI/React. |
| `--dry-run` | off | Forecast only; no code generation. |
| `--watch` | off | Emit an incremental watch manifest for one tick. |
| `--incremental-from` | — | Prior run dir, snapshot dir, or `intake_meta.json` for incremental intake. |

Produces under `<run-root>/<run-name>/`:

| Artifact | Contents |
|----------|----------|
| `intake_meta.json` | Source kind, pinned/resolved commit, source hash, lockfile fingerprints, toolchain, and `status`/`error_class`/`error_message` on failure. |
| `snapshot/` | Full materialized source tree at the pinned/resolved commit. |
| `migration_forecast.json` | *(with `--dry-run`)* difficulty score, confidence band, top risk modules, likely gaps, operator actions. |
| `incremental_watch.json` | *(with `--watch`)* invalidated stages, cache stats, determinism hash. |

### Step 2: Run the migration suite

```bash
doctor_frankentui migrate \
  --profiles analytics-empty,analytics-full \
  --run-root /tmp/doctor_frankentui/suite_runs \
  --suite-name migration_suite_001 \
  --app-command "cargo run -q -p ftui-demo-showcase"
```

`--profiles` defaults to all available profiles (see `list-profiles`). Use
`--fail-fast` to stop on the first profile failure, `--skip-report` to defer
report generation, and `--keep-going` to override `--fail-fast`.

Produces under `<run-root>/<suite-name>/`:

| Artifact | Contents |
|----------|----------|
| `<profile>/run_meta.json` | Per-run metadata: profile, status (`ok`/`failed`), trace id, duration, output/snapshot paths. |
| `suite_manifest.json` | Suite roll-up: success/failure counts, run index, trace ids, fallback/capture-error profiles. |
| `report.json` / `index.html` | Aggregated report (also produced standalone by `report`). |
| `suite_report.log` | Full stdout from report generation. |

### Step 3: Generate / regenerate a report

`migrate` generates a report unless `--skip-report`. To (re)build one from an
existing suite directory:

```bash
doctor_frankentui report \
  --suite-dir /tmp/doctor_frankentui/suite_runs/migration_suite_001 \
  --output-html /tmp/migration_report.html \
  --output-json /tmp/migration_report.json \
  --title "OpenTUI Migration — suite 001"
```

`--suite-dir` is required (it must contain per-run `run_meta.json` files).
`--title` defaults to `TUI Inspector Report`.

### Step 4: Certify readiness

```bash
doctor_frankentui certify \
  --project-dir /data/projects/frankentui \
  --run-root /tmp/doctor_frankentui/doctor \
  --capture-timeout-seconds 60 \
  --full
```

| Flag | Default | Notes |
|------|---------|-------|
| `--app-command` | `cargo run -q -p ftui-demo-showcase` | App to smoke-test. |
| `--project-dir` | `/data/projects/frankentui` | Working dir for runs. |
| `--full` | off | Run the more thorough certification suite. |
| `--capture-timeout-seconds` | `45` | Capture smoke timeout. |
| `--allow-degraded` | off | Exit `0` (instead of `30`) when capture is degraded. |
| `--run-root` | `/tmp/doctor_frankentui/doctor` | Artifact root. |
| `--observe` | `none` | `tmux` keeps a live session for inspection. |
| `--tmux-keep-open` | off | Leave the tmux session after the run. |

Writes `<run-root>/meta/doctor_summary.json` (status, capture-stack health,
degraded flag, capture/app smoke details). On `--observe tmux`, the app-smoke
fallback also captures the pane; if a lingering session cannot be terminated it
logs a warning and records `tmux_kill_failed: true` in the smoke summary.

---

## 4. Interpreting the certification report

The certification report (schema
`doctor_frankentui.migration_certification_report.v2`) carries a single
`final_verdict` plus the evidence that produced it.

### Verdicts

| Verdict | Meaning | Operator action |
|---------|---------|-----------------|
| `Accept` | All gates passed. `certification_passed == true`. | Proceed (subject to the rollout rubric, §6). |
| `Hold` | Passed with warnings; needs human review. | Review warnings; decide manually. |
| `Reject` | One or more critical checks failed. | Apply the remediation plan (§5), re-run. |
| `Rollback` | Failed, and the confidence stage recommends reverting. | Revert the migration; do not ship. |

`certification_passed` is exactly `final_verdict == Accept` — no other verdict is
a pass.

### How `final_verdict` is computed

1. **Any failure** — a stage with status `Fail`, or a clause with status `Failed`
   or `MissingEvidence` — yields `Reject`, *unless* the `Confidence` stage's
   verdict is `Rollback`, in which case it yields `Rollback`.
2. Otherwise, **any warning** — a `Warning` stage or clause — yields the policy's
   `warning_verdict` (default `Hold`).
3. Otherwise → `Accept`.

`MissingEvidence` is treated as a failure, not a pass: an unproven clause cannot
certify.

### Stages and the clause matrix

- **Stage results** (`stage_results`) — one per domain: `Semantic`,
  `SemanticProof`, `Visual`, `Performance`, `Accessibility`, `Confidence`,
  `Compliance`. Each has a status (`Pass`/`Warning`/`Fail`), an observed verdict,
  a risk level, evidence refs, and messages.
- **Clause matrix** (`clause_matrix`) — one row per semantic-contract clause with
  an aggregate status (`Passed`/`Warning`/`Failed`/`MissingEvidence`), the
  domains that touched it, and evidence refs. Start triage here: every `Failed`
  or `MissingEvidence` row points to a specific clause and its evidence.
- **Confidence intervals** — Bayesian posterior intervals; compare against the
  policy's `min_confidence_mean` and `max_confidence_interval_width` (§6).

---

## 5. Remediation playbook

For any non-`Accept` verdict the report includes a remediation plan (schema
`doctor_frankentui.certification_remediation_plan.v1`); for `Accept` the plan is
empty.

Each action carries:

- `rank` — work the list in rank order (1 first).
- `domain` + `target` — what it fixes (a clause id, stage, or composite target).
- `title` / `action` — the concrete step to take.
- `effort` — `Low` (hours) / `Medium` (days) / `High` (weeks+).
- `expected_confidence_impact` — estimated posterior gain (0–1).
- `expected_value_score` — priority = impact per unit effort.
- `failed_clause_ids` / `artifact_refs` / `evidence_messages` — exactly which
  clauses it resolves and where the evidence lives.

Actions are pre-sorted by value score (descending), then effort (ascending), so
the top of the list is the highest-leverage, lowest-cost work. `issue_exports`
provides ready-to-file issue templates for tracking.

**Loop:** pick the highest-ranked action → apply it → re-run `migrate` (and
`report`/`certify`) → confirm the targeted clause flips to `Passed` and the
verdict improves. Repeat until `Accept`.

---

## 6. Migration readiness rubric & phased rollout

`Accept` certifies *one* migration; shipping the migration **service** to wider
audiences is gated separately by the readiness rubric
(`MigrationReadinessRubric` in `crates/ftui-harness/src/rollout_scorecard.rs`),
which maps quantitative evidence to a rollout **stage**.

### Stages

`Alpha` (internal trials) → `Beta` (supervised production-adjacent) → `Ga`
(general availability for the declared support matrix — see
[`docs/opentui-import-support-matrix.md`](opentui-import-support-matrix.md)
for the versioned feature-class commitments, fallback guidance, and
non-goals). Stages are ordered and advanced one at a time.

### Default stage gates

A stage advance requires **all** of these to hold for the target stage:

| Gate | Alpha | Beta | GA |
|------|-------|------|----|
| Min certification pass ratio | 0.90 | 0.97 | 1.00 |
| Min corpus coverage ratio | 0.25 | 0.60 | 0.90 |
| Min reliability pass ratio | 0.95 | 0.98 | 0.995 |
| Min deterministic artifact classes | 3 | 5 | 7 |
| Benchmark gate required | yes | yes | yes |
| Max open release blockers | 2 | 0 | 0 |
| Required authority | Release owner | Release owner | Maintainer quorum |

### Verdicts and authority

Evaluating a target stage against an evidence snapshot yields:

- `Advance` — all quantitative gates and the authority check pass.
- `Hold` — evidence is insufficient (the decision lists which gates failed).
- `EmergencyHold` — an active emergency hold blocks advancement regardless of
  evidence.

Operator authority is ordered `Automation < OnCall < ReleaseOwner <
MaintainerQuorum`; the acting authority must meet the stage's
`required_authority`. Any active **emergency hold** —
`certification-regression`, `determinism-divergence`, `security-incident`,
`reliability-breach`, `missing-evidence`, or `operator-override` — forces
`EmergencyHold` and blocks all advancement until cleared. The decision serializes
to JSON (`MigrationReadinessDecision::to_json()`) as a release artifact.

> The certification policy profile sets the per-migration thresholds feeding the
> rubric. Built-in profiles: `strict_release` (default) requires
> `min_confidence_mean ≥ 0.90`, `max_confidence_interval_width ≤ 0.20`, machine
> proof, and clear compliance, with `warning_verdict = Hold`; `human_review`
> relaxes these (`0.75` / `0.35`, allows performance regression within policy,
> compliance not required) for supervised review.

---

## 7. Troubleshooting

### 7.1 Intake (`plan`) failures

`plan` classifies failures and exits with a class-specific code. The class and a
reason are written to `intake_meta.json` (`error_class`, `error_message`) and
printed as `intake_failed class=… reason=…`.

| Class | Exit | Typical cause | First checks |
|-------|------|---------------|--------------|
| `Auth` | 41 | Missing/invalid credentials, SSH key, token. | Verify Git auth to `--source`; try a manual `git ls-remote`. |
| `Network` | 42 | Clone/fetch timeout, DNS, connectivity. | Retry; check the network and the remote host. |
| `MissingFiles` | 43 | Source path absent, required files missing. | Confirm `--source` exists and contains the expected project. |
| `IncompatibleRepo` | 44 | Not a directory, doesn't match OpenTUI/React shape. | Inspect the snapshot; use `--allow-non-opentui` only if intentional. |
| `Unknown` | 45 | Unclassified (e.g. snapshot pipeline error). | Read `error_message` in `intake_meta.json`; check disk space. |

If `git archive` or `tar` stalls, `plan` aborts after 180 s with a timeout error
(class `Unknown`, exit 45) rather than hanging — suspect a corrupt or pathological
repository.

### 7.2 Suite (`migrate`) failures

- A profile failure is recorded in that run's `run_meta.json` (`status: "failed"`)
  and counted in `suite_manifest.json` (`failure_count`, `capture_error_profiles`).
- With `--fail-fast`, the suite stops at the first failure; without it (or with
  `--keep-going`), it runs all profiles and reports the tally.
- `fallback_profiles` in the manifest marks profiles that fell back to a secondary
  capture driver — investigate the primary driver if this is unexpected.
- Start with `suite_report.log`, then the failing run's `run_meta.json` and its
  capture logs (e.g. `vhs.log`).

### 7.3 Certify (`certify`) outcomes

- **Exit 30 (degraded capture):** the capture stack works but is degraded.
  `--allow-degraded` turns this into a success exit while still recording the
  degradation in `meta/doctor_summary.json`. Inspect that file for the specific
  capability that degraded.
- **Capture smoke timeout:** raise `--capture-timeout-seconds`; a wrapped external
  capture process that times out surfaces as exit `124`.
- **App smoke failure:** read the app smoke stdout/stderr logs under the run root;
  with `--observe tmux` the pane capture shows the live UI state.

### 7.4 General triage map

| Symptom | Look here |
|---------|-----------|
| Intake failed | `intake_meta.json` → `error_class` / `error_message` |
| Suite profile failed | `suite_report.log` → failing `<profile>/run_meta.json` → capture logs |
| Certification not `Accept` | clause matrix `Failed`/`MissingEvidence` rows → remediation plan |
| Capture degraded/timeout | `meta/doctor_summary.json`, capture logs, raise timeout |
| Non-zero exit, unclear cause | see the exit-code table (§8) |

---

## 8. Exit-code reference

| Code | Meaning |
|------|---------|
| 0 | Success. |
| 1 | General error (most `DoctorError` variants). |
| 30 | Capture stack degraded (`certify`; suppressed to 0 with `--allow-degraded`). |
| 41 | Intake `Auth` failure (`plan`). |
| 42 | Intake `Network` failure (`plan`). |
| 43 | Intake `MissingFiles` failure (`plan`). |
| 44 | Intake `IncompatibleRepo` failure (`plan`). |
| 45 | Intake `Unknown` failure (`plan`). |
| 124 | Wrapped external command (e.g. capture) timed out. |
| *other* | Propagated exit code from a failed external subprocess. |

---

## 9. Automation & machine output

For CI and scripted rollout gates, pass `--machine json` and consume the emitted
JSON plus the on-disk artifacts:

- `intake_meta.json` and `migration_forecast.json` — gate intake.
- `suite_manifest.json` / `report.json` — gate the suite (success/failure counts).
- the certification report — gate on `final_verdict == "Accept"` /
  `certification_passed`.
- `MigrationReadinessDecision::to_json()` and `RolloutEvidenceBundle::to_json()`
  (from `ftui-harness`) — the machine-readable stage/Go-NoGo artifacts that drive
  phased rollout (§6).

Because each command writes deterministic artifacts under `--run-root`, a CI
pipeline can run the stages in sequence, assert on the JSON at each step, and
archive the run directory as the audit trail for the migration.

---

## 10. Incident response & rollback strategy

When a migration that already passed the gates misbehaves in a production-like
setting, the operational response is **pre-declared and deterministic** rather
than improvised. The vocabulary lives in
`crates/ftui-harness/src/rollout_scorecard.rs`: a `MigrationIncidentReport` is
resolved by the `MigrationIncidentResponsePlaybook` into a
`MigrationIncidentResponse` that bundles severity, the emergency hold, an
artifact-driven rollback plan, rollback readiness, and a postmortem template.

### 10.1 Incident classes

Every incident is one of seven classes (`MigrationIncidentClass`). Each class
fixes a default severity and the rollout `EmergencyHoldReason` it raises:

| Class (`label`) | Default severity | Emergency hold reason |
|-----------------|------------------|-----------------------|
| `semantic-regression` | Sev2 | certification-regression |
| `determinism-divergence` | Sev1 | determinism-divergence |
| `performance-regression` | Sev3 | reliability-breach |
| `capability-gap-escape` | Sev2 | certification-regression |
| `security-breach` | Sev1 | security-incident |
| `certification-false-pass` | Sev1 | certification-regression |
| `rollback-failure` | Sev1 | reliability-breach |

Observed `MigrationIncidentSignal`s can **escalate** severity: the effective
severity is the worst of the class default and every signal's `escalates_to`.

### 10.2 Severity levels

| Severity | Ack deadline | Immediate rollback? | Rollback authority |
|----------|--------------|---------------------|--------------------|
| Sev1 (critical) | 15 min | yes | On-call |
| Sev2 (high) | 60 min | yes | On-call |
| Sev3 (moderate) | 240 min | no | Release owner |
| Sev4 (low) | 1440 min | no | Release owner |

Sev1/Sev2 empower the on-call operator to roll back **immediately**; any
incident at Sev3 or worse blocks further promotion (`blocks_promotion`).

### 10.3 Deterministic, artifact-driven rollback

`MigrationRollbackPlan::default_for(class, stage)` emits an ordered spine; each
artifact-bound step names the **artifact kind** it consumes, and `resolve()`
binds it to an *immutable* (`sha256:`-digested) artifact. Among multiple
candidates of a kind, resolution is deterministic (lexicographically smallest
`artifact_id`). Steps and required kinds:

1. `halt-promotion` — `release-gate-decision`
2. `quarantine-version` — `source-snapshot`
3. `restore-last-good` — `last-good-release`
4. `verify-determinism` — `determinism-baseline`
5. `reverify-certification` — `certification-report`
6. *(security only)* `rotate-exposed-credentials` — `secret-rotation-runbook`
6. *(rollback-failure only)* `escalate-manual-recovery` — `manual-recovery-runbook`
7. `record-postmortem` — *(no artifact)*

`MigrationRollbackPlan::readiness()` returns `Ready` only when every
artifact-bound step resolved; otherwise `Blocked` with the
`missing_artifact_kinds`. A mutable (non-`sha256:`) artifact does **not** count —
the rollback must be reproducible from content-addressed evidence.

### 10.4 Postmortem template

`MigrationPostmortemTemplate::for_incident()` seeds canonical timeline prompts
(detection → impact → root-cause → resolution), class-specific **prevention
actions**, and a backlog-linkage surface. A postmortem is only complete once it
`links_backlog()` (at least one `backlog_item_id`), tying the incident to
tracked follow-up and prevention work.

### 10.5 Machine output

`MigrationIncidentResponse::to_json()` (schema `1.0.0`) is the deterministic
evidence record for incident tooling and audits. It carries `incident_id`,
`class`, `severity`, `ack_deadline_minutes`, `emergency_hold_reason`,
`rollback_authority`, `requires_immediate_rollback`, `blocks_promotion`, the
originating `signals`, the resolved `rollback` plan, `readiness`, and the
`postmortem`. Identical reports serialize byte-for-byte identically, so the
record can be diffed and replayed.
