# FrankenTermJS Release Readiness

> The human rendering of the release-readiness program (bd-2vr05.12). The
> **source of truth is code**: `crates/ftui-web/src/release_readiness.rs`
> serializes the parity scorecard, browser support matrix, staged rollout
> plan, and go/no-go checklist as deterministic JSON
> (`frankenterm-release-readiness-v1`), unit-tested for consistency. The
> release rehearsal (`scripts/frankenterm_js_release_rehearsal_e2e.sh`)
> bundles those artifacts with every harvested evidence stream into the
> signoff packet. If this page disagrees with the emitted artifacts, the
> artifacts win.

## How a go/no-go decision is made

1. Run the rehearsal script. It executes every compat/conformance lane
   (including all three security/reliability arms), the SDK validation
   suite, and the stress/soak campaign, then writes
   `signoff_packet/` containing the four readiness artifacts, per-lane JSONL
   evidence, and `rehearsal_summary.json` (verdict `GO_FOR_SIGNOFF` only when
   every lane passed).
2. Check the parity scorecard: **zero open blockers** for the target rollout
   stage (`stage_ready`). Today the standing blocker is
   `wasm_packaging_out_of_tree` — the browser-facing JS wrapper crate is not
   vendored here, which gates `canary_cohort` and beyond until it is
   re-vendored and the conformance gates re-run against it.
3. Walk the go/no-go checklist: every `test_gate` item machine-green, `docs`
   items lockstep-verified, `operational` items signed by the release owner.

## Parity scorecard (summary)

Ten areas versus xterm.js expectations; each names its in-tree evidence arm.
Highlights: the event model is **divergent by design** (drain-driven, not
push-driven — the migration guide's biggest callout); `browser_delivery` is
**partial** behind the packaging blocker; everything else is **full** at the
engine level with named risks (e.g. webgpu certified against simulated
engines in-tree, image parity certified at protocol not pixel level).

## Browser support matrix (summary)

| Browser | dom | canvas | webgl | webgpu |
|---------|-----|--------|-------|--------|
| Chromium ≥120 | supported | supported | supported | supported |
| Firefox ≥121 | supported | supported | supported | best-effort |
| WebKit/Safari ≥17 | supported | supported | best-effort | best-effort |
| Other engines | best-effort | best-effort | unsupported | unsupported |

Invariant (unit-tested): the fallback chain always lands on `dom` — a boot
failure on any engine is release-blocking.

## Staged rollout

`internal_dogfood → opt_in_flag → canary_cohort → default_enablement`, each
with entry criteria, automatic rollback triggers (attach protocol-error
rates, sustained `queue.overflow`, canary error-rate deltas, fallback loops),
and telemetry checkpoints (attach transition timelines, `SdkErrorKind` code
rates, drain latency, renderer distribution). Rollback evidence emission is
proven by the stress campaign's negative control
(`rollback_trigger_evidence`).

## Stress/soak campaign

`ftui-web::frankenterm_js_release_stress_e2e` drives the production
host-driven pipeline through steady output, input floods, resize storms, and
a combined soak (`FTUI_RELEASE_STRESS_ITERS` scales it from CI-size to real
soak). Limits are documented as JSONL (`FTUI_RELEASE_STRESS`), replay
determinism is asserted (byte-identical patch hashes), and the frame-bytes
ceiling is enforced.
