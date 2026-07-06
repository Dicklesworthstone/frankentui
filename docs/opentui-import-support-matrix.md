# OpenTUI → FrankenTUI Migration Support Matrix & Compatibility Commitments

> Status: pre-1.0 (workspace 0.5.x) — reference for `bd-3bxhj.9.5`.
>
> **Matrix version:** `support-matrix-v1` · **Policy version:** `2026-07-06`
>
> This document is the versioned support matrix the migration pipeline's
> risk register names as the countermeasure for installed-base inertia
> (`failure_mode_risk.rs`, `InstalledBaseInertia`). It publishes which
> OpenTUI patterns are supported, under which policy class, at which
> certification scope, and with which rollout-stage commitment — plus
> explicit fallback guidance and non-goals for everything else.

The matrix is **derived from, and subordinate to, the machine-readable
contracts** listed in §8. When this document and a contract disagree, the
contract wins and this document has a bug.

---

## 1. Policy classes and certification scope

Every supported pattern is committed under exactly one **policy class**
(`TransformationHandlingClass` in `semantic_contract.rs`, serialized by the
transformation policy matrix, `schema_version: transform-policy-v1`):

| Policy class | Commitment | Certification consequence |
|---|---|---|
| `exact` | Semantics-preserving translation; behavior is contract-equal | Eligible for `AutoApprove`; strict semantic + visual clauses apply |
| `approximate` | Behavior-preserving with declared, bounded divergences | Requires `HumanReview`; divergences must be enumerated in the certification report |
| `extend_ftui` | Supported by extending FrankenTUI (tracked capability work) | Certification `Hold` until the extension ships; gap ticket mandatory |
| `unsupported` | No migration guarantee (see §5 for fallback guidance) | Certification `Reject` for affected segments; explicit gap ticket + fallback |

Certification scope is the seven domains of the certification report
(`CertificationDomain`, `doctor_frankentui.migration_certification_report.v2`):
`semantic`, `semantic_proof`, `visual`, `performance`, `accessibility`,
`confidence`, `compliance`. A **commitment in this matrix only covers the
domains listed for that row**; anything else is best-effort.

Verdict vocabulary: `VerdictOutcome::{Accept, Hold, Reject, Rollback}` with
`certification_passed == (final_verdict == accept)`. Policy profiles:
`strict_release` (min confidence mean 0.90, machine-verifiable proof +
compliance-clear required) and `human_review` (0.75, review lane).

## 2. The support matrix

Feature classes are the eight mapping-atlas categories (`MappingCategory`,
atlas `mapping-atlas-v3`). The **tier** column is the strongest policy class
available in the atlas for common patterns of that category; individual
constructs may be lower — the atlas entry (`MappingEntry.policy`) is
authoritative per construct.

| Feature class | Typical OpenTUI patterns | Tier | Certified domains | Available from stage |
|---|---|---|---|---|
| `view` | Component render trees, fragments | `exact` | semantic, semantic_proof, visual, confidence | Alpha |
| `state` | Local state, reducers, stores | `exact` | semantic, semantic_proof, confidence | Alpha |
| `event` | Key/mouse/focus handlers | `exact` | semantic, visual, confidence | Alpha |
| `layout` | Flex/grid trees, constraints | `exact` | semantic, visual, performance | Alpha |
| `style` | Themes, styles, cascades | `approximate` | visual (tolerance-classed), confidence | Alpha |
| `accessibility` | Roles, names, focus order, actions | `approximate` | accessibility, semantic | Beta |
| `effect` | Timers, IO, subscriptions | `approximate` / `extend_ftui` | semantic, confidence | Beta |
| `capability` | Host/terminal capabilities, exotic protocols | `extend_ftui` / `unsupported` | compliance (+ per-gap review) | Beta (supported subset) |

Platform constraints inherited from FrankenTUI itself:

- **Terminals:** the commitments above assume the terminal capability
  classes in `docs/reference/terminal-compatibility.md`; degraded terminals
  degrade visual commitments first (never semantic ones).
- **Web/WASM:** `ftui-web` parity is covered by the pane/web parity
  contracts; migration commitments for browser targets follow the same tier
  as the terminal target minus any capability-gap tickets opened for the
  web host.
- **Windows:** deferred per `docs/WINDOWS.md`; no migration commitment
  beyond `Hold` until the native backend lands.

## 3. Feature-class commitments by rollout stage

Commitments activate per rollout stage (`MigrationRolloutStage`:
`alpha` → `beta` → `ga`) and are enforced by the readiness rubric and
release gate (`rollout_scorecard.rs`; reasons like
`certification-threshold`, `release-blockers` are emitted verbatim):

| Gate | Alpha | Beta | GA |
|---|---|---|---|
| min certification pass ratio | 0.90 | 0.97 | 1.00 |
| min corpus coverage ratio | 0.25 | 0.60 | 0.90 |
| min reliability pass ratio | 0.95 | 0.98 | 0.995 |
| min deterministic artifacts | 3 | 5 | 7 |
| max open blockers | 2 | 0 | 0 |
| release-gate thresholds (cert / determinism / gaps / artifacts) | 0.90 / 0.99 / 2 / 5 | 0.97 / 1.00 / 0 / 6 | 1.00 / 1.00 / 0 / 8 |
| authority | release owner | release owner | maintainer quorum |

Interpretation: a feature class marked "Available from stage Beta" carries
**no compatibility commitment** while the pipeline is at Alpha — migrations
touching it certify at best `Hold` with a gap ticket.

## 4. What the commitments mean

- **Deterministic evidence.** Every commitment is backed by replayable
  artifacts (the five release-gate artifact kinds: `certification`,
  `critical-gaps`, `determinism`, `performance`, `readiness`, each with a
  `sha256:`-digested immutable reference).
- **Fail-closed certification.** A pattern outside its committed tier does
  not silently degrade: the linter/risk/verify gates reject, and the
  remediation plan (`doctor_frankentui.certification_remediation_plan.v1`)
  ranks the fix actions.
- **No silent scope creep.** New patterns enter this matrix only through an
  atlas version bump (§7), never by reinterpretation of an existing row.

## 5. Unsupported classes: fallback guidance and roadmap

Every `unsupported` or `extend_ftui` pattern gets a capability-gap ticket
(`GapSeverity`: `blocker`/`critical`/`major`/`minor`/`info`) and one of
these fallbacks, in preference order:

1. **Approximate re-expression.** Re-express the pattern with a supported
   FrankenTUI construct (the atlas entry's `remediation` strategy names the
   approach and effort band). Example: bespoke OpenTUI effect runners →
   `Cmd`/`Subscription` adapters.
2. **Extension track.** If the gap is a missing FrankenTUI capability, the
   gap ticket routes to the capability-closure epic (bd-3bxhj.5) and the
   pattern is committed as `extend_ftui` with a `Hold` certification until
   the extension ships.
3. **Manual port with certification.** Hand-port the segment; the
   certification pipeline still applies (the segment is certified like any
   other, it just isn't auto-translated).
4. **Explicit de-scope.** Record the segment as out of scope in the
   migration evidence pack; the release gate counts it against
   `critical-gaps` thresholds (§3), so de-scoping is visible, not silent.

Known unsupported/roadmap classes at policy version `2026-07-06`:

| Pattern class | Status | Fallback | Roadmap reference |
|---|---|---|---|
| Arbitrary async effect runners (e.g. WebSocket effects) | `extend_ftui` | Custom `Cmd` adapter (fallback 1) | capability-gap tickets under bd-3bxhj.5 |
| Pixel-exact styling beyond terminal cell model | `unsupported` | Tolerance-classed visual diff (`decorative_color` semantic class) | none (non-goal, §6) |
| Host-specific native integrations (clipboard managers, notification daemons) | `extend_ftui` | Process subscription adapters | bd-3bxhj.5 gap queue |
| Non-deterministic render paths (wall-clock-driven animation without seed control) | `unsupported` as-is | Re-express on the deterministic animation system | `docs/migration-map.md` §4 |
| Windows-native console specifics | `Hold` | Defer to native-backend track | `docs/WINDOWS.md` |

## 6. Explicit non-goals

- **Bug-for-bug compatibility.** `exact` means contract-equal semantics,
  not replication of OpenTUI defects; divergences from source *bugs* are
  documented in the certification report, not preserved.
- **Pixel-identity across terminals.** Visual commitments are
  cell-model + tolerance-class based, never pixel-based.
- **Unbounded ecosystem coverage.** Third-party OpenTUI plugins are
  supported only insofar as they reduce to patterns in §2.
- **Performance parity on degraded hosts.** Performance clauses apply on
  the canonical fixture matrix, not on arbitrarily constrained hardware.

## 7. Versioning and change history

This matrix follows the mapping-atlas versioning idiom
(`ATLAS_VERSION` / `ATLAS_COMPAT` / `ATLAS_COMPAT_NOTES`): the matrix
version is monotonic, older versions stay listed as compatible-with notes,
and every change lands as a new row here (never an in-place edit of an
existing row's meaning).

| Matrix version | Policy version | Compatible with | Change |
|---|---|---|---|
| `support-matrix-v1` | `2026-07-06` | — (initial) | Initial publication: 8 feature classes, 4 policy classes, stage-gated commitments, fallback catalog (bd-3bxhj.9.5) |

Amendment process: a change to any commitment requires (a) a version bump
in this table, (b) alignment with the atlas/policy-matrix versions in §8,
and (c) release-owner sign-off at the current rollout stage's authority
level (§3).

## 8. Machine-readable sources of truth

| Contract | Identifier | Where |
|---|---|---|
| Transformation policy matrix | `transform-policy-v1` (+ `policy_version`) | `docs/spec/opentui-transformation-policy-matrix.md`, `semantic_contract.rs::TransformationPolicyMatrix` |
| Mapping atlas | `mapping-atlas-v3` (compat: v2, v1) | `crates/doctor_frankentui/src/mapping_atlas.rs` |
| Certification report | `doctor_frankentui.migration_certification_report.v2` | `crates/doctor_frankentui/src/certification_report.rs` |
| Remediation plan | `doctor_frankentui.certification_remediation_plan.v1` | `crates/doctor_frankentui/src/certification_report.rs` |
| Readiness rubric + release gate | `readiness-rubric-v1` / `release-gate-v1` (report JSON `1.0.0`) | `crates/ftui-harness/src/rollout_scorecard.rs` |
| Flagship evidence packs | `flagship-migrations-v1` | `crates/doctor_frankentui/src/flagship_migrations.rs` |

See also: `docs/opentui-import-operations.md` (operator runbook, rubric and
incident-response detail), `docs/migration-map.md` (§4 survives/dropped),
`docs/api/pane-stability-contract.md` (the stability-tier idiom this
document follows).
