# Render stage dominance across canonical fixtures — 2026-09-19

Measured while wiring `ftui-harness::cost_surface` into the render gauntlet's
tail-latency gate (commit 595896be, bd-5h57k). `cost_surface`'s module doc opens
with four questions it was written to answer. Nothing had ever run it, so none of
them had an answer. Two of them do now.

## Method

`FixtureRunner::run(spec)` over every canonical **render**-family fixture, then
`CostSurfaceAnalyzer::from_baseline(&result.record)`. Stage means in microseconds,
as the analyzer reports them. One run on an rch worker, debug profile — these are
*shape* measurements, not performance evidence, and must not be quoted as
baselines or used in a promotion decision.

| Fixture | cell_mutation | buffer_diff | presenter_emit | frame_pipeline_total |
|---|---|---|---|---|
| `render_diff_sparse_80x24` | **205.150** | 26.465 | 58.025 | 290.710 |
| `render_diff_dense_80x24` | 395.170 | 52.590 | **713.390** | 1162.040 |
| `render_presenter_emit_120x40` | 603.173 | 89.487 | **615.400** | 1309.107 |
| `render_pipeline_full_200x60` | 1504.880 | 220.660 | **1559.570** | 3286.030 |

## "Which stage is the bottleneck?" — not the one the name suggests

`buffer_diff` is the **cheapest** stage in all four fixtures, at 9–18% of the
frame. The diff is the part of this pipeline with the most optimization machinery
pointed at it (dirty rows, tile grids, skip certificates, the quotient filter that
was written for it), and it is not where the time goes.

## "Does the bottleneck shift between sparse and dense?" — yes, decisively

- **Sparse**: `cell_mutation` dominates at **71%** of the frame; presenter emit is
  20%.
- **Dense**: `presenter_emit` dominates at **61%**; cell mutation falls to 34%.

The two stages swap rank with a 3.5× margin one way and a 1.8× margin the other.
Any optimization ranked on one of these workloads alone is ranked on the wrong
one. This is exactly the `DominanceMap::has_dominance_shift()` case that
`cost_surface` provides and that nothing was calling.

## Instrumentation resolution is not a problem here

`FixtureRunner` records stage latencies truncated to whole microseconds, so a
sub-microsecond stage would read as `0.0` and vanish from the surface. Measured
stages are 26–1560µs, three orders of magnitude above that floor, so the
attribution has real signal on these fixtures. Worth re-checking if a much
smaller viewport or a cheaper fixture family is ever added.

The truncation is also visible and behaves as predicted: components sum to
0.8–1.1µs *below* `frame_pipeline_total` in every row, since each of the three
stages loses up to 999ns to `floor` while the total loses at most 999ns once.
That is why the gauntlet's consistency check allows 3µs of slack before calling a
sum-above-total a miscount.

## What this does not tell us

Debug profile, one run, no percentile stability check. The dominance *ranks* are
robust (3.5× and 1.8× margins); the *magnitudes* are not evidence of anything.
Release-profile numbers with repeated runs would be needed before any of this
feeds a promotion scorecard.
