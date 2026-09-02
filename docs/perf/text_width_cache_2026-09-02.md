# Grapheme width cache: cached vs uncached (2026-09-02)

Benchmark: `cargo bench -p ftui-core --bench text_width_bench -- --warm-up-time 1 --measurement-time 3`
(criterion 0.8, debug-free bench profile, remote x86_64 worker via rch; numbers are medians).

Corpus: 7 realistic table rows (CJK labels, Korean, emoji status glyphs including a
ZWJ family and two flags, Latin with combining marks, an ASCII filler row) cycled 8
times, filtered to the 488 non-ASCII grapheme clusters a screen would re-measure
every frame. ASCII is answered inline in both paths and is not part of the count.

| Path | Time per 488 clusters | Per cluster | Throughput |
|---|---|---|---|
| `grapheme_width_uncached` (Unicode tables every time) | 49.6 µs | 102 ns | 9.8 Melem/s |
| `grapheme_width` cached, steady state (production path) | 11.3 µs | 23 ns | 43.2 Melem/s |
| `grapheme_width` cached, cold (cache cleared each iteration) | 17.5 µs | 36 ns | 27.8 Melem/s |

Steady state is 4.4x faster than the table lookup; even a cold pass over this corpus
is 2.8x faster because repeated clusters are computed once per frame instead of once
per occurrence.

Implementation: `ftui_core::text_width::grapheme_width` keeps a per-thread S3-FIFO
(`ftui_core::s3_fifo`, 4096 entries) keyed by a seeded 64-bit ahash of the cluster
bytes and storing the width as a byte. S3-FIFO was chosen because it is the one cache
that already lives below `ftui-text` in the dependency graph (the `LRU`/`W-TinyLFU`
implementations in `ftui-text::width_cache` cannot be used by `ftui-core`) and because
its scan resistance matches the workload: a log stream of one-off emoji must not evict
a table's hot CJK labels. `FTUI_WIDTH_CACHE=0` disables it; correctness is pinned by
`text_width::tests::width_cache_is_transparent_and_hits_on_repeat`,
`display_width_matches_uncached_sum_on_mixed_text`, and the property test
`crates/ftui-core/tests/proptest_width_cache.rs`.

Baseline row to add to `tests/baseline.json` when the perf gate (G25) lands:
`text_width_non_ascii` with p99 budget 30 ns per cluster at steady state.
