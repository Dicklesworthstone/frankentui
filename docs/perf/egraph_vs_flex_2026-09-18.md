# E-graph layout solver vs `Flex::split`

Measured 2026-09-18 on rch worker, `cargo bench -p ftui-layout --bench layout_bench -- layout/egraph`, criterion medians.

Context: `ftui_layout::egraph` is ~1,700 lines implementing equality saturation
over a layout constraint language. The module-reachability gate
(`make reachability`) reported it as referenced by nothing in production — only
by `benches/layout_bench.rs`. The README claimed its result was that layouts
"are optimized to simpler equivalent forms before the solver runs, reducing both
computation and allocation", which is two claims: that it runs, and that it
helps. This measures the second.

## The e-graph solver is slower on every scenario

| scenario | `flex_split` | `egraph_solve` | egraph is |
|---|---|---|---|
| typical_3w | 233 ns | 1.99 µs | 8.5x slower |
| dense_50w | 1.33 µs | 9.15 µs | 6.9x slower |
| deep_100w | 4.05 µs | 15.9 µs | 3.9x slower |
| pathological_200w | 5.15 µs | 99.8 µs | **19.4x slower** |
| large_500w | 8.11 µs | 65.8 µs | 8.1x slower |

Saturation cost scales with constraint count roughly as expected for the
engine itself:

| constraints | `egraph_saturation/solve` |
|---|---|
| 10 | 5.58 µs |
| 50 | 10.8 µs |
| 100 | 17.5 µs |
| 200 | 30.3 µs |
| 500 | 73.6 µs |

## Reading

The gap is not a tuning problem. Equality saturation pays a large fixed cost to
build an e-graph and run rewrites to a fixpoint, and it buys a *globally
optimal* expression. The direct solver pays almost nothing and produces the
layout immediately. At the constraint counts a terminal UI actually produces —
three to a few hundred — the optimum is not worth its price, and the worst
relative result is at `pathological_200w`, which is exactly the shape the
optimizer was supposed to help.

Note `pathological_200w` (99.8 µs) being slower than `large_500w` (65.8 µs):
saturation cost is driven by rewrite opportunities, not constraint count alone,
so a smaller adversarial set can cost more than a larger regular one.

## Decision

**Do not wire it into `Flex`/`Grid`.** Doing so would make every layout pass
4-19x slower to buy an optimality the output does not need.

The module is kept, not deleted: the saturation engine is sound and tested, the
benchmark is the evidence for this decision, and re-running it is how anyone
would revisit the question if the constraint language grew enough to change the
economics. The README's claim has been corrected to say where it runs (nowhere
on the layout path) and why.

This is also the answer to the question the reachability allowlist raises for
`ftui-layout::egraph`: it is unreachable by design, not by neglect.
