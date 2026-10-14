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

---

## `display_width` whole-line routing: paired same-invocation benchmark (2026-10-14, `bd-g00-root-epic-ewths.31.11`)

Command: `RCH_CARGO_WRAPPER_BYPASS=1 cargo bench -p ftui-core --bench text_width_bench -- "text_width/display_width" --warm-up-time 1 --measurement-time 1`

Compares three `display_width(text)` strategies in the same invocation over 32-line corpora (4 sample lines cycled 8x each; steady state after 1 warm-up pass, 100% cache hit rate on repeat frames):
1. `uncached_direct`: ASCII fast-path + uncached `grapheme_width_uncached` sum.
2. `cached_per_cluster` (selected production path): ASCII fast-path + `text.graphemes(true).map(grapheme_width).sum()` (ASCII graphemes answered inline, non-ASCII graphemes `<= 128B` via S3-FIFO).
3. `hybrid_multibyte_only`: S3-FIFO only for `>= 3`-byte clusters, uncached for 2-byte UTF-8.

| Corpus (32 lines) | `uncached_direct` median [95% CI] | `cached_per_cluster` median [95% CI] | `hybrid_multibyte_only` median [95% CI] | Winner & Speedup vs Uncached |
|---|---|---|---|---|
| `cjk_table` | 97.5 µs [87.6, 109.0 µs] | **55.7 µs** [49.9, 62.3 µs] | 53.5 µs [48.2, 59.3 µs] | `cached_per_cluster` (**1.75x**) |
| `emoji_logs` | 101.5 µs [91.8, 112.2 µs] | **86.1 µs** [77.7, 95.5 µs] | 84.3 µs [76.3, 92.9 µs] | `cached_per_cluster` (**1.18x**) |
| `mixed_latin1` | 59.7 µs [54.3, 66.1 µs] | **45.6 µs** [42.7, 49.2 µs] | 61.6 µs [56.6, 67.5 µs] | `cached_per_cluster` (**1.31x** vs uncached, **1.35x** vs hybrid) |
| `arabic` | 123.1 µs [115.8, 131.4 µs] | **66.2 µs** [60.8, 72.6 µs] | 150.2 µs [135.5, 165.7 µs] | `cached_per_cluster` (**1.86x** vs uncached, **2.27x** vs hybrid) |

